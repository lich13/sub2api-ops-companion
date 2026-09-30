// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { api, command, subscribe } from "./bridge";
import App from "./main";
import type { Account, OpsError, ViewState } from "./types";

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

const account = (id = 1, lastErrorId: number | null = null): Account => ({
  id,
  name: `账号 ${id}`,
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
  last_success_at: null,
  last_error_at: lastErrorId ? "2026-10-01T03:00:00Z" : null,
  last_error_id: lastErrorId,
  last_error_code: lastErrorId ? "upstream_error" : null,
  last_error_status: lastErrorId ? 503 : null,
  error_message: "",
  success_after_error: false,
  usage_windows: [],
});

const error = (id: number, name: string): OpsError => ({
  id,
  account_id: 1,
  account_name: name,
  group_id: null,
  group_name: "",
  created_at: "2026-10-01T03:00:00Z",
  model: "gpt-6-sol",
  requested_model: "gpt-6-sol",
  upstream_model: "gpt-6-sol",
  status_code: 503,
  upstream_status_code: 503,
  provider_error_code: "overloaded",
  message: "服务暂时不可用",
  request_id: `request-${id}`,
  resolved: false,
});

const state = (): ViewState => ({
  platform: "macos",
  foreground: true,
  connected: true,
  online: true,
  error: "",
  preferences: {
    base_url: "https://fixture.invalid",
    favorites: [],
    pinned: false,
    launch_at_login: false,
  },
  snapshot: {
    observed_at: "2026-10-01T03:00:00Z",
    accounts: [account(1, 10)],
    groups: [],
    errors: [],
    recoveries: [],
  },
});

const nav = (label: string) =>
  [...container.querySelectorAll<HTMLButtonElement>("aside nav button")].find(
    (button) => button.textContent?.trim().startsWith(label),
  )!;

function recordPage() {
  return {
    items: [],
    next_cursor: null,
    latest_id: null,
    observed_at: "2026-10-01T03:00:00Z",
    summary: { actual_cost: "0" },
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  vi.mocked(subscribe).mockResolvedValue(() => {});
  vi.mocked(command).mockImplementation(async (name) => {
    if (name === "get_state") return state() as never;
    return undefined as never;
  });
  vi.mocked(api).mockImplementation(async (_method, path) => {
    if (path.startsWith("/usage-records")) return recordPage() as never;
    if (path === "/errors") return { items: [], next_cursor: null } as never;
    if (path === "/quota-refresh") return { status: "idle", items: [], total: 0, completed: 0 } as never;
    throw new Error(`Unexpected fixture request: ${path}`);
  });
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
});

describe("main page surface ownership", () => {
  it("unmounts records and events surfaces across repeated navigation", async () => {
    await act(async () => root.render(<App />));

    await act(async () => nav("记录").click());
    expect(container.querySelectorAll('[data-page="records"]').length).toBe(1);
    expect(container.querySelectorAll(".records-view").length).toBe(1);
    expect(container.querySelectorAll(".events-view").length).toBe(0);

    await act(async () => nav("事件").click());
    expect(container.querySelectorAll('[data-page="events"]').length).toBe(1);
    expect(container.querySelectorAll(".records-view").length).toBe(0);
    expect(container.querySelectorAll(".events-view").length).toBe(1);

    await act(async () => nav("账号").click());
    expect(container.querySelectorAll(".records-view").length).toBe(0);
    expect(container.querySelectorAll(".events-view").length).toBe(0);

    await act(async () => nav("记录").click());
    expect(container.querySelectorAll(".records-view").length).toBe(1);
  });

  it("drops a late event page response after leaving and re-entering the page", async () => {
    let firstResolve!: (value: unknown) => void;
    let secondResolve!: (value: unknown) => void;
    let errorsCalls = 0;
    vi.mocked(api).mockImplementation(async (_method, path) => {
      if (path.startsWith("/usage-records")) return recordPage() as never;
      if (path === "/quota-refresh") return { status: "idle", items: [], total: 0, completed: 0 } as never;
      if (path === "/errors") {
        errorsCalls += 1;
        return new Promise((resolve) => {
          if (errorsCalls === 1) firstResolve = resolve;
          else secondResolve = resolve;
        });
      }
      throw new Error(`Unexpected fixture request: ${path}`);
    });

    await act(async () => root.render(<App />));
    await act(async () => nav("事件").click());
    await act(async () => nav("账号").click());
    await act(async () => nav("事件").click());

    await act(async () => firstResolve({ items: [error(11, "旧响应")], next_cursor: null }));
    expect(container.textContent).not.toContain("旧响应");

    await act(async () => secondResolve({ items: [error(12, "新响应")], next_cursor: null }));
    expect(container.textContent).toContain("新响应");
    expect(container.textContent).not.toContain("旧响应");
  });

  it("closes a detail request when the page changes", async () => {
    let resolveDetail!: (value: unknown) => void;
    vi.mocked(api).mockImplementation(async (_method, path) => {
      if (path.startsWith("/usage-records")) return recordPage() as never;
      if (path === "/quota-refresh") return { status: "idle", items: [], total: 0, completed: 0 } as never;
      if (path === "/errors") return { items: [error(21, "详情账号")], next_cursor: null } as never;
      if (path === "/errors/21") return new Promise((resolve) => { resolveDetail = resolve; });
      throw new Error(`Unexpected fixture request: ${path}`);
    });

    await act(async () => root.render(<App />));
    await act(async () => nav("事件").click());
    await act(async () => container.querySelector<HTMLButtonElement>(".error-row")!.click());
    await act(async () => nav("账号").click());
    await act(async () => resolveDetail(error(21, "旧详情")));
    expect(container.querySelector(".drawer")).toBeNull();
  });

  it("keeps full history while distinguishing current and resolved errors", async () => {
    const current = error(31, "当前错误账号");
    const resolved = { ...error(30, "历史错误账号"), resolved: true };
    vi.mocked(api).mockImplementation(async (_method, path) => {
      if (path.startsWith("/usage-records")) return recordPage() as never;
      if (path === "/quota-refresh") return { status: "idle", items: [], total: 0, completed: 0 } as never;
      if (path === "/errors") return { items: [resolved, current], next_cursor: null } as never;
      throw new Error(`Unexpected fixture request: ${path}`);
    });
    const initial = state();
    initial.snapshot!.accounts = [account(1, 31)];
    await act(async () => {
      vi.mocked(command).mockImplementation(async (name) =>
        name === "get_state" ? (initial as never) : (undefined as never),
      );
      root.render(<App />);
    });
    await act(async () => nav("事件").click());
    expect(container.querySelectorAll(".error-row")).toHaveLength(2);
    expect([...container.querySelectorAll(".error-state")].map((node) => node.textContent)).toEqual([
      "当前",
      "已解决",
    ]);
  });
});
