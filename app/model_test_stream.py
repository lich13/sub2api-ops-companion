"""Bounded Responses stream collection. Only final answer text is scored."""
from __future__ import annotations

import json
import re

import httpx


class TestFailure(Exception):
    def __init__(self, code: str, retryable: bool = False):
        self.code, self.retryable = code, retryable
        super().__init__(code)


def failure(status: int, payload: dict | None = None) -> TestFailure:
    error = (payload or {}).get("error") or payload or {}
    text = str(error).lower()
    if status in {401, 402, 403} or any(x in text for x in ("unauthorized", "insufficient_quota", "billing", "token_expired")):
        return TestFailure("auth_or_quota")
    if status == 429 or any(x in text for x in ("rate_limit", "usage_limit", "quota_exceeded")):
        return TestFailure("rate_limited")
    if status in {400, 404, 422} or any(x in text for x in ("model_not_found", "invalid_request", "invalid_parameter")):
        return TestFailure("invalid_model_or_request")
    return TestFailure("upstream_error", True)


class Collector:
    def __init__(self, expected: int):
        self.expected = expected
        self.parts: dict[tuple, str] = {}
        self.channels: dict[tuple, str] = {}
        self.completed = False
        self.returned_model: str | None = None

    @property
    def text(self):
        return ''.join(value for key, value in sorted(self.parts.items())
                       if self.channels.get(key, "final") == "final")

    def update(self, key: tuple, value: str, snapshot=False, channel="final"):
        if not isinstance(value, str):
            return
        old = self.parts.get(key, "")
        if snapshot:
            # A final snapshot repeats prior deltas. Never append it again.
            if old and not value.startswith(old) and not old.startswith(value):
                raise TestFailure("inconsistent_stream", True)
            text = value if len(value) >= len(old) else old
        else:
            text = old + value
        if sum(len(v.encode()) for k, v in self.parts.items() if k != key) + len(text.encode()) > 32768:
            raise TestFailure("output_limit", True)
        self.parts[key], self.channels[key] = text, channel
        if len(re.findall(r'[+-]?\d+', self.text)) > self.expected * 2:
            raise TestFailure("output_limit", True)

    def accept(self, event: dict):
        kind = event.get("type", "")
        response = event.get("response") or {}
        if kind == "response.completed" and isinstance(response.get("model"), str):
            self.returned_model = response["model"][:200]
        index = event.get("output_index", 0)
        part = event.get("content_index", 0)
        if kind == "response.incomplete":
            raise TestFailure("incomplete_stream", True)
        if kind in {"error", "response.failed"}:
            raise failure(0, response or event)
        if kind == "response.output_item.added":
            item = event.get("item") or {}
            self.channels[(index, 0)] = "final" if item.get("phase") in (None, "final", "final_answer") else "reasoning"
        if kind.startswith("response.output_text.") and kind.endswith(("delta", "done", "snapshot")):
            self.update((index, part), event.get("delta", event.get("text", "")),
                        snapshot=not kind.endswith("delta"), channel=self.channels.get((index, 0), "final"))
        elif "reasoning" in kind and kind.endswith(("delta", "done")):
            self.update((index, 1000 + event.get("summary_index", 0)), event.get("delta", event.get("text", "")),
                        snapshot=kind.endswith("done"), channel="reasoning")
        if kind == "response.completed":
            if response.get("status") not in (None, "completed"):
                raise TestFailure("incomplete_stream", True)
            for i, item in enumerate(response.get("output") or []):
                channel = "final" if item.get("type") != "reasoning" and item.get("phase") in (None, "final", "final_answer") else "reasoning"
                for j, content in enumerate(item.get("content") or []):
                    if content.get("type") == "output_text":
                        self.update((i, j), content.get("text", ""), snapshot=True, channel=channel)
            self.completed = True


async def execute(url: str, headers: dict, proxy: str | None, model: str, prompt: str, expected: int,
                  *, oauth: bool, client_factory=httpx.AsyncClient) -> tuple[str, str | None]:
    payload = {"model": model, "input": [{"role": "user", "content": [{"type": "input_text", "text": prompt}]}],
               "instructions": "You are a helpful assistant.", "stream": True, "store": False}
    # Codex's consumer endpoint rejects max_output_tokens. Its stream uses the
    # same client-side guards; API Key endpoints also receive a token budget.
    if not oauth:
        payload["max_output_tokens"] = min(4096, max(1024, expected * 4))
    collector = Collector(expected)
    timeout = httpx.Timeout(None, connect=10, write=10, pool=10)
    async with client_factory(timeout=timeout, follow_redirects=False, trust_env=False, proxy=proxy) as client:
        async with client.stream("POST", url, headers=headers, json=payload) as response:
            if response.status_code != 200:
                raise failure(response.status_code)
            buffer = b''
            # Bound each SSE frame before parsing it, including a single huge chunk.
            async for chunk in response.aiter_bytes(chunk_size=4096):
                buffer += chunk
                while b'\n' in buffer:
                    line, buffer = buffer.split(b'\n', 1)
                    if len(line) > 131072:
                        raise TestFailure("output_limit", True)
                    if not line.startswith(b'data:') or line[5:].strip() == b'[DONE]':
                        continue
                    try:
                        event = json.loads(line[5:])
                    except (ValueError, UnicodeError):
                        raise TestFailure("invalid_stream", True) from None
                    if isinstance(event, dict):
                        collector.accept(event)
                    if collector.completed:
                        return collector.text, collector.returned_model
                if len(buffer) > 131072:
                    raise TestFailure("output_limit", True)
    raise TestFailure("incomplete_stream", True)
