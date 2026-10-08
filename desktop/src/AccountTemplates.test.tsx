// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import AccountTemplates from "./AccountTemplates";
import { bindOperationConnection, getOperations, setOperations, type Operation } from "./accountOperations";
import { api, command } from "./bridge";
import type { Account } from "./types";

vi.mock("./bridge", () => ({ api: vi.fn(), command: vi.fn() }));
vi.mock("./mobile", () => ({ useBackAction: vi.fn() }));

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

type Profile = { whitelist: string[]; mappings: { source: string; target: string }[] };
type TemplateConfig = {
  version: string;
  configured: boolean;
  templates: Record<string, Profile>;
  template_meta: Record<string, { name: string; builtin: false }>;
  template_order: string[];
  template_versions: Record<string, string>;
  custom_templates?: { id: string; name: string }[];
};
type AccountConfig = {
  account?: {
    id: number;
    eligible: boolean;
    passthrough: boolean;
    version: string;
    config: Profile;
    reason?: string;
  };
};
type BatchResult = {
  batch_id: string;
  items: { id: string; batch_id: string; account_id: number; status: string }[];
  pending: number;
};

const profileAlpha: Profile = { whitelist: ["alpha-model"], mappings: [] };
const profileBeta: Profile = { whitelist: ["beta-model"], mappings: [] };
const baseConfig: TemplateConfig = {
  version: "config-v1",
  configured: true,
  templates: { alpha: profileAlpha, beta: profileBeta },
  template_meta: {
    alpha: { name: "Alpha", builtin: false },
    beta: { name: "Beta", builtin: false },
  },
  template_order: ["beta", "alpha"],
  template_versions: { alpha: "alpha-v7", beta: "beta-v3" },
  custom_templates: [{ id: "legacy-only", name: "Legacy only" }],
};

const account: Account = {
  id: 1,
  name: "OpenAI OAuth 1",
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
const secondAccount: Account = { ...account, id: 2, name: "OpenAI OAuth 2", version: "account-version-2" };
const thirdAccount: Account = { ...account, id: 3, name: "OpenAI OAuth 3", version: "account-version-3" };

function configFor(entries: [string, string, Profile][]): TemplateConfig {
  return {
    version: "config-v1",
    configured: entries.length > 0,
    templates: Object.fromEntries(entries.map(([id, , profile]) => [id, profile])),
    template_meta: Object.fromEntries(entries.map(([id, name]) => [id, { name, builtin: false as const }])),
    template_order: entries.map(([id]) => id),
    template_versions: Object.fromEntries(entries.map(([id]) => [id, "template-" + id + "-v1"])),
  };
}

function accountConfigFor(
  target: Account,
  overrides: Partial<NonNullable<AccountConfig["account"]>> = {},
): AccountConfig {
  return {
    account: {
      id: target.id,
      eligible: true,
      passthrough: false,
      version: "account-version-" + target.id,
      config: { whitelist: ["current-" + target.id], mappings: [] },
      ...overrides,
    },
  };
}

function clone<T>(value: T): T {
  return structuredClone(value);
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

let container: HTMLDivElement;
let root: Root;
let operationConnectionSequence = 0;

function resetOperationStore() {
  bindOperationConnection("account-template-test-" + ++operationConnectionSequence);
  setOperations([]);
}

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(command).mockResolvedValue(undefined as never);
  resetOperationStore();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  resetOperationStore();
  container.remove();
});

function buttonByLabel(label: string) {
  const result = container.querySelector<HTMLButtonElement>('button[aria-label="' + label + '"]');
  if (!result) throw new Error("missing button: " + label);
  return result;
}

function buttonByText(text: string) {
  const result = [...container.querySelectorAll<HTMLButtonElement>("button")].find(
    (node) => node.textContent?.includes(text),
  );
  if (!result) throw new Error("missing button: " + text);
  return result;
}

function inputByLabel(label: string) {
  const result = container.querySelector<HTMLInputElement>('input[aria-label="' + label + '"]');
  if (!result) throw new Error("missing input: " + label);
  return result;
}

function selectByLabel(label: string) {
  const result = container.querySelector<HTMLSelectElement>('select[aria-label="' + label + '"]');
  if (!result) throw new Error("missing select: " + label);
  return result;
}

async function flush() {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();
  });
}

async function setInput(node: HTMLInputElement, value: string) {
  await act(async () => {
    const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
    setter.call(node, value);
    node.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

async function setSelect(node: HTMLSelectElement, value: string) {
  await act(async () => {
    node.value = value;
    node.dispatchEvent(new Event("change", { bubbles: true }));
  });
}

type ApiOptions = {
  config?: TemplateConfig;
  accountConfigs?: Record<number, AccountConfig>;
  createdId?: string;
  batchResponse?: BatchResult;
  operationsResponse?: BatchResult;
  failTemplateReads?: number;
};

function installApi(options: ApiOptions = {}) {
  let current = clone(options.config ?? baseConfig);
  let revision = 1;
  let failedReads = options.failTemplateReads ?? 0;
  const createdId = options.createdId ?? "created-template";
  const defaultAccounts: Record<number, AccountConfig> = {
    1: accountConfigFor(account),
    2: accountConfigFor(secondAccount),
    3: accountConfigFor(thirdAccount),
  };
  const accountConfigs = { ...defaultAccounts, ...options.accountConfigs };

  vi.mocked(api).mockImplementation(async (method, path, body) => {
    if (method === "GET" && path === "/account-templates") {
      if (failedReads > 0) {
        failedReads -= 1;
        throw new Error("temporary template read failure");
      }
      return clone(current) as never;
    }

    if (method === "GET" && path.startsWith("/account-templates?account_id=")) {
      const id = Number(path.slice(path.indexOf("=") + 1));
      const value = accountConfigs[id];
      if (!value) throw new Error("missing account fixture " + id);
      return clone(value) as never;
    }

    if (method === "POST" && path === "/account-templates/apply") {
      return clone(options.batchResponse ?? {
        batch_id: "batch-default",
        items: [],
        pending: 0,
      }) as never;
    }

    if (method === "GET" && path.startsWith("/account-operations?batch_id=")) {
      return clone(options.operationsResponse ?? {
        batch_id: "batch-default",
        items: [],
        pending: 0,
      }) as never;
    }

    if (method === "POST" && path === "/account-templates") {
      const input = body as { name?: string; whitelist?: string[]; mappings?: Profile["mappings"] };
      current = clone(current);
      current.templates[createdId] = {
        whitelist: input.whitelist ?? [],
        mappings: input.mappings ?? [],
      };
      current.template_meta[createdId] = { name: input.name ?? createdId, builtin: false };
      current.template_order.push(createdId);
      current.template_versions[createdId] = "template-" + createdId + "-v1";
      current.version = "config-v" + ++revision;
      current.configured = true;
      return clone(current) as never;
    }

    if (method === "PUT" && path.startsWith("/account-templates/")) {
      const id = path.slice("/account-templates/".length);
      const input = body as { name?: string; whitelist?: string[]; mappings?: Profile["mappings"] };
      if (!current.templates[id]) throw new Error("missing template fixture " + id);
      current = clone(current);
      current.templates[id] = {
        whitelist: input.whitelist ?? current.templates[id].whitelist,
        mappings: input.mappings ?? current.templates[id].mappings,
      };
      current.template_meta[id] = {
        name: input.name ?? current.template_meta[id].name,
        builtin: false,
      };
      current.template_versions[id] = "template-" + id + "-v" + (revision + 1);
      current.version = "config-v" + ++revision;
      return clone(current) as never;
    }

    if (method === "DELETE" && path.startsWith("/account-templates/")) {
      const id = path.slice("/account-templates/".length);
      current = clone(current);
      delete current.templates[id];
      delete current.template_meta[id];
      delete current.template_versions[id];
      current.template_order = current.template_order.filter((entry) => entry !== id);
      current.version = "config-v" + ++revision;
      current.configured = current.template_order.length > 0;
      return clone(current) as never;
    }

    throw new Error("unexpected " + method + " " + path);
  });

  return {
    get current() {
      return current;
    },
  };
}

async function renderTemplates(options: {
  accounts?: Account[];
  initialAccount?: Account | null;
  initialAccounts?: Account[];
  online?: boolean;
} = {}) {
  const accounts = options.accounts ?? [account];
  await act(async () => {
    root.render(
      <AccountTemplates
        accounts={accounts}
        initialAccount={options.initialAccount}
        initialAccounts={options.initialAccounts}
        online={options.online ?? true}
        close={() => {}}
        report={() => {}}
      />,
    );
  });
  await flush();
}

describe("unified account templates", () => {
  it("renders every template in template_order and ignores the legacy custom_templates field", async () => {
    installApi();
    await renderTemplates();

    expect(
      [...container.querySelectorAll<HTMLInputElement>('input[aria-label^="模板名称 "]')].map((input) => input.value),
    ).toEqual(["Beta", "Alpha"]);
    expect(inputByLabel("模板名称 beta").value).toBe("Beta");
    expect(inputByLabel("模板名称 alpha").value).toBe("Alpha");
    expect(container.textContent).not.toContain("Legacy only");
    expect(
      [...selectByLabel("选择账号模板").options].map((option) => option.textContent),
    ).toEqual(["Beta", "Alpha"]);
  });

  it("saves one card without discarding another card's independent draft", async () => {
    const server = installApi();
    await renderTemplates();

    await setInput(inputByLabel("模板名称 alpha"), "Alpha renamed");
    await setInput(inputByLabel("模板名称 beta"), "Beta draft");
    await act(async () => buttonByLabel("保存Alpha renamed模板").click());
    await flush();

    const update = vi.mocked(api).mock.calls.find(
      ([method, path]) => method === "PUT" && path === "/account-templates/alpha",
    );
    expect(update?.[2]).toMatchObject({
      expected_version: "config-v1",
      name: "Alpha renamed",
      whitelist: ["alpha-model"],
      mappings: [],
    });
    expect(inputByLabel("模板名称 beta").value).toBe("Beta draft");
    expect(buttonByLabel("保存Beta draft模板").disabled).toBe(false);
    expect(inputByLabel("模板名称 alpha").value).toBe("Alpha renamed");
    expect(server.current.version).toBe("config-v2");
  });

  it.each(["full", "degraded", "takeover"])(
    "renames and deletes legacy template id %s through the unified endpoints",
    async (id) => {
      installApi({
        config: configFor([[id, id, { whitelist: [id + "-model"], mappings: [] }]]),
      });
      await renderTemplates();

      await setInput(inputByLabel("模板名称 " + id), "Renamed " + id);
      await act(async () => buttonByLabel("保存Renamed " + id + "模板").click());
      await flush();

      const put = vi.mocked(api).mock.calls.find(
        ([method, path]) => method === "PUT" && path === "/account-templates/" + id,
      );
      expect(put?.[2]).toMatchObject({
        expected_version: "config-v1",
        name: "Renamed " + id,
      });

      await act(async () => buttonByLabel("删除Renamed " + id + "模板").click());
      expect(vi.mocked(api).mock.calls.some(
        ([method, path]) => method === "DELETE" && path === "/account-templates/" + id,
      )).toBe(false);
      await act(async () => buttonByText("确认删除").click());
      await flush();

      const deletion = vi.mocked(api).mock.calls.find(
        ([method, path]) => method === "DELETE" && path === "/account-templates/" + id,
      );
      expect(deletion?.[2]).toMatchObject({ expected_version: "config-v2" });
      expect(container.querySelector('input[aria-label="模板名称 ' + id + '"]')).toBeNull();
    },
  );

  it("creates into an empty collection and leaves it empty after deleting the last template", async () => {
    const server = installApi({ config: configFor([]), createdId: "created-last" });
    await renderTemplates();

    expect(container.querySelectorAll('input[aria-label^="模板名称 "]')).toHaveLength(0);
    await act(async () => buttonByText("新增模板").click());
    await setInput(inputByLabel("新模板名称"), "First template");
    await act(async () => buttonByLabel("保存新模板").click());
    await flush();

    const creation = vi.mocked(api).mock.calls.find(
      ([method, path]) => method === "POST" && path === "/account-templates",
    );
    expect(creation?.[2]).toMatchObject({
      expected_version: "config-v1",
      name: "First template",
      whitelist: [],
      mappings: [],
    });
    expect(inputByLabel("模板名称 created-last").value).toBe("First template");

    await act(async () => buttonByLabel("删除First template模板").click());
    await act(async () => buttonByText("确认删除").click());
    await flush();

    expect(vi.mocked(api).mock.calls.some(
      ([method, path]) => method === "DELETE" && path === "/account-templates/created-last",
    )).toBe(true);
    expect(container.querySelectorAll('input[aria-label^="模板名称 "]')).toHaveLength(0);
    expect(server.current.template_order).toEqual([]);
  });

  it("shows a failed read and recovers through the explicit reread action", async () => {
    installApi({ failTemplateReads: 1 });
    await renderTemplates();

    expect(container.querySelector('[role="alert"]')?.textContent).toContain("temporary template read failure");
    await act(async () => buttonByText("重新读取").click());
    await flush();

    expect(inputByLabel("模板名称 alpha").value).toBe("Alpha");
    expect(container.querySelector('[role="alert"]')).toBeNull();
  });

  it("reads each locked batch account, previews the diff, uses the selected template version, and follows the batch", async () => {
    const batchResponse: BatchResult = {
      batch_id: "batch-42",
      items: [
        { id: "operation-1", batch_id: "batch-42", account_id: 1, status: "queued" },
        { id: "operation-2", batch_id: "batch-42", account_id: 2, status: "queued" },
      ],
      pending: 2,
    };
    const operationsResponse: BatchResult = {
      ...batchResponse,
      items: batchResponse.items.map((item) => ({ ...item, status: "completed" })),
      pending: 0,
    };
    installApi({
      batchResponse,
      operationsResponse,
      accountConfigs: {
        1: accountConfigFor(account),
        2: accountConfigFor(secondAccount),
      },
    });
    await renderTemplates({ accounts: [account, secondAccount], initialAccounts: [account, secondAccount] });
    expect(selectByLabel("选择应用账号").disabled).toBe(true);

    const reads = vi.mocked(api).mock.calls
      .filter(([method, path]) => method === "GET" && path.startsWith("/account-templates?account_id="))
      .map(([, path]) => path)
      .sort();
    expect(reads).toEqual(["/account-templates?account_id=1", "/account-templates?account_id=2"]);

    await setSelect(selectByLabel("选择账号模板"), "alpha");
    expect(container.textContent).toContain("current-1");
    expect(container.textContent).toContain("current-2");
    expect(container.textContent).toContain("alpha-model");

    await setInput(inputByLabel("模板名称 beta"), "Beta draft");
    expect(buttonByText("应用模板").disabled).toBe(false);
    await setInput(inputByLabel("模板名称 alpha"), "Alpha draft");
    expect(buttonByText("应用模板").disabled).toBe(true);
    await act(async () => buttonByText("放弃修改").click());
    expect(buttonByText("应用模板").disabled).toBe(false);

    await act(async () => buttonByText("应用模板").click());
    await flush();

    const apply = vi.mocked(api).mock.calls.find(
      ([method, path]) => method === "POST" && path === "/account-templates/apply",
    );
    expect(apply?.[2]).toMatchObject({
      template_id: "alpha",
      template_version: "alpha-v7",
      accounts: [
        { account_id: 1, expected_version: "account-version-1" },
        { account_id: 2, expected_version: "account-version-2" },
      ],
    });
    const request = apply?.[2] as { request_id?: unknown; client_id?: unknown };
    expect(typeof request?.request_id).toBe("string");
    expect(String(request?.request_id).length).toBeGreaterThan(0);
    expect(typeof request?.client_id).toBe("string");
    expect(String(request?.client_id).length).toBeGreaterThan(0);
    expect(vi.mocked(api).mock.calls.some(
      ([method, path]) => method === "GET" && path === "/account-operations?batch_id=batch-42",
    )).toBe(true);
    expect(container.textContent).toContain("已完成");
  });

  it("discards a late accepted batch after an epoch change while preserving it on the original connection", async () => {
    const latePost = deferred<BatchResult>();
    const acceptedBatch: BatchResult = {
      batch_id: "batch-late",
      items: [
        { id: "operation-late", batch_id: "batch-late", account_id: 1, status: "queued" },
      ],
      pending: 1,
    };
    let serverConnection = "connection-before-late-response";
    const serverBatches = new Map<string, Map<string, BatchResult>>();
    serverBatches.set(serverConnection, new Map());
    const currentConnectionOperation: Operation = {
      id: "operation-current-connection",
      account_id: 99,
      account_name: "Current connection account",
      action: "account_template",
      status: "queued",
      requested: {},
    };
    bindOperationConnection("connection-before-late-response");
    setOperations([]);
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method === "GET" && path === "/account-templates") return clone(baseConfig) as never;
      if (method === "GET" && path === "/account-templates?account_id=1") {
        return accountConfigFor(account) as never;
      }
      if (method === "POST" && path === "/account-templates/apply") {
        serverBatches.get(serverConnection)?.set(acceptedBatch.batch_id, clone(acceptedBatch));
        return latePost.promise as never;
      }
      if (method === "GET" && path === "/account-operations?batch_id=batch-late") {
        const batch = serverBatches.get(serverConnection)?.get(acceptedBatch.batch_id);
        if (!batch) throw new Error("batch not found on " + serverConnection);
        return clone(batch) as never;
      }
      throw new Error("unexpected " + method + " " + path);
    });

    await renderTemplates({ initialAccount: account });
    await setSelect(selectByLabel("选择账号模板"), "alpha");
    await act(async () => buttonByText("应用模板").click());
    await flush();
    expect(vi.mocked(api).mock.calls.some(
      ([method, path]) => method === "POST" && path === "/account-templates/apply",
    )).toBe(true);

    serverConnection = "connection-after-late-response";
    bindOperationConnection(serverConnection);
    setOperations([currentConnectionOperation]);
    await act(async () => {
      latePost.resolve(clone(acceptedBatch));
      await latePost.promise;
    });
    await flush();

    expect(vi.mocked(api).mock.calls.some(
      ([method, path]) => method === "GET" && path === "/account-operations?batch_id=batch-late",
    )).toBe(false);
    expect(container.textContent).not.toContain("已完成");
    expect(vi.mocked(api).mock.calls.some(
      ([method, path]) => method === "POST" && path === "/account-operations/operation-late/cancel",
    )).toBe(false);
    expect(getOperations()).toEqual([currentConnectionOperation]);

    serverConnection = "connection-before-late-response";
    bindOperationConnection(serverConnection);
    setOperations([]);
    let restored: BatchResult | undefined;
    await act(async () => {
      restored = await api<BatchResult>("GET", "/account-operations?batch_id=batch-late");
    });
    expect(restored).toEqual(acceptedBatch);
    expect(vi.mocked(api).mock.calls.filter(
      ([method, path]) => method === "GET" && path === "/account-operations?batch_id=batch-late",
    )).toHaveLength(1);
  });

  it("keeps initialAccount as a single-account entry point", async () => {
    installApi();
    await renderTemplates({ accounts: [account, secondAccount], initialAccount: secondAccount });

    expect(vi.mocked(api).mock.calls.some(
      ([method, path]) => method === "GET" && path === "/account-templates?account_id=2",
    )).toBe(true);
    const accountSelect = selectByLabel("选择应用账号");
    expect(accountSelect.value).toBe("2");
    expect(accountSelect.disabled).toBe(true);

    await setSelect(selectByLabel("选择账号模板"), "alpha");
    await act(async () => buttonByText("应用模板").click());
    await flush();
    const apply = vi.mocked(api).mock.calls.find(
      ([method, path]) => method === "POST" && path === "/account-templates/apply",
    );
    expect(apply?.[2]).toMatchObject({
      template_id: "alpha",
      accounts: [{ account_id: 2, expected_version: "account-version-2" }],
    });
  });

  it("falls back to the template collection version when a template version is absent", async () => {
    const config = clone(baseConfig);
    delete config.template_versions.alpha;
    installApi({ config });
    await renderTemplates({ initialAccount: account });

    await setSelect(selectByLabel("选择账号模板"), "alpha");
    await act(async () => buttonByText("应用模板").click());
    await flush();

    const apply = vi.mocked(api).mock.calls.find(
      ([method, path]) => method === "POST" && path === "/account-templates/apply",
    );
    expect(apply?.[2]).toMatchObject({
      template_id: "alpha",
      template_version: "config-v1",
      accounts: [{ account_id: 1, expected_version: "account-version-1" }],
    });
  });

  it("shows each ineligible reason and still submits a mixed batch", async () => {
    const batchResponse: BatchResult = {
      batch_id: "batch-mixed",
      items: [
        { id: "operation-1", batch_id: "batch-mixed", account_id: 1, status: "queued" },
        { id: "operation-2", batch_id: "batch-mixed", account_id: 2, status: "failed" },
      ],
      pending: 1,
    };
    installApi({
      batchResponse,
      operationsResponse: { ...batchResponse, pending: 0 },
      accountConfigs: {
        1: accountConfigFor(account),
        2: accountConfigFor(secondAccount, {
          eligible: false,
          reason: "账号策略不允许独立模板",
        }),
      },
    });
    await renderTemplates({ accounts: [account, secondAccount], initialAccounts: [account, secondAccount] });

    expect(container.textContent).toContain("账号策略不允许独立模板");
    expect(buttonByText("应用模板").disabled).toBe(false);
    await act(async () => buttonByText("应用模板").click());
    await flush();

    const apply = vi.mocked(api).mock.calls.find(
      ([method, path]) => method === "POST" && path === "/account-templates/apply",
    );
    expect(apply?.[2]).toMatchObject({
      accounts: [
        { account_id: 1, expected_version: "account-version-1" },
        { account_id: 2, expected_version: "account-version-2" },
      ],
    });
    expect(vi.mocked(api).mock.calls.some(
      ([method, path]) => method === "GET" && path === "/account-operations?batch_id=batch-mixed",
    )).toBe(true);
  });

  it("disables batch apply only when every selected account is ineligible", async () => {
    installApi({
      accountConfigs: {
        1: accountConfigFor(account, { eligible: false, reason: "账号 1 不适用" }),
        2: accountConfigFor(secondAccount, {
          eligible: true,
          passthrough: true,
          reason: "账号 2 是透传模式",
        }),
      },
    });
    await renderTemplates({ accounts: [account, secondAccount], initialAccounts: [account, secondAccount] });

    expect(container.textContent).toContain("账号 1 不适用");
    expect(container.textContent).toContain("账号 2 是透传模式");
    expect(buttonByText("应用模板").disabled).toBe(true);
    await act(async () => buttonByText("应用模板").click());
    expect(vi.mocked(api).mock.calls.some(
      ([method, path]) => method === "POST" && path === "/account-templates/apply",
    )).toBe(false);
  });

  it("keeps an empty eligible-account candidate list unselected", async () => {
    const unsupported = { ...account, platform: "anthropic" };
    installApi();
    await renderTemplates({ accounts: [unsupported] });

    expect(selectByLabel("选择应用账号").value).toBe("0");
    expect(selectByLabel("选择应用账号").options).toHaveLength(1);
    expect(buttonByText("应用模板").disabled).toBe(true);
    expect(vi.mocked(api).mock.calls.some(
      ([method, path]) => method === "GET" && path.startsWith("/account-templates?account_id="),
    )).toBe(false);
  });

  it("shows a read error when the initial single account no longer exists", async () => {
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method === "GET" && path === "/account-templates") return clone(baseConfig) as never;
      if (method === "GET" && path === "/account-templates?account_id=99") {
        throw new Error("404 account not found");
      }
      throw new Error("unexpected " + method + " " + path);
    });
    const missingAccount = { ...account, id: 99, name: "Removed account" };
    await renderTemplates({ accounts: [account], initialAccount: missingAccount });

    expect(container.querySelector('[role="alert"]')?.textContent).toContain("404 account not found");
    expect(buttonByText("应用模板").disabled).toBe(true);
  });

  it("discards an account response that arrives after the selected account changes", async () => {
    const stale = deferred<AccountConfig>();
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method === "GET" && path === "/account-templates") return clone(baseConfig) as never;
      if (method === "GET" && path === "/account-templates?account_id=1") return stale.promise as never;
      if (method === "GET" && path === "/account-templates?account_id=2") {
        return accountConfigFor(secondAccount) as never;
      }
      throw new Error("unexpected " + method + " " + path);
    });
    await renderTemplates({ accounts: [account, secondAccount] });

    expect(selectByLabel("选择应用账号").value).toBe("1");
    await setSelect(selectByLabel("选择应用账号"), "2");
    await flush();
    await act(async () => {
      stale.resolve(accountConfigFor(account));
      await stale.promise;
    });

    expect(container.textContent).toContain("current-2");
    expect(container.textContent).not.toContain("current-1");
  });

  it("drops a template response from a connection that went offline", async () => {
    const stale = deferred<TemplateConfig>();
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method === "GET" && path === "/account-templates") return stale.promise as never;
      if (method === "GET" && path === "/account-templates?account_id=1") return accountConfigFor(account) as never;
      throw new Error("unexpected " + method + " " + path);
    });

    await renderTemplates({ online: true });
    await act(async () => {
      root.render(
        <AccountTemplates accounts={[account]} online={false} close={() => {}} report={() => {}} />,
      );
    });
    await flush();
    await act(async () => {
      stale.resolve(baseConfig);
      await stale.promise;
    });

    expect(container.querySelector('input[aria-label="模板名称 alpha"]')).toBeNull();
  });
});
