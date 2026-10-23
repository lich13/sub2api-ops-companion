// @vitest-environment jsdom
import { act } from 'react';
import { createRoot } from 'react-dom/client';
import { beforeEach, afterEach, it, expect, vi } from 'vitest';
import TestDialog, { nextBeijingMinute, appointmentTimestamp } from './TestDialog';
import { api, command } from './bridge';
import { setOperations } from './accountOperations';
import type { Account } from './types';
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
  expect(date.type).toBe('datetime-local'); expect(date.step).toBe('60');
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

it('rounds Beijing time across midnight and rejects invalid calendar minutes', () => {
  expect(nextBeijingMinute(Date.parse('2030-01-02T15:59:59Z'))).toBe('2030-01-03T00:00');
  expect(nextBeijingMinute(Date.parse('2030-01-02T01:30:00Z'))).toBe('2030-01-02T09:31');
  expect(appointmentTimestamp('2030-01-03T00:00')).toBe(Date.parse('2030-01-02T16:00:00Z'));
  for (const value of ['', 'broken', '2030-02-30T10:00', '2030-01-02T10:01:22']) expect(Number.isNaN(appointmentTimestamp(value))).toBe(true);
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
