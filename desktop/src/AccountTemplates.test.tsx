// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import AccountTemplates from "./AccountTemplates";
import { accountOperation } from "./accountOperations";
import { api, command } from "./bridge";
import type { Account } from "./types";

vi.mock("./bridge", () => ({ api: vi.fn(), command: vi.fn() }));
vi.mock("./mobile", () => ({ useBackAction: vi.fn() }));
vi.mock("./accountOperations", () => ({ accountOperation: vi.fn() }));

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

type Profile = { whitelist: string[]; mappings: { source: string; target: string }[] };
type Config = { version: string; configured: boolean; templates: Record<"full" | "degraded" | "takeover", Profile> };
type AccountConfig = { account?: { id: number; eligible: boolean; passthrough: boolean; version: string; config: Profile } };

const account: Account = {
  id: 1,
  name: "OpenAI OAuth",
  priority: 1,
  platform: "openai",
  type: "oauth",
  status: "active",
  schedulable: true,
  available: true,
  group_ids: [],
  blockers: [],
  managed: false,
  version: "account-version-1",
  last_success_at: null,
  last_error_at: null,
  last_error_id: null,
  last_error_code: null,
  last_error_status: null,
  error_message: "",
  success_after_error: false,
  usage_windows: [],
};

const config: Config = {
  version: "template-version-1",
  configured: true,
  templates: {
    full: { whitelist: ["gpt-6-sol"], mappings: [] },
    degraded: { whitelist: ["gpt-6-luna"], mappings: [] },
    takeover: { whitelist: [], mappings: [{ source: "gpt-6-*", target: "gpt-6-luna" }] },
  },
};

const accountConfig: AccountConfig = {
  account: {
    id: 1,
    eligible: true,
    passthrough: false,
    version: "account-template-version-1",
    config: { whitelist: ["account-model"], mappings: [] },
  },
};

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(command).mockResolvedValue(undefined as never);
  vi.mocked(accountOperation).mockResolvedValue(undefined as never);
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
});

function button(text: string) {
  const result = [...container.querySelectorAll<HTMLButtonElement>("button")].find(
    (node) => node.textContent?.includes(text),
  );
  if (!result) throw new Error("missing button: " + text);
  return result;
}

async function setInput(node: HTMLInputElement, value: string) {
  await act(async () => {
    const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
    setter.call(node, value);
    node.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

async function renderLoaded() {
  vi.mocked(api).mockImplementation(async (method, path) => {
    if (method === "GET" && path === "/account-templates") return config as never;
    if (method === "GET" && path === "/account-templates?account_id=1") return accountConfig as never;
    throw new Error("unexpected " + method + " " + path);
  });
  await act(async () => root.render(
    <AccountTemplates
      accounts={[account]}
      initialAccount={account}
      online
      close={() => {}}
      report={() => {}}
    />,
  ));
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}

describe("account template reads and drafts", () => {
  it("keeps independent template and account reads when responses arrive in reverse order", async () => {
    const templateRead = deferred<Config>();
    const accountRead = deferred<AccountConfig>();
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method !== "GET") throw new Error("unexpected method " + method);
      if (path === "/account-templates") return templateRead.promise as never;
      if (path === "/account-templates?account_id=1") return accountRead.promise as never;
      throw new Error("unexpected path " + path);
    });

    await act(async () => root.render(
      <AccountTemplates
        accounts={[account]}
        initialAccount={account}
        online
        close={() => {}}
        report={() => {}}
      />,
    ));
    expect(vi.mocked(api).mock.calls.map((call) => call[1])).toEqual([
      "/account-templates",
      "/account-templates?account_id=1",
    ]);

    await act(async () => {
      accountRead.resolve(accountConfig);
      await accountRead.promise;
    });
    await act(async () => {
      templateRead.resolve(config);
      await templateRead.promise;
    });

    expect(container.textContent).toContain("当前：account-model");
    expect(container.textContent).toContain("目标：gpt-6-sol");
    expect(container.querySelector<HTMLInputElement>('[aria-label="满血白名单 1"]')?.value).toBe("gpt-6-sol");
    expect(container.querySelector("[role=alert]")).toBeNull();
  });

  it("supports discarding a draft and saving the edited template with its revision", async () => {
    await renderLoaded();
    const whitelist = container.querySelector<HTMLInputElement>('[aria-label="满血白名单 1"]')!;
    await setInput(whitelist, "gpt-6-astra");
    expect(button("保存模板").disabled).toBe(false);

    await act(async () => button("放弃修改").click());
    expect(whitelist.value).toBe("gpt-6-sol");
    expect(button("保存模板").disabled).toBe(true);

    await setInput(whitelist, "gpt-6-astra");
    const saved: Config = {
      ...config,
      version: "template-version-2",
      templates: { ...config.templates, full: { whitelist: ["gpt-6-astra"], mappings: [] } },
    };
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method === "PUT" && path === "/account-templates") return saved as never;
      if (method === "GET" && path === "/account-templates?account_id=1") return accountConfig as never;
      if (method === "GET" && path === "/account-templates") return config as never;
      throw new Error("unexpected " + method + " " + path);
    });
    await act(async () => button("保存模板").click());

    expect(vi.mocked(api).mock.calls.find(([method]) => method === "PUT")?.[2]).toEqual({
      expected_version: "template-version-1",
      full: { whitelist: ["gpt-6-astra"], mappings: [] },
      degraded: config.templates.degraded,
      takeover: config.templates.takeover,
    });
    expect(container.querySelector<HTMLInputElement>('[aria-label="满血白名单 1"]')?.value).toBe("gpt-6-astra");
    expect(button("保存模板").disabled).toBe(true);
  });

  it("blocks applying a dirty draft and applies the saved revision after discard", async () => {
    await renderLoaded();
    const whitelist = container.querySelector<HTMLInputElement>('[aria-label="满血白名单 1"]')!;
    await setInput(whitelist, "gpt-6-astra");
    expect(button("应用模板").disabled).toBe(true);
    await act(async () => button("应用模板").click());
    expect(accountOperation).not.toHaveBeenCalled();

    await act(async () => button("放弃修改").click());
    expect(button("应用模板").disabled).toBe(false);
    await act(async () => button("应用模板").click());

    expect(accountOperation).toHaveBeenCalledWith(
      account,
      "account_template",
      { template_id: "full", template_version: "template-version-1" },
      "account-template-version-1",
    );
    expect(command).toHaveBeenCalledWith("refresh");
  });
});
