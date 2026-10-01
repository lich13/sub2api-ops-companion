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
});
