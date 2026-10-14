// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ConnectionBatchEntry, ConnectionBatchResults, useConnectionBatch } from "./ConnectionBatch";
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
  managed: false,
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
    <button data-testid="start" onClick={() => void control.start(accounts)}>提交所选账号</button>
    <output data-testid="batch-id">{control.batch?.batch_id ?? ""}</output>
    <output data-testid="busy">{String(control.busy)}</output>
    <output data-testid="starting">{String(control.starting)}</output>
    <output data-testid="error">{control.error}</output>
    <div className="account-toolbar"><ConnectionBatchEntry control={control} /></div>
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

function entryButton() {
  const button = container.querySelector<HTMLButtonElement>('.account-toolbar button[aria-label="查看测活结果"]');
  if (!button) throw new Error("missing batch result entry");
  return button;
}

function expectHiddenResult() {
  expect(container.querySelector('section[aria-label="批量测试连接"]')).toBeNull();
  expect(container.querySelector('section[aria-label="批量测试连接摘要"]')).toBeNull();
}

async function clickEntry() {
  await act(async () => entryButton().click());
  await flush();
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
    await advance();

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

  it("persists a closed timed-out request through remount and recovers it only after explicit reopen", async () => {
    const storageKey = "https://timeout.example.invalid";
    const recovered = batch("batch-recovered-hidden", [completed(1)]);
    let attempts = 0;
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method === "POST" && path === "/account-operations/batch") {
        if (++attempts === 1) throw new Error("请求超时，提交结果未知");
        return recovered as never;
      }
      if (method === "GET" && path === "/account-operations?batch_id=batch-recovered-hidden") return recovered as never;
      throw new Error(`Unexpected fixture request: ${method} ${path}`);
    });
    const accountWithPersistableVersion = account(1, {
      version: "a".repeat(64),
      operation_versions: { test: "b".repeat(64) },
    });
    await renderHarness({ storageKey, connectionKey: "timeout:1", accounts: [accountWithPersistableVersion] });
    await click("start");

    expect(output("error")).toContain("请求超时");
    expect(output("busy")).toBe("true");
    expect(submissions()).toHaveLength(1);
    const originalRequest = submissions()[0]?.[2];
    await clickResultButton("关闭结果");
    expectHiddenResult();
    expect(entryButton().textContent).toContain("测活结果");
    expect(entryButton().getAttribute("title")).toBe("提交结果待确认");
    expect(entryButton().querySelector('[role="status"]')?.textContent).toBe("提交结果待确认");
    expect(document.activeElement).toBe(entryButton());
    expect(output("busy")).toBe("true");
    expect(persistedStates()[0]?.value).toEqual({ request: originalRequest, visible: false });

    await act(async () => root.unmount());
    root = createRoot(container);
    await renderHarness({ storageKey, connectionKey: "timeout:2", accounts: [account(2)] });
    expectHiddenResult();
    expect(entryButton().getAttribute("title")).toBe("提交结果待确认");
    expect(output("busy")).toBe("true");
    expect(submissions()).toHaveLength(1);

    await clickEntry();
    expect(container.querySelector('section[aria-label="批量测试连接"]')).toBeTruthy();
    expect(document.activeElement).toBe(container.querySelector('section[aria-label="批量测试连接"]'));
    expect(submissions()).toHaveLength(1);
    await clickResultButton("恢复提交结果");
    expect(submissions()).toHaveLength(2);
    expect(submissions()[1]?.[2]).toEqual(originalRequest);
    expect(output("batch-id")).toBe("batch-recovered-hidden");
    expect(output("busy")).toBe("false");
    expect(resultRow(account(1).name).textContent).toContain("通过");
  });

  it("persists the batch under an opaque key and restores it with GET after remounting the same server", async () => {
    const storageKey = "https://persist.example.invalid";
    const savedBatchId = "a".repeat(32);
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method === "POST" && path === "/account-operations/batch") return batch(savedBatchId, [operation(1)]) as never;
      if (method === "GET" && path === `/account-operations?batch_id=${savedBatchId}`) return batch(savedBatchId, [completed(1)]) as never;
      throw new Error(`Unexpected fixture request: ${method} ${path}`);
    });
    await renderHarness({ storageKey, connectionKey: "persist:1" });
    await click("start");
    await advance();
    expect(resultRow(account(1).name).textContent).toContain("通过");
    await clickResultButton("关闭结果");
    expectHiddenResult();
    expect(entryButton().getAttribute("aria-label")).toBe("查看测活结果");
    expect(entryButton().textContent?.trim()).toBe("测活结果");
    expect(entryButton().getAttribute("title")).toBe("已结束 1/1 · 通过 1");
    expect(document.activeElement).toBe(entryButton());

    const saved = persistedStates().find(({ value }) => value.batchId === savedBatchId);
    expect(saved?.value).toEqual({ batchId: savedBatchId, visible: false });
    expect(saved?.key).toBeTruthy();
    expect(saved?.key).not.toContain(storageKey);
    const initialReads = reads().length;

    await act(async () => root.unmount());
    root = createRoot(container);
    await renderHarness({ storageKey, connectionKey: "persist:2", accounts: [] });
    await advance();

    expect(submissions()).toHaveLength(1);
    expect(api).toHaveBeenCalledWith("GET", `/account-operations?batch_id=${savedBatchId}`);
    expect(reads().length).toBeGreaterThan(initialReads);
    expect(output("batch-id")).toBe(savedBatchId);
    expectHiddenResult();
    expect(entryButton().getAttribute("title")).toBe("已结束 1/1 · 通过 1");
    expect(container.textContent).not.toContain(account(1).name);

    await clickEntry();
    expect(container.querySelector('section[aria-label="批量测试连接"]')).toBeTruthy();
    expect(document.activeElement).toBe(container.querySelector('section[aria-label="批量测试连接"]'));
    expect(resultRow(account(1).name).textContent).toContain("通过");
    expect(submissions()).toHaveLength(1);
  });

  it("opens a saved batch by default when its legacy preference has no visibility field", async () => {
    const storageKey = "https://legacy.example.invalid";
    const savedBatchId = "c".repeat(32);
    const savedBatch = batch(savedBatchId, [completed(1)]);
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method === "POST" && path === "/account-operations/batch") return savedBatch as never;
      if (method === "GET" && path === `/account-operations?batch_id=${savedBatchId}`) return savedBatch as never;
      throw new Error(`Unexpected fixture request: ${method} ${path}`);
    });
    await renderHarness({ storageKey, connectionKey: "legacy:1" });
    await click("start");

    const saved = persistedStates().find(({ value }) => value.batchId === savedBatchId);
    expect(saved).toBeTruthy();
    localStorage.setItem(saved!.key, JSON.stringify({ batchId: savedBatchId }));
    await act(async () => root.unmount());
    root = createRoot(container);

    await renderHarness({ storageKey, connectionKey: "legacy:2", accounts: [] });
    await advance();

    expect(submissions()).toHaveLength(1);
    expect(container.querySelector('section[aria-label="批量测试连接"]')).toBeTruthy();
    expect(resultRow(account(1).name).textContent).toContain("通过");
  });

  it("opens the new batch automatically after a completed result was closed", async () => {
    const firstBatchId = "d".repeat(32);
    const secondBatchId = "e".repeat(32);
    let submissionsCount = 0;
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method === "POST" && path === "/account-operations/batch") {
        const nextId = ++submissionsCount === 1 ? firstBatchId : secondBatchId;
        const accountId = submissionsCount;
        return batch(nextId, [completed(accountId)]) as never;
      }
      if (method === "GET" && path === `/account-operations?batch_id=${firstBatchId}`) return batch(firstBatchId, [completed(1)]) as never;
      if (method === "GET" && path === `/account-operations?batch_id=${secondBatchId}`) return batch(secondBatchId, [completed(2)]) as never;
      throw new Error(`Unexpected fixture request: ${method} ${path}`);
    });
    await renderHarness({ accounts: [account(1)] });
    await click("start");
    await clickResultButton("关闭结果");
    expectHiddenResult();
    expect(entryButton()).toBeTruthy();

    await renderHarness({ accounts: [account(2)] });
    await click("start");

    expect(submissions()).toHaveLength(2);
    expect(output("batch-id")).toBe(secondBatchId);
    expect(container.querySelector('section[aria-label="批量测试连接"]')).toBeTruthy();
    expect(container.querySelector('section[aria-label="批量测试连接摘要"]')).toBeNull();
    expect(container.querySelector('.account-toolbar button[aria-label="查看测活结果"]')).toBeNull();
    expect(resultRow(account(2).name).textContent).toContain("通过");
    expect(container.textContent).not.toContain(account(1).name);
  });

  it("keeps a running batch busy after hiding its results", async () => {
    const runningBatch = batch("batch-hidden-running", [operation(1, { status: "running" })]);
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method === "POST" && path === "/account-operations/batch") return runningBatch as never;
      if (method === "GET" && path === "/account-operations?batch_id=batch-hidden-running") return runningBatch as never;
      throw new Error(`Unexpected fixture request: ${method} ${path}`);
    });
    await renderHarness();
    await click("start");

    expect(output("busy")).toBe("true");
    expect(resultRow(account(1).name).textContent).toContain("测试中");
    await clickResultButton("关闭结果");

    expectHiddenResult();
    expect(entryButton().getAttribute("aria-label")).toBe("查看测活结果");
    expect(entryButton().querySelector('[role="status"]')?.textContent).toBe("已完成 0/1 · 通过 0");
    expect(output("busy")).toBe("true");
    await click("start");
    expect(submissions()).toHaveLength(1);
    expectHiddenResult();
  });

  it("keeps a closed result hidden when a pending submission responds late", async () => {
    const response = deferred<OperationBatch>();
    const lateBatch = batch("batch-late-after-close", [operation(1, { status: "running" })]);
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method === "POST" && path === "/account-operations/batch") return response.promise as never;
      if (method === "GET" && path === "/account-operations?batch_id=batch-late-after-close") return lateBatch as never;
      throw new Error(`Unexpected fixture request: ${method} ${path}`);
    });
    await renderHarness();
    await click("start");

    expect(output("starting")).toBe("true");
    expect(output("busy")).toBe("true");
    await clickResultButton("关闭结果");
    expectHiddenResult();
    expect(entryButton().getAttribute("title")).toBe("正在提交");
    expect(entryButton().querySelector('[role="status"]')?.textContent).toBe("正在提交");
    expect(document.activeElement).toBe(entryButton());
    expect(output("busy")).toBe("true");
    expect(persistedStates()[0]?.value).toMatchObject({ visible: false, request: { request_id: expect.any(String) } });

    await click("start");
    expect(submissions()).toHaveLength(1);
    await act(async () => response.resolve(lateBatch));
    await flush();

    expect(output("batch-id")).toBe("batch-late-after-close");
    expect(output("starting")).toBe("false");
    expect(output("busy")).toBe("true");
    expectHiddenResult();
    expect(entryButton().querySelector('[role="status"]')?.textContent).toBe("已完成 0/1 · 通过 0");
    expect(persistedStates()[0]?.value).toEqual({ batchId: "batch-late-after-close", visible: false });
    expect(submissions()).toHaveLength(1);
  });

  it("restores each connection's visibility independently when switching between connections", async () => {
    const firstBatchId = "1".repeat(32);
    const secondBatchId = "2".repeat(32);
    const first = batch(firstBatchId, [completed(1)]);
    const second = batch(secondBatchId, [completed(2)]);
    vi.mocked(api).mockImplementation(async (method, path, body) => {
      if (method === "POST" && path === "/account-operations/batch") {
        const submitted = body as { accounts: { account_id: number }[] };
        return (submitted.accounts[0]?.account_id === 1 ? first : second) as never;
      }
      if (method === "GET" && path === `/account-operations?batch_id=${firstBatchId}`) return first as never;
      if (method === "GET" && path === `/account-operations?batch_id=${secondBatchId}`) return second as never;
      throw new Error(`Unexpected fixture request: ${method} ${path}`);
    });

    await renderHarness({
      connectionKey: "connection-a:1",
      storageKey: "https://connection-a.example.invalid",
      accounts: [account(1)],
    });
    await click("start");
    await clickResultButton("关闭结果");
    expectHiddenResult();

    await renderHarness({
      connectionKey: "connection-b:1",
      storageKey: "https://connection-b.example.invalid",
      accounts: [account(2)],
    });
    await click("start");
    expect(resultRow(account(2).name).textContent).toContain("通过");

    await renderHarness({
      connectionKey: "connection-a:2",
      storageKey: "https://connection-a.example.invalid",
      accounts: [account(1)],
    });
    expectHiddenResult();
    expect(entryButton().getAttribute("title")).toBe("已结束 1/1 · 通过 1");

    await renderHarness({
      connectionKey: "connection-b:2",
      storageKey: "https://connection-b.example.invalid",
      accounts: [account(2)],
    });
    expect(resultRow(account(2).name).textContent).toContain("通过");
    expect(container.querySelector('section[aria-label="批量测试连接"]')).toBeTruthy();
    expect(submissions()).toHaveLength(2);
    expect(persistedStates().map(({ value }) => [value.batchId, value.visible]).sort()).toEqual([
      [firstBatchId, false],
      [secondBatchId, true],
    ]);
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
    await advance();
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
    expect(resultRow(account(1).name).textContent).toContain("重连后需人工核对");
    await act(async () => oldRead.resolve(batch(savedBatchId, [completed(1, {
      result: { ...completed(1).result, actual_model: "stale-response-model" },
    })])));
    await flush();
    expect(output("batch-id")).toBe(savedBatchId);
    expect(submissions()).toHaveLength(1);
    expect(container.textContent).not.toContain("stale-response-model");
    expect(resultRow(account(1).name).textContent).toMatch(/需确认|待核对/);
    expect(resultRow(account(1).name).textContent).not.toContain("通过");
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
