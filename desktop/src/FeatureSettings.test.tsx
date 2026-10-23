// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { api, command, subscribe } from "./bridge";
import App from "./main";
import type { Account, Config, ConfigSection, ViewState } from "./types";

vi.mock("./bridge", () => ({
  api: vi.fn(), command: vi.fn(), subscribe: vi.fn(),
  updates: vi.fn(async () => () => {}), watchWindowFocus: vi.fn(async () => () => {}), preview: true,
}));
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

const account = (id: number, type: "oauth" | "apikey", platform = "openai"): Account => ({
  id, name: `Fixture ${id}`, type, platform, priority: id, status: "active", schedulable: true, available: true,
  group_ids: [], blockers: [], version: "a".repeat(64), last_success_at: null,
  last_error_at: null, last_error_id: null, last_error_code: null, last_error_status: null,
  error_message: "", success_after_error: false, usage_windows: [],
});

let container: HTMLDivElement;
let root: Root;
let initial: ViewState;
let config: Config;
let receive: (value: ViewState) => void;

beforeEach(() => {
  vi.resetAllMocks();
  vi.mocked(subscribe).mockImplementation(async (callback) => { receive = callback; return () => {}; });
  initial = {
    connected: true, online: true, platform: "macos", foreground: true, error: "",
    preferences: { base_url: "https://example.invalid", favorites: [], pinned: false, launch_at_login: false },
    snapshot: {
      observed_at: "2026-10-08T00:00:00Z", accounts: [account(1, "oauth"), account(2, "oauth"), account(11, "apikey"), account(21, "apikey", "grok")],
      groups: [], errors: [], recoveries: [],
    },
  };
  config = {
    oauth: {
      revision: "oauth-r1", oauth_recovery_monitor_enabled: true, oauth_auto_reset_credit_enabled: false,
      oauth_daily_test_enabled: false, oauth_daily_test_time: "09:00", oauth_usage_refresh_concurrency: 3,
      oauth_recovery_test_concurrency: 2, oauth_early_probe_batch_size: 5, oauth_recovery_test_model_id: "fixture-model",
      model_detection_model_id: "gpt-6-luna",
      oauth_recovery_connection_account_ids: [1], oauth_recovery_model_account_ids: [2],
    },
    bark: { revision: "bark-r1", enabled: false },
  };
  vi.mocked(command).mockImplementation(async (name) => name === "get_state" ? initial as never : undefined as never);
  vi.mocked(api).mockImplementation(async (method, path) => {
    if (method === "GET" && path === "/quota-refresh") return { status: "idle", items: [], total: 0, completed: 0 } as never;
    if (method === "GET" && path === "/config") return config as never;
    if (method === "GET" && path === "/model-groups") return { groups: [] } as never;
    throw new Error(`Unexpected fixture request: ${method} ${path}`);
  });
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
});

async function openFeatures() {
  await act(async () => root.render(<App/>));
  const nav = [...container.querySelectorAll<HTMLButtonElement>("aside nav button")].find((button) => button.textContent === "功能")!;
  await act(async () => nav.click());
}

function form(title: string) {
  return [...container.querySelectorAll<HTMLFormElement>("form")].find((node) => node.querySelector("h2")?.textContent === title)!;
}

function saveButton(target: HTMLFormElement) {
  return [...target.querySelectorAll<HTMLButtonElement>("button")].find((button) => button.textContent === "保存")!;
}

const writes = () => vi.mocked(api).mock.calls.filter(([method]) => method !== "GET");

describe("feature settings drafts", () => {
  it("keeps unsubmitted recovery fields across all three feature tabs", async () => {
    await openFeatures();
    const target=form("恢复、测活与模型检测");
    const model=[...target.querySelectorAll("label.field")].find(n=>n.textContent?.includes("自动检测模型"))!.querySelector("input")!;
    await act(async()=>{Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,"value")!.set!.call(model,"draft-model");model.dispatchEvent(new Event("input",{bubbles:true}));});
    for(const name of ["models","ip","recovery"]) await act(async()=>container.querySelector<HTMLButtonElement>(`#feature-tab-${name}`)!.click());
    expect(model.value).toBe("draft-model");expect(writes()).toEqual([]);
    expect(container.querySelector("#feature-panel-recovery")?.hasAttribute("hidden")).toBe(false);
    expect(container.querySelector("#feature-panel-ip")?.hasAttribute("hidden")).toBe(true);
  });

  it("shows recovery and model settings without retired Key fallback controls", async () => {
    await openFeatures();
    expect(form("恢复、测活与模型检测")).toBeDefined();
    expect(container.textContent).toContain("模型配置");
    expect(container.textContent).not.toContain("Key 调度回退");
    expect(container.querySelector(".fallback-account")).toBeNull();
    expect(writes()).toEqual([]);
  });
  it("saves mutually exclusive recovery modes together without changing the automatic card-use setting", async () => {
    await openFeatures();
    const target = form("恢复、测活与模型检测");
    const columns = () => [...target.querySelectorAll(".recovery-columns fieldset")];
    const input = (column: number, name: string) => [...columns()[column].querySelectorAll("label")]
      .find((node) => node.textContent?.includes(name))!.querySelector<HTMLInputElement>('input[type="checkbox"]')!;
    expect(input(0, "Fixture 1").checked).toBe(true);
    expect(input(1, "Fixture 2").checked).toBe(true);
    await act(async () => input(1, "Fixture 1").click());
    expect(input(0, "Fixture 1").checked).toBe(false);
    expect(input(1, "Fixture 1").checked).toBe(true);
    expect(input(1, "Fixture 2").checked).toBe(true);
    expect(writes()).toEqual([]);
    expect(config.oauth.oauth_recovery_connection_account_ids).toEqual([1]);
    expect(config.oauth.oauth_recovery_model_account_ids).toEqual([2]);
    const original = vi.mocked(api).getMockImplementation()!;
    vi.mocked(api).mockImplementation(async (method, path, body) => {
      if (method === "PUT" && path === "/config/oauth") return {
        ...config.oauth, revision: "oauth-r2", oauth_recovery_connection_account_ids: [], oauth_recovery_model_account_ids: [2, 1],
      } as never;
      return original(method, path, body);
    });
    await act(async () => saveButton(target).click());
    expect(writes()).toEqual([["PUT", "/config/oauth", {
      expected_revision: "oauth-r1", changes: { oauth_recovery_connection_account_ids: [], oauth_recovery_model_account_ids: [2, 1] },
    }]]);
    expect(target.querySelector('[aria-label="7d 100% 且 429 时自动用卡"]')?.getAttribute("aria-checked")).toBe("false");
    expect(saveButton(target).disabled).toBe(true);
  });

  it("defaults and saves the global automatic detection model in feature settings", async () => {
    await openFeatures();
    const target = form("恢复、测活与模型检测");
    const field = [...target.querySelectorAll<HTMLLabelElement>("label.field")]
      .find((node) => node.textContent?.includes("自动检测模型"))!;
    const model = field.querySelector<HTMLInputElement>("input")!;
    expect(field.textContent).toContain("自动检测模型");
    expect(target.textContent).not.toContain("当前 Companion 连接内统一使用此模型");
    expect(model.value).toBe("gpt-6-luna");

    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(model, "fixture-model");
      model.dispatchEvent(new Event("input", { bubbles: true }));
    });
    expect(writes()).toEqual([]);
    expect(config.oauth.model_detection_model_id).toBe("gpt-6-luna");

    const original = vi.mocked(api).getMockImplementation()!;
    vi.mocked(api).mockImplementation(async (method, path, body) => {
      if (method === "PUT" && path === "/config/oauth") return {
        ...config.oauth, revision: "oauth-r2", model_detection_model_id: "fixture-model",
      } as never;
      return original(method, path, body);
    });
    await act(async () => saveButton(target).click());

    expect(writes()).toEqual([["PUT", "/config/oauth", {
      expected_revision: "oauth-r1", changes: { model_detection_model_id: "fixture-model" },
    }]]);
    expect(config.oauth.model_detection_model_id).toBe("gpt-6-luna");
    expect(saveButton(target).disabled).toBe(true);
    expect(target.querySelector("fieldset")?.disabled).toBe(false);
  });

  it("keeps the upgrade warning and disables recovery controls on an old service", async () => {
    delete config.oauth.model_detection_model_id;
    await openFeatures();
    const target = form("恢复、测活与模型检测");
    expect(target.textContent).toContain("请升级服务端以配置全局自动检测模型。");
    expect(target.querySelector(".bad-text")).not.toBeNull();
    expect(target.querySelector<HTMLFieldSetElement>("fieldset.recovery-compatibility")?.disabled).toBe(true);
  });
});
