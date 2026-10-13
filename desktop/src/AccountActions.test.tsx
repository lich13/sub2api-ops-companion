// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { api, command, subscribe } from "./bridge";
import App from "./main";
import { useAccountActions } from "./AccountActions";
import { handleBack } from "./mobile";
import type { Account, Group, ViewState } from "./types";

vi.mock("./bridge", () => ({
  api: vi.fn(),
  command: vi.fn(),
  subscribe: vi.fn(),
  updates: vi.fn(async () => () => {}),
  watchWindowFocus: vi.fn(async () => () => {}),
  preview: true,
}));
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

const account = (id: number, overrides: Partial<Account> = {}): Account => ({
  id,
  name: `Fixture ${id}`,
  priority: id,
  platform: "openai",
  type: "oauth",
  status: "active",
  schedulable: true,
  available: true,
  group_ids: [1],
  blockers: [],
  managed: false,
  recoverable: true,
  version: `account-version-${id}`,
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

const group = (id: number, name: string, platform = "openai"): Group => ({
  id,
  name,
  platform,
  sort_order: id,
  account_id: null,
  account_name: "",
  model: "fixture-model",
  upstream_model: "fixture-model",
  upstream_response_model: "fixture-model",
  called_at: null,
  recent_accounts: [],
});

const state = (accounts: Account[] = [account(1)]): ViewState => ({
  platform: "macos",
  foreground: true,
  connected: true,
  online: true,
  error: "",
  preferences: {
    base_url: "https://example.invalid",
    favorites: [],
    pinned: false,
    launch_at_login: false,
  },
  snapshot: {
    observed_at: "2026-10-01T03:00:00Z",
    accounts,
    groups: [group(1, "左组"), group(2, "右组"), group(3, "Grok 左", "grok"), group(4, "Grok 右", "grok")],
    errors: [],
    recoveries: [],
  },
});

const openAILabels = ["模型测试", "标记降智", "定时检测", "测试连接", "恢复状态", "应用模板", "模型白名单", "模型映射", "选择代理", "删除"];
let container: HTMLDivElement;
let root: Root;
let receive: (next: ViewState) => void;

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: Error) => void;
  const promise = new Promise<T>((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
}

const labels = (scope: ParentNode) => [...scope.querySelectorAll("button")].map((node) => node.textContent?.trim());
const menu = () => document.body.querySelector<HTMLElement>(".account-action-popup");
const button = (text: string, scope: ParentNode = container) =>
  [...scope.querySelectorAll<HTMLButtonElement>("button")].find((node) => node.textContent?.trim() === text)!;
const navigation = (name: string) => button(name, container.querySelector("aside nav")!);
const groupCard = (name: string) =>
  [...container.querySelectorAll<HTMLElement>(".group-account")].find((node) => node.querySelector("strong")?.textContent === name)!;

async function render(initial: ViewState = state()) {
  vi.mocked(command).mockImplementation(async (name) => name === "get_state" ? initial as never : undefined as never);
  await act(async () => root.render(<App />));
}

async function openGroupMenu(name = "Fixture 1") {
  await act(async () => groupCard(name).querySelector<HTMLButtonElement>(".account-more-trigger")!.click());
}

async function openGroups() {
  await act(async () => navigation("分组").click());
}

async function moveTo(name: string, destination: string) {
  await act(async () => groupCard(name).querySelector<HTMLButtonElement>(".group-account-select")!.click());
  await act(async () => button(destination, container.querySelector(".group-destination-bar")!).click());
}

beforeEach(() => {
  vi.clearAllMocks();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  vi.mocked(subscribe).mockImplementation(async (callback) => { receive = callback; return () => {}; });
  vi.mocked(api).mockImplementation(async (method, path) => {
    if (method !== "GET") throw new Error(`Unexpected fixture write: ${method} ${path}`);
    if (path === "/quota-refresh") return { status: "idle", items: [], total: 0, completed: 0 } as never;
    if (path === "/account-operations") return { items: [] } as never;
    if (/^\/accounts\/\d+\/models(?:\?purpose=model_test)?$/.test(path)) return [{ id: "fixture-model", type: "text" }] as never;
    if (/^\/accounts\/\d+\/model-tests\/latest$/.test(path)) return null as never;
    if (/^\/accounts\/\d+\/model-detection$/.test(path)) return { enabled: false, interval_minutes: 30, model_id: "fixture-model", version: "detection-version", status: "disabled" } as never;
    if (path === "/modeltrace/fingerprint-bank") return { version: { revision: "fixture-revision", sha256: "fixture-hash", built_at: "2026-10-01T00:00:00Z", analyzer_version: 1 }, source: "bundled", status: "ready" } as never;
    if (path === "/account-templates") return { version: "template-version", configured: false, templates: {}, template_meta: {}, template_order: [] } as never;
    if (path.startsWith("/account-templates?account_id=")) return { account: { id: 1, eligible: true, passthrough: false, version: "config-version", config: { whitelist: [], mappings: [] } } } as never;
    throw new Error(`Unexpected fixture read: ${path}`);
  });
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
});

describe("shared account actions", () => {
  it.each(["oauth", "apikey"])("expands every supported OpenAI %s action in the account table while retaining the group menu", async (type) => {
    await render(state([account(1, { type })]));
    const panel = container.querySelector<HTMLElement>(".accounts-table .account-actions")!;
    expect(labels(panel)).toEqual(openAILabels);
    expect(panel.querySelector(".account-more-trigger")).toBeNull();
    expect([...panel.querySelectorAll("button")].map((node) => !!node.querySelector("svg"))).toEqual([
      true, true, true, true, true, true, true, true, true, true,
    ]);
    const bulk = container.querySelector<HTMLElement>('[role="group"][aria-label="批量账号操作"]')!;
    expect([...bulk.querySelectorAll("button")].map((node) => !!node.querySelector("svg"))).toEqual([true, true, true, true]);

    await openGroups();
    await openGroupMenu();

    expect(labels(menu()!)).toEqual(openAILabels);
    expect(menu()?.parentElement).toBe(document.body);
    expect(groupCard("Fixture 1").querySelector("details")).toBeNull();
    expect(vi.mocked(api).mock.calls.every(([method]) => method === "GET")).toBe(true);
  });

  it.each([true, false])("limits a Grok group menu to supported actions with recovery eligibility %s", async (recoverable) => {
    await render(state([account(3, { platform: "grok", group_ids: [3], recoverable })]));
    await openGroups();
    await act(async () => button("Grok", container.querySelector(".group-platforms")!).click());
    await openGroupMenu("Fixture 3");

    expect(labels(menu()!)).toEqual(recoverable
      ? ["测试连接", "恢复状态", "模型白名单", "模型映射", "选择代理", "删除"]
      : ["测试连接", "模型白名单", "模型映射", "选择代理", "删除"]);
  });

  it("provides the complete group menu on Android and gives it first ownership of Back", async () => {
    await render({ ...state(), platform: "android" });
    await act(async () => button("分组管理").click());
    await openGroupMenu();
    expect(labels(menu()!)).toEqual(openAILabels);
    const fallback = vi.fn();

    await act(async () => handleBack(fallback));

    expect(menu()).toBeNull();
    expect(fallback).not.toHaveBeenCalled();
    expect(container.querySelectorAll('.group-manager:not([hidden])')).toHaveLength(1);
  });

  it.each([
    ["模型测试", "模型测试", "/accounts/1/models?purpose=model_test"],
    ["定时检测", "定时检测", "/accounts/1/model-detection"],
    ["测试连接", "测试连接", "/accounts/1/models"],
    ["应用模板", "账号模板", "/account-templates?account_id=1"],
  ])("opens %s from the group card for that account", async (action, dialogLabel, request) => {
    await render();
    await openGroups();
    await openGroupMenu();
    await act(async () => button(action, menu()!).click());

    expect(menu()).toBeNull();
    const dialog = container.querySelector(`[role="dialog"][aria-label="${dialogLabel}"]`);
    expect(dialog).not.toBeNull();
    expect(dialog?.textContent).toContain("Fixture 1");
    expect(api).toHaveBeenCalledWith("GET", request);
    expect(vi.mocked(api).mock.calls.every(([method]) => method === "GET")).toBe(true);
  });

  it("refreshes a submitted degradation mark after its menu closes and the page changes", async () => {
    const pending = deferred<{ verified: boolean; degradation_mark: { marked: boolean; version: string } }>();
    const original = vi.mocked(api).getMockImplementation()!;
    vi.mocked(api).mockImplementation(async (method, path, body) => {
      if (method === "PUT" && path === "/accounts/1/degradation-mark") return pending.promise as never;
      return original(method, path, body);
    });
    await render();
    await openGroups();
    await openGroupMenu();
    await act(async () => button("标记降智", menu()!).click());

    expect(menu()).toBeNull();
    expect(api).toHaveBeenCalledWith("PUT", "/accounts/1/degradation-mark", { marked: true, expected_mark_version: "mark-version-1" });
    await act(async () => navigation("账号").click());
    expect(command).not.toHaveBeenCalledWith("refresh");
    await act(async () => pending.resolve({ verified: true, degradation_mark: { marked: true, version: "mark-version-2" } }));

    expect(command).toHaveBeenCalledWith("refresh");
    expect(vi.mocked(api).mock.calls.filter(([method]) => method !== "GET")).toHaveLength(1);
  });

  it.each(["rejected", "unverified"])("keeps %s degradation feedback visible after the menu closes", async (outcome) => {
    const pending = deferred<{ verified: boolean; degradation_mark: { marked: boolean; version: string } }>();
    const original = vi.mocked(api).getMockImplementation()!;
    vi.mocked(api).mockImplementation(async (method, path, body) => {
      if (method === "PUT" && path === "/accounts/1/degradation-mark") return pending.promise as never;
      return original(method, path, body);
    });
    await render();
    await openGroups();
    await openGroupMenu();
    await act(async () => button("标记降智", menu()!).click());
    await act(async () => navigation("账号").click());
    await act(async () => {
      if (outcome === "rejected") pending.reject(new Error("标记保存失败 fixture"));
      else pending.resolve({ verified: false, degradation_mark: { marked: true, version: "mark-version-2" } });
    });

    expect(container.textContent).toContain(outcome === "rejected" ? "标记保存失败 fixture" : "降智标记保存未确认");
    expect(command).not.toHaveBeenCalledWith("refresh");
  });

  it("recovers an eligible group account once and reports the result after its menu closes", async () => {
    const pending = deferred<{ verified: boolean }>();
    const original = vi.mocked(api).getMockImplementation()!;
    vi.mocked(api).mockImplementation(async (method, path, body) => {
      if (method === "POST" && path === "/accounts/1/recover-state") return pending.promise as never;
      return original(method, path, body);
    });
    await render();
    await openGroups();
    await openGroupMenu();
    await act(async () => button("恢复状态", menu()!).click());
    expect(menu()).toBeNull();
    await openGroupMenu();
    expect(button("恢复状态", menu()!).disabled).toBe(true);
    await act(async () => button("恢复状态", menu()!).click());
    await act(async () => pending.resolve({ verified: true }));

    expect(api).toHaveBeenCalledWith("POST", "/accounts/1/recover-state", { expected_version: "account-version-1" });
    expect(vi.mocked(api).mock.calls.filter(([method]) => method !== "GET")).toHaveLength(1);
    expect(container.textContent).toContain("状态已恢复");
    expect(command).toHaveBeenCalledWith("refresh");
  });

  it("closes group menus on connection changes", async () => {
    const initial = state();
    await render(initial);
    await openGroups();
    await openGroupMenu();
    expect(menu()).not.toBeNull();

    await act(async () => receive({ ...initial, connection_revision: 1 }));

    expect(menu()).toBeNull();
    expect(container.querySelectorAll('.group-manager:not([hidden])')).toHaveLength(1);
  });

  it("deletes from a group only after confirmation and preserves the other account's pending move", async () => {
    const original = vi.mocked(api).getMockImplementation()!;
    const first = { ...account(1), operation_versions: { delete: "account-version-1" } };
    vi.mocked(api).mockImplementation(async (method, path, body) => {
      if (method === "POST" && path === "/account-operations/batch" && (body as { action?: string })?.action === "delete") {
        return {
          batch_id: "a".repeat(32), pending: 0,
          items: [{
            id: "1".repeat(32), batch_id: "a".repeat(32), account_id: 1, account_name: "Fixture 1", action: "delete",
            status: "completed", reason: "", requested: { detach_managed: false }, result: { deleted: true, verified: true, detached: false },
          }],
        } as never;
      }
      if (method === "POST" && path === "/account-operations/batch" && (body as { action?: string })?.action === "groups") {
        const request = body as { accounts: [{ account_id: number; expected_version: string; payload: Record<string, unknown> }] };
        return {
          batch_id: "b".repeat(32), pending: 0,
          items: [{
            id: "2".repeat(32), batch_id: "b".repeat(32), account_id: 2, account_name: "Fixture 2", action: "groups",
            status: "completed", reason: "", requested: request.accounts[0].payload, result: { verified: true },
          }],
        } as never;
      }
      return original(method, path, body);
    });
    await render(state([first, account(2, { group_ids: [2] })]));
    await openGroups();
    await moveTo("Fixture 1", "右组");
    await moveTo("Fixture 2", "左组");
    await openGroupMenu();
    await act(async () => button("删除", menu()!).click());

    expect(menu()).toBeNull();
    expect(container.querySelector('[aria-labelledby="delete-title"]')?.textContent).toContain("Fixture 1");
    expect(vi.mocked(api).mock.calls.filter(([method]) => method !== "GET")).toHaveLength(0);
    await act(async () => button("确认删除").click());

    expect(groupCard("Fixture 1")).toBeUndefined();
    expect(container.querySelector(".group-draft-toolbar")?.textContent).toContain("1 个账号待应用");
    expect(groupCard("Fixture 2").classList.contains("dirty")).toBe(true);
    await act(async () => button("应用变更 1", container.querySelector(".group-draft-toolbar")!).click());
    const writes = vi.mocked(api).mock.calls.filter(([method, path]) => method === "POST" && path === "/account-operations/batch");
    expect(writes).toHaveLength(2);
    expect(writes[0]).toEqual(["POST", "/account-operations/batch", expect.objectContaining({
      action: "delete", payload: { confirmed: true },
      accounts: [{ account_id: 1, expected_version: "account-version-1", payload: { detach_managed: false } }],
    })]);
    expect(writes[1]).toEqual(["POST", "/account-operations/batch", expect.objectContaining({
      action: "groups", payload: {},
      accounts: [{ account_id: 2, expected_version: "account-version-2", payload: { scope_group_ids: [1, 2], group_ids: [1] } }],
    })]);
  });

  it.each(["openai", "grok"])("exposes settings entries for %s without adding ModelTrace actions", async (platform) => {
    const settings = vi.fn();
    function Probe() {
      const items = useAccountActions({
        online: true,
        connectionKey: "fixture-connection",
        report: vi.fn(),
        modelTest: vi.fn(),
        modelDetection: vi.fn(),
        test: vi.fn(),
        template: vi.fn(),
        remove: vi.fn(),
        settings,
      })(account(1, { platform, group_ids: platform === "grok" ? [3] : [1] }));
      return <div>{items.map((item) => <button key={item.id} data-action={item.id} data-has-icon={item.icon ? "true" : "false"} onClick={item.run}>{item.label}</button>)}</div>;
    }

    await act(async () => root.render(<Probe />));
    const actions = [...container.querySelectorAll<HTMLButtonElement>("[data-action]")];
    expect(actions.map((item) => item.dataset.action)).toEqual(expect.arrayContaining(["model_whitelist", "model_mapping", "proxy"]));
    expect(actions.every((item) => item.dataset.hasIcon === "true")).toBe(true);
    expect(actions.some((item) => item.dataset.action?.toLowerCase().includes("modeltrace"))).toBe(false);
    await act(async () => actions.find((item) => item.dataset.action === "model_whitelist")!.click());
    await act(async () => actions.find((item) => item.dataset.action === "model_mapping")!.click());
    await act(async () => actions.find((item) => item.dataset.action === "proxy")!.click());
    expect(settings.mock.calls.map(([, section]) => section)).toEqual(["whitelist", "mappings", "proxy"]);
  });
});
