// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { api, command, subscribe } from "./bridge";
import type { OperationBatch } from "./accountOperations";
import App from "./main";
import type { Account, ViewState } from "./types";

vi.mock("./bridge", () => ({
  api: vi.fn(),
  command: vi.fn(),
  subscribe: vi.fn(),
  updates: vi.fn(async () => () => {}),
  watchWindowFocus: vi.fn(async () => () => {}),
  preview: true,
}));

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

let container: HTMLDivElement;
let root: Root;
let initialState: ViewState;

const account = (id: number): Account => ({
  id,
  name: `选择测试账号 ${id}`,
  priority: id,
  platform: "openai",
  type: "oauth",
  status: "active",
  schedulable: true,
  available: true,
  group_ids: [],
  blockers: [],
  managed: false,
  version: "a".repeat(64),
  operation_versions: { test: "b".repeat(64) },
  last_success_at: null,
  last_error_at: null,
  last_error_id: null,
  last_error_code: null,
  last_error_status: null,
  error_message: "",
  success_after_error: false,
  usage_windows: [],
});

const state = (platform: "macos" | "android"): ViewState => ({
  platform,
  foreground: true,
  connected: true,
  online: true,
  error: "",
  preferences: {
    base_url: "https://main-041.example.invalid",
    favorites: [],
    pinned: false,
    launch_at_login: false,
  },
  snapshot: {
    observed_at: "2026-10-14T00:00:00Z",
    accounts: [account(1), account(2)],
    groups: [],
    errors: [],
    recoveries: [],
  },
});

const recordPage = () => ({
  items: [],
  next_cursor: null,
  latest_id: null,
  observed_at: "2026-10-14T00:00:00Z",
  summary: { actual_cost: "0" },
});

const finishedBatch: OperationBatch = {
  batch_id: "a".repeat(32),
  pending: 0,
  items: [{
    id: "batch-041-operation-1",
    batch_id: "a".repeat(32),
    account_id: 1,
    account_name: account(1).name,
    action: "test",
    requested: { confirmed: true, model_id: "fixture-model" },
    status: "completed",
    result: {
      requested_model: "fixture-model",
      actual_model: "fixture-model",
      success: true,
      duration_ms: 10,
      completed_at: "2026-10-14T00:00:01Z",
      error: "",
      uncertain: false,
    },
  }],
};

beforeEach(() => {
  vi.clearAllMocks();
  localStorage.clear();
  initialState = state("macos");
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  vi.mocked(subscribe).mockResolvedValue(() => {});
  vi.mocked(command).mockImplementation(async (name) =>
    name === "get_state" ? (initialState as never) : (undefined as never),
  );
  vi.mocked(api).mockImplementation(async (method, path) => {
    if (method === "POST" && path === "/account-operations/batch") return finishedBatch as never;
    if (method === "GET" && path === `/account-operations?batch_id=${finishedBatch.batch_id}`) return finishedBatch as never;
    if (path.startsWith("/usage-records")) return recordPage() as never;
    if (path.startsWith("/errors?category=") || path === "/recoveries") return { items: [], next_cursor: null } as never;
    if (path === "/quota-refresh") return { status: "idle", items: [], total: 0, completed: 0 } as never;
    throw new Error(`Unexpected fixture request: ${method} ${path}`);
  });
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  localStorage.clear();
});

async function renderApp() {
  await act(async () => root.render(<App />));
  await act(async () => { await Promise.resolve(); await Promise.resolve(); });
}

const selectionBar = () => container.querySelector<HTMLElement>(".selection-bar");

describe("main account selection surface", () => {
  it("shows batch results in the account toolbar and restores focus through the entry", async () => {
    await renderApp();
    expect(selectionBar()).toBeNull();

    const accountCheckbox = container.querySelector<HTMLInputElement>(
      `[aria-label="选择 ${account(1).name}"]`,
    )!;
    await act(async () => accountCheckbox.click());
    expect(selectionBar()?.textContent).toContain("已选 1 个账号");

    await act(async () =>
      [...container.querySelectorAll<HTMLButtonElement>(".selection-actions button")]
        .find((button) => button.textContent?.includes("测试连接"))!
        .click(),
    );
    await act(async () => { await Promise.resolve(); await Promise.resolve(); });
    const resultsSelector = 'section[aria-label="批量测试连接"]';
    expect(container.querySelector(resultsSelector)?.textContent).toContain("通过");
    expect(container.querySelector('section[aria-label="批量测试连接摘要"]')).toBeNull();

    await act(async () => container.querySelector<HTMLButtonElement>(`${resultsSelector} button`)!.click());
    const entry = container.querySelector<HTMLButtonElement>(
      '.account-toolbar button[aria-label="查看测活结果"]',
    )!;
    expect(container.querySelector(resultsSelector)).toBeNull();
    expect(container.querySelector('section[aria-label="批量测试连接摘要"]')).toBeNull();
    expect(entry.textContent?.trim()).toBe("测活结果");
    expect(entry.title).toBe("已结束 1/1 · 通过 1");
    expect(document.activeElement).toBe(entry);

    await act(async () => accountCheckbox.click());
    expect(selectionBar()).toBeNull();
    expect(entry.isConnected).toBe(true);

    await act(async () => entry.click());
    expect(container.querySelector(resultsSelector)).toBeTruthy();
    expect(document.activeElement).toBe(container.querySelector(resultsSelector));
    expect(container.querySelector('.account-toolbar button[aria-label="查看测活结果"]')).toBeNull();
  });

  it("places mobile select-all in the sort row and hides the selection bar when no account is selected", async () => {
    initialState = state("android");
    await renderApp();

    const mobileSort = container.querySelector<HTMLElement>(".mobile-sort")!;
    const selectAll = mobileSort.querySelector<HTMLInputElement>(
      'input[aria-label="全选当前筛选账号"]',
    )!;
    expect(selectAll.closest(".mobile-sort")).toBe(mobileSort);
    expect(selectionBar()).toBeNull();

    await act(async () => selectAll.click());
    expect(selectionBar()?.textContent).toContain("已选 2 个账号");
    expect(selectionBar()?.querySelector('input[aria-label="全选当前筛选账号"]')).toBeNull();
    expect(selectAll.closest(".mobile-sort")).toBe(mobileSort);

    await act(async () => selectAll.click());
    expect(selectionBar()).toBeNull();
  });
});
