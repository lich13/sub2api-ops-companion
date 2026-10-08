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
  group_ids: [], blockers: [], managed: false, version: "a".repeat(64), last_success_at: null,
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
      oauth_recovery_connection_account_ids: [1], oauth_recovery_model_account_ids: [2],
    },
    key_fallback: { revision: "key-r1", openai_enabled: true, grok_enabled: false, managed_account_ids: [11], coexist_account_ids: [] },
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

function keyInputs(id: number) {
  const row = [...container.querySelectorAll(".fallback-account")].find((node) => node.textContent?.includes(`Fixture ${id}`))!;
  const inputs = row.querySelectorAll<HTMLInputElement>('input[type="checkbox"]');
  return { managed: inputs[0], coexist: inputs[1] };
}

const writes = () => vi.mocked(api).mock.calls.filter(([method]) => method !== "GET");

describe("feature settings drafts", () => {
  it("saves Key coexist choices only on submit and removes coexist when management is unchecked", async () => {
    await openFeatures();
    const target = form("Key 调度回退");
    expect(keyInputs(11).coexist.disabled).toBe(false);
    expect(keyInputs(21).coexist.disabled).toBe(true);
    expect(saveButton(target).disabled).toBe(true);
    await act(async () => keyInputs(11).coexist.click());
    await act(async () => keyInputs(21).managed.click());
    await act(async () => keyInputs(21).coexist.click());
    expect(writes()).toEqual([]);
    expect(config.key_fallback.coexist_account_ids).toEqual([]);
    expect(config.key_fallback.managed_account_ids).toEqual([11]);
    await act(async () => receive({ ...initial, snapshot: { ...initial.snapshot!, observed_at: "2026-10-08T00:00:10Z" } }));
    expect(keyInputs(11).coexist.checked).toBe(true);
    expect(keyInputs(21).coexist.checked).toBe(true);

    let finishSave!: (value: ConfigSection) => void;
    const original = vi.mocked(api).getMockImplementation()!;
    vi.mocked(api).mockImplementation(async (method, path, body) => {
      if (method === "PUT" && path === "/config/key_fallback") return new Promise((resolve) => { finishSave = resolve; });
      return original(method, path, body);
    });
    await act(async () => saveButton(target).click());
    expect(writes()).toEqual([["PUT", "/config/key_fallback", {
      expected_revision: "key-r1", changes: { coexist_account_ids: [11, 21], managed_account_ids: [11, 21] },
    }]]);
    expect(target.querySelector("fieldset")?.disabled).toBe(true);
    const saved = { ...config.key_fallback, revision: "key-r2", managed_account_ids: [11, 21], coexist_account_ids: [11, 21] };
    await act(async () => finishSave(saved));
    expect(saveButton(target).disabled).toBe(true);
    expect(target.querySelector("fieldset")?.disabled).toBe(false);

    await act(async () => keyInputs(11).managed.click());
    expect(keyInputs(11).coexist.checked).toBe(false);
    expect(keyInputs(11).coexist.disabled).toBe(true);
    expect(keyInputs(21).coexist.checked).toBe(true);
    expect(writes()).toHaveLength(1);
    await act(async () => saveButton(target).click());
    expect(writes()[1]).toEqual(["PUT", "/config/key_fallback", {
      expected_revision: "key-r2", changes: { managed_account_ids: [21], coexist_account_ids: [21] },
    }]);
    await act(async () => finishSave({ ...saved, revision: "key-r3", managed_account_ids: [21], coexist_account_ids: [21] }));
    expect(saveButton(target).disabled).toBe(true);
    expect(container.textContent).toContain("设置已保存");
  });

  it("saves mutually exclusive recovery modes together without changing the automatic card-use setting", async () => {
    await openFeatures();
    const target = form("OAuth 恢复与测活");
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
});
