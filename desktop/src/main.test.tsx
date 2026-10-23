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

const eventTab = (label: string) =>
  [...container.querySelectorAll<HTMLButtonElement>('.events-view [role="tab"]')].find(
    (button) => button.textContent === label,
  )!;

const eventNames = () =>
  [...container.querySelectorAll(".error-row strong")].map((node) => node.textContent);

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

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
  localStorage.removeItem("sub2ops-account-sort");
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
    if (path.startsWith("/errors?category=") || path === "/recoveries") return { items: [], next_cursor: null } as never;
    if (path === "/account-priorities") return { items: [], version: "fixture-priority-version", active_task: null, last_task: null } as never;
    if (path === "/quota-refresh") return { status: "idle", items: [], total: 0, completed: 0 } as never;
    throw new Error(`Unexpected fixture request: ${path}`);
  });
});

afterEach(async () => {
  await act(async () => root.unmount());
  localStorage.removeItem("sub2ops-account-sort");
  container.remove();
});

describe("main page surface ownership", () => {
  it("adds a desktop priority page while keeping five mobile navigation items", async () => {
    await act(async () => root.render(<App />));
    expect([...container.querySelectorAll<HTMLButtonElement>("aside nav button")].map((button) => button.textContent?.trim())).toEqual([
      "账号", "分组", "优先级", "记录", "事件", "功能", "设置",
    ]);
    await act(async () => nav("优先级").click());
    expect(container.querySelectorAll('[data-page="priorities"]')).toHaveLength(1);

    await act(async () => root.unmount());
    root = createRoot(container);
    const mobileState = state();
    mobileState.platform = "android";
    vi.mocked(command).mockImplementation(async (name) => name === "get_state" ? mobileState as never : undefined as never);
    await act(async () => root.render(<App />));
    expect([...container.querySelectorAll<HTMLButtonElement>(".bottom-nav button")].map((button) => button.textContent?.trim())).toEqual([
      "账号", "记录", "事件", "功能", "设置",
    ]);
  });

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
      if (path === "/errors?category=degradation") {
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
      if (path === "/errors?category=degradation") return { items: [error(21, "详情账号")], next_cursor: null } as never;
      if (path === "/errors/21?category=degradation") return new Promise((resolve) => { resolveDetail = resolve; });
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
      if (path === "/errors?category=degradation") return { items: [resolved, current], next_cursor: null } as never;
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

describe("categorized event history", () => {
  it("switches all four tabs and paginates each error category without duplicate IDs or snapshot rows", async () => {
    const initial = state();
    initial.snapshot!.errors = [error(999, "仅快照错误")];
    vi.mocked(command).mockImplementation(async (name) => name === "get_state" ? initial as never : undefined as never);
    const original = vi.mocked(api).getMockImplementation()!;
    const pages: Record<string, { items: OpsError[]; next_cursor: number | null }> = {
      "/errors?category=degradation": { items: [error(12, "降智最新"), error(11, "降智分页边界")], next_cursor: 11 },
      "/errors?category=degradation&before_id=11": { items: [error(11, "降智边界更新"), error(10, "降智更早")], next_cursor: null },
      "/errors?category=other": { items: [error(12, "其他最新"), error(8, "其他分页边界")], next_cursor: 8 },
      "/errors?category=other&before_id=8": { items: [error(8, "其他边界更新"), error(7, "其他更早")], next_cursor: null },
    };
    vi.mocked(api).mockImplementation(async (method, path, body) => {
      if (pages[path]) return pages[path] as never;
      if (path === "/capabilities") return { features: { detection_logs: true } } as never;
      if (path === "/detection-logs?limit=50") return { items: [], next_cursor: null } as never;
      if (path === "/recoveries") return {
        items: [{ id: 1, account_id: 1, account_name: "恢复记录账号", model_id: "fixture-model", test_completed_at: null, recovered_at: null, legacy: true }],
        next_cursor: null,
      } as never;
      return original(method, path, body);
    });
    await act(async () => root.render(<App />));
    await act(async () => nav("事件").click());
    expect([...container.querySelectorAll('.events-view [role="tab"]')].map((node) => node.textContent)).toEqual(["降智错误", "其他错误", "恢复历史", "日志"]);
    expect(eventNames()).toEqual(["降智最新", "降智分页边界"]);
    expect(container.textContent).not.toContain("仅快照错误");
    expect(vi.mocked(api).mock.calls.filter(([, path]) => path.startsWith("/errors?")).map(([, path]) => path)).toEqual(["/errors?category=degradation"]);

    await act(async () => container.querySelector<HTMLButtonElement>("#degradation-panel .load-more")!.click());
    expect(eventNames()).toEqual(["降智最新", "降智边界更新", "降智更早"]);
    expect(container.querySelector("#degradation-panel .load-more")).toBeNull();
    await act(async () => eventTab("其他错误").click());
    expect(eventTab("其他错误").getAttribute("aria-selected")).toBe("true");
    expect(eventNames()).toEqual(["其他最新", "其他分页边界"]);
    await act(async () => container.querySelector<HTMLButtonElement>("#other-panel .load-more")!.click());
    expect(eventNames()).toEqual(["其他最新", "其他边界更新", "其他更早"]);

    await act(async () => eventTab("恢复历史").click());
    expect(container.querySelector("#recoveries-panel")?.hasAttribute("hidden")).toBe(false);
    expect(container.querySelector("#recoveries-panel")?.textContent).toContain("恢复记录账号");
    expect(eventNames()).toEqual([]);
    expect(vi.mocked(api).mock.calls.filter(([, path]) => path.startsWith("/errors?")).map(([, path]) => path)).toEqual([
      "/errors?category=degradation", "/errors?category=degradation&before_id=11",
      "/errors?category=other", "/errors?category=other&before_id=8",
    ]);
    await act(async () => eventTab("日志").click());
    expect(container.querySelector("#logs-panel")?.hasAttribute("hidden")).toBe(false);
    expect(container.querySelector("#logs-panel")?.textContent).toContain("暂无自动检测记录");
    expect(vi.mocked(api).mock.calls).toContainEqual(["GET", "/capabilities"]);
    expect(vi.mocked(api).mock.calls).toContainEqual(["GET", "/detection-logs?limit=50"]);
    await act(async () => eventTab("降智错误").click());
    expect(eventNames()).toEqual(["降智最新", "降智分页边界"]);
    expect(container.querySelector("#recoveries-panel")?.hasAttribute("hidden")).toBe(true);
  });

  it("discards a late category response and a late detail after switching tabs", async () => {
    const oldPage = deferred<{ items: OpsError[]; next_cursor: number | null }>();
    const oldDetail = deferred<OpsError>();
    let degradationReads = 0;
    const original = vi.mocked(api).getMockImplementation()!;
    vi.mocked(api).mockImplementation(async (method, path, body) => {
      if (path === "/errors?category=degradation") {
        degradationReads += 1;
        return (degradationReads === 1 ? oldPage.promise : { items: [error(61, "降智新代次")], next_cursor: null }) as never;
      }
      if (path === "/errors?category=other") return { items: [error(61, "其他分类同 ID")], next_cursor: null } as never;
      if (path === "/errors/61?category=other") return oldDetail.promise as never;
      return original(method, path, body);
    });
    await act(async () => root.render(<App />));
    await act(async () => nav("事件").click());
    await act(async () => eventTab("其他错误").click());
    expect(eventNames()).toEqual(["其他分类同 ID"]);
    await act(async () => oldPage.resolve({ items: [error(61, "迟到降智旧页")], next_cursor: 60 }));
    expect(eventNames()).toEqual(["其他分类同 ID"]);
    await act(async () => container.querySelector<HTMLButtonElement>(".error-row")!.click());
    expect(vi.mocked(api).mock.calls).toContainEqual(["GET", "/errors/61?category=other"]);
    await act(async () => eventTab("降智错误").click());
    await act(async () => oldDetail.resolve(error(61, "迟到其他详情")));
    expect(eventNames()).toEqual(["降智新代次"]);
    expect(container.querySelector(".drawer")).toBeNull();
    expect(container.textContent).not.toContain("迟到其他详情");
    expect(container.querySelector("#degradation-panel .load-more")).toBeNull();
  });

  it("clears both categories on connection changes and ignores an old connection's pagination response", async () => {
    const initial = state();
    let receive!: (value: ViewState) => void;
    vi.mocked(subscribe).mockImplementation(async (callback) => { receive = callback; return () => {}; });
    const oldMore = deferred<{ items: OpsError[]; next_cursor: number | null }>();
    const fresh = deferred<{ items: OpsError[]; next_cursor: number | null }>();
    let connection = 0;
    const original = vi.mocked(api).getMockImplementation()!;
    vi.mocked(api).mockImplementation(async (method, path, body) => {
      if (path === "/errors?category=degradation") return (connection ? fresh.promise : { items: [error(31, "旧连接降智")], next_cursor: 31 }) as never;
      if (path === "/errors?category=degradation&before_id=31") return oldMore.promise as never;
      if (path === "/errors?category=other") return { items: [error(31, connection ? "新连接其他" : "旧连接其他")], next_cursor: null } as never;
      return original(method, path, body);
    });
    await act(async () => root.render(<App />));
    await act(async () => nav("事件").click());
    await act(async () => eventTab("其他错误").click());
    expect(eventNames()).toEqual(["旧连接其他"]);
    await act(async () => eventTab("降智错误").click());
    await act(async () => container.querySelector<HTMLButtonElement>("#degradation-panel .load-more")!.click());
    await act(async () => { connection = 1; receive({ ...initial, connection_revision: 1 }); });
    expect(eventNames()).toEqual([]);
    expect(container.querySelector("#degradation-panel .load-more")).toBeNull();
    await act(async () => oldMore.resolve({ items: [error(30, "迟到旧连接分页")], next_cursor: 29 }));
    expect(eventNames()).toEqual([]);
    await act(async () => fresh.resolve({ items: [error(31, "新连接降智")], next_cursor: null }));
    expect(eventNames()).toEqual(["新连接降智"]);
    await act(async () => eventTab("其他错误").click());
    expect(eventNames()).toEqual(["新连接其他"]);
    expect(container.textContent).not.toContain("旧连接");
  });
});

describe("account sorting and Android connection settings", () => {
  it("defaults Android accounts to recent calls and keeps priority sorting", async () => {
    const initial = state();
    initial.platform = "android";
    localStorage.setItem("sub2ops-account-sort", "invalid-sort");
    initial.snapshot!.accounts = [
      Object.assign(account(1), { last_called_at: null, last_success_at: "2026-10-06T00:00:00Z" }),
      Object.assign(account(2), { last_called_at: "2026-10-07T00:00:00Z" }),
      Object.assign(account(3), { last_called_at: "2026-10-05T00:00:00Z" }),
    ];
    vi.mocked(command).mockImplementation(async (name) =>
      name === "get_state" ? (initial as never) : (undefined as never),
    );
    await act(async () => root.render(<App />));
    const names = () =>
      [...container.querySelectorAll<HTMLElement>(".mobile-account .mobile-identity strong")].map((node) => node.textContent);
    expect(names()).toEqual(["账号 2", "账号 1", "账号 3"]);
    const sort = container.querySelector<HTMLSelectElement>('select[aria-label="账号排序"]')!;
    expect(sort.value).toBe("recent:desc");
    expect([...sort.options].map((option) => option.value)).toEqual([
      "recent:desc", "recent:asc", "priority:asc", "priority:desc",
    ]);
    for (const value of ["recent:desc", "recent:asc", "priority:asc", "priority:desc"]) {
      await act(async () => {
        sort.value = value;
        sort.dispatchEvent(new Event("change", { bubbles: true }));
      });
      expect(sort.value).toBe(value);
      expect(localStorage.getItem("sub2ops-account-sort")).toBe(value);
    }
    await act(async () => {
      sort.value = "priority:asc";
      sort.dispatchEvent(new Event("change", { bubbles: true }));
    });
    expect(names()).toEqual(["账号 1", "账号 2", "账号 3"]);
    expect(sort.value).toBe("priority:asc");
    expect(localStorage.getItem("sub2ops-account-sort")).toBe("priority:asc");

    await act(async () => {
      sort.value = "priority:desc";
      sort.dispatchEvent(new Event("change", { bubbles: true }));
      sort.value = "recent:asc";
      sort.dispatchEvent(new Event("change", { bubbles: true }));
    });
    expect(sort.value).toBe("recent:asc");
    expect(localStorage.getItem("sub2ops-account-sort")).toBe("recent:asc");
  });

  it("shares the saved sort between desktop controls and restores it after remount", async () => {
    const initial = state();
    initial.platform = "macos";
    let receive!: (value: ViewState) => void;
    vi.mocked(subscribe).mockImplementation(async (callback) => {
      receive = callback;
      return () => {};
    });
    initial.snapshot!.accounts = [
      Object.assign(account(1), { last_called_at: "2026-10-06T00:00:00Z" }),
      Object.assign(account(2), { last_called_at: "2026-10-07T00:00:00Z" }),
      Object.assign(account(3), { last_called_at: "2026-10-05T00:00:00Z" }),
    ];
    vi.mocked(command).mockImplementation(async (name) =>
      name === "get_state" ? (initial as never) : (undefined as never),
    );
    await act(async () => root.render(<App />));
    const desktopSort = container.querySelector<HTMLSelectElement>('select[aria-label="账号排序"]')!;
    expect(desktopSort.value).toBe("recent:desc");
    await act(async () =>
      container.querySelector<HTMLButtonElement>(".sort-button")!.click(),
    );
    expect(desktopSort.value).toBe("priority:asc");
    expect(container.querySelector('th[aria-sort="ascending"]')).not.toBeNull();
    await act(async () => {
      desktopSort.value = "priority:desc";
      desktopSort.dispatchEvent(new Event("change", { bubbles: true }));
    });
    expect(desktopSort.value).toBe("priority:desc");
    expect(localStorage.getItem("sub2ops-account-sort")).toBe("priority:desc");
    await act(async () => nav("记录").click());
    await act(async () => nav("账号").click());
    expect(container.querySelector<HTMLSelectElement>('select[aria-label="账号排序"]')?.value).toBe("priority:desc");
    await act(async () => receive({ ...initial, connection_revision: 1 }));
    expect(container.querySelector<HTMLSelectElement>('select[aria-label="账号排序"]')?.value).toBe("priority:desc");

    for (const value of ["recent:desc", "recent:asc", "priority:asc", "priority:desc"] as const) {
      await act(async () => root.unmount());
      root = createRoot(container);
      initial.platform = "android";
      localStorage.setItem("sub2ops-account-sort", value);
      await act(async () => root.render(<App />));
      const mobileSort = container.querySelector<HTMLSelectElement>('select[aria-label="账号排序"]')!;
      expect(mobileSort.value).toBe(value);
      expect(localStorage.getItem("sub2ops-account-sort")).toBe(value);
    }
  });

  it("falls back to recent descending when localStorage cannot be read", async () => {
    const initial = state();
    initial.platform = "android";
    vi.mocked(command).mockImplementation(async (name) =>
      name === "get_state" ? (initial as never) : (undefined as never),
    );
    const getItem = vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
      throw new Error("storage unavailable");
    });
    await act(async () => root.render(<App />));
    expect(container.querySelector<HTMLSelectElement>('select[aria-label="账号排序"]')?.value).toBe("recent:desc");
    getItem.mockRestore();
  });

  it("updates the selected sort even when localStorage rejects the write", async () => {
    const initial = state();
    initial.platform = "android";
    vi.mocked(command).mockImplementation(async (name) =>
      name === "get_state" ? (initial as never) : (undefined as never),
    );
    const originalSetItem = Storage.prototype.setItem;
    const setItem = vi.spyOn(Storage.prototype, "setItem").mockImplementation((key, value) => {
      if (key === "sub2ops-account-sort") throw new Error("storage unavailable");
      return originalSetItem.call(localStorage, key, value);
    });
    await act(async () => root.render(<App />));
    const sort = container.querySelector<HTMLSelectElement>('select[aria-label="账号排序"]')!;
    await act(async () => {
      sort.value = "priority:asc";
      sort.dispatchEvent(new Event("change", { bubbles: true }));
    });
    expect(sort.value).toBe("priority:asc");
    expect(setItem).toHaveBeenCalledWith("sub2ops-account-sort", "priority:asc");
    setItem.mockRestore();
  });

  it("shows Android release-page copy and distinct upstream diagnostics with manual retry", async () => {
    const initial = state();
    initial.platform = "android";
    const diagnostics = [
      { state: "network_unreachable", endpoint: "verify", http_status: null, message: "无法连接 Sub2API 上游", retryable: true, checked_at: "2026-10-07T00:00:00Z" },
      { state: "route_not_found", endpoint: "verify", http_status: 404, message: "Sub2API 管理接口路径不存在", retryable: true, checked_at: "2026-10-07T00:01:00Z" },
      { state: "auth_rejected", endpoint: "verify", http_status: 403, message: "管理员 API Key 被上游拒绝，请重新连接", retryable: false, checked_at: "2026-10-07T00:02:00Z" },
    ];
    let statusReads = 0;
    vi.mocked(command).mockImplementation(async (name) =>
      name === "get_state" ? (initial as never) : (undefined as never),
    );
    vi.mocked(api).mockImplementation(async (_method, path) => {
      if (path === "/connection-status") return diagnostics[Math.min(statusReads++, diagnostics.length - 1)] as never;
      if (path === "/config") return { bark: { revision: "fixture", enabled: false, device_key_set: false } } as never;
      if (path === "/account-operations") return { items: [] } as never;
      if (path === "/quota-refresh") return { status: "idle", items: [], total: 0, completed: 0 } as never;
      if (path.startsWith("/errors?category=")) return { items: [], next_cursor: null } as never;
      throw new Error(`Unexpected fixture request: ${path}`);
    });
    await act(async () => root.render(<App />));
    const settings = [...container.querySelectorAll<HTMLButtonElement>(".bottom-nav button")].find((button) => button.textContent?.includes("设置"))!;
    await act(async () => settings.click());
    await act(async () => { await Promise.resolve(); await Promise.resolve(); });
    expect(container.textContent).toContain("查看发布页");
    expect(container.textContent).toContain(diagnostics[0].message);

    const retry = () => [...container.querySelectorAll<HTMLButtonElement>("button")].find((button) =>
      /重试|重新检查|检查连接/.test(`${button.textContent} ${button.getAttribute("aria-label")} ${button.title}`),
    );
    for (const diagnostic of diagnostics.slice(1)) {
      const button = retry();
      expect(button).toBeTruthy();
      await act(async () => button!.click());
      await act(async () => { await Promise.resolve(); await Promise.resolve(); });
      expect(container.textContent).toContain(diagnostic.message);
    }
    expect(statusReads).toBe(3);
    const accounts = [...container.querySelectorAll<HTMLButtonElement>(".bottom-nav button")].find((button) => button.textContent?.includes("账号"))!;
    await act(async () => accounts.click());
    expect(container.querySelector(".mobile-account")?.textContent).toContain("账号 1");
  });
});

it("returns to initial account filters, selection, disclosures and scroll while retaining saved display preferences", async () => {
  const initial=state();
  initial.snapshot!.accounts=[{...account(1),group_ids:[7]},account(2)];
  initial.snapshot!.groups=[{id:7,name:"Fixture group",platform:"openai",status:"active"} as never];
  vi.mocked(command).mockImplementation(async name=>name==="get_state" ? initial as never : undefined as never);
  await act(async()=>root.render(<App/>));
  const select=async(label:string,value:string)=>act(async()=>{const input=container.querySelector<HTMLSelectElement>(`select[aria-label="${label}"]`)!;input.value=value;input.dispatchEvent(new Event("change",{bubbles:true}));});
  const search=container.querySelector<HTMLInputElement>('.filters input[placeholder="搜索名称或账号 ID"]')!;
  await act(async()=>{Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,"value")!.set!.call(search,"账号 1");search.dispatchEvent(new Event("input",{bubbles:true}));});
  for(const [label,value] of [["分组筛选","7"],["平台筛选","openai"],["类型筛选","oauth"],["状态筛选","ready"],["账号排序","priority:desc"]]) await select(label,value);
  await act(async()=>container.querySelector<HTMLInputElement>('[aria-label="全选当前筛选账号"]')!.click());
  const detail=container.querySelector<HTMLDetailsElement>(".accounts-table details"); if(detail) detail.open=true;
  const content=container.querySelector<HTMLElement>(".content")!;content.scrollTop=240;content.scrollLeft=60;
  expect(search.value).toBe("账号 1");
  await act(async()=>nav("记录").click());
  expect(container.querySelector(".breadcrumb strong")?.textContent).toBe("记录");
  expect(container.querySelector('.filters input[placeholder="搜索名称或账号 ID"]')).toBeNull();
  await act(async()=>nav("账号").click());
  expect(container.querySelector<HTMLInputElement>('.filters input[placeholder="搜索名称或账号 ID"]')?.value).toBe("");
  for(const label of ["分组筛选","平台筛选","类型筛选","状态筛选"]) expect(container.querySelector<HTMLSelectElement>(`select[aria-label="${label}"]`)?.value).toBe("");
  expect(container.querySelector<HTMLInputElement>('[aria-label="全选当前筛选账号"]')?.checked).toBe(false);
  expect(container.querySelector(".accounts-table details[open]")).toBeNull();
  expect(container.querySelector<HTMLSelectElement>('[aria-label="账号排序"]')?.value).toBe("priority:desc");
  expect(content.scrollTop).toBe(0); expect(content.scrollLeft).toBe(0);
});
