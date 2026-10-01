import json
import unittest

from app.model_test_stream import TestFailure
from tests import test_modeltrace022 as execution
from tests import test_modeltrace023 as transport


def terminal(reason, text='', status='incomplete'):
    return {'object': 'response', 'status': status, 'model': 'gpt-6-luna',
            'incomplete_details': {'reason': reason},
            'output': [{'type': 'message', 'content': [{'type': 'output_text', 'text': text}]}],
            'usage': {'input_tokens': 120, 'output_tokens': 4096,
                      'output_tokens_details': {'reasoning_tokens': 3890}}}


class TerminalHTTPTests(unittest.IsolatedAsyncioTestCase):
    asyncSetUp = transport.Transport023Tests.asyncSetUp
    request = transport.Transport023Tests.request

    async def test_provider_budget_and_filter_terminal_keep_partial_samples(self):
        for reason in ('max_output_tokens', 'max_tokens', 'content_filter'):
            for mode in ('sse', 'json'):
                with self.subTest(reason=reason, mode=mode):
                    text = '' if reason == 'content_filter' else execution.OUTPUT
                    response = terminal(reason, text)
                    self.content_type = 'application/json' if mode == 'json' else 'text/event-stream'
                    self.body = (json.dumps(response) if mode == 'json' else
                                 'event: response.incomplete\r\ndata: ' + json.dumps({'response': response}) + '\r\n\r\n').encode()
                    before = len(self.requests)
                    result, diag = await self.request()
                    self.assertEqual(result, (text, 'gpt-6-luna'))
                    self.assertEqual(len(self.requests)-before, 1)
                    self.assertEqual(diag['end_reason'], reason)
                    self.assertEqual(diag['provider_status'], 'incomplete')
                    self.assertEqual((diag['output_tokens'], diag['reasoning_tokens']), (4096, 3890))
                    self.assertEqual(diag['max_output_tokens'], 4096)
                    self.assertEqual(diag['last_event_type'], 'response' if mode == 'json' else 'response.incomplete')
                    for sensitive in (execution.OUTPUT, 'fixture challenge', 'fixture-key'):
                        self.assertNotIn(sensitive, json.dumps(diag))

    async def test_unknown_terminal_is_distinct_from_retryable_disconnection(self):
        self.content_type = 'text/event-stream'
        self.body = ('data: ' + json.dumps({'type': 'response.incomplete', 'response': terminal('unknown reason')}) + '\n\n').encode()
        with self.assertRaises(TestFailure) as error:
            await self.request()
        self.assertEqual((error.exception.code, error.exception.retryable), ('incomplete_response', False))
        self.body = b'data: {"type":"response.created","response":{"status":"in_progress"}}\n\n'
        with self.assertRaises(TestFailure) as error:
            await self.request()
        self.assertEqual((error.exception.code, error.exception.retryable), ('incomplete_stream', True))

    async def test_actual_payload_budget_and_terminal_diagnostics_for_both_protocols(self):
        for extra in ({}, {'openai_responses_mode': 'force_chat_completions'}):
            chat = bool(extra)
            self.content_type = 'text/event-stream'
            events = ([{'choices': [{'delta': {'content': execution.OUTPUT}, 'finish_reason': 'length'}],
                        'usage': {'completion_tokens': 4096, 'completion_tokens_details': {'reasoning_tokens': 3890}}}]
                      if chat else [{'type': 'response.created', 'response': {'status': 'in_progress'}},
                                    {'type': 'response.completed', 'response': terminal('max_output_tokens', execution.OUTPUT)}])
            self.body = ''.join('data: ' + json.dumps(event) + '\n\n' for event in events).encode()
            result, diag = await self.request(extra=extra)
            self.assertEqual(result[0], execution.OUTPUT)
            body = self.requests[-1]['body']
            self.assertEqual(body['max_completion_tokens' if chat else 'max_output_tokens'], 4096)
            self.assertFalse({'tools', 'reasoning', 'reasoning_effort', 'thinking'} & body.keys())
            self.assertEqual(diag['end_reason'], 'length' if chat else 'max_output_tokens')
            self.assertEqual(diag['first_event_type'], 'chat.completion' if chat else 'response.created')
            self.assertEqual(diag['last_event_type'], 'chat.completion' if chat else 'response.completed')
            self.assertEqual(diag['reasoning_tokens'], 3890)


class TerminalTaskHTTPTests(unittest.IsolatedAsyncioTestCase):
    start = execution.ModelConcurrencyTests.start

    async def asyncSetUp(self):
        await transport.Transport023Tests.asyncSetUp(self)
        await execution.ModelConcurrencyTests.asyncSetUp(self)
        self.row['credentials']['base_url'] = self.url
        self.content_type = 'text/event-stream'

    async def test_explicit_token_terminal_uses_three_requests_not_nine(self):
        for output in ('', execution.OUTPUT):
            self.body = ('data: ' + json.dumps({'type': 'response.incomplete',
                         'response': terminal('max_output_tokens', output)}) + '\n\n').encode()
            before = len(self.requests)
            job = await self.start(3)
            await self.tests.tasks[job['id']]
            result = self.tests.get(job['id'])
            self.assertEqual((len(self.requests)-before, result['attempts']), (3, 3))
            self.assertEqual(result['completed_groups'], 3)
            self.assertEqual(result['valid_groups'], 3 if output else 0)
            self.assertEqual(result['status'], 'completed')
            self.assertFalse(result['can_retry'])
            self.assertTrue(all(group['attempts'] == 1 and group['diagnostics']['end_reason'] == 'max_output_tokens'
                                for group in result['groups']))
            self.assertNotIn(execution.OUTPUT, self.tests.store.path.read_text())
