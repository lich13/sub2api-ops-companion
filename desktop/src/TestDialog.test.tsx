// @vitest-environment jsdom
import { act } from 'react';
import { createRoot } from 'react-dom/client';
import { beforeEach, afterEach, it, expect, vi } from 'vitest';
import TestDialog, { nextBeijingMinute, appointmentTimestamp, nextCycleAppointment } from './TestDialog';
import { api, command } from './bridge';
import { setOperations } from './accountOperations';
import type { Account, UsageSummary, UsageWindow } from './types';
vi.mock('./bridge', () => ({ api: vi.fn(), command: vi.fn(), runTest: vi.fn() }));
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
let container: HTMLDivElement, root: ReturnType<typeof createRoot>;
const account = { id: 42, name: 'Fixture', platform: 'anthropic', type: 'apikey', version: 'a'.repeat(64), operation_versions: { test: 'b'.repeat(64) } } as unknown as Account;
const models = [{ id: 'claude-custom-model', display_name: 'Claude custom', type: 'model' }, { id: 'upstream-extra', display_name: 'Upstream extra', type: 'model' }];
beforeEach(() => {
  vi.useFakeTimers(); vi.setSystemTime(new Date('2030-01-02T01:30:00Z')); vi.clearAllMocks(); localStorage.clear(); setOperations([]);
  container = document.createElement('div'); document.body.append(container); root = createRoot(container);
  vi.mocked(command).mockResolvedValue(undefined as never);
  vi.mocked(api).mockImplementation(async (method, path, body) => {
    const fields = body as Record<string, unknown> | undefined;
    if (path.endsWith('/models?purpose=model_test')) return models as never;
    if (path === '/account-operations') return { items: [] } as never;
    if (method === 'POST' && path === '/accounts/42/operations') return { id: 'c'.repeat(32), account_id: 42, action: 'test', status: 'queued', requested: fields?.payload, scheduled_at: fields?.scheduled_at } as never;
    if (path.endsWith('/cancel')) return { id: 'c'.repeat(32), account_id: 42, action: 'test', status: 'cancelled', requested: {} } as never;
    throw new Error(path);
  });
});
afterEach(async () => { await act(async () => root.unmount()); container.remove(); vi.useRealTimers(); });
async function change(element: HTMLInputElement | HTMLSelectElement, value: string) {
  await act(async () => {
    Object.getOwnPropertyDescriptor(element instanceof HTMLSelectElement ? HTMLSelectElement.prototype : HTMLInputElement.prototype, 'value')!.set!.call(element, value);
    element.dispatchEvent(new Event('input', { bubbles: true })); element.dispatchEvent(new Event('change', { bubbles: true }));
  });
}
it('uses the complete upstream directory and submits one connection test at a Beijing minute, then cancels it', async () => {
  await act(async () => root.render(<TestDialog account={account} online close={() => {}}/>));
  const model = container.querySelector<HTMLSelectElement>('[aria-label="测试模型"]')!;
  expect([...model.options].map(option => option.value)).toEqual(['', ...models.map(model => model.id)]);
  await change(container.querySelector<HTMLSelectElement>('[aria-label="执行方式"]')!, 'scheduled');
  await change(model, 'upstream-extra');
  const date = container.querySelector<HTMLInputElement>('[aria-label="预约时间"]')!;
  expect(date.type).toBe('datetime-local'); expect(date.step).toBe('1');
  const submit = () => [...container.querySelectorAll('button')].find(button => button.textContent === '预约测试')!;
  expect(date.value).toBe('2030-01-02T09:31');
  await change(date, '2030-01-02T09:30'); expect(submit().disabled).toBe(false);
  await act(async () => submit().click());
  expect(container.textContent).toContain('预约时间须晚于当前北京时间');
  expect(vi.mocked(api).mock.calls.filter(([method]) => method === 'POST')).toHaveLength(0);
  await change(date, '2030-01-02T09:31'); expect(submit().disabled).toBe(false);
  await act(async () => submit().click());
  expect(api).toHaveBeenCalledWith('POST', '/accounts/42/operations', expect.objectContaining({ action: 'test', scheduled_at: '2030-01-02T01:31:00.000Z', payload: { confirmed: true, mode: 'default', model_id: 'upstream-extra', prompt: '' } }));
  expect(container.querySelector('.test-schedules')?.textContent).toContain('upstream-extra');
  await act(async () => container.querySelector<HTMLButtonElement>('[aria-label^="取消预约"]')!.click());
  expect(container.querySelector('.test-schedules')).toBeNull();
});

it('rounds the default minute across midnight, accepts seconds and rejects invalid calendar times', () => {
  expect(nextBeijingMinute(Date.parse('2030-01-02T15:59:59Z'))).toBe('2030-01-03T00:00');
  expect(nextBeijingMinute(Date.parse('2030-01-02T01:30:00Z'))).toBe('2030-01-02T09:31');
  expect(appointmentTimestamp('2030-01-03T00:00')).toBe(Date.parse('2030-01-02T16:00:00Z'));
  expect(appointmentTimestamp('2030-01-02T10:01:22')).toBe(Date.parse('2030-01-02T02:01:22Z'));
  for (const value of ['', 'broken', '2030-02-30T10:00', '2030-01-02T10:01:60', '2030-01-02T10:01:22.123']) expect(Number.isNaN(appointmentTimestamp(value))).toBe(true);
});

const windowAt = (label: string, reset_at: string | null): UsageWindow => ({ key: label, label, reset_at, used_percent: label === '5h' ? 100 : 16, status: 'known', source: 'upstream_snapshot', observed_at: null });
const cycleUsage = (): UsageSummary => ({ branch: 'openai_oauth', native_read: true, windows: [windowAt('7d', '2030-01-09T01:16:35Z'), windowAt('5h', '2030-01-02T01:46:35Z')], actions: ['query_usage'], today: null, reset_credits: null });
it('fills the nearest refreshed cycle plus three seconds without submitting until confirmed', async () => {
  const previous = vi.mocked(api).getMockImplementation()!;
  vi.mocked(api).mockImplementation((method, path, body) => path === '/accounts/42/usage' ? Promise.resolve(cycleUsage() as never) : previous(method, path, body));
  await act(async () => root.render(<TestDialog account={{ ...account, type: 'oauth', usage: { ...cycleUsage(), windows: [windowAt('5h', '2030-01-02T01:40:00Z')] } }} online close={() => {}}/>));
  await change(container.querySelector<HTMLSelectElement>('[aria-label="执行方式"]')!, 'scheduled');
  await act(async () => [...container.querySelectorAll('button')].find(button => button.textContent === '最近周期 +3 秒')!.click());
  expect(api).toHaveBeenCalledWith('GET', '/accounts/42/usage');
  expect(appointmentTimestamp(container.querySelector<HTMLInputElement>('[aria-label="预约时间"]')!.value)).toBe(Date.parse('2030-01-02T01:46:38Z'));
  expect(vi.mocked(api).mock.calls.filter(([method]) => method === 'POST')).toHaveLength(0);
  await act(async () => [...container.querySelectorAll('button')].find(button => button.textContent === '预约测试')!.click());
  expect(api).toHaveBeenCalledWith('POST', '/accounts/42/operations', expect.objectContaining({ scheduled_at: '2030-01-02T01:46:38.000Z' }));
});
it('chooses an upcoming API Key plan, ignoring expired, invalid and inactive cycles', () => {
  const usage = { ...cycleUsage(), windows: [windowAt('old', '2030-01-01T00:00:00Z'), windowAt('unknown', 'invalid')], key_quota: {
    state: 'ok',
    plans: [{ reset_at: '2030-01-02T03:00:00Z' }], subscriptions: [
      { active: false, expires_at: null, plans: [{ reset_at: '2030-01-02T01:31:00Z' }] },
      { active: true, expires_at: '2030-01-01T00:00:00Z', plans: [{ reset_at: '2030-01-02T01:32:00Z' }] },
      { active: true, expires_at: null, plans: [{ reset_at: '2030-01-02T02:00:59Z' }] },
    ],
  } } as UsageSummary;
  expect(nextCycleAppointment(usage)).toBe(Date.parse('2030-01-02T02:01:02Z'));
  expect(nextCycleAppointment(undefined, [windowAt('unknown', null)])).toBeNull();
  expect(nextCycleAppointment({ ...usage, windows: [{ ...windowAt('failed', '2030-01-02T01:31:00Z'), status: 'error' }], key_quota: { ...usage.key_quota!, state: 'error', stale: true } })).toBeNull();
  expect(nextCycleAppointment(undefined, [windowAt('5h', '2030-01-02T15:59:59Z')])).toBe(Date.parse('2030-01-02T16:00:02Z'));
});
it('retains the edited appointment when cycle time is unavailable or its read fails', async () => {
  const previous = vi.mocked(api).getMockImplementation()!;
  let fail = false;
  vi.mocked(api).mockImplementation((method, path, body) => path === '/accounts/42/usage'
    ? fail ? Promise.reject(new Error('fixture read failed')) : Promise.resolve({ ...cycleUsage(), windows: [] } as never)
    : previous(method, path, body));
  await act(async () => root.render(<TestDialog account={{ ...account, usage: cycleUsage() }} online close={() => {}}/>));
  await change(container.querySelector<HTMLSelectElement>('[aria-label="执行方式"]')!, 'scheduled');
  const date = container.querySelector<HTMLInputElement>('[aria-label="预约时间"]')!;
  await change(date, '2030-01-02T12:00:05');
  const fill = () => [...container.querySelectorAll('button')].find(button => button.textContent === '最近周期 +3 秒')!;
  await act(async () => fill().click());
  expect(container.textContent).toContain('暂无可用的周期重置时间');
  expect(appointmentTimestamp(date.value)).toBe(Date.parse('2030-01-02T04:00:05Z'));
  fail = true;
  await act(async () => fill().click());
  expect(container.textContent).toContain('周期时间读取失败');
  expect(appointmentTimestamp(date.value)).toBe(Date.parse('2030-01-02T04:00:05Z'));
});
it('submits the default model during a slow directory load only once and retains the selection', async () => {
  let directory!: (value: never) => void, finish!: (value: never) => void;
  const previous = vi.mocked(api).getMockImplementation()!;
  vi.mocked(api).mockImplementation((method, path, body) => {
    if (path.endsWith('/models?purpose=model_test')) return new Promise(resolve => { directory = resolve; });
    if (method === 'POST') return new Promise(resolve => { finish = resolve; });
    return previous(method, path, body);
  });
  await act(async () => root.render(<TestDialog account={account} online close={() => {}}/>));
  await change(container.querySelector<HTMLSelectElement>('[aria-label="执行方式"]')!, 'scheduled');
  const submit = [...container.querySelectorAll('button')].find(button => button.textContent === '预约测试')!;
  expect(submit.disabled).toBe(false);
  await act(async () => { submit.click(); submit.click(); });
  expect(vi.mocked(api).mock.calls.filter(([method]) => method === 'POST')).toHaveLength(1);
  expect(api).toHaveBeenCalledWith('POST', '/accounts/42/operations', expect.objectContaining({ payload: expect.objectContaining({ model_id: '' }) }));
  expect(container.textContent).toContain('正在提交预约');
  await act(async () => directory(models as never));
  expect(container.querySelector<HTMLSelectElement>('[aria-label="测试模型"]')!.value).toBe('');
  await act(async () => finish({ id: 'c'.repeat(32), account_id: 42, action: 'test', status: 'queued', requested: {} } as never));
});
it('keeps edits while open, recalculates on reopen and explains an empty or stale time', async () => {
  await act(async () => root.render(<TestDialog account={account} online close={() => {}}/>));
  const execution = container.querySelector<HTMLSelectElement>('[aria-label="执行方式"]')!;
  await change(execution, 'scheduled');
  await change(container.querySelector<HTMLInputElement>('[aria-label="预约时间"]')!, '2030-01-02T12:00');
  await change(execution, 'now'); await change(execution, 'scheduled');
  expect(container.querySelector<HTMLInputElement>('[aria-label="预约时间"]')!.value).toBe('2030-01-02T12:00');
  await change(container.querySelector<HTMLInputElement>('[aria-label="预约时间"]')!, '');
  await act(async () => [...container.querySelectorAll('button')].find(button => button.textContent === '预约测试')!.click());
  expect(container.textContent).toContain('请填写预约时间');
  await act(async () => root.render(null));
  vi.setSystemTime(new Date('2030-01-02T02:40:12Z'));
  await act(async () => root.render(<TestDialog account={account} online close={() => {}}/>));
  await change(container.querySelector<HTMLSelectElement>('[aria-label="执行方式"]')!, 'scheduled');
  expect(container.querySelector<HTMLInputElement>('[aria-label="预约时间"]')!.value).toBe('2030-01-02T10:41');
  vi.setSystemTime(new Date('2030-01-02T02:42:00Z'));
  await act(async () => [...container.querySelectorAll('button')].find(button => button.textContent === '预约测试')!.click());
  expect(container.textContent).toContain('预约时间须晚于当前北京时间');
});
it('explains a failed directory while allowing the default model and shows offline state', async () => {
  const previous = vi.mocked(api).getMockImplementation()!;
  vi.mocked(api).mockImplementation((method, path, body) => path.endsWith('/models?purpose=model_test') ? Promise.reject(new Error('fixture')) : previous(method, path, body));
  await act(async () => root.render(<TestDialog account={account} online close={() => {}}/>));
  expect(container.textContent).toContain('模型列表读取失败');
  await change(container.querySelector<HTMLSelectElement>('[aria-label="执行方式"]')!, 'scheduled');
  expect([...container.querySelectorAll('button')].find(button => button.textContent === '预约测试')!.disabled).toBe(false);
  await act(async () => root.render(<TestDialog account={account} online={false} close={() => {}}/>));
  expect(container.textContent).toContain('当前离线');
  expect([...container.querySelectorAll('button')].find(button => button.textContent === '预约测试')!.disabled).toBe(true);
});

it.each(['scheduled', 'cycle'])('closes only after the server accepts a %s appointment', async execution => {
  const close = vi.fn();
  const current = { ...account, platform: 'openai', type: 'oauth' };
  await act(async () => root.render(<TestDialog account={current} online close={close}/>));
  await change(container.querySelector<HTMLSelectElement>('[aria-label="执行方式"]')!, execution);
  await act(async () => [...container.querySelectorAll('button')].find(button => button.textContent === '预约测试')!.click());
  expect(close).toHaveBeenCalledTimes(1);
  const body = vi.mocked(api).mock.calls.find(([method, path]) => method === 'POST' && path.endsWith('/operations'))![2] as Record<string, unknown>;
  if (execution === 'cycle') {
    expect(body.repeat).toBe('quota_cycle');
    expect(body.scheduled_at).toBeUndefined();
    expect(body.payload).toMatchObject({ mode: 'default', model_id: '' });
  } else expect(body.repeat).toBeUndefined();
});

it.each(['rejected', 'needs_confirmation'])('keeps the appointment dialog open when submission is %s', async status => {
  const close = vi.fn(), previous = vi.mocked(api).getMockImplementation()!;
  vi.mocked(api).mockImplementation((method, path, body) => method === 'POST'
    ? status === 'rejected' ? Promise.reject(new Error('fixture submission failed')) : Promise.resolve({ id: 'c'.repeat(32), status, reason: 'fixture confirmation required', requested: {} } as never)
    : previous(method, path, body));
  await act(async () => root.render(<TestDialog account={account} online close={close}/>));
  await change(container.querySelector<HTMLSelectElement>('[aria-label="执行方式"]')!, 'scheduled');
  await act(async () => [...container.querySelectorAll('button')].find(button => button.textContent === '预约测试')!.click());
  expect(close).not.toHaveBeenCalled();
  expect(container.querySelector('[role="alert"]')?.textContent).toContain('fixture');
});

it('offers continuous appointments only for OpenAI OAuth and prevents a second active series', async () => {
  await act(async () => root.render(<TestDialog account={account} online close={() => {}}/>));
  expect(container.querySelector('option[value="cycle"]')).toBeNull();
  await act(async () => root.render(<TestDialog account={{ ...account, platform: 'openai', type: 'oauth' }} online close={() => {}}/>));
  await change(container.querySelector<HTMLSelectElement>('[aria-label="执行方式"]')!, 'cycle');
  await act(async () => setOperations([{ id: 'c'.repeat(32), account_id: 42, account_name: 'Fixture', action: 'test', status: 'running', requested: {}, repeat: 'quota_cycle', repeat_state: 'active', repeat_series_id: 'c'.repeat(32) }]));
  expect([...container.querySelectorAll('button')].find(button => button.textContent === '预约测试')!.disabled).toBe(true);
  expect(container.querySelector('[aria-label="停止连续预约"]')).not.toBeNull();
});
