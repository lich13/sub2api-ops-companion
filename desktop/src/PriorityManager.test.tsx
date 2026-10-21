// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import PriorityManager from "./PriorityManager";
import { api, command } from "./bridge";
import { clientId } from "./accountOperations";
import type { Account } from "./types";
import type { PriorityAccount, PriorityTask, PriorityView } from "./priorities";

vi.mock("./bridge", () => ({ api: vi.fn(), command: vi.fn() }));
vi.mock("./accountOperations", () => ({ clientId: vi.fn(() => "fixture-client") }));
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

let container: HTMLDivElement;
let root: Root;
const originalElementFromPoint = Object.getOwnPropertyDescriptor(document, "elementFromPoint");
const originalPointerCapture = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "setPointerCapture");

const priorityItem = (id: number, name: string, priority: number, overrides: Partial<PriorityAccount> = {}): PriorityAccount => ({
  id,
  name,
  platform: id === 103 ? "grok" : "openai",
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
  managed: false,
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

const items = [
  priorityItem(101, "Alpha", 0),
  priorityItem(102, "Beta", 1),
  priorityItem(103, "Grok locked", 2, { editable: false, reason: "由上游托管" }),
];

const view = (overrides: Partial<PriorityView> = {}): PriorityView => ({
  items,
  version: "view-version",
  active_task: null,
  last_task: null,
  ...overrides,
});

const props = (overrides: Partial<Parameters<typeof PriorityManager>[0]> = {}) => ({
  accounts: items.map(account),
  active: true,
  online: true,
  mobile: false,
  connectionKey: "fixture-connection",
  back: vi.fn(),
  changed: vi.fn(),
  ...overrides,
});

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

function task(status: string, overrides: Partial<PriorityTask> = {}): PriorityTask {
  return {
    id: "fixture-task",
    status,
    cancel_requested: false,
    reason: "等待执行",
    total: 3,
    change_count: 2,
    verified_count: 0,
    created_at: "2026-10-20T12:00:00Z",
    updated_at: "2026-10-20T12:00:00Z",
    completed_at: null,
    items: items.map((item, index) => ({
      account_id: item.id,
      account_name: item.name,
      original_priority: item.priority,
      target_priority: index,
      observed_priority: null,
      status: (index === 2 ? "unchanged" : "pending") as "unchanged" | "pending",
    })),
    ...overrides,
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  localStorage.clear();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  vi.mocked(command).mockResolvedValue(undefined as never);
  vi.mocked(clientId).mockReturnValue("fixture-client");
  vi.mocked(api).mockImplementation(async (_method, path) => {
    if (path === "/capabilities") return { features: { account_priority_ordering: true } } as never;
    if (path === "/account-priorities") return view() as never;
    if (path === "/account-priority-orders/fixture-task") return task("completed", { verified_count: 3, reason: "全部已确认" }) as never;
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

describe("global priority manager", () => {
  it("shows the server upgrade state without attempting to read priority rows", async () => {
    vi.mocked(api).mockImplementation(async (_method, path) => {
      if (path === "/capabilities") return { features: { account_priority_ordering: false } } as never;
      throw new Error(`Unexpected fixture request: ${path}`);
    });

    await render();
    expect(container.querySelector('[role="alert"]')?.textContent).toContain("请升级服务端以管理全局优先级");
    expect(api).not.toHaveBeenCalledWith("GET", "/account-priorities");
  });

  it("renders the ordered accounts and keeps unsupported rows locked", async () => {
    await render();
    const rows = container.querySelectorAll<HTMLLIElement>(".priority-list > li");
    expect(rows).toHaveLength(3);
    expect([...rows].map((row) => row.querySelector(".priority-identity strong")?.textContent)).toEqual([
      "Alpha", "Beta", "Grok locked",
    ]);
    const locked = rows[2].querySelector<HTMLButtonElement>(".priority-grip")!;
    expect(locked.disabled).toBe(true);
    expect(rows[2].textContent).toContain("由上游托管");

    const search = container.querySelector<HTMLInputElement>('input[aria-label="搜索优先级账号"]')!;
    await act(async () => {
      const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
      setter.call(search, "Beta");
      search.dispatchEvent(new Event("input", { bubbles: true }));
    });
    expect(container.querySelectorAll(".priority-list > li")).toHaveLength(1);
    expect(container.textContent).toContain("筛选结果仅供查看");
  });

  it("keeps a draft across page deactivation and applies the reordered IDs", async () => {
    await render();
    await act(async () => container.querySelector<HTMLButtonElement>('[aria-label="下移 Alpha"]')!.click());
    expect(container.textContent).toContain("2 个账号待应用");
    expect(container.querySelector(".priority-list > li")?.textContent).toContain("Beta");

    await rerender({ active: false });
    expect(container.querySelector('[aria-label="全局优先级管理"]')).toBeNull();
    await rerender({ active: true });
    expect(container.textContent).toContain("2 个账号待应用");
    expect(container.querySelector(".priority-list > li")?.textContent).toContain("Beta");

    const response = task("completed", { verified_count: 3, reason: "全部已确认" });
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (path === "/capabilities") return { features: { account_priority_ordering: true } } as never;
      if (path === "/account-priorities") return view() as never;
      if (method === "POST" && path === "/account-priority-orders") return response as never;
      throw new Error(`Unexpected fixture request: ${method} ${path}`);
    });
    await act(async () => container.querySelector<HTMLButtonElement>(".priority-draft button.primary")!.click());
    await settle();
    const submit = vi.mocked(api).mock.calls.find(([method, path]) => method === "POST" && path === "/account-priority-orders");
    expect(submit?.[2]).toEqual(expect.objectContaining({
      ordered_account_ids: [102, 101, 103],
      expected_version: "view-version",
      client_id: "fixture-client",
      request_id: expect.any(String),
    }));
    expect(container.textContent).toContain("任务已提交");
  });

  it("reuses the same request identity when a priority submission is retried", async () => {
    let attempts = 0;
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (path === "/capabilities") return { features: { account_priority_ordering: true } } as never;
      if (path === "/account-priorities") return view() as never;
      if (method === "POST" && path === "/account-priority-orders") {
        attempts += 1;
        if (attempts === 1) throw new Error("temporary priority failure");
        return task("completed", { verified_count: 3, reason: "全部已确认" }) as never;
      }
      throw new Error(`Unexpected fixture request: ${method} ${path}`);
    });

    await render();
    await act(async () => container.querySelector<HTMLButtonElement>('[aria-label="下移 Alpha"]')!.click());
    const apply = () => container.querySelector<HTMLButtonElement>(".priority-draft button.primary")!;
    await act(async () => apply().click());
    await settle();
    expect(container.querySelector('[role="alert"]')?.textContent).toContain("temporary priority failure");
    await act(async () => apply().click());
    await settle();

    const submissions = vi.mocked(api).mock.calls.filter(([method, path]) => method === "POST" && path === "/account-priority-orders");
    expect(submissions).toHaveLength(2);
    expect(submissions[0][2]).toEqual(submissions[1][2]);
  });

  it("loads an active task detail and allows stopping pending work", async () => {
    const pending = task("queued");
    const stopped = task("cancelled", { cancel_requested: true, reason: "剩余修改已停止" });
    let cancelled = false;
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (path === "/capabilities") return { features: { account_priority_ordering: true } } as never;
      if (path === "/account-priorities") return view({ active_task: cancelled ? stopped : pending }) as never;
      if (method === "GET" && path === "/account-priority-orders/fixture-task") return (cancelled ? stopped : pending) as never;
      if (method === "POST" && path === "/account-priority-orders/fixture-task/cancel") {
        cancelled = true;
        return stopped as never;
      }
      throw new Error(`Unexpected fixture request: ${method} ${path}`);
    });

    await render();
    const progress = container.querySelector('[aria-label="优先级排序进度"]')!;
    expect(progress.textContent).toContain("等待执行");
    await act(async () => progress.querySelector<HTMLButtonElement>("button:not(.text-button)")!.click());
    await settle();
    expect(api).toHaveBeenCalledWith("POST", "/account-priority-orders/fixture-task/cancel", {});
    expect(container.textContent).toContain("已停止");
  });

  it("supports keyboard moves, undo and discard while filters block changes", async () => {
    await render();
    const order = () => [...container.querySelectorAll<HTMLElement>("[data-priority-id]")].map(row => Number(row.dataset.priorityId));
    const clickText = async (text: string) => act(async () => {
      [...container.querySelectorAll<HTMLButtonElement>("button")].find(button => button.textContent === text)!.click();
    });
    await act(async () => container.querySelector('[aria-label="拖动 Alpha #101"]')!.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true })));
    expect(order()).toEqual([102, 101, 103]);
    const platform = container.querySelector<HTMLSelectElement>('[aria-label="优先级平台"]')!;
    await act(async () => { platform.value = "openai"; platform.dispatchEvent(new Event("change", { bubbles: true })); });
    expect(container.querySelector<HTMLButtonElement>(".priority-draft .primary")!.disabled).toBe(true);
    expect([...container.querySelectorAll<HTMLButtonElement>(".priority-grip")].every(button => button.disabled)).toBe(true);
    await act(async () => { platform.value = ""; platform.dispatchEvent(new Event("change", { bubbles: true })); });
    await clickText("撤销");
    expect(order()).toEqual([101, 102, 103]);
    await act(async () => container.querySelector<HTMLButtonElement>('[aria-label="下移 Alpha"]')!.click());
    await clickText("放弃全部");
    expect(order()).toEqual([101, 102, 103]);
    expect(container.querySelector(".priority-draft")).toBeNull();
  });

  it("reorders by pointer and cancels touch, Escape and hidden-page drags without changing the draft", async () => {
    vi.useFakeTimers();
    vi.stubGlobal("PointerEvent", MouseEvent);
    Object.defineProperty(HTMLElement.prototype, "setPointerCapture", { configurable: true, value: () => {} });
    const point = vi.fn();
    Object.defineProperty(document, "elementFromPoint", { configurable: true, value: point });
    const pointer = async (target: Element, type: string, touch = false) => act(async () => {
      const event = new MouseEvent(type, { button: 0, clientX: 30, clientY: 30, bubbles: true });
      Object.defineProperties(event, { pointerId: { value: 1 }, pointerType: { value: touch ? "touch" : "mouse" } });
      target.dispatchEvent(event);
    });
    await render({ mobile: true });
    const order = () => [...container.querySelectorAll<HTMLElement>("[data-priority-id]")].map(row => Number(row.dataset.priorityId));
    const grip = () => container.querySelector('[aria-label="拖动 Alpha #101"]')!;
    point.mockReturnValue(container.querySelector('[data-priority-id="102"]'));
    await pointer(grip(), "pointerdown", true);
    await act(async () => vi.advanceTimersByTime(350));
    expect(container.querySelector(".priority-drag-ghost")).not.toBeNull();
    await pointer(grip(), "pointercancel", true);
    expect(container.querySelector(".priority-drag-ghost")).toBeNull();
    expect(order()).toEqual([101, 102, 103]);
    for (const reason of ["escape", "hidden"] as const) {
      await pointer(grip(), "pointerdown", true);
      await act(async () => vi.advanceTimersByTime(350));
      await act(async () => {
        if (reason === "escape") window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
        else { vi.spyOn(document, "hidden", "get").mockReturnValue(true); document.dispatchEvent(new Event("visibilitychange")); }
      });
      expect(container.querySelector(".priority-drag-ghost")).toBeNull();
      expect(order()).toEqual([101, 102, 103]);
    }
    await pointer(grip(), "pointerdown", true);
    await act(async () => vi.advanceTimersByTime(350));
    await pointer(grip(), "pointerup", true);
    expect(order()).toEqual([102, 101, 103]);
  });

  it("drops late reads from a previous connection and preserves a submitted task across page changes", async () => {
    let resolveOld!: (value: PriorityView) => void;
    let resolveSubmit!: (value: PriorityTask) => void;
    const old = new Promise<PriorityView>(resolve => { resolveOld = resolve; });
    const submitted = new Promise<PriorityTask>(resolve => { resolveSubmit = resolve; });
    let first = true;
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (path === "/capabilities") return { features: { account_priority_ordering: true } } as never;
      if (path === "/account-priorities") { if (first) { first = false; return old as never; } return view() as never; }
      if (method === "POST" && path === "/account-priority-orders") return submitted as never;
      if (path === "/account-priority-orders/fixture-task") return task("queued") as never;
      throw new Error(`Unexpected fixture request: ${path}`);
    });
    await render();
    await rerender({ connectionKey: "next-connection" });
    await act(async () => resolveOld(view({ items: [priorityItem(999, "Old connection", 0)] })));
    expect(container.textContent).not.toContain("Old connection");
    await act(async () => container.querySelector<HTMLButtonElement>('[aria-label="下移 Alpha"]')!.click());
    await act(async () => container.querySelector<HTMLButtonElement>(".priority-draft .primary")!.click());
    await rerender({ active: false, connectionKey: "next-connection" });
    await act(async () => resolveSubmit(task("queued")));
    vi.mocked(api).mockImplementation(async (_method, path) => {
      if (path === "/capabilities") return { features: { account_priority_ordering: true } } as never;
      if (path === "/account-priorities") return view({ active_task: task("queued") }) as never;
      return task("queued") as never;
    });
    await rerender({ connectionKey: "next-connection" });
    expect(container.textContent).toContain("等待执行");
    expect(container.querySelector<HTMLButtonElement>(".priority-grip")!.disabled).toBe(true);
    expect(vi.mocked(api).mock.calls.filter(([method, path]) => method === "POST" && path === "/account-priority-orders")).toHaveLength(1);
  });

  it("blocks normalization of locked accounts and reparses a failed order against a fresh version", async () => {
    const failed = task("needs_confirmation", { items: [task("queued").items![1], task("queued").items![0], task("queued").items![2]] });
    let latest = view({ items: items.map(item => ({ ...item, priority: item.priority + 5 })), last_task: failed });
    vi.mocked(api).mockImplementation(async (_method, path) => {
      if (path === "/capabilities") return { features: { account_priority_ordering: true } } as never;
      if (path === "/account-priorities") return latest as never;
      return failed as never;
    });
    await render();
    expect(container.textContent).toContain("重编号会修改不可编辑账号：Grok locked #103");
    expect(container.querySelector<HTMLButtonElement>(".priority-draft .primary")!.disabled).toBe(true);
    latest = view({ version: "fresh-version", last_task: failed });
    await act(async () => [...container.querySelectorAll<HTMLButtonElement>("button")].find(button => button.textContent === "重新核对")!.click());
    expect(container.querySelector(".priority-list > li")?.textContent).toContain("Beta");
    expect(container.querySelector<HTMLButtonElement>(".priority-draft .primary")!.disabled).toBe(false);
    latest = view({ version: "new-membership", items: [items[0], items[2]], last_task: failed });
    await act(async () => [...container.querySelectorAll<HTMLButtonElement>("button")].find(button => button.textContent === "重新核对")!.click());
    expect(container.textContent).toContain("账号集合已变化，已按当前优先级重新准备");
    expect(container.querySelectorAll("[data-priority-id]")).toHaveLength(2);
  });
});
