// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import ProxyBatchDialog from "./ProxyBatchDialog";
import { api, command } from "./bridge";
import { bindOperationConnection, type Operation } from "./accountOperations";
import type { Account } from "./types";

vi.mock("./bridge", () => ({ api: vi.fn(), command: vi.fn() }));
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

type Settings = {
  account_id: number;
  priority: number;
  concurrency: number | null;
  load_factor: number | null;
  effective_load_factor: number;
  proxy_id: number | null;
  model_config: { whitelist: string[]; mappings: { source: string; target: string }[] };
  capabilities: Record<string, { editable: boolean; reason: string }>;
  versions: Record<string, string>;
};

const account = (id: number, overrides: Partial<Account> = {}): Account => ({
  id,
  name: `账号 ${id}`,
  priority: id,
  concurrency: 2,
  load_factor: 80,
  platform: "openai",
  type: "oauth",
  status: "active",
  schedulable: true,
  available: true,
  group_ids: [],
  blockers: [],
  recoverable: true,
  version: `account-version-${id}`,
  operation_versions: { proxy: `operation-version-${id}` },
  last_success_at: null,
  last_error_at: null,
  last_error_id: null,
  last_error_code: null,
  last_error_status: null,
  error_message: "",
  success_after_error: false,
  usage_windows: [],
  degradation_mark: { marked: false, version: `mark-version-${id}` },
  ...overrides,
});

const settings = (id: number, overrides: Partial<Settings> = {}): Settings => ({
  account_id: id,
  priority: id,
  concurrency: 2,
  load_factor: 80,
  effective_load_factor: 80,
  proxy_id: 2,
  model_config: { whitelist: [], mappings: [] },
  capabilities: {
    model_config: { editable: true, reason: "" },
    proxy: { editable: true, reason: "" },
    sync_models: { editable: true, reason: "" },
  },
  versions: {
    model_config: `model-version-${id}`,
    proxy: `proxy-version-${id}`,
    sync_models: `sync-version-${id}`,
  },
  ...overrides,
});

const proxies = [
  { id: 2, name: "东京代理", protocol: "http", status: "active" },
  { id: 3, name: "停用代理", protocol: "socks5", status: "disabled" },
  { id: 4, name: "香港代理", protocol: "https", status: "active" },
];

const batch = (batchId: string, items: Operation[], pending = items.filter((item) => ["queued", "running", "checking", "needs_confirmation"].includes(item.status)).length) => ({
  batch_id: batchId,
  items,
  pending,
});

let container: HTMLDivElement;
let root: Root;
const close = vi.fn();

const click = async (element: Element) => {
  await act(async () => (element as HTMLElement).click());
};

const input = async (selector: string, value: string) => {
  const element = container.querySelector<HTMLInputElement>(selector)!;
  const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
  await act(async () => {
    setter?.call(element, value);
    element.dispatchEvent(new Event("input", { bubbles: true }));
  });
  return element;
};

const button = (text: string) =>
  [...container.querySelectorAll<HTMLButtonElement>("button")].find((node) => node.textContent?.trim() === text)!;

async function flush() {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}

async function renderDialog(accounts: Account[] = [account(1)], online = true, connectionKey = "proxy-connection-1") {
  bindOperationConnection(connectionKey);
  await act(async () => {
    root.render(<ProxyBatchDialog accounts={accounts} online={online} connectionKey={connectionKey} close={close} report={vi.fn()} />);
    await Promise.resolve();
  });
  await flush();
}

beforeEach(() => {
  vi.clearAllMocks();
  vi.useRealTimers();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  vi.mocked(command).mockResolvedValue(undefined as never);
  vi.mocked(api).mockImplementation(async (method, path) => {
    if (method === "POST" && path === "/proxies/2/test") return {
      success: true, message: "代理连接正常", latency_ms: 31, ip_address: "203.0.113.7",
      city: "Taipei", region: "Taiwan", country: "Taiwan", country_code: "TW",
    } as never;
    if (method === "POST" && path === "/proxies/2/quality-check") return {
      score: 88, grade: "yellow", summary: "一个目标需要关注", exit_ip: "203.0.113.8",
      country: "Taiwan", base_latency_ms: 20, items: [],
    } as never;
    if (method === "GET" && path === "/proxies") return structuredClone(proxies) as never;
    const match = path.match(/^\/accounts\/(\d+)\/settings$/);
    if (method === "GET" && match) return settings(Number(match[1])) as never;
    throw new Error(`Unexpected fixture request: ${method} ${path}`);
  });
});

afterEach(async () => {
  vi.useRealTimers();
  await act(async () => root.unmount());
  container.remove();
});

describe("proxy batch dialog", () => {
  it("reads the proxy catalog and each account, previews current and target values, filters options, and explains inherited rows", async () => {
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method === "GET" && path === "/proxies") return structuredClone(proxies) as never;
      if (method === "GET" && path === "/accounts/1/settings") return settings(1, { proxy_id: 2 }) as never;
      if (method === "GET" && path === "/accounts/2/settings") {
        return settings(2, {
          proxy_id: null,
          capabilities: {
            model_config: { editable: true, reason: "" },
            proxy: { editable: false, reason: "代理继承自父账号，不能单独修改" },
            sync_models: { editable: true, reason: "" },
          },
        }) as never;
      }
      throw new Error(`Unexpected fixture request: ${method} ${path}`);
    });
    await renderDialog([account(1), account(2)]);

    expect(api).toHaveBeenCalledWith("GET", "/proxies");
    expect(api).toHaveBeenCalledWith("GET", "/accounts/1/settings");
    expect(api).toHaveBeenCalledWith("GET", "/accounts/2/settings");
    expect(container.textContent).toContain("东京代理 #2");
    expect(container.textContent).toContain("请选择代理");
    expect(container.textContent).toContain("代理继承自父账号，不能单独修改");
    expect(container.querySelectorAll(".proxy-batch-previews article")).toHaveLength(2);
    expect(vi.mocked(api).mock.calls.some(([method]) => method !== "GET")).toBe(false);

    const selectedBefore = [...container.querySelectorAll<HTMLInputElement>('input[type="radio"]')]
      .filter((input) => input.checked).map((input) => input.value);
    await click(container.querySelector<HTMLButtonElement>('[aria-label="检测代理 2"]')!);
    expect(vi.mocked(api).mock.calls).toContainEqual(["POST", "/proxies/2/test"]);
    expect([...container.querySelectorAll<HTMLInputElement>('input[type="radio"]')]
      .filter((input) => input.checked).map((input) => input.value)).toEqual(selectedBefore);
    expect(vi.mocked(api).mock.calls.some(([method, path]) => method === "POST" && path === "/account-operations/batch")).toBe(false);

    await input('[aria-label="搜索代理"]', "香港");
    expect(container.querySelector(".proxy-options")?.textContent).toContain("香港代理");
    expect(container.querySelector(".proxy-options")?.textContent).not.toContain("东京代理");

    await click(container.querySelector<HTMLInputElement>('input[type="radio"][value="4"]')
      ?? [...container.querySelectorAll<HTMLInputElement>('input[type="radio"]')].find((node) => node.parentElement?.textContent?.includes("香港代理"))!);
    expect(container.textContent).toContain("目标香港代理 #4");
    expect(container.textContent).toContain("当前东京代理 #2");
    expect(vi.mocked(api).mock.calls.filter(([method]) => method !== "GET")
      .map(([method, path]) => [method, path])).toEqual([["POST", "/proxies/2/test"]]);
  });

  it("keeps clearing a target local until confirmation, then sends only the proxy batch contract", async () => {
    const response = batch("batch-clear", [{ id: "job-clear", batch_id: "batch-clear", account_id: 1, account_name: "Fixture", action: "proxy", requested: { proxy_id: null }, status: "completed", reason: "已确认" }], 0);
    vi.mocked(api).mockImplementation(async (method, path, body) => {
      if (method === "GET" && path === "/proxies") return structuredClone(proxies) as never;
      if (method === "GET" && path === "/accounts/1/settings") return settings(1, { proxy_id: 2 }) as never;
      if (method === "POST" && path === "/account-operations/batch") return response as never;
      throw new Error(`Unexpected fixture request: ${method} ${path} ${JSON.stringify(body)}`);
    });
    await renderDialog();

    const clear = [...container.querySelectorAll<HTMLInputElement>('input[type="radio"]')]
      .find((node) => node.parentElement?.textContent?.includes("不使用代理"))!;
    await click(clear);
    expect(container.textContent).toContain("目标不使用代理");
    expect(vi.mocked(api).mock.calls.some(([method]) => method !== "GET")).toBe(false);

    await click(button("确认应用"));
    const writes = vi.mocked(api).mock.calls.filter(([method]) => method !== "GET");
    expect(writes).toHaveLength(1);
    expect(writes[0]?.[0]).toBe("POST");
    expect(writes[0]?.[1]).toBe("/account-operations/batch");
    expect(writes[0]?.[2]).toMatchObject({
      action: "proxy",
      payload: { proxy_id: null },
      accounts: [{ account_id: 1, expected_version: "proxy-version-1" }],
    });
    const body = writes[0]?.[2] as { request_id: unknown; client_id: unknown; payload: Record<string, unknown> };
    expect(typeof body.request_id).toBe("string");
    expect(typeof body.client_id).toBe("string");
    expect(close).toHaveBeenCalledOnce();
    expect(Object.keys(body.payload)).toEqual(["proxy_id"]);
    expect(JSON.stringify(body)).not.toMatch(/password|token|secret/i);
  });

  it("submits a mixed eligible batch and renders no-change and ineligible results per account", async () => {
    const first = account(1), inherited = account(2), third = account(3);
    const response = batch("batch-mixed", [
      { id: "job-1", batch_id: "batch-mixed", account_id: 1, account_name: first.name, action: "proxy", requested: { proxy_id: 2 }, status: "completed", reason: "无变化" },
      { id: "job-2", batch_id: "batch-mixed", account_id: 2, account_name: inherited.name, action: "proxy", requested: { proxy_id: 2 }, status: "failed", reason: "代理继承自父账号，不能单独修改" },
      { id: "job-3", batch_id: "batch-mixed", account_id: 3, account_name: third.name, action: "proxy", requested: { proxy_id: 2 }, status: "completed", reason: "已确认生效" },
    ], 0);
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method === "GET" && path === "/proxies") return structuredClone(proxies) as never;
      const match = path.match(/^\/accounts\/(\d+)\/settings$/);
      if (method === "GET" && match) {
        const id = Number(match[1]);
        return id === 1 ? settings(1, { proxy_id: 2 }) : id === 2
          ? settings(2, { proxy_id: null, capabilities: { ...settings(2).capabilities, proxy: { editable: false, reason: "代理继承自父账号，不能单独修改" } } })
          : settings(3, { proxy_id: 4 });
      }
      if (method === "POST" && path === "/account-operations/batch") return response as never;
      throw new Error(`Unexpected fixture request: ${method} ${path}`);
    });
    await renderDialog([first, inherited, third]);
    const target = [...container.querySelectorAll<HTMLInputElement>('input[type="radio"]')]
      .find((node) => node.parentElement?.textContent?.includes("东京代理"))!;
    await click(target);
    await click(button("确认应用"));

    const write = vi.mocked(api).mock.calls.find(([method, path]) => method === "POST" && path === "/account-operations/batch");
    expect(write?.[2]).toMatchObject({
      action: "proxy",
      payload: { proxy_id: 2 },
      accounts: [
        { account_id: 1, expected_version: "proxy-version-1" },
        { account_id: 2, expected_version: "proxy-version-2" },
        { account_id: 3, expected_version: "proxy-version-3" },
      ],
    });
    expect(close).not.toHaveBeenCalled();
    expect(container.textContent).toContain("无变化");
    expect(container.textContent).toContain("代理继承自父账号，不能单独修改");
    expect(container.textContent).toContain("已确认生效");
  });

  it("retries a failed POST with the same request and client IDs", async () => {
    const firstFailure = new Error("代理批量请求暂时失败");
    const response = batch("batch-retry", [], 0);
    let attempts = 0;
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method === "GET" && path === "/proxies") return structuredClone(proxies) as never;
      if (method === "GET" && path === "/accounts/1/settings") return settings(1) as never;
      if (method === "POST" && path === "/account-operations/batch") {
        attempts++;
        if (attempts === 1) throw firstFailure;
        return response as never;
      }
      throw new Error(`Unexpected fixture request: ${method} ${path}`);
    });
    await renderDialog();
    await click([...container.querySelectorAll<HTMLInputElement>('input[type="radio"]')]
      .find((node) => node.parentElement?.textContent?.includes("香港代理"))!);
    await click(button("确认应用"));
    await flush();
    expect(container.textContent).toContain("代理批量请求暂时失败");

    await click(button("确认应用"));
    const writes = vi.mocked(api).mock.calls.filter(([method, path]) => method === "POST" && path === "/account-operations/batch");
    expect(writes).toHaveLength(2);
    const firstBody = writes[0]?.[2] as { request_id: string; client_id: string };
    const secondBody = writes[1]?.[2] as { request_id: string; client_id: string };
    expect(secondBody.request_id).toBe(firstBody.request_id);
    expect(secondBody.client_id).toBe(firstBody.client_id);
  });

  it("follows a pending batch through the operations endpoint", async () => {
    vi.useFakeTimers();
    const queued: Operation = { id: "job-follow", batch_id: "batch-follow", account_id: 1, account_name: "账号 1", action: "proxy", requested: { proxy_id: 4 }, status: "queued", reason: "等待账号空闲" };
    const completed: Operation = { ...queued, status: "completed", reason: "", result: { verified: true } };
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method === "GET" && path === "/proxies") return structuredClone(proxies) as never;
      if (method === "GET" && path === "/accounts/1/settings") return settings(1) as never;
      if (method === "POST" && path === "/account-operations/batch") return batch("batch-follow", [queued], 1) as never;
      if (method === "GET" && path === "/account-operations?batch_id=batch-follow") return batch("batch-follow", [completed], 0) as never;
      throw new Error(`Unexpected fixture request: ${method} ${path}`);
    });
    await renderDialog();
    await click([...container.querySelectorAll<HTMLInputElement>('input[type="radio"]')]
      .find((node) => node.parentElement?.textContent?.includes("香港代理"))!);
    await click(button("确认应用"));
    await flush();
    expect(container.textContent).toContain("等待账号空闲");

    await act(async () => {
      await vi.advanceTimersByTimeAsync(2000);
    });
    await flush();
    expect(api).toHaveBeenCalledWith("GET", "/account-operations?batch_id=batch-follow");
    expect(container.textContent).toContain("已确认生效");
  });

  it("drops a late batch response after the operation connection changes", async () => {
    let resolvePost!: (value: unknown) => void;
    const latePost = new Promise<unknown>((resolve) => { resolvePost = resolve; });
    const queued: Operation = { id: "job-late", batch_id: "batch-late", account_id: 1, account_name: "账号 1", action: "proxy", requested: { proxy_id: 4 }, status: "queued", reason: "等待账号空闲" };
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method === "GET" && path === "/proxies") return structuredClone(proxies) as never;
      if (method === "GET" && path === "/accounts/1/settings") return settings(1) as never;
      if (method === "POST" && path === "/account-operations/batch") return latePost as never;
      throw new Error(`Unexpected fixture request: ${method} ${path}`);
    });
    await renderDialog([account(1)], true, "connection-before");
    await click([...container.querySelectorAll<HTMLInputElement>('input[type="radio"]')]
      .find((node) => node.parentElement?.textContent?.includes("香港代理"))!);
    await click(button("确认应用"));
    await flush();
    expect(api).toHaveBeenCalledWith("POST", "/account-operations/batch", expect.anything());

    bindOperationConnection("connection-after");
    await act(async () => resolvePost(batch("batch-late", [queued], 1)));
    await flush();

    expect(vi.mocked(api).mock.calls.some(([method, path]) => method === "GET" && path === "/account-operations?batch_id=batch-late")).toBe(false);
    expect(container.textContent).not.toContain("等待账号空闲");
    expect(container.textContent).not.toContain("已确认生效");
  });

  it("drops a late proxy diagnostic after the batch connection changes", async () => {
    let resolveDiagnostic!: (value: unknown) => void;
    const lateDiagnostic = new Promise<unknown>((resolve) => { resolveDiagnostic = resolve; });
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method === "GET" && path === "/proxies") return structuredClone(proxies) as never;
      if (method === "GET" && path === "/accounts/1/settings") return settings(1) as never;
      if (method === "POST" && path === "/proxies/2/test") return lateDiagnostic as never;
      throw new Error(`Unexpected fixture request: ${method} ${path}`);
    });
    await renderDialog([account(1)], true, "connection-before");
    await click(container.querySelector<HTMLButtonElement>('[aria-label="检测代理 2"]')!);
    expect(vi.mocked(api).mock.calls).toContainEqual(["POST", "/proxies/2/test"]);

    await act(async () => {
      root.render(<ProxyBatchDialog accounts={[account(1)]} online connectionKey="connection-after" close={close} report={vi.fn()} />);
      await Promise.resolve();
    });
    await act(async () => resolveDiagnostic({
      success: true, message: "late fixture result", latency_ms: 31, ip_address: "198.51.100.99",
      city: "Taipei", region: "Taiwan", country: "Taiwan", country_code: "TW",
    }));
    await flush();

    expect(container.textContent).not.toContain("198.51.100.99");
    expect(container.textContent).not.toContain("late fixture result");
  });
});
