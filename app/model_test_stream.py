"""Account-pinned HTTP collection; transport completion is separate from scoring."""
from __future__ import annotations

import json
import re
import time
from contextlib import asynccontextmanager

import httpx

MAX_TEXT = 32 * 1024
MAX_FRAME = 256 * 1024
MAX_OUTPUT_TOKENS = 4096


class TestFailure(Exception):
    def __init__(self, code: str, retryable: bool = False):
        self.code, self.retryable = code, retryable
        super().__init__(code)


def failure(status: int, payload: dict | None = None) -> TestFailure:
    error = (payload or {}).get("error") or payload or {}
    text = str(error).lower()
    if status in {401, 402, 403} or any(x in text for x in ("unauthorized", "invalid_api_key", "invalid api key", "authentication_error", "insufficient_quota", "billing", "token_expired")):
        return TestFailure("auth_or_quota")
    if status == 429 or any(x in text for x in ("rate_limit", "usage_limit", "quota_exceeded", "concurrency limit exceeded", "concurrency_limit_exceeded")):
        return TestFailure("rate_limited")
    if status in {400, 404, 422} or any(x in text for x in ("model_not_found", "invalid_request", "invalid_parameter")):
        return TestFailure("invalid_model_or_request")
    return TestFailure("upstream_error", True)


def isolated_answer(text: str) -> str:
    """Hide explicit thought blocks, including an unfinished tag at a chunk edge."""
    stack, parts, cursor = [], [], 0
    for match in re.finditer(r'<\s*(/?)\s*(think|thinking|reasoning|analysis|seed:think)\s*>', text, re.I):
        if not stack:
            parts.append(text[cursor:match.start()])
        if match[1]:
            if stack and stack[-1] == match[2].lower():
                stack.pop()
        else:
            stack.append(match[2].lower())
        cursor = match.end()
    if not stack:
        tail = text[cursor:]
        if '<' in tail and '>' not in tail[tail.rfind('<'):]:
            tail = tail[:tail.rfind('<')]
        parts.append(tail)
    return ''.join(parts)


class Collector:
    def __init__(self, expected: int):
        self.expected = expected
        self.parts: dict[tuple, str] = {}
        self.channels: dict[tuple, str] = {}
        self.completed = False
        self.returned_model: str | None = None
        self.recognized = False
        self.protocol: str | None = None
        self.end_reason = ""
        self.metadata: dict = {}

    def response_metadata(self, response):
        status = response.get('status')
        if isinstance(status, str) and status in {'completed', 'incomplete', 'failed', 'in_progress', 'queued', 'cancelled'}:
            self.metadata['provider_status'] = status
        usage = response.get('usage') or {}
        if isinstance(usage, dict):
            details = usage.get('output_tokens_details') or usage.get('completion_tokens_details') or {}
            for key, value in {'input_tokens': usage.get('input_tokens', usage.get('prompt_tokens')),
                               'output_tokens': usage.get('output_tokens', usage.get('completion_tokens')),
                               'reasoning_tokens': details.get('reasoning_tokens') if isinstance(details, dict) else None}.items():
                if type(value) is int and 0 <= value <= 10**12:
                    self.metadata[key] = value

    def incomplete(self, response):
        details = response.get('incomplete_details') or {}
        reason = details.get('reason') if isinstance(details, dict) else None
        # A provider-declared budget/filter terminal is not a broken connection.
        # Keep any answer for scoring; an empty/short answer never causes a paid retry.
        if isinstance(reason, str) and reason in {'max_output_tokens', 'max_tokens', 'content_filter'}:
            self.response_output(response)
            self.completed, self.end_reason = True, reason
            self.metadata['incomplete_reason'] = reason
            return
        self.metadata['incomplete_reason'] = 'unknown'
        raise TestFailure('incomplete_response')

    @property
    def text(self):
        return ''.join(value for key, value in sorted(self.parts.items())
                       if self.channels.get(key, "final") == "final")

    def update(self, key: tuple, value: str, snapshot=False, channel="final", *, guard=True):
        if not isinstance(value, str):
            return
        # A cumulative/final snapshot replaces its own block. It is not another delta.
        text = value if snapshot else self.parts.get(key, "") + value
        if sum(len(v.encode()) for k, v in self.parts.items() if k != key) + len(text.encode()) > MAX_TEXT:
            raise TestFailure("output_limit", True)
        self.parts[key], self.channels[key] = text, channel
        if guard and not snapshot and channel == "final":
            answer = isolated_answer(self.text)
            complete = list(re.finditer(r'(?<![\w.+-])[+-]?\d+(?![\w.+-])', answer))
            if sum(m.end() < len(answer) for m in complete) > self.expected * 2:
                raise TestFailure("output_limit", True)

    def response_output(self, response):
        if isinstance(response.get('model'), str):
            self.returned_model = response['model'][:200]
        for i, item in enumerate(response.get('output') or []):
            if not isinstance(item, dict):
                continue
            channel = 'final' if item.get('type') != 'reasoning' and item.get('phase') in (None, 'final', 'final_answer') else 'reasoning'
            for j, content in enumerate(item.get('content') or []):
                if isinstance(content, dict) and content.get('type') in {'output_text', 'text'}:
                    self.update((i, j), content.get('text', ''), snapshot=True, channel=channel, guard=False)
            for j, content in enumerate(item.get('summary') or []):
                if isinstance(content, dict):
                    self.update((i, 1000+j), content.get('text', ''), snapshot=True, channel='reasoning', guard=False)
        if isinstance(response.get('output_text'), str) and not response.get('output'):
            self.update((0, 0), response['output_text'], snapshot=True, guard=False)

    def accept(self, event: dict, event_name: str = '', *, whole=False):
        kind = event.get('type') or event_name
        response = event.get('response') or {}
        if not isinstance(kind, str) or not isinstance(response, dict):
            raise TestFailure('protocol_mismatch')
        self.response_metadata(response if response else event)
        if event.get('error') or response.get('error') or kind in {'error', 'response.failed'}:
            raise failure(0, response or event)
        if kind == 'response.incomplete':
            self.recognized, self.protocol = True, 'responses'
            self.incomplete(response)
            return
        if 'choices' in event:
            self.recognized, self.protocol = True, 'chat_completions'
            if isinstance(event.get('model'), str):
                self.returned_model = event['model'][:200]
            for choice in event.get('choices') or []:
                if not isinstance(choice, dict) or choice.get('index', 0) != 0:
                    continue
                message = choice.get('message') if whole else choice.get('delta')
                message = message if isinstance(message, dict) else {}
                self.update((0, 0), message.get('content', ''), snapshot=whole, guard=not whole)
                self.update((0, 1000), message.get('reasoning_content', message.get('reasoning', '')), snapshot=whole, channel='reasoning', guard=False)
                if choice.get('finish_reason') is not None:
                    self.completed, self.end_reason = True, str(choice['finish_reason'])[:40]
            return
        if whole and ('output' in event or 'output_text' in event or event.get('object') == 'response'):
            self.recognized, self.protocol = True, 'responses'
            if event.get('status') == 'failed':
                raise failure(0, event)
            if event.get('status') == 'incomplete':
                self.incomplete(event)
                return
            if event.get('status') != 'completed':
                raise TestFailure('incomplete_stream', True)
            self.response_output(event)
            self.completed, self.end_reason = True, 'completed'
            return
        if isinstance(kind, str) and kind.startswith(('response.', 'reasoning.')):
            self.recognized, self.protocol = True, 'responses'
        index, part = event.get('output_index', 0), event.get('content_index', 0)
        if type(index) is not int or type(part) is not int or type(event.get('summary_index', 0)) is not int:
            raise TestFailure('protocol_mismatch')
        if kind == 'response.output_item.done':
            item = event.get('item')
            if isinstance(item, dict):
                for j, content in enumerate(item.get('content') or []):
                    if isinstance(content, dict) and content.get('type') in {'output_text', 'text'}:
                        self.update((index, j), content.get('text', ''), snapshot=True, channel=self.channels.get((index, 0), 'final'), guard=False)
        if kind == 'response.output_item.added':
            item = event.get('item') or {}
            self.channels[(index, 0)] = 'final' if item.get('type') != 'reasoning' and item.get('phase') in (None, 'final', 'final_answer') else 'reasoning'
        if kind.startswith('response.output_text.') and kind.endswith(('delta', 'done', 'snapshot')):
            self.update((index, part), event.get('delta', event.get('text', '')),
                        snapshot=not kind.endswith('delta'), channel=self.channels.get((index, 0), 'final'))
        elif 'reasoning' in kind and kind.endswith(('delta', 'done')):
            self.update((index, 1000+event.get('summary_index', 0)), event.get('delta', event.get('text', '')),
                        snapshot=kind.endswith('done'), channel='reasoning', guard=False)
        if kind == 'response.completed':
            if response.get('status') == 'incomplete':
                self.incomplete(response)
                return
            if response.get('status') not in (None, 'completed'):
                raise TestFailure('incomplete_stream', True)
            self.response_output(response)
            self.completed, self.end_reason = True, 'completed'


@asynccontextmanager
async def client_scope(client, factory, proxy):
    if client is not None:
        yield client
    else:
        async with factory(timeout=httpx.Timeout(None, connect=10, write=10, pool=10),
                           follow_redirects=False, trust_env=False, proxy=proxy) as created:
            yield created


async def execute(url: str, headers: dict, proxy: str | None, model: str, prompt: str, expected: int,
                  *, oauth: bool, protocol='responses', client_factory=httpx.AsyncClient, client=None,
                  on_first_text=None, on_stage=None, diagnostics=None) -> tuple[str, str | None]:
    payload = ({'model': model, 'messages': [{'role': 'user', 'content': prompt}], 'stream': True,
                'max_completion_tokens': MAX_OUTPUT_TOKENS} if protocol == 'chat_completions' else
               {'model': model, 'input': [{'role': 'user', 'content': [{'type': 'input_text', 'text': prompt}]}],
                'instructions': 'You are a helpful assistant.', 'stream': True, 'store': False})
    if not oauth and protocol == 'responses':
        payload['max_output_tokens'] = MAX_OUTPUT_TOKENS
    collector = Collector(expected)
    started, first_text = time.monotonic(), False
    diag = diagnostics if diagnostics is not None else {}
    diag.update(protocol=protocol, bytes=0, events=0)
    if not oauth:
        diag['max_output_tokens'] = MAX_OUTPUT_TOKENS

    async def received(event, name='', whole=False):
        nonlocal first_text
        diag['events'] += 1
        event_type = event.get('type') or name or ('chat.completion' if 'choices' in event else 'response' if whole else 'unknown')
        if not isinstance(event_type, str) or not re.fullmatch(r'[A-Za-z0-9._-]{1,80}', event_type):
            event_type = 'unknown'
        diag.setdefault('first_event_type', event_type)
        diag['last_event_type'] = event_type
        if 'first_event_ms' not in diag:
            diag['first_event_ms'] = round((time.monotonic()-started)*1000)
        try:
            collector.accept(event, name, whole=whole)
        finally:
            diag.update(collector.metadata)
            diag['response_protocol'] = collector.protocol
        if isolated_answer(collector.text) and not first_text:
            first_text = True
            diag['ttft_ms'] = round((time.monotonic()-started)*1000)
            if on_first_text:
                await on_first_text(diag['ttft_ms'])

    async def frame(lines):
        name, data = '', []
        for line in lines:
            if line.startswith(b'event:'):
                name = line[6:].strip().decode('utf-8', errors='replace')
            elif line.startswith(b'data:'):
                data.append(line[5:].lstrip(b' '))
        if not data:
            return
        raw = b'\n'.join(data).strip()
        if raw == b'[DONE]':
            if collector.protocol == 'chat_completions' and collector.text:
                collector.completed, collector.end_reason = True, 'done'
            return
        try:
            event = json.loads(raw)
        except (ValueError, UnicodeError):
            raise TestFailure('invalid_stream', True) from None
        if not isinstance(event, dict):
            raise TestFailure('protocol_mismatch')
        await received(event, name)

    try:
        async with client_scope(client, client_factory, proxy) as active_client:
            async with active_client.stream('POST', url, headers=headers, json=payload) as response:
                diag.update(http_status=response.status_code,
                            content_type=response.headers.get('content-type', '').split(';')[0][:80],
                            headers_ms=round((time.monotonic()-started)*1000))
                if on_stage:
                    await on_stage('receiving')
                if response.status_code != 200:
                    raw = b''
                    async for chunk in response.aiter_bytes():
                        raw += chunk[:MAX_TEXT-len(raw)]
                        if len(raw) >= MAX_TEXT:
                            break
                    try:
                        error = json.loads(raw)
                    except (ValueError, UnicodeError):
                        error = {}
                    raise failure(response.status_code, error if isinstance(error, dict) else {})
                buffer, lines, frame_size, mode = b'', [], 0, None
                async for chunk in response.aiter_bytes():
                    diag['bytes'] += len(chunk)
                    buffer += chunk
                    if mode is None and buffer.strip():
                        mode = 'json' if buffer.lstrip().startswith(b'{') else 'sse'
                    if mode == 'json':
                        if len(buffer) > MAX_FRAME:
                            raise TestFailure('output_limit', True)
                        continue
                    while b'\n' in buffer:
                        line, buffer = buffer.split(b'\n', 1)
                        line = line.rstrip(b'\r')
                        frame_size += len(line)
                        if frame_size > MAX_FRAME:
                            raise TestFailure('output_limit', True)
                        if not line:
                            await frame(lines)
                            lines, frame_size = [], 0
                            if collector.completed:
                                return collector.text, collector.returned_model
                        else:
                            lines.append(line)
                    if len(buffer) + frame_size > MAX_FRAME:
                        raise TestFailure('output_limit', True)
                if mode == 'json':
                    try:
                        event = json.loads(buffer)
                    except (ValueError, UnicodeError):
                        raise TestFailure('invalid_stream', True) from None
                    if not isinstance(event, dict):
                        raise TestFailure('protocol_mismatch')
                    await received(event, whole=True)
                else:
                    if buffer:
                        lines.append(buffer.rstrip(b'\r'))
                    await frame(lines)
                if collector.completed:
                    return collector.text, collector.returned_model
                raise TestFailure('incomplete_stream', True) if collector.recognized else TestFailure('protocol_mismatch')
    except TestFailure as exc:
        diag['end_reason'] = exc.code
        raise
    except httpx.HTTPError:
        diag['end_reason'] = 'network_error'
        raise
    finally:
        diag['duration_ms'] = round((time.monotonic()-started)*1000)
        diag.setdefault('end_reason', collector.end_reason or 'cancelled')
