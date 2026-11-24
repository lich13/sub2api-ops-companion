import { BridgeError } from "./bridgeError";
import { api, command } from "./bridge";
import type {
  BatchHeader,
  OperationCurrentFields,
  OperationHeader,
  OperationReceipt,
  OperationRequestFields,
} from "./protocol";
import type { Account, DegradationMark, TestEvent } from "./types";

export type Operation = Omit<
  OperationHeader,
  | "account_name"
  | "next_retry_at"
  | "repeat"
  | "repeat_state"
  | "requested"
  | "current"
  | "result"
  | "events"
> & {
  account_name: string;
  next_retry_at?: string | null;
  repeat?: "quota_cycle";
  repeat_state?: "active" | "continued" | "stopped" | "needs_confirmation";
  requested: Omit<OperationRequestFields, "proxy_id"> & {
    proxy_id?: number | null;
  } & Record<string, unknown>;
  current?: OperationCurrentFields & Record<string, unknown>;
  result?: Omit<OperationReceipt, "degradation_mark"> & {
    degradation_mark?: DegradationMark;
  } & Record<string, unknown>;
  events?: TestEvent[];
};
export type OperationBatch = Omit<BatchHeader, "items"> & {
  items: Operation[];
};
export const operationTerminal = new Set([
  "completed",
  "failed",
  "cancelled",
  "superseded",
  "needs_confirmation",
]);
export const repeating = (job: Operation) =>
  job.repeat === "quota_cycle" &&
  ["active", "needs_confirmation"].includes(job.repeat_state ?? "");
export const operationPending = (job: Operation) =>
  !operationTerminal.has(job.status) ||
  job.status === "needs_confirmation" ||
  repeating(job);
export function publishOperation(job: Operation) {
  setOperations([
    job,
    ...getOperations().filter(
      (row) =>
        row.id !== job.id &&
        (!job.repeat_series_id ||
          row.repeat_series_id !== job.repeat_series_id),
    ),
  ]);
}
const listeners = new Set<() => void>();
let rows: Operation[] = [],
  epoch = 0,
  connection = "",
  memoryClientId = "";
const clientKey = "sub2ops-operation-client";
export function clientId() {
  if (memoryClientId) return memoryClientId;
  let storage: Storage | null = null;
  try {
    storage = globalThis.localStorage ?? null;
    const saved = storage?.getItem(clientKey);
    if (saved) {
      memoryClientId = saved;
      return saved;
    }
  } catch {
    /* Restricted or full WebView storage falls back to in-memory client ID. */
  }
  memoryClientId = crypto.randomUUID();
  try {
    storage?.setItem(clientKey, memoryClientId);
  } catch {
    /* Best-effort persistence. */
  }
  return memoryClientId;
}
function sameOperation(a: Operation, b: Operation) {
  return (
    a === b ||
    (a.id === b.id &&
      a.account_id === b.account_id &&
      a.account_name === b.account_name &&
      a.action === b.action &&
      a.status === b.status &&
      a.batch_id === b.batch_id &&
      a.reason === b.reason &&
      a.started_at === b.started_at &&
      a.scheduled_at === b.scheduled_at &&
      a.attempt_count === b.attempt_count &&
      a.next_retry_at === b.next_retry_at &&
      a.repeat === b.repeat &&
      a.repeat_series_id === b.repeat_series_id &&
      a.repeat_state === b.repeat_state &&
      a.repeat_reason === b.repeat_reason &&
      a.next_operation_id === b.next_operation_id &&
      a.next_event === b.next_event)
  );
}
export function bindOperationConnection(key: string) {
  if (connection === key) return;
  connection = key;
  ++epoch;
  setOperations([]);
}
const emptyPendingOperations: readonly Operation[] = [];
let pendingByAccount = new Map<number, Operation[]>();
function rebuildPendingIndex(items: Operation[]) {
  const nextMap = new Map<number, Operation[]>();
  for (const job of items) {
    if (!operationPending(job)) continue;
    const list = nextMap.get(job.account_id);
    if (list) list.push(job);
    else nextMap.set(job.account_id, [job]);
  }
  for (const [accountId, nextList] of nextMap) {
    const prevList = pendingByAccount.get(accountId);
    if (
      prevList &&
      prevList.length === nextList.length &&
      prevList.every((item, idx) => sameOperation(item, nextList[idx]))
    ) {
      nextMap.set(accountId, prevList);
    }
  }
  pendingByAccount = nextMap;
}
export const getOperations = () => rows;
export const getPendingAccountOperations = (
  accountId: number,
): readonly Operation[] =>
  pendingByAccount.get(accountId) ?? emptyPendingOperations;
export function subscribeOperations(listener: () => void) {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}
export function setOperations(next: Operation[]) {
  if (
    rows.length === next.length &&
    rows.every((item, index) => sameOperation(item, next[index]))
  )
    return;
  rows = next;
  rebuildPendingIndex(next);
  listeners.forEach((listener) => listener());
}
export const operationEpoch = () => epoch;
export async function submitAccountBatch(
  action: "delete" | "groups" | "recover",
  accounts: {
    account_id: number;
    expected_version: string;
    payload?: Record<string, unknown>;
  }[],
  requestId: string,
): Promise<OperationBatch> {
  const current = epoch;
  const batch = await api<OperationBatch>("POST", "/account-operations/batch", {
    action,
    payload: action === "delete" ? { confirmed: true } : {},
    accounts,
    request_id: requestId,
    client_id: clientId(),
  });
  if (current !== epoch) throw BridgeError.changed();
  setOperations([
    ...batch.items,
    ...rows.filter((r) => !batch.items.some((j) => j.id === r.id)),
  ]);
  return batch;
}
const adaptivePollDelaysMs = [35, 80, 180, 350, 500];
export async function followAccountBatch(
  initial: OperationBatch,
  update: (batch: OperationBatch) => void,
  alive: () => boolean = () => true,
): Promise<void> {
  const current = epoch;
  let batch = initial,
    step = 0;
  while (alive() && current === epoch) {
    update(batch);
    if (batch.items.every((j) => operationTerminal.has(j.status))) {
      void command("refresh");
      return;
    }
    const delay =
      adaptivePollDelaysMs[Math.min(step++, adaptivePollDelaysMs.length - 1)];
    await new Promise((resolve) => setTimeout(resolve, delay));
    if (!alive() || current !== epoch) return;
    const next = await api<OperationBatch>(
      "GET",
      `/account-operations?batch_id=${batch.batch_id}`,
    );
    if (!alive() || current !== epoch) return;
    batch = next;
    setOperations([
      ...batch.items,
      ...rows.filter((r) => !batch.items.some((j) => j.id === r.id)),
    ]);
  }
}
export async function submitOperation(
  account: Account,
  action: string,
  payload: Record<string, unknown>,
  expected?: string,
  scheduledAt?: string,
  repeat?: "quota_cycle",
): Promise<Operation> {
  const current = epoch;
  const version =
    expected ??
    (action === "degradation_mark"
      ? account.degradation_mark?.version
      : account.operation_versions[action]);
  if (!version) throw new Error("请刷新账号后重试");
  const job = await api<Operation>(
    "POST",
    `/accounts/${account.id}/operations`,
    {
      action,
      payload,
      expected_version: version,
      request_id: crypto.randomUUID(),
      client_id: clientId(),
      ...(scheduledAt ? { scheduled_at: scheduledAt } : {}),
      ...(repeat ? { repeat } : {}),
    },
  );
  if (current !== epoch) throw BridgeError.changed();
  publishOperation(job);
  return job;
}
export async function followOperation<T>(
  initial: Operation,
  onEvent?: (event: TestEvent) => void,
  onUpdate?: (job: Operation) => void,
): Promise<T> {
  const current = epoch;
  let job = initial,
    after = 0,
    step = 0;
  while (true) {
    if (current !== epoch) throw BridgeError.changed();
    if (onEvent) {
      for (const event of job.events ?? []) onEvent(event);
      after = job.next_event ?? after;
    }
    onUpdate?.(job);
    if (operationTerminal.has(job.status)) {
      // Drain all remaining connection-test events before presenting completion.
      if (onEvent && (job.events?.length ?? 0) === 100) {
        job = await api<Operation>(
          "GET",
          `/account-operations/${job.id}?after_event=${after}`,
        );
        continue;
      }
      if (job.status === "completed") return job.result as T;
      throw new Error(
        job.reason ||
          (
            {
              needs_confirmation: "请求已保留，请在待办中核对",
              cancelled: "已取消",
              superseded: "已由新请求替代",
            } as Record<string, string>
          )[job.status] ||
          "操作失败",
      );
    }
    onEvent?.({
      type: "status",
      text:
        job.status === "queued"
          ? job.reason || "已排队，等待执行"
          : job.status === "checking"
            ? "核对结果"
            : "测试中",
    });
    const delay =
      adaptivePollDelaysMs[Math.min(step++, adaptivePollDelaysMs.length - 1)];
    await new Promise((resolve) => setTimeout(resolve, delay));
    if (current !== epoch) throw BridgeError.changed();
    const next = await api<Operation>(
      "GET",
      `/account-operations/${job.id}${onEvent ? `?after_event=${after}` : ""}`,
    );
    if (current !== epoch) throw BridgeError.changed();
    job = next;
    setOperations(
      rows.some((r) => r.id === job.id)
        ? rows.map((r) => (r.id === job.id ? job : r))
        : [job, ...rows],
    );
  }
}
export async function accountOperation<T>(
  account: Account,
  action: string,
  payload: Record<string, unknown>,
  expected?: string,
  onUpdate?: (job: Operation) => void,
): Promise<T> {
  const job = await submitOperation(account, action, payload, expected);
  const result = await followOperation<T>(job, undefined, onUpdate);
  void command("refresh");
  return result;
}
