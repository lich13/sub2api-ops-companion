// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import PriorityManager from "./PriorityManager";
import { api, command } from "./bridge";
import { clientId } from "./accountOperations";
import type { Account } from "./types";
import type { PriorityAccount, PriorityPlatform, PriorityTask, PriorityView } from "./priorities";

vi.mock("./bridge", () => ({ api: vi.fn(), command: vi.fn() }));
vi.mock("./accountOperations", () => ({ clientId: vi.fn(() => "fixture-client") }));
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

let container: HTMLDivElement;
let root: Root;
const originalElementFromPoint = Object.getOwnPropertyDescriptor(document, "elementFromPoint");
const originalPointerCapture = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "setPointerCapture");

const priorityItem = (id: number, name: string, priority: number, platform = "openai", overrides: Partial<PriorityAccount> = {}): PriorityAccount => ({
  id,
  name,
  platform,
  type: "oauth",
  priority,
  version: `priority-version-${id}`,
  editable: true,
  reason: "",
  status: "active",
  schedulable: true,
  ...overrides,
});

const account = (item: PriorityAccount): Account => ({
  id: item.id,
  name: item.name,
  priority: item.priority,
  platform: item.platform,
  type: item.type,
  status: item.status,
  schedulable: item.schedulable,
  available: true,
  group_ids: [],
  blockers: [],
  recoverable: true,
  version: `account-version-${item.id}`,
  operation_versions: { priority: item.version },
  last_success_at: null,
  last_error_at: null,
  last_error_id: null,
  last_error_code: null,
  last_error_status: null,
  error_message: "",
  success_after_error: false,
  usage_windows: [],
});

const openaiItems = [priorityItem(101, "Alpha", 1), priorityItem(102, "Beta", 2)];
const grokItems = [priorityItem(201, "Grok Alpha", 1, "grok"), priorityItem(202, "Grok Beta", 2, "grok")];
const allItems = [...openaiItems, ...grokItems];

const platform = (name: string, items: PriorityAccount[], version = `${name}-version`, last_task: PriorityTask | null = null): PriorityPlatform => ({
  platform: name,
  items,
  version,
  last_task,
});

const view = (overrides: Partial<PriorityView> = {}): PriorityView => ({
  platforms: [platform("openai", openaiItems), platform("grok", grokItems)],
  active_task: null,
  legacy_last_task: null,
  ...overrides,
});

const props = (overrides: Partial<Parameters<typeof PriorityManager>[0]> = {}) => ({
  accounts: allItems.map(account),
  active: true,
  online: true,
  mobile: false,
  connectionKey: "fixture-connection",
  back: vi.fn(),
  changed: vi.fn(),
  schedule: vi.fn(() => null),
  ...overrides,
});

function task(status: string, overrides: Partial<PriorityTask> = {}): PriorityTask {
  return {
    id: "fixture-task",
    status,
    cancel_requested: false,
    reason: "等待执行",
    total: 2,
    change_count: 2,
    verified_count: 0,
    created_at: "2026-10-20T12:00:00Z",
    updated_at: "2026-10-20T12:00:00Z",
    completed_at: null,
    items: openaiItems.map((item, index) => ({
      account_id: item.id,
      account_name: item.name,
      platform: item.platform,
      original_priority: item.priority,
      target_priority: index + 1,
      observed_priority: null,
      status: "pending" as const,
    })),
    platform: "openai",
    base: 1,
    waiting_reason: "",
    waiting_account_ids: [],
    ...overrides,
  };
}

async function settle() {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();
  });
}

async function render(overrides: Partial<Parameters<typeof PriorityManager>[0]> = {}) {
  await act(async () => root.render(<PriorityManager {...props(overrides)} />));
  await settle();
}

async function rerender(overrides: Partial<Parameters<typeof PriorityManager>[0]> = {}) {
  await act(async () => root.render(<PriorityManager {...props(overrides)} />));
  await settle();
}

const list = (platformName = "openai") => container.querySelector<HTMLOListElement>(`[data-priority-platform="${platformName}"] .priority-list`)!;
const order = (platformName = "openai") => [...list(platformName).querySelectorAll<HTMLElement>("[data-priority-id]")].map(row => Number(row.dataset.priorityId));
const button = (label: string) => [...container.querySelectorAll<HTMLButtonElement>("button")].find(item => item.textContent?.trim() === label)!;

beforeEach(() => {
  vi.clearAllMocks();
  localStorage.clear();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  vi.mocked(command).mockResolvedValue(undefined as never);
  vi.mocked(clientId).mockReturnValue("fixture-client");
  vi.mocked(api).mockImplementation(async (_method, path) => {
    if (path === "/capabilities") return { features: { account_priority_ordering: true, account_priority_per_platform: true } } as never;
    if (path === "/account-priorities?schema=2") return view() as never;
    if (path === "/account-priority-orders/fixture-task") return task("completed", { verified_count: 2, reason: "全部已确认" }) as never;
    throw new Error(`Unexpected fixture request: ${path}`);
  });
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.useRealTimers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  if (originalElementFromPoint) Object.defineProperty(document, "elementFromPoint", originalElementFromPoint);
  else Reflect.deleteProperty(document, "elementFromPoint");
  if (originalPointerCapture) Object.defineProperty(HTMLElement.prototype, "setPointerCapture", originalPointerCapture);
  else Reflect.deleteProperty(HTMLElement.prototype, "setPointerCapture");
});

describe("platform priority manager", () => {
  it.each([true, false])("waits for actual editability before showing a restriction (%s)", async editable => {
    const items = [priorityItem(101, "Alpha", 10, "openai", { editable })];
    let resolve!: (value: PriorityView) => void;
    const pending = new Promise<PriorityView>(done => { resolve = done; });
    vi.mocked(api).mockImplementation(async (_method, path) => {
      if (path === "/capabilities") return { features: { account_priority_per_platform: true } } as never;
      if (path === "/account-priorities?schema=2") return pending as never;
      throw new Error(path);
    });
    await render({ accounts: items.map(account) });
    expect(container.textContent).toContain("正在读取优先级");
    expect(container.querySelector('[role="alert"]')).toBeNull();
    expect(button("应用变更").disabled).toBe(true);
    await act(async () => { resolve(view({ platforms: [platform("openai", items)] })); });
    expect(container.textContent?.includes("重编号会修改不可编辑账号")).toBe(!editable);
    expect(button("应用变更").disabled).toBe(!editable);
  });

  it("keeps an initial list failure visible without enabling fallback edits", async () => {
    vi.mocked(api).mockImplementation(async (_method, path) => {
      if (path === "/capabilities") return { features: { account_priority_per_platform: true } } as never;
      throw new Error("fixture list unavailable");
    });
    await render({ accounts: [account(priorityItem(101, "Alpha", 10))] });
    expect(container.querySelector('[role="alert"]')?.textContent).toBe("fixture list unavailable");
    expect(container.textContent).not.toContain("重编号会修改不可编辑账号");
    expect(button("应用变更").disabled).toBe(true);
  });

  it.each(["capabilities", "list"])("continues task polling after a failed %s refresh", async failure => {
    vi.useFakeTimers();
    const running = task("running"), finished = task("completed", { verified_count: 2, reason: "全部已确认" });
    let capsReads = 0, listReads = 0, polls = 0;
    vi.mocked(api).mockImplementation(async (_method, path) => {
      if (path === "/capabilities") {
        if (++capsReads === 2 && failure === "capabilities") throw new Error("fixture refresh failed");
        return { features: { account_priority_per_platform: true } } as never;
      }
      if (path === "/account-priorities?schema=2") {
        if (++listReads === 2 && failure === "list") throw new Error("fixture refresh failed");
        return view({ active_task: polls >= 2 ? null : running, platforms: [platform("openai", openaiItems, "openai-version", polls >= 2 ? finished : running)] }) as never;
      }
      if (path.endsWith("&details=0")) return (++polls >= 2 ? finished : running) as never;
      throw new Error(path);
    });
    await render();
    await act(async () => container.querySelector<HTMLButtonElement>('[aria-label="刷新优先级"]')!.click());
    expect(container.querySelector('[role="alert"]')?.textContent).toBe("fixture refresh failed");
    await act(async () => { await vi.advanceTimersByTimeAsync(1000); });
    expect(polls).toBe(1);
    await act(async () => { await vi.advanceTimersByTimeAsync(1000); });
    expect(polls).toBe(2);
    expect(container.querySelector(".priority-result")?.textContent).toContain("全部已确认 · 2/2 已确认");
    expect(command).toHaveBeenCalledExactlyOnceWith("refresh");
    await act(async () => { await vi.advanceTimersByTimeAsync(2000); });
    expect(polls).toBe(2);
  });

  it.each(["inactive", "offline", "hidden", "connection"])("ignores an in-flight task response after becoming %s", async change => {
    vi.useFakeTimers();
    const running = task("running");
    let resolve!: (value: PriorityTask) => void;
    const pending = new Promise<PriorityTask>(done => { resolve = done; });
    let listReads = 0, polls = 0;
    vi.mocked(api).mockImplementation(async (_method, path) => {
      if (path === "/capabilities") return { features: { account_priority_per_platform: true } } as never;
      if (path === "/account-priorities?schema=2") return view({ active_task: ++listReads === 1 ? running : null }) as never;
      if (path.endsWith("&details=0")) { ++polls; return pending as never; }
      throw new Error(path);
    });
    await render();
    await act(async () => { await vi.advanceTimersByTimeAsync(1000); });
    expect(polls).toBe(1);
    if (change === "hidden") {
      vi.spyOn(document, "hidden", "get").mockReturnValue(true);
      await act(async () => document.dispatchEvent(new Event("visibilitychange")));
    } else await rerender(change === "inactive" ? { active: false } : change === "offline" ? { online: false } : { connectionKey: "new-fixture-connection" });
    await act(async () => { resolve(task("completed", { verified_count: 2 })); });
    await act(async () => { await vi.advanceTimersByTimeAsync(3000); });
    expect(polls).toBe(1);
    expect(container.querySelector(".priority-result")).toBeNull();
    expect(command).not.toHaveBeenCalled();
  });

  it("protects only edited drafts while preserving normalization previews", async () => {
    const changed = vi.fn(), items = [priorityItem(101, "Alpha", 10), priorityItem(102, "Beta", 20)];
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (path === "/capabilities") return { features: { account_priority_per_platform: true } } as never;
      if (path === "/account-priorities?schema=2") return view({ platforms: [platform("openai", items)] }) as never;
      if (method === "POST") return task("queued") as never;
      throw new Error(path);
    });
    const input = { changed, accounts: items.map(account) };
    await render({ ...input, active: false });
    expect(changed).toHaveBeenLastCalledWith(0);
    await rerender(input);
    expect(changed).toHaveBeenLastCalledWith(0);
    expect(button("应用变更").disabled).toBe(false);
    const move = async () => act(async () => list().querySelector<HTMLButtonElement>('[aria-label="下移 Alpha"]')!.click());
    await move();
    expect(changed).toHaveBeenLastCalledWith(2);
    await rerender({ ...input, active: false });
    expect(changed).toHaveBeenLastCalledWith(2);
    await rerender(input);
    await act(async () => button("撤销").click());
    expect(changed).toHaveBeenLastCalledWith(0);
    await move();
    await act(async () => button("放弃全部").click());
    expect(changed).toHaveBeenLastCalledWith(0);
    await move();
    await act(async () => button("应用变更").click());
    expect(changed).toHaveBeenLastCalledWith(0);
  });

  it("reads the schema 2 view, uses platform tabs, and preserves drafts while switching tabs", async () => {
    await render();
    expect(api).toHaveBeenCalledWith("GET", "/account-priorities?schema=2");
    expect([...container.querySelectorAll<HTMLButtonElement>('[role="tab"]')].map(item => item.textContent?.replace(/\d+$/, ""))).toEqual(["Codex", "Grok"]);
    expect(container.querySelector<HTMLElement>('[data-priority-platform="openai"]')!.hidden).toBe(false);
    expect(container.querySelector<HTMLElement>('[data-priority-platform="grok"]')!.hidden).toBe(true);

    await act(async () => container.querySelector<HTMLButtonElement>('[aria-label="下移 Alpha"]')!.click());
    expect(order()).toEqual([102, 101]);

    await act(async () => container.querySelector<HTMLButtonElement>('[role="tab"][aria-controls="priority-platform-grok"]')!.click());
    expect(container.querySelector<HTMLElement>('[data-priority-platform="openai"]')!.hidden).toBe(true);
    expect(container.querySelector<HTMLElement>('[data-priority-platform="grok"]')!.hidden).toBe(false);
    expect(container.querySelector('[aria-label="有未应用草稿"]')).not.toBeNull();

    await act(async () => container.querySelector<HTMLButtonElement>('[role="tab"][aria-controls="priority-platform-openai"]')!.click());
    expect(order()).toEqual([102, 101]);
  });

  it("keeps search read-only, supports keyboard moves, and undoes a same-platform edit", async () => {
    await render();
    const search = container.querySelector<HTMLInputElement>('input[aria-label="搜索优先级账号"]')!;
    await act(async () => {
      const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
      setter.call(search, "Alpha");
      search.dispatchEvent(new Event("input", { bubbles: true }));
    });
    expect(list().querySelectorAll("li")).toHaveLength(1);
    expect(list().querySelector<HTMLButtonElement>('[aria-label="拖动 Alpha"]')!.disabled).toBe(true);
    expect(container.textContent).not.toContain("搜索结果仅供查看");

    await act(async () => {
      const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
      setter.call(search, "");
      search.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await act(async () => list().querySelector<HTMLButtonElement>('[aria-label="拖动 Alpha"]')!.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true })));
    expect(order()).toEqual([102, 101]);
    await act(async () => button("撤销").click());
    expect(order()).toEqual([101, 102]);
  });

  it("includes platform and schema fields in a submission and refreshes terminal work once", async () => {
    const completed = task("completed", { verified_count: 2, reason: "全部已确认" });
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (path === "/capabilities") return { features: { account_priority_ordering: true, account_priority_per_platform: true } } as never;
      if (path === "/account-priorities?schema=2") return view() as never;
      if (method === "POST" && path === "/account-priority-orders?schema=2") return completed as never;
      if (method === "GET" && path === "/account-priority-orders/fixture-task?schema=2") return completed as never;
      throw new Error(`Unexpected fixture request: ${method} ${path}`);
    });

    await render();
    await act(async () => container.querySelector<HTMLButtonElement>('[aria-label="下移 Alpha"]')!.click());
    await act(async () => button("应用变更").click());
    await settle();

    const submit = vi.mocked(api).mock.calls.find(([method, path]) => method === "POST" && path === "/account-priority-orders?schema=2");
    expect(submit?.[2]).toEqual(expect.objectContaining({
      platform: "openai",
      ordered_account_ids: [102, 101],
      expected_version: "openai-version",
      client_id: "fixture-client",
      request_id: expect.any(String),
    }));
    expect(vi.mocked(api).mock.calls.filter(([method, path]) => method === "GET" && path === "/account-priority-orders/fixture-task?schema=2")).toHaveLength(0);
    expect(vi.mocked(api).mock.calls.filter(([method, path]) => method === "GET" && path === "/account-priorities?schema=2")).toHaveLength(2);
    expect(vi.mocked(command).mock.calls.filter(([name]) => name === "refresh")).toHaveLength(1);
  });

  it("polls summaries and shows waiting accounts without a detail panel", async () => {
    vi.useFakeTimers();
    const pending = task("queued", { waiting_reason: "等待上游请求结束", waiting_account_ids: [101] });
    let summaryCalls = 0;
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (path === "/capabilities") return { features: { account_priority_ordering: true, account_priority_per_platform: true } } as never;
      if (path === "/account-priorities?schema=2") return view({ active_task: pending }) as never;
      if (method === "GET" && path === "/account-priority-orders/fixture-task?schema=2&details=0") { summaryCalls += 1; return pending as never; }
      if (method === "GET" && path === "/account-priority-orders/fixture-task?schema=2") return pending as never;
      throw new Error(`Unexpected fixture request: ${method} ${path}`);
    });

    await render();
    expect(container.textContent).toContain("等待上游请求结束：Alpha #101");
    expect(vi.mocked(api).mock.calls.some(([, path]) => path === "/account-priority-orders/fixture-task?schema=2&details=0")).toBe(false);
    await act(async () => { await vi.advanceTimersByTimeAsync(1000); });
    await settle();
    expect(summaryCalls).toBe(1);
    expect(vi.mocked(api).mock.calls.filter(([method, path]) => method === "GET" && path === "/account-priority-orders/fixture-task?schema=2")).toHaveLength(0);

    expect(button("逐项详情")).toBeUndefined();
    expect(container.querySelector("progress")).toBeNull();
  });

  it("finishes a terminal poll with one list read and one refresh", async () => {
    vi.useFakeTimers();
    const pending = task("queued");
    const completed = task("completed", { verified_count: 2, reason: "全部已确认" });
    let summaryCalls = 0;
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (path === "/capabilities") return { features: { account_priority_ordering: true, account_priority_per_platform: true } } as never;
      if (path === "/account-priorities?schema=2") return view({ active_task: pending }) as never;
      if (method === "GET" && path === "/account-priority-orders/fixture-task?schema=2&details=0") { summaryCalls += 1; return completed as never; }
      if (method === "GET" && path === "/account-priority-orders/fixture-task?schema=2") return completed as never;
      throw new Error(`Unexpected fixture request: ${method} ${path}`);
    });
    await render();
    await act(async () => { await vi.advanceTimersByTimeAsync(1000); });
    await settle();
    expect(summaryCalls).toBe(1);
    expect(vi.mocked(api).mock.calls.filter(([method, path]) => method === "GET" && path === "/account-priority-orders/fixture-task?schema=2")).toHaveLength(0);
    expect(vi.mocked(api).mock.calls.filter(([method, path]) => method === "GET" && path === "/account-priorities?schema=2")).toHaveLength(2);
    expect(vi.mocked(command).mock.calls.filter(([name]) => name === "refresh")).toHaveLength(1);
  });

  it("uses the refreshed list when a task finishes while the page is hidden", async () => {
    const pending = task("running"), completed = task("completed", { verified_count: 2 });
    let finished = false;
    vi.mocked(api).mockImplementation(async (_method, path) => {
      if (path === "/capabilities") return { features: { account_priority_per_platform: true } } as never;
      if (path === "/account-priorities?schema=2") return view({ active_task: finished ? null : pending, platforms: [platform("openai", openaiItems, "openai-version", finished ? completed : pending)] }) as never;
      if (path === "/account-priority-orders/fixture-task?schema=2") return completed as never;
      throw new Error(path);
    });
    await render();
    await render({ active: false });
    finished = true;
    await render({ active: true });
    await settle();
    expect(vi.mocked(api).mock.calls.filter(([, path]) => path === "/account-priority-orders/fixture-task?schema=2")).toHaveLength(0);
    expect(vi.mocked(api).mock.calls.filter(([, path]) => path === "/account-priorities?schema=2")).toHaveLength(2);
    expect(vi.mocked(command).mock.calls.filter(([name]) => name === "refresh")).toHaveLength(1);
  });

  it("allows a draft on another platform while a platform task is busy, but never submits it", async () => {
    const pending = task("running", { platform: "openai" });
    vi.mocked(api).mockImplementation(async (_method, path) => {
      if (path === "/capabilities") return { features: { account_priority_ordering: true, account_priority_per_platform: true } } as never;
      if (path === "/account-priorities?schema=2") return view({ active_task: pending }) as never;
      throw new Error(`Unexpected fixture request: ${path}`);
    });
    await render();
    expect(list("openai").querySelector<HTMLButtonElement>(".priority-grip")!.disabled).toBe(true);

    await act(async () => container.querySelector<HTMLButtonElement>('[role="tab"][aria-controls="priority-platform-grok"]')!.click());
    await act(async () => container.querySelector<HTMLButtonElement>('[aria-label="下移 Grok Alpha"]')!.click());
    expect(order("grok")).toEqual([202, 201]);
    expect(container.querySelector<HTMLButtonElement>('[data-priority-platform="grok"] .priority-draft .primary')!.disabled).toBe(true);
    expect(vi.mocked(api).mock.calls.some(([method]) => method === "POST")).toBe(false);
  });

  it("rejects a cross-platform pointer drop while accepting a same-platform drop", async () => {
    vi.stubGlobal("PointerEvent", MouseEvent);
    Object.defineProperty(HTMLElement.prototype, "setPointerCapture", { configurable: true, value: () => {} });
    const point = vi.fn();
    Object.defineProperty(document, "elementFromPoint", { configurable: true, value: point });
    await render();
    const grip = () => list().querySelector<HTMLButtonElement>('[aria-label="拖动 Alpha"]')!;
    const pointer = async (target: Element, type: string, moved = false) => act(async () => {
      const event = new MouseEvent(type, { button: 0, clientX: moved ? 40 : 30, clientY: moved ? 40 : 30, bubbles: true });
      Object.defineProperties(event, { pointerId: { value: 1 }, pointerType: { value: "mouse" } });
      target.dispatchEvent(event);
    });

    point.mockReturnValue(container.querySelector<HTMLElement>('[data-priority-platform="grok"]'));
    await pointer(grip(), "pointerdown");
    await pointer(grip(), "pointermove", true);
    await pointer(grip(), "pointerup", true);
    expect(order()).toEqual([101, 102]);

    point.mockReturnValue(list().querySelector('[data-priority-id="102"]'));
    await pointer(grip(), "pointerdown");
    await pointer(grip(), "pointermove", true);
    await pointer(grip(), "pointerup", true);
    expect(order()).toEqual([102, 101]);
  });

  it("keeps legacy full-list reads read-only and renders a base zero task", async () => {
    const oldItems = [priorityItem(101, "Alpha", 1), priorityItem(102, "Beta", 2)];
    const legacy = task("completed", { platform: null, base: 0, reason: "旧服务端任务", verified_count: 2 });
    vi.mocked(api).mockImplementation(async (_method, path) => {
      if (path === "/capabilities") return { features: { account_priority_ordering: true, account_priority_per_platform: false } } as never;
      if (path === "/account-priorities") return { items: oldItems, version: "legacy-version", active_task: null, last_task: null } as never;
      throw new Error(`Unexpected fixture request: ${path}`);
    });
    await render();
    expect(api).toHaveBeenCalledWith("GET", "/account-priorities");
    expect(api).not.toHaveBeenCalledWith("GET", "/account-priorities?schema=2");
    expect([...container.querySelectorAll<HTMLButtonElement>(".priority-grip")].every(item => item.disabled)).toBe(true);
    expect(legacy.base).toBe(0);
    expect(container.textContent).toContain("请升级服务端以按平台管理优先级");
  });

  it("drops late responses after a connection change and keeps the new task", async () => {
    let resolveOld!: (value: PriorityView) => void;
    let listCalls = 0;
    const old = new Promise<PriorityView>(resolve => { resolveOld = resolve; });
    const pending = task("queued", { id: "new-task" });
    vi.mocked(api).mockImplementation(async (_method, path) => {
      if (path === "/capabilities") return { features: { account_priority_ordering: true, account_priority_per_platform: true } } as never;
      if (path === "/account-priorities?schema=2") {
        listCalls += 1;
        return listCalls === 1 ? old as never : view({ active_task: pending }) as never;
      }
      throw new Error(`Unexpected fixture request: ${path}`);
    });
    await render();
    await rerender({ connectionKey: "next-connection" });
    await act(async () => resolveOld(view({ platforms: [platform("openai", [priorityItem(999, "Old connection", 1)])] })));
    await settle();
    expect(container.textContent).not.toContain("Old connection");
    expect(container.textContent).toContain("等待执行");
    expect(container.querySelector<HTMLButtonElement>('[data-priority-platform="openai"] .priority-grip')!.disabled).toBe(true);
  });

  it("prevents normalization when a locked row would be renumbered", async () => {
    const locked = priorityItem(102, "Locked", 3, "openai", { editable: false, reason: "由上游托管" });
    const accounts = [priorityItem(101, "Alpha", 5), locked];
    vi.mocked(api).mockImplementation(async (_method, path) => {
      if (path === "/capabilities") return { features: { account_priority_ordering: true, account_priority_per_platform: true } } as never;
      if (path === "/account-priorities?schema=2") return view({ platforms: [platform("openai", accounts)] }) as never;
      throw new Error(`Unexpected fixture request: ${path}`);
    });
    await render();
    expect(container.textContent).toContain("重编号会修改不可编辑账号：Locked #102");
    expect(container.querySelector<HTMLButtonElement>('[data-priority-platform="openai"] .priority-draft .primary')!.disabled).toBe(true);
  });

  it.each([2, 3, 4])("uses the width threshold for %i platforms and falls back to tabs", async count => {
    const groups = [...view().platforms, platform("anthropic", [priorityItem(301, "Claude", 1, "anthropic")]), platform("custom", [priorityItem(401, "Custom", 1, "custom")])].slice(0, count);
    vi.mocked(api).mockImplementation(async (_method, path) => {
      if (path === "/capabilities") return { features: { account_priority_per_platform: true } } as never;
      if (path === "/account-priorities?schema=2") return view({ platforms: groups }) as never;
      throw new Error(path);
    });
    let width = count === 2 ? 960 : count === 3 ? 984 : 1600;
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
      return { width: this.classList.contains("priority-manager") ? width : 0, height: 0, top: 0, bottom: 0, left: 0, right: 0, x: 0, y: 0, toJSON: () => ({}) } as DOMRect;
    });
    await render();
    expect(container.querySelector(".priority-platforms")?.classList.contains("is-columns")).toBe(count <= 3);
    expect(container.querySelector('[role="tablist"]') === null).toBe(count <= 3);
    expect([...container.querySelectorAll<HTMLElement>("[data-priority-platform]")].every(section => !section.hidden)).toBe(count <= 3);

    await act(async () => root.unmount());
    container.replaceChildren();
    root = createRoot(container);
    width--;
    await render();
    expect(container.querySelector(".priority-platforms")?.classList.contains("is-columns")).toBe(false);
    expect(container.querySelector('[role="tablist"]')).not.toBeNull();
    expect(container.querySelector<HTMLElement>('[data-priority-platform="grok"]')!.hidden).toBe(true);
  });
});

it.each(["completed", "failed", "cancelled", "needs_confirmation"])("dismisses %s priority results after three seconds without replaying history", async status => {
  vi.useFakeTimers();
  const terminal = task(status, { verified_count: 1, reason: "fixture result" });
  let finished = false;
  vi.mocked(api).mockImplementation(async (method, path) => {
    if (path === "/capabilities") return { features: { account_priority_per_platform: true } } as never;
    if (path === "/account-priorities?schema=2") return view({ platforms: [platform("openai", openaiItems, "openai-version", finished ? terminal : null)] }) as never;
    if (method === "POST") { finished = true; return terminal as never; }
    if (path === "/account-priority-orders/fixture-task?schema=2") return terminal as never;
    throw new Error(path);
  });
  await render();
  await act(async () => list().querySelector<HTMLButtonElement>('[aria-label="下移 Alpha"]')!.click());
  await act(async () => button("应用变更").click());
  await settle();
  expect(container.querySelector(".priority-result")?.textContent).toContain("fixture result");
  expect(container.querySelector("progress")).toBeNull();
  await act(async () => { await vi.advanceTimersByTimeAsync(2999); });
  expect(container.querySelector(".priority-result")).not.toBeNull();
  await act(async () => { await vi.advanceTimersByTimeAsync(1); });
  expect(container.querySelector(".priority-result")).toBeNull();
  await rerender({ active: false });
  await rerender({ active: true });
  expect(container.querySelector(".priority-result")).toBeNull();
  if (status !== "completed") {
    expect(button("重新核对")).toBeTruthy();
    await act(async () => button("重新核对").click());
    expect(vi.mocked(api).mock.calls.filter(([method, path]) => method === "GET" && path === "/account-priority-orders/fixture-task?schema=2")).toHaveLength(1);
  }
});

it("renders scheduling from live accounts while retaining a priority draft", async () => {
  const schedule = vi.fn((account: Account) => <button role="switch" aria-label={`${account.name}调度`} aria-checked={account.schedulable}/>);
  await render({ schedule });
  await act(async () => list().querySelector<HTMLButtonElement>('[aria-label="下移 Alpha"]')!.click());
  const changed = allItems.map(account).map(value => value.id === 101 ? { ...value, schedulable: false } : value);
  await rerender({ schedule, accounts: changed });
  expect(order()).toEqual([102, 101]);
  expect(list().querySelector('[aria-label="Alpha调度"]')?.getAttribute("aria-checked")).toBe("false");
  expect(list().querySelector('[data-priority-id="101"]')?.textContent).toContain("不可调度");
  expect(vi.mocked(api).mock.calls.some(([method]) => method === "POST")).toBe(false);
});

it("retains an accepted submission while hidden and refreshes the result on reentry", async () => {
  const changed = vi.fn(), terminal = task("completed", { verified_count: 2 });
  let finish!: (value: PriorityTask) => void, accepted = false;
  vi.mocked(api).mockImplementation(async (method, path) => {
    if (path === "/capabilities") return { features: { account_priority_per_platform: true } } as never;
    if (path === "/account-priorities?schema=2") return view({ platforms: [platform("openai", accepted ? [priorityItem(102, "Beta", 1), priorityItem(101, "Alpha", 2)] : openaiItems, "openai-version", accepted ? terminal : null)] }) as never;
    if (method === "POST") return new Promise<PriorityTask>(resolve => { finish = resolve; }) as never;
    throw new Error(path);
  });
  await render({ changed });
  await act(async () => list().querySelector<HTMLButtonElement>('[aria-label="下移 Alpha"]')!.click());
  await act(async () => button("应用变更").click());
  await rerender({ changed, active: false });
  expect(changed).toHaveBeenLastCalledWith(2);
  await act(async () => { accepted = true; finish(terminal); });
  expect(changed).toHaveBeenLastCalledWith(0);
  await rerender({ changed });
  expect(order()).toEqual([102, 101]);
  expect(container.querySelector('[aria-label="有未应用草稿"]')).toBeNull();
  expect(vi.mocked(api).mock.calls.filter(([method]) => method === "POST")).toHaveLength(1);
});

it("keeps empty slots at every priority and applies a move into a slot without renumbering peers", async () => {
  const parallel = view({ platforms: [{ ...platform("openai", openaiItems), parallelism: 3 }] });
  vi.mocked(api).mockImplementation(async (method, path) => {
    if (path === "/capabilities") return { features: { account_priority_ordering: true, account_priority_per_platform: true } } as never;
    if (path === "/account-priorities?schema=2") return parallel as never;
    if (method === "POST") return task("queued", { parallelism: 3 }) as never;
    throw new Error(path);
  });
  await render();
  expect(list().querySelectorAll(".priority-slot-empty")).toHaveLength(7);
  expect([...list().querySelectorAll(".priority-lane-label")].map(node => node.textContent)).toEqual(["1", "2", "3"]);
  expect([...list().querySelectorAll(".priority-slot-grid")].map(node => node.children.length)).toEqual([3, 3, 3]);
  await act(async () => list().querySelector<HTMLButtonElement>('[aria-label="拖动 Beta"]')!.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowUp", bubbles: true })));
  await act(async () => button("应用变更").click());
  expect(api).toHaveBeenCalledWith("POST", "/account-priority-orders?schema=2", expect.objectContaining({ ordered_account_ids: [101, 102], target_priorities: [1, 1], parallelism: 3 }));
});
