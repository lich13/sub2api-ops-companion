// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import KeepaliveDialog from "./KeepaliveDialog";
import { api, command } from "./bridge";
import type { Account, AccountKeepalive } from "./types";

vi.mock("./bridge", () => ({ api: vi.fn(), command: vi.fn() }));
vi.mock("./mobile", () => ({ useBackAction: vi.fn() }));

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

const account: Account = {
  id: 42,
  name: "保活账号",
  priority: 42,
  platform: "openai",
  type: "oauth",
  status: "active",
  schedulable: true,
  available: true,
  group_ids: [7],
  blockers: [],
  managed: false,
  version: "account-version-42",
  last_success_at: null,
  last_error_at: null,
  last_error_id: null,
  last_error_code: null,
  last_error_status: null,
  error_message: "",
  success_after_error: false,
  usage_windows: [],
};

const version1 = "a".repeat(64);
const baseKeepalive: AccountKeepalive = {
  account_id: 42,
  enabled: false,
  generation: 1,
  version: version1,
  status: "disabled",
  reason: "",
  paused_reason: "",
  last_restore_at: null,
  restore_count: 0,
  budget_used: 0,
  next_allowed_at: null,
  read_only: false,
};

let container: HTMLDivElement;
let root: Root;

function valueFor(id: number, overrides: Partial<AccountKeepalive> = {}): AccountKeepalive {
  return { ...baseKeepalive, account_id: id, ...overrides };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: Error) => void;
  const promise = new Promise<T>((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
}

function button(text: string) {
  const result = [...container.querySelectorAll<HTMLButtonElement>("button")].find(
    (node) => node.textContent?.includes(text),
  );
  if (!result) throw new Error("missing button: " + text);
  return result;
}

function detail(label: string) {
  const term = [...container.querySelectorAll(".detection-meta dt")].find((node) => node.textContent === label);
  return term?.nextElementSibling?.textContent;
}

async function mount(options: {
  online?: boolean;
  connectionKey?: string;
  account?: Account;
  close?: () => void;
  report?: (error: unknown) => void;
} = {}) {
  await act(async () => root.render(
    <KeepaliveDialog
      account={options.account ?? account}
      online={options.online ?? true}
      connectionKey={options.connectionKey ?? "fixture-connection"}
      close={options.close ?? (() => {})}
      report={options.report ?? (() => {})}
    />,
  ));
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}

beforeEach(() => {
  vi.clearAllMocks();
  vi.useFakeTimers();
  vi.mocked(command).mockResolvedValue(undefined as never);
  vi.mocked(api).mockImplementation(async (method, path) => {
    if (method === "GET" && path === "/accounts/42/keepalive") return baseKeepalive as never;
    throw new Error("unexpected " + method + " " + path);
  });
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.useRealTimers();
});

describe("account keepalive", () => {
  it("shows the status reason and confirmed restore count, then saves only the toggle and version", async () => {
    const current = valueFor(42, {
      status: "paused",
      reason: "账号暂时不可调度",
      paused_reason: "恢复次数达到上限",
      last_restore_at: "2026-10-15T12:30:00Z",
      restore_count: 2,
      budget_used: 3,
      next_allowed_at: "2026-10-15T12:31:00Z",
    });
    const saved = { ...current, enabled: true, version: "b".repeat(64) };
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method === "GET" && path === "/accounts/42/keepalive") return current as never;
      if (method === "PUT" && path === "/accounts/42/keepalive") return saved as never;
      throw new Error("unexpected " + method + " " + path);
    });
    await mount();

    expect(detail("状态")).toBe("已暂停");
    expect(detail("原因")).toBe("账号暂时不可调度");
    expect(container.textContent).toContain("2/5 次已确认恢复");
    await act(async () => container.querySelector<HTMLInputElement>('input[type="checkbox"]')!.click());
    await act(async () => button("保存").click());

    expect(api).toHaveBeenCalledWith("PUT", "/accounts/42/keepalive", {
      enabled: true,
      expected_version: version1,
    });
    expect(command).toHaveBeenCalledWith("refresh");
    expect(container.querySelector<HTMLInputElement>('input[type="checkbox"]')?.checked).toBe(true);
  });

  it.each([
    ["disabled", "未启用"],
    ["waiting", "等待检测"],
    ["paused", "已暂停"],
    ["running", "恢复中"],
    ["needs_confirmation", "待核对"],
  ])("accepts the %s status", async (status, label) => {
    vi.mocked(api).mockResolvedValueOnce(valueFor(42, { status }) as never);
    await mount();

    expect(detail("状态")).toBe(label);
    expect(container.querySelector('[role="alert"]')).toBeNull();
  });

  it.each([
    ["a different account", { account_id: 43 }],
    ["a malformed version", { version: "a".repeat(63) }],
    ["a negative generation", { generation: -1 }],
    ["a fractional generation", { generation: 1.5 }],
    ["an unknown status", { status: "unknown" }],
    ["a restore count above five", { restore_count: 6, budget_used: 6 }],
    ["a budget above five", { restore_count: 1, budget_used: 6 }],
    ["a budget below the restore count", { restore_count: 2, budget_used: 1 }],
  ])("rejects a response with %s", async (_case, overrides) => {
    vi.mocked(api).mockResolvedValueOnce(valueFor(42, overrides) as never);
    await mount();

    expect(container.querySelector('[role="alert"]')?.textContent).toContain("保活设置响应无效");
  });

  it("keeps a read-only value visible and disables changes", async () => {
    vi.mocked(api).mockResolvedValueOnce(valueFor(42, { enabled: true, read_only: true }) as never);
    await mount();

    expect(container.textContent).toContain("只读，无法修改保活设置");
    expect(container.querySelector<HTMLInputElement>('input[type="checkbox"]')?.checked).toBe(true);
    expect(container.querySelector<HTMLInputElement>('input[type="checkbox"]')?.disabled).toBe(true);
    expect(button("保存").disabled).toBe(true);
    expect(vi.mocked(api).mock.calls.filter(([method]) => method === "PUT")).toHaveLength(0);
  });

  it("does not read or enable controls while offline", async () => {
    await mount({ online: false });

    expect(api).not.toHaveBeenCalled();
    expect(container.textContent).toContain("连接离线");
    expect(button("重新读取").disabled).toBe(true);
    expect(button("保存").disabled).toBe(true);
    expect(container.querySelector('input[type="checkbox"]')).toBeNull();
  });

  it("updates the live status during polling without replacing an edited toggle", async () => {
    const polled = valueFor(42, {
      enabled: false,
      generation: 2,
      version: "b".repeat(64),
      status: "running",
      reason: "正在恢复",
      restore_count: 1,
      budget_used: 1,
    });
    let reads = 0;
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method === "GET" && path === "/accounts/42/keepalive") return (reads++ === 0 ? baseKeepalive : polled) as never;
      throw new Error("unexpected " + method + " " + path);
    });
    await mount();
    await act(async () => container.querySelector<HTMLInputElement>('input[type="checkbox"]')!.click());

    await act(async () => { await vi.advanceTimersByTimeAsync(2000); });

    expect(container.querySelector<HTMLInputElement>('input[type="checkbox"]')?.checked).toBe(true);
    expect(detail("状态")).toBe("恢复中");
    expect(detail("原因")).toBe("正在恢复");
    expect(container.textContent).toContain("1/5 次已确认恢复");
  });

  it("sends at most one save while the first write is pending", async () => {
    const pending = deferred<AccountKeepalive>();
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method === "GET" && path === "/accounts/42/keepalive") return baseKeepalive as never;
      if (method === "PUT" && path === "/accounts/42/keepalive") return pending.promise as never;
      throw new Error("unexpected " + method + " " + path);
    });
    await mount();
    await act(async () => container.querySelector<HTMLInputElement>('input[type="checkbox"]')!.click());
    await act(async () => button("保存").click());
    await act(async () => button("保存").click());

    expect(button("保存").disabled).toBe(true);
    expect(vi.mocked(api).mock.calls.filter(([method]) => method === "PUT")).toHaveLength(1);
    await act(async () => pending.resolve({ ...baseKeepalive, enabled: true, version: "b".repeat(64) }));
  });

  it("lets a 409 be followed by a fresh read of the server value", async () => {
    const conflict = new Error("HTTP 409 冲突");
    const refreshed = valueFor(42, { enabled: true, status: "waiting", generation: 2, version: "b".repeat(64) });
    const report = vi.fn();
    let reads = 0;
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method === "GET" && path === "/accounts/42/keepalive") return (reads++ === 0 ? baseKeepalive : refreshed) as never;
      if (method === "PUT" && path === "/accounts/42/keepalive") throw conflict;
      throw new Error("unexpected " + method + " " + path);
    });
    await mount({ report });
    await act(async () => container.querySelector<HTMLInputElement>('input[type="checkbox"]')!.click());
    await act(async () => button("保存").click());

    expect(container.querySelector('[role="alert"]')?.textContent).toContain("409");
    expect(report).toHaveBeenCalledWith(conflict);
    await act(async () => button("重新读取").click());

    expect(reads).toBe(2);
    expect(container.querySelector<HTMLInputElement>('input[type="checkbox"]')?.checked).toBe(true);
    expect(container.querySelector('[role="alert"]')).toBeNull();
  });

  it("explains when an older service does not provide keepalive", async () => {
    vi.mocked(api).mockRejectedValueOnce(new Error("HTTP 404 Not Found"));
    await mount();

    expect(container.querySelector('[role="alert"]')?.textContent).toContain("当前服务版本不支持账号保活");
  });

  it("preserves the not-found message for a deleted account without suggesting a service upgrade", async () => {
    const error = "[404] 账号不存在";
    vi.mocked(api).mockRejectedValueOnce(new Error(error));
    await mount();

    expect(container.querySelector('[role="alert"]')?.textContent).toBe(error);
    expect(container.textContent).not.toContain("当前服务版本不支持账号保活");
  });

  it("ignores reads from the previous connection and account", async () => {
    const stale = deferred<AccountKeepalive>();
    const nextConnection = valueFor(42, { status: "waiting", reason: "新连接数据" });
    const nextAccount = valueFor(43, { status: "paused", reason: "新账号数据" });
    let account42Reads = 0;
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method === "GET" && path === "/accounts/42/keepalive") {
        return (account42Reads++ === 0 ? stale.promise : nextConnection) as never;
      }
      if (method === "GET" && path === "/accounts/43/keepalive") return nextAccount as never;
      throw new Error("unexpected " + method + " " + path);
    });
    const close = vi.fn(), report = vi.fn();
    await mount({ connectionKey: "first", close, report });

    await act(async () => root.render(
      <KeepaliveDialog account={account} online connectionKey="second" close={close} report={report}/>,
    ));
    await act(async () => { await Promise.resolve(); await Promise.resolve(); });
    await act(async () => root.render(
      <KeepaliveDialog account={{ ...account, id: 43, name: "新账号" }} online connectionKey="second" close={close} report={report}/>,
    ));
    await act(async () => { await Promise.resolve(); await Promise.resolve(); });
    await act(async () => stale.resolve(valueFor(42, { status: "running", reason: "旧连接数据" })));

    expect(container.querySelector('[role="dialog"]')?.textContent).toContain("新账号 #43");
    expect(detail("状态")).toBe("已暂停");
    expect(detail("原因")).toBe("新账号数据");
    expect(container.textContent).not.toContain("旧连接数据");
  });

  it("ignores a save failure that arrives after the dialog closes", async () => {
    const pending = deferred<AccountKeepalive>();
    const failure = new Error("late save failure");
    const close = vi.fn(), report = vi.fn();
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method === "GET" && path === "/accounts/42/keepalive") return baseKeepalive as never;
      if (method === "PUT" && path === "/accounts/42/keepalive") return pending.promise as never;
      throw new Error("unexpected " + method + " " + path);
    });
    await mount({ close, report });
    await act(async () => container.querySelector<HTMLInputElement>('input[type="checkbox"]')!.click());
    await act(async () => button("保存").click());
    await act(async () => container.querySelector<HTMLButtonElement>('[aria-label="关闭账号保活"]')!.click());
    expect(close).toHaveBeenCalledOnce();
    await act(async () => root.render(null));
    await act(async () => { pending.reject(failure); await Promise.resolve(); });

    expect(report).not.toHaveBeenCalled();
    expect(command).not.toHaveBeenCalledWith("refresh");
  });
});
