// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { api, command, subscribe } from "./bridge";
import type { Account, Group, OpsError, ViewState } from "./types";

vi.mock("./bridge", () => ({
  api: vi.fn(),
  command: vi.fn(),
  subscribe: vi.fn(),
  updates: vi.fn(async () => () => {}),
  preview: true,
}));
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
Object.assign(globalThis, {
  ResizeObserver: class {
    observe() {}
    disconnect() {}
    unobserve() {}
  },
});

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(api).mockImplementation(async (method, path) => {
    throw new Error(`Unexpected mocked API request: ${method} ${path}`);
  });
  window.history.replaceState(null, "", "/?panel=quick");
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  window.history.replaceState(null, "", "/");
});

const activity = (log_id: number, called_at: string) => ({
  log_id,
  account_id: 73,
  account_name: "目标账号",
  model: "gpt-6-sol",
  upstream_model: "gpt-6-sol",
  called_at,
});

const group = (
  id: number,
  name: string,
  called_at: string | null,
  recent_accounts: Group["recent_accounts"],
): Group => ({
  id,
  name,
  platform: "openai",
  account_id: 73,
  account_name: "目标账号",
  model: "gpt-6-sol",
  upstream_model: "gpt-6-sol",
  upstream_response_model: "gpt-6-sol",
  called_at,
  recent_accounts,
});

const account: Account = {
  id: 73,
  name: "目标账号",
  priority: 1,
  platform: "openai",
  type: "apikey",
  status: "active",
  schedulable: false,
  available: false,
  group_ids: [5, 8, 11, 20],
  blockers: [],
  managed: false,
  version: "account-73-version",
  last_success_at: null,
  last_error_at: "2026-09-29T23:00:00Z",
  last_error_id: 880,
  last_error_code: "upstream_failure",
  last_error_status: 502,
  error_message: "上游异常",
  success_after_error: false,
  usage_windows: [],
  degradation_mark: { marked: true, version: "mark-version" },
};

const errorDetail: OpsError = {
  id: 880,
  account_id: 73,
  account_name: "目标账号",
  group_id: 5,
  group_name: "同时间小 ID",
  created_at: "2026-09-29T23:00:00Z",
  model: "gpt-6-sol",
  requested_model: "gpt-6-sol",
  upstream_model: "gpt-6-sol",
  status_code: 502,
  upstream_status_code: 502,
  provider_error_code: "upstream_failure",
  message: "上游异常详情",
  request_id: "request-880",
  resolved: false,
  content: "mock 诊断详情",
  content_limited: false,
};

function quickAccount(id: number, name: string, overrides: Partial<Account> = {}): Account {
  return {
    ...account,
    id,
    name,
    priority: id,
    schedulable: true,
    available: true,
    group_ids: [],
    blockers: [],
    recoverable: false,
    managed: false,
    version: `account-${id}-version`,
    last_success_at: null,
    last_error_at: null,
    last_error_id: null,
    last_error_code: null,
    last_error_status: null,
    error_message: "",
    success_after_error: false,
    degradation_mark: undefined,
    ...overrides,
  };
}

function quickGroup(id: number, name: string, value: Account): Group {
  const calledAt = "2026-10-14T00:00:00Z";
  const call = { ...activity(id, calledAt), account_id: value.id, account_name: value.name };
  return {
    ...group(id, name, calledAt, [call]),
    account_id: value.id,
    account_name: value.name,
    recent_accounts: [call],
  };
}

function quickState(accounts: Account[], groups: Group[], errors: OpsError[], overrides: Partial<ViewState> = {}): ViewState {
  return {
    connected: true,
    online: true,
    platform: "macos",
    foreground: true,
    error: "",
    preferences: {
      base_url: "https://quick-fixture.invalid",
      favorites: [],
      pinned: false,
      launch_at_login: false,
    },
    snapshot: {
      observed_at: "2026-10-14T00:00:00Z",
      accounts,
      groups,
      errors,
      recoveries: [],
    },
    ...overrides,
  };
}

async function renderQuickState(value: ViewState) {
  let deliver!: (next: ViewState) => void;
  vi.mocked(subscribe).mockImplementation(async (callback) => {
    deliver = callback;
    return () => {};
  });
  vi.mocked(command).mockImplementation(async (name) =>
    name === "get_state" ? (value as never) : (undefined as never),
  );
  const { default: App } = await import("./main");
  await act(async () => root.render(<App />));
  await act(async () => { await Promise.resolve(); await Promise.resolve(); });
  return {
    deliver: async (next: ViewState) => {
      await act(async () => deliver(next));
      await act(async () => { await Promise.resolve(); await Promise.resolve(); });
    },
  };
}

function quickGroupRow(name: string): Element {
  const row = [...container.querySelectorAll<HTMLElement>(".quick-groups .group-row")]
    .find((node) => node.querySelector(".group-heading strong")?.textContent === name);
  if (!row) throw new Error(`missing quick group: ${name}`);
  return row;
}

function quickErrorRow(name: string): Element {
  const row = [...container.querySelectorAll<HTMLElement>(".error-list .quick-error-row")]
    .find((node) => node.querySelector(".quick-error-open")?.textContent?.includes(name));
  if (!row) throw new Error(`missing quick error: ${name}`);
  return row;
}

describe("quick group activity", () => {
  it("orders current activity, keeps pin preferences, and routes mocked actions to the account ID", async () => {
    const at = "2026-09-30T00:00:00Z";
    const state: ViewState = {
      connected: true,
      online: true,
      platform: "macos",
      foreground: true,
      error: "",
      preferences: {
        base_url: "https://fixture.invalid",
        favorites: [73, 99],
        pinned: false,
        launch_at_login: false,
      },
      snapshot: {
        observed_at: at,
        accounts: [account],
        groups: [
          group(20, "较早活动", "2026-09-29T23:00:00Z", [
            activity(20, "2026-09-29T23:00:00Z"),
          ]),
          group(11, "同刻较大 ID", at, [activity(15, at)]),
          group(5, "同时间小 ID", at, [activity(11, at)]),
          group(2, "已移出成员", "2099-01-01T00:00:00Z", [
            activity(2000, "2099-01-01T00:00:00Z"),
          ]),
          group(3, "空活动", "2099-01-01T00:00:00Z", []),
          group(8, "无效时间", "2099-01-01T00:00:00Z", [
            activity(3000, "not-a-time"),
          ]),
        ],
        errors: [errorDetail],
        recoveries: [],
      },
    };
    vi.mocked(subscribe).mockResolvedValue(() => {});
    vi.mocked(command).mockImplementation(async (name) => {
      if (name === "get_state") return state as never;
      return undefined as never;
    });
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method === "GET" && path === "/errors/880")
        return errorDetail as never;
      if (method === "POST" && path === "/accounts/73/schedulable")
        return { verified: true } as never;
      throw new Error(`Unexpected mocked API request: ${method} ${path}`);
    });
    const { default: App } = await import("./main");

    await act(async () => root.render(<App />));

    const groupNames = () =>
      [...container.querySelectorAll(".group-heading strong")].map(
        (node) => node.textContent,
      );
    expect(groupNames()).toEqual([
      "同时间小 ID",
      "同刻较大 ID",
      "较早活动",
      "已移出成员",
      "空活动",
      "无效时间",
    ]);
    const groupBadge = container.querySelector<HTMLElement>(
      ".quick-groups .compact-identity .degradation-badge.compact",
    );
    expect(groupBadge?.textContent).toBe("");
    expect(groupBadge?.getAttribute("aria-label")).toBe("降智");
    const movedGroup = [...container.querySelectorAll(".group-row")].find(
      (row) =>
        row.querySelector(".group-heading strong")?.textContent ===
        "已移出成员",
    );
    expect(movedGroup?.querySelector(".recent-account")).toBeNull();
    expect(movedGroup?.textContent).toContain("暂无成功调用");
    const buttonNames = [...container.querySelectorAll("button")].map((node) =>
      [node.textContent, node.getAttribute("aria-label"), node.title].join(" "),
    );
    expect(buttonNames.join(" ")).not.toMatch(/收藏/);

    await act(async () =>
      [...container.querySelectorAll<HTMLButtonElement>(".quick-tabs button")]
        .find((button) => button.textContent?.startsWith("异常"))!
        .click(),
    );
    const errorBadge = container.querySelector<HTMLElement>(
      ".error-list .error-row .degradation-badge.compact",
    );
    expect(errorBadge?.textContent).toBe("");
    expect(errorBadge?.getAttribute("aria-label")).toBe("降智");
    await act(async () =>
      [...container.querySelectorAll<HTMLButtonElement>(".quick-tabs button")]
        .find((button) => button.textContent === "分组")!
        .click(),
    );

    await act(async () =>
      container.querySelector<HTMLButtonElement>('[title="固定面板"]')!.click(),
    );
    expect(command).toHaveBeenCalledWith(
      "preferences",
      expect.objectContaining({ favorites: [73, 99], pinned: true }),
    );

    await act(async () =>
      container
        .querySelector<HTMLButtonElement>('[aria-label="目标账号调度"]')!
        .click(),
    );
    expect(api).toHaveBeenCalledWith("POST", "/accounts/73/schedulable", {
      schedulable: true,
      expected_version: "account-73-version",
      detach_managed: false,
    });

    await act(async () => {
      [...container.querySelectorAll<HTMLButtonElement>(".compact-times button")]
        .find((button) => button.textContent?.includes("上次错误"))!
        .click();
    });
    expect(api).toHaveBeenCalledWith("GET", "/errors/880");
    expect(container.querySelector(".drawer")?.textContent).toContain(
      "mock 诊断详情",
    );
    expect(
      vi.mocked(api).mock.calls.filter(([method]) => method !== "GET"),
    ).toEqual([
      [
        "POST",
        "/accounts/73/schedulable",
        {
          schedulable: true,
          expected_version: "account-73-version",
          detach_managed: false,
        },
      ],
    ]);
  });

  it("marks unavailable, blocked and recoverable accounts in both quick tabs, but leaves history errors unmarked", async () => {
    const recoverable = quickAccount(73, "受限恢复账号", {
      available: false,
      recoverable: true,
      group_ids: [23],
      last_error_id: 880,
      last_error_at: "2026-10-13T23:00:00Z",
      last_error_code: "upstream_failure",
      last_error_status: 502,
    });
    const history = quickAccount(74, "历史错误账号", {
      group_ids: [24],
      last_error_id: 882,
      last_error_at: "2026-10-13T22:00:00Z",
      last_error_code: "upstream_failure",
      last_error_status: 502,
    });
    const blocked = quickAccount(75, "阻塞账号", {
      group_ids: [25],
      blockers: [{ code: "cooldown", label: "冷却限制" }],
    });
    const recoverableOnly = quickAccount(76, "可恢复账号", {
      group_ids: [26],
      recoverable: true,
    });
    const unavailableOnly = quickAccount(77, "不可用账号", {
      group_ids: [27],
      available: false,
    });
    const currentError = {
      ...errorDetail,
      id: 880,
      account_id: recoverable.id,
      account_name: recoverable.name,
      group_id: 23,
      group_name: "受限账号组",
    };
    const historicError = {
      ...errorDetail,
      id: 881,
      account_id: history.id,
      account_name: history.name,
      group_id: 24,
      group_name: "历史账号组",
    };
    await renderQuickState(quickState(
      [recoverable, history, blocked, recoverableOnly, unavailableOnly],
      [
        quickGroup(23, "受限账号组", recoverable),
        quickGroup(24, "历史账号组", history),
        quickGroup(25, "阻塞账号组", blocked),
        quickGroup(26, "可恢复账号组", recoverableOnly),
        quickGroup(27, "不可用账号组", unavailableOnly),
      ],
      [currentError, historicError],
    ));

    const restrictedGroup = quickGroupRow("受限账号组");
    const unavailableName = restrictedGroup.querySelector(".quick-account-restricted");
    expect(unavailableName).toBeTruthy();
    expect(unavailableName?.querySelector('[role="img"][aria-label="调度受限：存在可恢复的状态限制"]')).toBeTruthy();
    expect(quickGroupRow("阻塞账号组").querySelector(
      '.quick-account-restricted [role="img"][aria-label="调度受限：冷却限制"]',
    )).toBeTruthy();
    expect(quickGroupRow("可恢复账号组").querySelector(".quick-account-restricted")).toBeTruthy();
    expect(quickGroupRow("不可用账号组").querySelector(
      '.quick-account-restricted [role="img"][aria-label="调度受限：暂不可调度"]',
    )).toBeTruthy();
    expect(quickGroupRow("历史账号组").querySelector(".quick-account-restricted")).toBeNull();

    const controls = restrictedGroup.querySelector(".quick-account-controls");
    expect(controls?.querySelector('button.quick-recover[aria-label="恢复状态：受限恢复账号"]')).toBeTruthy();
    expect(controls?.querySelector('[role="switch"][aria-label="受限恢复账号调度"]')).toBeTruthy();
    expect(quickGroupRow("不可用账号组").querySelector(".quick-recover")).toBeNull();
    expect(quickGroupRow("阻塞账号组").querySelector(".quick-recover")).toBeNull();
    expect(quickGroupRow("历史账号组").querySelector(".quick-recover")).toBeNull();

    await act(async () =>
      [...container.querySelectorAll<HTMLButtonElement>(".quick-tabs button")]
        .find((button) => button.textContent?.startsWith("异常"))!
        .click(),
    );
    const recoverError = quickErrorRow(recoverable.name);
    const openError = recoverError.querySelector<HTMLButtonElement>(".quick-error-open");
    const recoverButton = recoverError.querySelector<HTMLButtonElement>(
      'button.quick-recover[aria-label="恢复状态：受限恢复账号"]',
    );
    expect(openError?.tagName).toBe("BUTTON");
    expect(recoverButton).toBeTruthy();
    expect(openError?.parentElement).toBe(recoverButton?.parentElement);
    expect(openError?.contains(recoverButton ?? null)).toBe(false);
    expect(recoverError.querySelector(
      '.quick-account-restricted [role="img"][aria-label="调度受限：存在可恢复的状态限制"]',
    )).toBeTruthy();

    const historyError = quickErrorRow(history.name);
    expect(historyError.querySelector(".quick-account-restricted")).toBeNull();
    expect(historyError.querySelector(".quick-recover")).toBeNull();
  });

  it("uses the versioned account recovery action once and clears the restriction only after a new snapshot", async () => {
    const recoverable = quickAccount(73, "受限恢复账号", {
      available: false,
      recoverable: true,
      group_ids: [23],
      last_error_id: 880,
    });
    const currentError = { ...errorDetail, account_id: recoverable.id, account_name: recoverable.name, group_id: 23 };
    const initial = quickState(
      [recoverable],
      [quickGroup(23, "受限账号组", recoverable)],
      [currentError],
    );
    let finishRecovery!: (value: { verified: boolean }) => void;
    const pending = new Promise<{ verified: boolean }>((resolve) => { finishRecovery = resolve; });
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method === "POST" && path === "/accounts/73/recover-state") return pending as never;
      throw new Error(`Unexpected mocked API request: ${method} ${path}`);
    });
    const { deliver } = await renderQuickState(initial);

    const recoverButton = container.querySelector<HTMLButtonElement>(
      '.quick-groups .quick-account-controls button.quick-recover[aria-label="恢复状态：受限恢复账号"]',
    )!;
    expect(quickGroupRow("受限账号组").querySelector(".quick-account-restricted")).toBeTruthy();
    await act(async () => {
      recoverButton.click();
      recoverButton.click();
    });
    const recoveryCalls = vi.mocked(api).mock.calls.filter((call) =>
      call[0] === "POST" && call[1] === "/accounts/73/recover-state",
    );
    expect(recoveryCalls).toEqual([[
      "POST",
      "/accounts/73/recover-state",
      { expected_version: "account-73-version" },
    ]]);
    expect(recoverButton.disabled).toBe(true);
    expect(vi.mocked(api).mock.calls.filter(([, path]) =>
      path.endsWith("/schedulable") || path.endsWith("/degradation-mark"),
    )).toEqual([]);

    await act(async () => finishRecovery({ verified: true }));
    await act(async () => { await Promise.resolve(); await Promise.resolve(); });
    expect(command).toHaveBeenCalledWith("refresh");

    const restored = {
      ...recoverable,
      available: true,
      recoverable: false,
      blockers: [],
      version: "account-73-version-after-recovery",
    };
    await deliver(quickState(
      [restored],
      [quickGroup(23, "受限账号组", restored)],
      [currentError],
    ));
    expect(quickGroupRow("受限账号组").querySelector(".quick-account-restricted")).toBeNull();
    expect(quickGroupRow("受限账号组").querySelector(".quick-recover")).toBeNull();

    await act(async () =>
      [...container.querySelectorAll<HTMLButtonElement>(".quick-tabs button")]
        .find((button) => button.textContent?.startsWith("异常"))!
        .click(),
    );
    const refreshedError = quickErrorRow(restored.name);
    expect(refreshedError.querySelector(".quick-account-restricted")).toBeNull();
    expect(refreshedError.querySelector(".quick-recover")).toBeNull();
    expect(vi.mocked(api).mock.calls.filter(([, path]) =>
      path.endsWith("/schedulable") || path.endsWith("/degradation-mark"),
    )).toEqual([]);
  });

  it("disables recovery offline and ignores a pending response after switching connections", async () => {
    const recoverable = quickAccount(73, "受限恢复账号", {
      available: false,
      recoverable: true,
      group_ids: [23],
    });
    const offline = quickState(
      [recoverable],
      [quickGroup(23, "受限账号组", recoverable)],
      [],
      { online: false },
    );
    await renderQuickState(offline);
    const offlineButton = container.querySelector<HTMLButtonElement>(
      'button.quick-recover[aria-label="恢复状态：受限恢复账号"]',
    )!;
    expect(offlineButton.disabled).toBe(true);
    await act(async () => offlineButton.click());
    expect(api).not.toHaveBeenCalled();

    await act(async () => root.unmount());
    root = createRoot(container);
    const pendingState = quickState(
      [recoverable],
      [quickGroup(23, "受限账号组", recoverable)],
      [],
    );
    let finishRecovery!: (value: { verified: boolean }) => void;
    const pending = new Promise<{ verified: boolean }>((resolve) => { finishRecovery = resolve; });
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method === "POST" && path === "/accounts/73/recover-state") return pending as never;
      throw new Error(`Unexpected mocked API request: ${method} ${path}`);
    });
    const { deliver } = await renderQuickState(pendingState);
    const button = container.querySelector<HTMLButtonElement>(
      'button.quick-recover[aria-label="恢复状态：受限恢复账号"]',
    )!;
    await act(async () => button.click());
    const newConnection = quickState([], [], [], {
      preferences: { ...pendingState.preferences, base_url: "https://other-connection.invalid" },
      connection_revision: 1,
    });
    await deliver(newConnection);
    const refreshCountBeforeCompletion = vi.mocked(command).mock.calls
      .filter(([name]) => name === "refresh").length;
    await act(async () => finishRecovery({ verified: true }));
    await act(async () => { await Promise.resolve(); await Promise.resolve(); });

    expect(vi.mocked(command).mock.calls.filter(([name]) => name === "refresh"))
      .toHaveLength(refreshCountBeforeCompletion);
    expect(container.textContent).not.toContain("受限恢复账号：状态已恢复");
    expect(vi.mocked(api).mock.calls.filter((call) =>
      call[0] === "POST" && call[1] === "/accounts/73/recover-state",
    )).toHaveLength(1);
  });
});
