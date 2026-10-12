// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import AccountSettingsDialog, { type AccountSettings } from "./AccountSettingsDialog";
import { api, command } from "./bridge";
import { followOperation, operationEpoch, submitOperation, type Operation } from "./accountOperations";
import type { Account } from "./types";

vi.mock("./bridge", () => ({ api: vi.fn(), command: vi.fn() }));
vi.mock("./mobile", () => ({ useBackAction: vi.fn() }));
vi.mock("./accountOperations", () => ({
  followOperation: vi.fn(),
  operationEpoch: vi.fn(),
  submitOperation: vi.fn(),
}));

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

const account = (overrides: Partial<Account> = {}) => ({
  id: 101,
  name: "Fixture account",
  priority: 1,
  concurrency: 4,
  load_factor: 80,
  platform: "openai",
  type: "oauth",
  status: "active",
  schedulable: true,
  available: true,
  group_ids: [],
  blockers: [],
  managed: false,
  recoverable: true,
  version: "account-version",
  operation_versions: { model_config: "model-version", proxy: "proxy-version", sync_models: "sync-version" },
  last_success_at: null,
  last_error_at: null,
  last_error_id: null,
  last_error_code: null,
  last_error_status: null,
  error_message: "",
  success_after_error: false,
  usage_windows: [],
  ...overrides,
} as Account);

const settings = (overrides: Partial<AccountSettings> = {}): AccountSettings => ({
  account_id: 101,
  priority: 1,
  concurrency: 4,
  load_factor: 80,
  effective_load_factor: 80,
  proxy_id: 2,
  model_config: {
    whitelist: ["gpt-fixture"],
    mappings: [{ source: "alias", target: "gpt-fixture" }],
  },
  capabilities: {
    model_config: { editable: true, reason: "" },
    proxy: { editable: true, reason: "" },
    sync_models: { editable: true, reason: "" },
  },
  versions: { model_config: "model-version", proxy: "proxy-version", sync_models: "sync-version" },
  ...overrides,
});

const proxies = [
  { id: 2, name: "Fixture proxy", protocol: "http", status: "active" },
  { id: 3, name: "Offline proxy", protocol: "socks5", status: "disabled" },
];

const operation = (action: string): Operation => ({
  id: `job-${action}`,
  account_id: 101,
  account_name: "Fixture account",
  action,
  status: "queued",
  requested: {},
});

const catalog = {
  models: ["gpt-fixture", "gpt-new", "alias"],
  warnings: ["fixture catalog warning"],
  synced_at: "2026-10-11T00:00:00Z",
};

let container: HTMLDivElement;
let root: Root;

const click = async (element: Element) => {
  await act(async () => (element as HTMLElement).click());
};

const change = async (element: HTMLInputElement, value: string) => {
  await act(async () => {
    const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
    setter?.call(element, value);
    element.dispatchEvent(new Event("input", { bubbles: true }));
  });
};

const button = (text: string, scope: ParentNode = container) =>
  [...scope.querySelectorAll<HTMLButtonElement>("button")].find((node) => node.textContent?.trim() === text)!;

async function renderDialog(section: "whitelist" | "mappings" | "proxy" = "whitelist", value = account()) {
  await act(async () => {
    root.render(<AccountSettingsDialog account={value} initialSection={section} online close={vi.fn()} />);
    await Promise.resolve();
  });
  await act(async () => Promise.resolve());
}

beforeEach(() => {
  vi.clearAllMocks();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  vi.mocked(operationEpoch).mockReturnValue(1);
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
    if (method !== "GET") throw new Error(`Unexpected fixture write: ${method} ${path}`);
    if (path === "/proxies") return structuredClone(proxies) as never;
    if (path.startsWith("/accounts/") && path.endsWith("/settings")) return structuredClone(settings()) as never;
    throw new Error(`Unexpected fixture read: ${path}`);
  });
  vi.mocked(submitOperation).mockImplementation(async (_account, action, payload) => ({
    ...operation(action),
    requested: payload,
  }));
  vi.mocked(followOperation).mockImplementation(async (initial: Operation, _onEvent, onUpdate) => {
    onUpdate?.(initial);
    return initial.action === "sync_models" ? structuredClone(catalog) as never : { verified: true, field_version: `${initial.action}-next` } as never;
  });
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
});

describe("account settings dialog", () => {
  it("loads settings and proxy options through separate reads", async () => {
    await renderDialog("proxy");
    expect(api).toHaveBeenCalledWith("GET", "/accounts/101/settings");
    expect(api).toHaveBeenCalledWith("GET", "/proxies");
    expect(container.textContent).toContain("Fixture proxy");
    expect(container.textContent).not.toContain("password");
    expect(container.textContent).not.toContain("token");
  });

  it("keeps sync results unselected until a model is added to the draft and saved", async () => {
    await renderDialog("whitelist");
    await click(button("同步上游模型"));

    const candidates = [...container.querySelectorAll<HTMLInputElement>('.settings-model-options input[type="checkbox"]')];
    expect(candidates).toHaveLength(3);
    expect(candidates.every((input) => !input.checked)).toBe(true);
    expect(vi.mocked(submitOperation).mock.calls.map(([, action]) => action)).toEqual(["sync_models"]);

    const newModel = candidates.find((input) => input.parentElement?.textContent?.includes("gpt-new"))!;
    await click(newModel);
    expect(newModel.checked).toBe(true);
    await click(button("添加所选"));
    expect(container.querySelector<HTMLInputElement>('[aria-label="白名单模型 2"]')?.value).toBe("gpt-new");
    expect(vi.mocked(submitOperation).mock.calls.map(([, action]) => action)).toEqual(["sync_models"]);

    await click(button("保存"));
    const save = vi.mocked(submitOperation).mock.calls.find(([, action]) => action === "model_config");
    expect(save?.[2]).toEqual({
      whitelist: ["gpt-fixture", "gpt-new"],
      mappings: [{ source: "alias", target: "gpt-fixture" }],
    });
  });

  it("does not replace a local draft when the parent refreshes the same account", async () => {
    const current = account();
    await renderDialog("whitelist", current);
    await change(container.querySelector<HTMLInputElement>('[aria-label="白名单模型 1"]')!, "gpt-draft");
    await act(async () => {
      root.render(<AccountSettingsDialog account={{ ...current, priority: 99 }} initialSection="whitelist" online close={vi.fn()} />);
      await Promise.resolve();
    });

    expect(container.querySelector<HTMLInputElement>('[aria-label="白名单模型 1"]')?.value).toBe("gpt-draft");
    expect(vi.mocked(api).mock.calls.filter(([method, path]) => method === "GET" && path === "/accounts/101/settings")).toHaveLength(1);
  });

  it("reports empty mappings and source conflicts without submitting", async () => {
    await renderDialog("mappings");
    await click(button("添加映射"));
    expect(container.querySelector('[role="alert"]')?.textContent).toContain("映射源只允许末尾通配符");
    await click(button("保存"));
    expect(vi.mocked(submitOperation).mock.calls.some(([, action]) => action === "model_config")).toBe(false);

    await click(container.querySelector<HTMLButtonElement>('[aria-label="移除映射 2"]')!);
    await click(button("添加映射"));
    await change(container.querySelector<HTMLInputElement>('[aria-label="请求模型 2"]')!, "gpt-fixture");
    await change(container.querySelector<HTMLInputElement>('[aria-label="转发模型 2"]')!, "target-model");
    expect(container.querySelector('[role="alert"]')?.textContent).toContain("模型条目冲突：gpt-fixture");
    expect(button("保存").disabled).toBe(true);
  });

  it("shows the inherited capability reason and disables model writes", async () => {
    const inherited = settings({
      capabilities: {
        model_config: { editable: false, reason: "模型配置继承自父账号" },
        proxy: { editable: true, reason: "" },
        sync_models: { editable: false, reason: "上游账号不支持同步" },
      },
    });
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method === "GET" && path === "/accounts/101/settings") return inherited as never;
      if (method === "GET" && path === "/proxies") return structuredClone(proxies) as never;
      throw new Error(`Unexpected fixture request: ${method} ${path}`);
    });
    await renderDialog("whitelist");

    expect(container.textContent).toContain("模型配置继承自父账号");
    expect(button("保存").disabled).toBe(true);
    expect(button("同步上游模型").disabled).toBe(true);
    expect(button("添加模型").disabled).toBe(true);
  });

  it("clears a proxy with a minimal payload and never sends credential fields", async () => {
    await renderDialog("proxy");
    const clear = container.querySelector<HTMLInputElement>('input[type="radio"][value=""]')
      ?? [...container.querySelectorAll<HTMLInputElement>('input[type="radio"]')].find((input) => input.parentElement?.textContent?.includes("不使用代理"))!;
    await click(clear);
    await click(button("保存"));

    const save = vi.mocked(submitOperation).mock.calls.find(([, action]) => action === "proxy");
    expect(save?.[2]).toEqual({ proxy_id: null });
    expect(JSON.stringify(save?.[2])).not.toMatch(/token|password|username|secret/i);
  });

  it("runs proxy diagnostics without changing the draft or submitting a save", async () => {
    await renderDialog("proxy");
    await click(container.querySelector<HTMLButtonElement>('[aria-label="检测代理 2"]')!);

    expect(vi.mocked(api)).toHaveBeenCalledWith("POST", "/proxies/2/test");
    expect(vi.mocked(submitOperation).mock.calls.some(([, action]) => action === "proxy")).toBe(false);
    expect(button("保存").disabled).toBe(true);
    expect(container.textContent).toContain("203.0.113.7");
    expect([...container.querySelectorAll<HTMLInputElement>('input[type="radio"]')].filter((input) => input.checked)).toHaveLength(1);
  });

  it("keeps diagnostics available for an inherited read-only proxy while blocking selection", async () => {
    const inherited = settings({
      capabilities: {
        model_config: { editable: true, reason: "" },
        proxy: { editable: false, reason: "代理继承自父账号，不能单独修改" },
        sync_models: { editable: true, reason: "" },
      },
    });
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method === "POST" && path === "/proxies/2/test") return {
        success: true, message: "代理连接正常", latency_ms: 31, ip_address: "203.0.113.7",
        city: "Taipei", region: "Taiwan", country: "Taiwan", country_code: "TW",
      } as never;
      if (method === "GET" && path === "/accounts/101/settings") return inherited as never;
      if (method === "GET" && path === "/proxies") return structuredClone(proxies) as never;
      throw new Error(`Unexpected fixture request: ${method} ${path}`);
    });
    await renderDialog("proxy");

    expect(container.querySelector<HTMLInputElement>('input[type="radio"]:checked')?.disabled).toBe(true);
    expect(container.querySelector<HTMLButtonElement>('[aria-label="检测代理 2"]')?.disabled).toBe(false);
    await click(container.querySelector<HTMLButtonElement>('[aria-label="检测代理 2"]')!);
    expect(container.textContent).toContain("连接成功");
    expect(vi.mocked(submitOperation).mock.calls).toHaveLength(0);
  });

  it("ignores a late settings response from an older account", async () => {
    let resolveOld!: (value: AccountSettings) => void;
    let resolveNew!: (value: AccountSettings) => void;
    const old = new Promise<AccountSettings>((resolve) => { resolveOld = resolve; });
    const fresh = new Promise<AccountSettings>((resolve) => { resolveNew = resolve; });
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method !== "GET") throw new Error(`Unexpected fixture write: ${method} ${path}`);
      if (path === "/accounts/101/settings") return old as never;
      if (path === "/accounts/202/settings") return fresh as never;
      throw new Error(`Unexpected fixture read: ${path}`);
    });
    const first = account({ id: 101, name: "Old account" });
    const second = account({ id: 202, name: "New account" });
    await act(async () => root.render(<AccountSettingsDialog account={first} initialSection="whitelist" online close={vi.fn()} />));
    await act(async () => root.render(<AccountSettingsDialog account={second} initialSection="whitelist" online close={vi.fn()} />));
    await act(async () => resolveNew(settings({ account_id: 202, model_config: { whitelist: ["new-model"], mappings: [] } })));
    expect(container.querySelector<HTMLInputElement>('[aria-label="白名单模型 1"]')?.value).toBe("new-model");

    await act(async () => resolveOld(settings({ account_id: 101, model_config: { whitelist: ["old-model"], mappings: [] } })));
    expect(container.querySelector<HTMLInputElement>('[aria-label="白名单模型 1"]')?.value).toBe("new-model");
    expect(container.textContent).toContain("New account");
  });
});
