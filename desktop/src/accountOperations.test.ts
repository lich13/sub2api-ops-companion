import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { accountOperation, bindOperationConnection, followAccountBatch, followOperation, getOperations, setOperations, submitAccountBatch, submitOperation, type Operation, type OperationBatch } from './accountOperations';
import { api } from './bridge';
import type { Account } from './types';
vi.mock('./bridge', () => ({ api: vi.fn(), command: vi.fn().mockResolvedValue(undefined) }));
const account = { id: 421, name: 'Key', version: 'v'.repeat(64), operation_versions: { priority: 'p'.repeat(64) } } as unknown as Account;
const job: Operation = { id: 'a'.repeat(32), account_id: 421, account_name: 'Key', action: 'priority', status: 'queued', requested: { priority: 4 } };
beforeEach(() => { vi.clearAllMocks(); setOperations([]); bindOperationConnection(crypto.randomUUID()); });
afterEach(() => vi.useRealTimers());
describe('durable operations', () => {
  it('keeps a stable in-memory client ID when access to browser storage is denied', async () => {
    vi.resetModules();
    const { clientId } = await import('./accountOperations');
    const descriptor = Object.getOwnPropertyDescriptor(globalThis, 'localStorage');
    Object.defineProperty(globalThis, 'localStorage', {
      configurable: true,
      get() { throw new DOMException('fixture storage denied', 'SecurityError'); },
    });
    try {
      const first = clientId();
      expect(first).toMatch(/^[0-9a-f-]{36}$/);
      expect(clientId()).toBe(first);
    } finally {
      if (descriptor) Object.defineProperty(globalThis, 'localStorage', descriptor);
      else Reflect.deleteProperty(globalThis, 'localStorage');
    }
  });

  it('submits delete and group changes as one idempotent account batch', async () => {
    const batch: OperationBatch = {
      batch_id: 'b'.repeat(32),
      pending: 0,
      items: [{ ...job, batch_id: 'b'.repeat(32), action: 'delete', status: 'completed', requested: {} }],
    };
    vi.mocked(api).mockResolvedValue(batch);
    const requestId = 'request-delete-1';
    await submitAccountBatch('delete', [{ account_id: 421, expected_version: account.version, payload: {} }], requestId);
    expect(api).toHaveBeenCalledExactlyOnceWith('POST', '/account-operations/batch', {
      action: 'delete', payload: { confirmed: true },
      accounts: [{ account_id: 421, expected_version: account.version, payload: {} }],
      request_id: requestId, client_id: expect.any(String),
    });
    expect(getOperations()).toEqual(batch.items);

    vi.mocked(api).mockResolvedValue({ ...batch, items: [{ ...batch.items[0], action: 'groups', requested: { scope_group_ids: [1, 2], group_ids: [2] } }] });
    await submitAccountBatch('groups', [{ account_id: 421, expected_version: account.version, payload: { scope_group_ids: [1, 2], group_ids: [2] } }], 'request-groups-1');
    expect(api).toHaveBeenLastCalledWith('POST', '/account-operations/batch', {
      action: 'groups', payload: {},
      accounts: [{ account_id: 421, expected_version: account.version, payload: { scope_group_ids: [1, 2], group_ids: [2] } }],
      request_id: 'request-groups-1', client_id: expect.any(String),
    });
  });

  it('polls a nonterminal account batch and keeps the terminal result in operations', async () => {
    vi.useFakeTimers();
    const initial: OperationBatch = {
      batch_id: 'c'.repeat(32), pending: 1,
      items: [{ ...job, batch_id: 'c'.repeat(32), status: 'queued', action: 'groups' }],
    };
    const completed: OperationBatch = {
      ...initial, pending: 0,
      items: [{ ...initial.items[0], status: 'completed', reason: '', result: { verified: true } }],
    };
    vi.mocked(api).mockResolvedValue(completed);
    const updates: OperationBatch[] = [];
    const followed = followAccountBatch(initial, (value) => updates.push(value));
    await vi.advanceTimersByTimeAsync(0);
    await vi.advanceTimersByTimeAsync(1000);
    await followed;
    expect(api).toHaveBeenCalledExactlyOnceWith('GET', '/account-operations?batch_id=' + initial.batch_id);
    expect(updates.map((value) => value.items[0].status)).toEqual(['queued', 'completed']);
    expect(getOperations()[0].status).toBe('completed');
  });

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
