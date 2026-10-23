import { expect, it, vi } from 'vitest';
import { api } from './bridge';
import { readNativeUsage } from './nativeUsage';
import type { UsageSummary } from './types';
vi.mock('./bridge', () => ({ api: vi.fn() }));

it('bounds automatic reads to four, merges duplicates and skips stale queued connections', async () => {
  const finishes: (() => void)[] = [];
  const value = { status: 'known' } as UsageSummary;
  vi.mocked(api).mockImplementation(() => new Promise(resolve => finishes.push(() => resolve(value as never))));
  let current = true;
  const reads = Array.from({ length: 6 }, (_, i) => readNativeUsage(`fixture:${i}`, i, () => i !== 4 || current));
  const rejected = reads[4].catch(error => error.message);
  expect(readNativeUsage('fixture:0', 0, () => true)).toBe(reads[0]);
  expect(api).toHaveBeenCalledTimes(4);
  current = false;
  finishes[0](); await reads[0]; await Promise.resolve(); await Promise.resolve();
  expect(await rejected).toBe('用量读取已取消');
  expect(api).toHaveBeenCalledTimes(5);
  expect(api).not.toHaveBeenCalledWith('GET', '/accounts/4/usage');
  finishes.slice(1).forEach(finish => finish());
  await Promise.all(reads.filter((_, i) => i !== 4)); await Promise.resolve();
  const next = readNativeUsage('fixture:0', 0, () => true);
  expect(api).toHaveBeenCalledTimes(6);
  finishes.at(-1)!(); await next; await Promise.resolve();
});

it('releases a failed read so the next refresh can retry', async () => {
  vi.mocked(api).mockRejectedValueOnce(new Error('fixture failure'));
  await expect(readNativeUsage('failure:1', 1, () => true)).rejects.toThrow('fixture failure');
  await Promise.resolve();
  vi.mocked(api).mockResolvedValueOnce({ status: 'known' } as never);
  await expect(readNativeUsage('failure:1', 1, () => true)).resolves.toEqual({ status: 'known' });
});
