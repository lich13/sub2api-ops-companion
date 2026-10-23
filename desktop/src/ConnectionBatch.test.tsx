// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ConnectionBatchResults, useConnectionBatch } from "./ConnectionBatch";
import { api, command } from "./bridge";
import { bindOperationConnection, setOperations, type Operation, type OperationBatch } from "./accountOperations";
import type { Account } from "./types";

vi.mock("./bridge", () => ({ api: vi.fn(), command: vi.fn() }));
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

const account = (id: number, overrides: Partial<Account> = {}): Account => ({
  id,
  name: `连接账号 ${id}`,
  priority: id,
  platform: "openai",
  type: "oauth",
  status: "active",
  schedulable: true,
  available: true,
  group_ids: [],
  blockers: [],
  version: `account-version-${id}`,
  operation_versions: { test: `test-version-${id}` },
  last_success_at: null,
  last_error_at: null,
  last_error_id: null,
  last_error_code: null,
  last_error_status: null,
  error_message: "",
  success_after_error: false,
  usage_windows: [],
  ...overrides,
});

function operation(id: number, overrides: Partial<Operation> = {}): Operation {
  return {
    id: `test-operation-${id}`,
    batch_id: "connection-batch",
    account_id: id,
    account_name: account(id).name,
    action: "test",
    requested: { confirmed: true, model_id: "requested-model" },
    status: "queued",
    ...overrides,
  };
}

function batch(batchId: string, items: Operation[]): OperationBatch {
  return {
    batch_id: batchId,
    items: items.map((item) => ({ ...item, batch_id: batchId })),
    pending: items.filter((item) => ["queued", "running", "checking"].includes(item.status)).length,
  };
}

function completed(id: number, overrides: Partial<Operation> = {}): Operation {
  return operation(id, {
    status: "completed",
    result: {
      requested_model: "requested-model",
      actual_model: "actual-model",
      success: true,
      duration_ms: 120,
      completed_at: "2026-10-13T02:00:00Z",
      error: "",
      uncertain: false,
    },
    ...overrides,
  });
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

type HarnessProps = {
  accounts?: Account[];
  connectionKey?: string;
  storageKey?: string;
  online?: boolean;
  active?: boolean;
};

function Harness({
  accounts = [account(1)],
  connectionKey = "connection-a:1",
  storageKey = "https://connection-a.example.invalid",
  online = true,
  active = true,
}: HarnessProps) {
  const control = useConnectionBatch({ connectionKey, storageKey, online, active });
  return <>
    <button id="connection-batch-start" data-testid="start" onClick={() => void control.start(accounts)}>提交所选账号</button>
    <output data-testid="batch-id">{control.batch?.batch_id ?? ""}</output>
    <output data-testid="busy">{String(control.busy)}</output>
    <output data-testid="starting">{String(control.starting)}</output>
    <output data-testid="error">{control.error}</output>
    <div className="account-toolbar" />
    <ConnectionBatchResults control={control} />
  </>;
}

let container: HTMLDivElement;
let root: Root;
let sequence = 0;

function fixtureButton(name: "start") {
  return container.querySelector<HTMLButtonElement>(`[data-testid="${name}"]`)!;
}

function output(name: "batch-id" | "busy" | "starting" | "error") {
  return container.querySelector(`[data-testid="${name}"]`)?.textContent;
}

function resultRow(name: string): Element {
  const row = [...container.querySelectorAll("tbody tr, article, [role='row']")]
    .find((node) => node.textContent?.includes(name));
  if (!row) throw new Error(`missing result row: ${name}`);
  return row;
}

const submissions = () => vi.mocked(api).mock.calls
  .filter(([method, path]) => method === "POST" && path === "/account-operations/batch");
const reads = () => vi.mocked(api).mock.calls
  .filter(([method, path]) => method === "GET" && path.startsWith("/account-operations?batch_id="));

async function flush() {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}

async function renderHarness(props: HarnessProps = {}) {
  bindOperationConnection(props.connectionKey ?? "connection-a:1");
  await act(async () => root.render(<Harness {...props} />));
  await flush();
}

async function click(name: "start") {
  await act(async () => fixtureButton(name).click());
  await flush();
}

async function clickResultButton(label: string) {
  const button = [...container.querySelectorAll<HTMLButtonElement>('section[aria-label="批量测试连接"] button')]
    .find((node) => node.textContent?.trim() === label);
  if (!button) throw new Error(`missing result button: ${label}`);
  await act(async () => button.click());
  await flush();
}

function expectHiddenResult() {
  expect(container.querySelector('section[aria-label="批量测试连接"]')).toBeNull();
  expect(container.querySelector('section[aria-label="批量测试连接摘要"]')).toBeNull();
}

type PersistedState = { batchId?: string; request?: { request_id?: string }; visible?: boolean };

function persistedStates() {
  return Object.keys(localStorage).flatMap((key) => {
    try {
      const value = JSON.parse(localStorage.getItem(key) ?? "{}") as PersistedState;
      return value && typeof value === "object" && ("batchId" in value || "request" in value) ? [{ key, value }] : [];
    } catch {
      return [];
    }
  });
}

async function advance(ms = 10_000) {
  await act(async () => { await vi.advanceTimersByTimeAsync(ms); });
  await flush();
}

beforeEach(() => {
  vi.resetAllMocks();
  vi.useFakeTimers();
  localStorage.clear();
  bindOperationConnection(`connection-test-reset-${++sequence}`);
  setOperations([]);
  vi.mocked(command).mockResolvedValue(undefined as never);
  vi.mocked(api).mockImplementation(async (method, path) => {
    throw new Error(`Unexpected fixture request: ${method} ${path}`);
  });
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.clearAllTimers();
  vi.useRealTimers();
  localStorage.clear();
  setOperations([]);
});

describe("connection batch tests", () => {
  it("submits the selected accounts once with test versions and blocks repeated clicks while starting or running", async () => {
    const response = deferred<OperationBatch>();
    vi.mocked(api).mockReturnValue(response.promise as never);
    await renderHarness({ accounts: [account(1), account(2)] });

    await act(async () => {
      fixtureButton("start").click();
      fixtureButton("start").click();
    });
    expect(submissions()).toHaveLength(1);
    expect(output("starting")).toBe("true");
    const body = submissions()[0]?.[2];
    expect(body).toEqual({
      action: "test",
      payload: { confirmed: true },
      accounts: [
        { account_id: 1, expected_version: "test-version-1" },
        { account_id: 2, expected_version: "test-version-2" },
      ],
      request_id: expect.any(String),
      client_id: expect.any(String),
    });
    expect(body).toMatchObject({ request_id: expect.stringMatching(/\S/), client_id: expect.stringMatching(/\S/) });

    await act(async () => response.resolve(batch("batch-submit", [operation(1), operation(2)])));
    expect(output("batch-id")).toBe("batch-submit");
    expect(output("starting")).toBe("false");
    expect(output("busy")).toBe("true");
    await click("start");
    expect(submissions()).toHaveLength(1);
  });

  it("renders each account, requested and actual models, and only explicit successful results as passing", async () => {
    const mixed = batch("batch-results", [
      completed(1, { result: { ...completed(1).result, requested_model: "alias-model", actual_model: "provider-model" } }),
      completed(2, { result: { ...completed(2).result, success: false, error: "上游明确返回测试失败" } }),
      operation(3, {
        status: "needs_confirmation",
        reason: "连接中断，未收到终态",
        result: { requested_model: "requested-model", actual_model: "", success: false, uncertain: true, error: "未收到终态" },
      }),
      completed(4, { requested: { model_id: "" }, result: { ...completed(4).result, requested_model: "", actual_model: "grok-fixture-model" } }),
      completed(5, { result: { requested_model: "requested-model", actual_model: "unconfirmed-model" } }),
    ]);
    vi.mocked(api).mockResolvedValue(mixed as never);
    await renderHarness({ accounts: [account(1), account(2), account(3), account(4, { platform: "grok" }), account(5)] });
    await click("start");

    const passed = resultRow(account(1).name).textContent;
    expect(passed).toContain("通过");
    expect(passed).toContain("alias-model");
    expect(passed).toContain("provider-model");
    expect(passed).toContain("0.12 s");
    expect(resultRow(account(2).name).textContent).toContain("失败");
    expect(resultRow(account(2).name).textContent).toContain("上游明确返回测试失败");
    expect(resultRow(account(2).name).textContent).not.toContain("通过");
    expect(resultRow(account(3).name).textContent).toMatch(/需确认|待核对/);
    expect(resultRow(account(3).name).textContent).not.toContain("通过");
    expect(resultRow(account(4).name).textContent).toContain("原生默认");
    expect(resultRow(account(4).name).textContent).toContain("grok-fixture-model");
    expect(resultRow(account(5).name).textContent).not.toContain("通过");
    expect(output("busy")).toBe("false");
  });

  it("polls the batch endpoint for queued accounts and stops polling once every item is terminal", async () => {
    const queued = batch("batch-follow", [operation(1), operation(2)]);
    const done = batch("batch-follow", [completed(1), completed(2)]);
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method === "POST" && path === "/account-operations/batch") return queued as never;
      if (method === "GET" && path === "/account-operations?batch_id=batch-follow") return done as never;
      throw new Error(`Unexpected fixture request: ${method} ${path}`);
    });
    await renderHarness({ accounts: [account(1), account(2)] });
    await click("start");
    await advance(1000);

    expect(api).toHaveBeenCalledWith("GET", "/account-operations?batch_id=batch-follow");
    expect(resultRow(account(1).name).textContent).toContain("通过");
    expect(resultRow(account(2).name).textContent).toContain("通过");
    expect(output("busy")).toBe("false");
    const terminalReads = reads().length;
    await advance(30_000);
    expect(reads()).toHaveLength(terminalReads);
  });

  it("retries a timed out submission with the same IDs and original account snapshot", async () => {
    let attempts = 0;
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method === "POST" && path === "/account-operations/batch") {
        if (++attempts === 1) throw new Error("请求超时，提交结果未知");
        return batch("batch-retried", [completed(1), completed(2)]) as never;
      }
      if (method === "GET" && path === "/account-operations?batch_id=batch-retried") return batch("batch-retried", [completed(1), completed(2)]) as never;
      throw new Error(`Unexpected fixture request: ${method} ${path}`);
    });
    await renderHarness({ accounts: [account(1), account(2)] });
    await click("start");
    expect(output("error")).toContain("请求超时");
    expect(submissions()).toHaveLength(1);

    await renderHarness({ accounts: [account(3)] });
    await clickResultButton("恢复提交结果");
    expect(submissions()).toHaveLength(2);
    expect(submissions()[1]?.[2]).toEqual(submissions()[0]?.[2]);
    expect(output("batch-id")).toBe("batch-retried");
    expect(output("error")).toBe("");
    expect(resultRow(account(1).name).textContent).toContain("通过");
    expect(resultRow(account(2).name).textContent).toContain("通过");
  });

  it("restores an unconfirmed submission visibly without resubmitting automatically", async () => {
    vi.mocked(api).mockRejectedValue(new Error("fixture timeout"));
    await renderHarness({ accounts: [account(1, { operation_versions: { test: "a".repeat(64) } })] }); await click("start");
    const original = submissions()[0]?.[2];
    await act(async () => root.unmount()); root = createRoot(container);
    await renderHarness();
    expect(container.textContent).toContain("恢复提交结果");
    expect(submissions()).toHaveLength(1);
    vi.mocked(api).mockResolvedValue(batch("a".repeat(32), [completed(1)]) as never);
    await clickResultButton("恢复提交结果");
    expect(submissions()[1]?.[2]).toEqual(original);
  });

  it("restores saved completed batches silently and preserves the opaque storage key", async () => {
    const savedId = "b".repeat(32);
    vi.mocked(api).mockResolvedValue(batch(savedId, [completed(1)]) as never);
    await renderHarness(); await click("start");
    const saved = persistedStates()[0];
    expect(saved.key).not.toContain("example.invalid");
    localStorage.setItem(saved.key, JSON.stringify({ batchId: savedId }));
    await act(async () => root.unmount()); root = createRoot(container);
    await renderHarness();
    expectHiddenResult();
    expect(submissions()).toHaveLength(1);
    expect(api).toHaveBeenCalledWith("GET", `/account-operations?batch_id=${savedId}`);
  });

  it("keeps a manually hidden running batch busy without adding a result entry", async () => {
    vi.mocked(api).mockResolvedValue(batch("c".repeat(32), [operation(1)]) as never);
    await renderHarness(); await click("start");
    await clickResultButton("关闭结果");
    expectHiddenResult();
    expect(output("busy")).toBe("true");
    expect(document.activeElement).toBe(fixtureButton("start"));
    expect(container.querySelector(".account-toolbar button")).toBeNull();
    await click("start"); expect(submissions()).toHaveLength(1);
  });

  it("pauses batch polling while inactive and resumes it when active again", async () => {
    let finish = false;
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method === "POST" && path === "/account-operations/batch") return batch("batch-paused", [operation(1)]) as never;
      if (method === "GET" && path === "/account-operations?batch_id=batch-paused") {
        return batch("batch-paused", [finish ? completed(1) : operation(1)]) as never;
      }
      throw new Error(`Unexpected fixture request: ${method} ${path}`);
    });
    await renderHarness();
    await click("start");
    await renderHarness({ active: false });
    const beforePause = reads().length;
    await advance(30_000);
    expect(reads()).toHaveLength(beforePause);
    expect(output("batch-id")).toBe("batch-paused");

    finish = true;
    await renderHarness({ active: true });
    await advance(1000);
    expect(reads().length).toBeGreaterThan(beforePause);
    expect(resultRow(account(1).name).textContent).toContain("通过");
  });

  it("ignores a delayed POST from a previous connection without overwriting the new batch", async () => {
    const oldResponse = deferred<OperationBatch>();
    let attempt = 0;
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method === "POST" && path === "/account-operations/batch") {
        if (++attempt === 1) return oldResponse.promise as never;
        return batch("batch-new-connection", [completed(2)]) as never;
      }
      if (method === "GET" && path === "/account-operations?batch_id=batch-new-connection") return batch("batch-new-connection", [completed(2)]) as never;
      throw new Error(`Unexpected fixture request: ${method} ${path}`);
    });
    await renderHarness();
    await click("start");
    await renderHarness({
      connectionKey: "connection-b:2",
      storageKey: "https://connection-b.example.invalid",
      accounts: [account(2)],
    });
    await click("start");
    expect(output("batch-id")).toBe("batch-new-connection");

    await act(async () => oldResponse.resolve(batch("batch-old-connection", [operation(1)])));
    await advance();
    expect(output("batch-id")).toBe("batch-new-connection");
    expect(container.textContent).not.toContain(account(1).name);
    expect(reads().some(([, path]) => path.includes("batch-old-connection"))).toBe(false);
    expect(output("error")).toBe("");
  });

  it("ignores a delayed polling response after switching connection generations", async () => {
    const oldRead = deferred<OperationBatch>();
    const savedBatchId = "b".repeat(32);
    const confirmedAfterReconnect = batch(savedBatchId, [operation(1, {
      status: "needs_confirmation",
      reason: "重连后需人工核对",
      result: { success: false, uncertain: true, requested_model: "requested-model", actual_model: "" },
    })]);
    let readCount = 0;
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method === "POST" && path === "/account-operations/batch") return batch(savedBatchId, [operation(1)]) as never;
      if (method === "GET" && path === `/account-operations?batch_id=${savedBatchId}`) {
        if (++readCount === 1) return oldRead.promise as never;
        return confirmedAfterReconnect as never;
      }
      throw new Error(`Unexpected fixture request: ${method} ${path}`);
    });
    await renderHarness();
    await click("start");
    expect(readCount).toBe(1);

    await renderHarness({ connectionKey: "connection-a:2" });
    expect(readCount).toBe(2);
    expectHiddenResult();
    await act(async () => oldRead.resolve(batch(savedBatchId, [completed(1, {
      result: { ...completed(1).result, actual_model: "stale-response-model" },
    })])));
    await flush();
    expect(output("batch-id")).toBe(savedBatchId);
    expect(submissions()).toHaveLength(1);
    expect(container.textContent).not.toContain("stale-response-model");
    expectHiddenResult();
  });

  it("stops not-started work with one batch cancel request and keeps running results visible", async () => {
    const initial = batch("batch-cancel", [operation(1), operation(2, { status: "running" })]);
    const stopped = batch("batch-cancel", [operation(1, { status: "cancelled", reason: "批次已停止" }), operation(2, { status: "running" })]);
    let cancelled = false;
    vi.mocked(api).mockImplementation(async (method, path, body) => {
      if (method === "POST" && path === "/account-operations/batch") return initial as never;
      if (method === "POST" && path === "/account-operations/batches/batch-cancel/cancel") {
        expect(body).toEqual({});
        cancelled = true;
        return stopped as never;
      }
      if (method === "GET" && path === "/account-operations?batch_id=batch-cancel") return (cancelled ? stopped : initial) as never;
      throw new Error(`Unexpected fixture request: ${method} ${path}`);
    });
    await renderHarness({ accounts: [account(1), account(2)] });
    await click("start");
    await clickResultButton("停止未开始");

    expect(vi.mocked(api).mock.calls.filter(([, path]) => path.endsWith("/cancel"))).toEqual([
      ["POST", "/account-operations/batches/batch-cancel/cancel", {}],
    ]);
    expect(resultRow(account(1).name).textContent).toMatch(/已取消|已停止/);
    expect(resultRow(account(2).name).textContent).toMatch(/测试中|执行中/);
    expect(output("batch-id")).toBe("batch-cancel");
  });

  it("does not submit an empty selection or submit while offline", async () => {
    await renderHarness({ accounts: [] });
    await click("start");
    expect(submissions()).toHaveLength(0);
    await renderHarness({ accounts: [account(1)], online: false });
    await click("start");
    expect(submissions()).toHaveLength(0);
    expect(output("starting")).toBe("false");
  });

  it("keeps the submitted account list visible when later selection or filtering changes", async () => {
    vi.mocked(api).mockResolvedValue(batch("batch-snapshot", [operation(1), operation(2)]) as never);
    await renderHarness({ accounts: [account(1), account(2)] });
    await click("start");
    await renderHarness({ accounts: [account(3)] });
    await renderHarness({ accounts: [] });

    expect(submissions()).toHaveLength(1);
    expect(submissions()[0]?.[2]).toMatchObject({
      accounts: [
        { account_id: 1, expected_version: "test-version-1" },
        { account_id: 2, expected_version: "test-version-2" },
      ],
    });
    expect(resultRow(account(1).name)).toBeTruthy();
    expect(resultRow(account(2).name)).toBeTruthy();
    expect(container.textContent).not.toContain(account(3).name);
    expect(output("batch-id")).toBe("batch-snapshot");
  });
});

it.each(["completed", "failed", "cancelled", "needs_confirmation", "superseded"])("auto-closes a terminal %s batch at its fixed deadline", async status => {
  const result = batch("b".repeat(32), [completed(1, { status })]);
  vi.mocked(api).mockResolvedValue(result as never);
  await renderHarness(); await click("start");
  await advance(2999);
  expect(container.querySelector('.connection-batch')).not.toBeNull();
  await advance(1);
  expectHiddenResult();
  expect(persistedStates()[0].value.visible).toBe(false);
  await renderHarness(); await advance(5000);
  expectHiddenResult();
  expect(container.querySelector(".account-toolbar button")).toBeNull();
});

it("keeps unfinished batches open and clears terminal timers when a new batch starts", async () => {
  let result = batch("c".repeat(32), [operation(1)]);
  vi.mocked(api).mockImplementation(async () => result as never);
  await renderHarness(); await click("start"); await advance(4000);
  expect(container.querySelector('.connection-batch')).not.toBeNull();
  result = batch("c".repeat(32), [completed(1)]);
  await advance(1000); await advance(2000);
  result = batch("d".repeat(32), [operation(1)]);
  await click("start"); await advance(1000);
  expect(output("batch-id")).toBe("d".repeat(32));
  expect(container.querySelector('.connection-batch')).not.toBeNull();
  await renderHarness({ connectionKey: "connection-b:1", storageKey: "https://connection-b.example.invalid" });
  await advance(4000); expectHiddenResult();
});


it("keeps an unconfirmed new submission visible when the prior batch completion timer expires", async () => {
  let attempted = false;
  const done = batch("e".repeat(32), [completed(1)]);
  vi.mocked(api).mockImplementation(async (method) => {
    if (method === "POST" && attempted) throw new Error("fixture submission unknown");
    if (method === "POST") attempted = true;
    return done as never;
  });
  await renderHarness(); await click("start"); await advance(2000);
  await click("start"); await advance(5000);
  expect(output("busy")).toBe("true");
  expect(output("error")).toBe("fixture submission unknown");
  expect(container.querySelector('.connection-batch')).not.toBeNull();
  expect(container.querySelector('.connection-batch [role="status"]')?.textContent).toBe("提交结果待确认");
  expect(container.querySelectorAll('.connection-batch tbody tr')).toHaveLength(0);
});

 it("does not restart the deadline after losing focus or changing pages", async () => {
  vi.mocked(api).mockResolvedValue(batch("f".repeat(32), [completed(1)]) as never);
  await renderHarness(); await click("start"); await advance(2000);
  await renderHarness({ active: false }); await advance(999);
  expect(container.querySelector(".connection-batch")).not.toBeNull();
  await advance(1); expectHiddenResult();
  await renderHarness({ active: true }); expectHiddenResult();
 });

 it("hides an expired result immediately on resume even when timers were suspended", async () => {
  vi.mocked(api).mockResolvedValue(batch("f".repeat(32), [completed(1)]) as never);
  await renderHarness(); await click("start");
  vi.setSystemTime(Date.now() + 5000);
  await act(async () => window.dispatchEvent(new Event("focus")));
  expectHiddenResult();
 });
