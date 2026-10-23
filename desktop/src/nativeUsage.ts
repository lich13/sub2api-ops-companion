import { api } from "./bridge";
import type { UsageSummary } from "./types";

type Read = {
  identity: string; accountId: number; current: (() => boolean)[];
  promise: Promise<UsageSummary>; resolve: (value: UsageSummary) => void; reject: (error: unknown) => void;
};
const pending = new Map<string, Read>(), waiting: Read[] = [];
let active = 0;

function drain() {
  while (active < 4 && waiting.length) {
    const read = waiting.shift()!;
    if (!read.current.some(current => current())) {
      pending.delete(read.identity);
      read.reject(new Error("用量读取已取消"));
      continue;
    }
    ++active;
    void api<UsageSummary>("GET", `/accounts/${read.accountId}/usage`)
      .then(read.resolve, read.reject).finally(() => {
        pending.delete(read.identity); --active; drain();
      });
  }
}

// Only automatic usage reads share these slots. User actions use their existing transport.
export function readNativeUsage(identity: string, accountId: number, current: () => boolean) {
  const existing = pending.get(identity);
  if (existing) { existing.current.push(current); return existing.promise; }
  let resolve!: Read["resolve"], reject!: Read["reject"];
  const promise = new Promise<UsageSummary>((done, fail) => { resolve = done; reject = fail; });
  const read = { identity, accountId, current: [current], promise, resolve, reject };
  pending.set(identity, read); waiting.push(read); drain();
  return read.promise;
}
