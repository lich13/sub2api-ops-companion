import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { accountOperation, bindOperationConnection, followOperation, getOperations, setOperations, submitOperation, type Operation } from './accountOperations';
import { api } from './bridge';
import type { Account } from './types';
vi.mock('./bridge', () => ({ api: vi.fn(), command: vi.fn().mockResolvedValue(undefined) }));
const account = { id: 421, name: 'Key', version: 'v'.repeat(64), operation_versions: { priority: 'p'.repeat(64) } } as unknown as Account;
const job: Operation = { id: 'a'.repeat(32), account_id: 421, account_name: 'Key', action: 'priority', status: 'queued', requested: { priority: 4 } };
beforeEach(() => { vi.clearAllMocks(); setOperations([]); bindOperationConnection(crypto.randomUUID()); });
afterEach(() => vi.useRealTimers());
describe('durable operations', () => {
  it('submits field version and shows queued request without treating it as a conflict', async () => {
    vi.mocked(api).mockResolvedValue(job);
    const result = await submitOperation(account, 'priority', { priority: 4 });
    expect(result.status).toBe('queued');
    expect(api).toHaveBeenCalledWith('POST', '/accounts/421/operations', expect.objectContaining({ expected_version: 'p'.repeat(64), payload: { priority: 4 }, request_id: expect.any(String) }));
    expect(getOperations()).toHaveLength(1);
  });
  it('keeps real conflict for confirmation', async () => {
    vi.mocked(api).mockResolvedValue({ ...job, status: 'needs_confirmation', reason: '优先级已变化', current: { priority: 6 } });
    await expect(accountOperation(account, 'priority', { priority: 4 })).rejects.toThrow('优先级已变化');
    expect(getOperations()[0].requested.priority).toBe(4);
    expect(getOperations()[0].current?.priority).toBe(6);
  });
  it('discards old connection responses without cancelling durable request', async () => {
    vi.useFakeTimers();
    let finish: (value: unknown) => void = () => {};
    vi.mocked(api).mockImplementation(() => new Promise((resolve) => { finish = resolve; }));
    const pending = followOperation(job).catch((e: Error) => e.message);
    await vi.advanceTimersByTimeAsync(500);
    bindOperationConnection('next');
    finish({ ...job, status: 'completed', result: { verified: true } });
    expect(await pending).toContain('连接已切换');
    expect(getOperations()).toEqual([]);
    expect(api).toHaveBeenCalledTimes(1);
  });
});
