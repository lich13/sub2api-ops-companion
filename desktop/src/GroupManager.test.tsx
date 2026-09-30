// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import GroupManager from "./GroupManager";
import { api, command } from "./bridge";
import type { Account, Group } from "./types";

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

const account = (
  id: number,
  name: string,
  groupIds: number[],
  platform = "openai",
  type = "apikey",
): Account => ({
  id,
  name,
  priority: id,
  platform,
  type,
  status: "active",
  schedulable: true,
  available: true,
  group_ids: groupIds,
  blockers: [],
  managed: false,
  version: `version-${id}`,
  last_success_at: null,
  last_error_at: null,
  last_error_id: null,
  last_error_code: null,
  last_error_status: null,
  error_message: "",
  success_after_error: false,
  usage_windows: [],
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

const groups = [
  group(1, "左组"),
  group(2, "右组"),
  group(3, "Grok 左", "grok"),
  group(4, "Grok 右", "grok"),
];

function render(accounts: Account[], overrides: Partial<React.ComponentProps<typeof GroupManager>> = {}) {
  return root.render(
    <GroupManager
      accounts={accounts}
      groups={groups}
      active
      mobile={false}
      online
      connectionKey="fixture"
      back={vi.fn()}
      report={vi.fn()}
      changed={vi.fn()}
      {...overrides}
    />,
  );
}

const accountButton = (name: string) =>
  [...container.querySelectorAll<HTMLButtonElement>(".group-account-select")].find(
    (node) => node.querySelector("strong")?.textContent === name,
  )!;

const draftApplyButton = () =>
  [...container.querySelectorAll<HTMLButtonElement>(".group-draft-toolbar button")].find(
    (node) => node.textContent?.includes("应用变更"),
  )!;

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(command).mockResolvedValue(undefined as never);
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
});

describe("group manager drafts", () => {
  it("routes point selections to the left, right, intersection, and unassigned destinations", async () => {
    const accounts = [
      account(1, "Alpha", [1]),
      account(2, "Beta", [2]),
      account(3, "Both", [1, 2]),
      account(4, "Pending", []),
    ];
    await act(async () => render(accounts));
    const choose = async (name: string) => {
      await act(async () => accountButton(name).click());
    };
    const destination = async (name: string) => {
      await act(async () =>
        [...container.querySelectorAll<HTMLButtonElement>(".group-destination-bar button")]
          .find((node) => node.textContent === name)!
          .click(),
      );
    };

    await choose("Alpha");
    await destination("右组");
    await choose("Beta");
    await destination("左组");
    await choose("Both");
    await destination("两组共有");
    expect(container.textContent).toContain("2 个账号待应用");
    await choose("Both");
    await destination("移出当前分组");
    await choose("Pending");
    await destination("两组共有");

    expect(container.textContent).toContain("4 个账号待应用");
    expect([...container.querySelectorAll(".group-account.dirty strong")].map((node) => node.textContent).sort()).toEqual([
      "Alpha",
      "Beta",
      "Both",
      "Pending",
    ]);
    expect(api).not.toHaveBeenCalled();
  });

  it("keeps moves local until apply, submits each account, and stops after the first failure", async () => {
    const accounts = [
      account(1, "Alpha", [1]),
      account(2, "Beta", [2]),
      account(3, "Gamma", [1]),
    ];
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method === "PUT" && path === "/accounts/1/groups") return { verified: true } as never;
      if (method === "PUT" && path === "/accounts/2/groups") throw new Error("第二个账号写入失败");
      throw new Error(`Unexpected request: ${method} ${path}`);
    });

    await act(async () => render(accounts));
    await act(async () => accountButton("Alpha").click());
    await act(async () =>
      [...container.querySelectorAll<HTMLButtonElement>(".group-destination-bar button")]
        .find((node) => node.textContent === "右组")!
        .click(),
    );
    await act(async () => accountButton("Beta").click());
    await act(async () =>
      [...container.querySelectorAll<HTMLButtonElement>(".group-destination-bar button")]
        .find((node) => node.textContent === "左组")!
        .click(),
    );

    expect(vi.mocked(api).mock.calls).toEqual([]);
    expect(container.textContent).toContain("2 个账号待应用");

    await act(async () => draftApplyButton().click());
    expect(vi.mocked(api).mock.calls).toEqual([
      [
        "PUT",
        "/accounts/1/groups",
        {
          expected_version: "version-1",
          scope_group_ids: [1, 2],
          group_ids: [2],
        },
      ],
      [
        "PUT",
        "/accounts/2/groups",
        {
          expected_version: "version-2",
          scope_group_ids: [1, 2],
          group_ids: [1],
        },
      ],
    ]);
    expect(container.textContent).toContain("已保存 1 个账号");
    expect(container.textContent).toContain("第二个账号写入失败");
    expect(container.textContent).toContain("1 个账号待应用");
    expect(command).toHaveBeenCalledWith("refresh");
  });

  it("supports keyboard destinations, undo, discard, and preserves drafts across platforms", async () => {
    const accounts = [account(1, "Alpha", [1]), account(2, "Grok", [3], "grok")];
    await act(async () => render(accounts));
    await act(async () => accountButton("Alpha").click());

    const rightZone = container.querySelector<HTMLElement>(".zone-right")!;
    expect(rightZone.tabIndex).toBe(0);
    await act(async () => rightZone.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true })));
    expect(container.textContent).toContain("1 个账号待应用");
    expect(accountButton("Alpha").closest(".group-account")?.className).toContain("dirty");

    await act(async () =>
      [...container.querySelectorAll<HTMLButtonElement>(".group-draft-toolbar button")]
        .find((node) => node.textContent?.includes("撤销"))!
        .click(),
    );
    expect(container.textContent).not.toContain("个账号待应用");

    await act(async () => accountButton("Alpha").click());
    await act(async () =>
      [...container.querySelectorAll<HTMLButtonElement>(".group-destination-bar button")]
        .find((node) => node.textContent === "右组")!
        .click(),
    );
    await act(async () =>
      [...container.querySelectorAll<HTMLButtonElement>("[role=tab]")]
        .find((node) => node.textContent === "Grok")!
        .click(),
    );
    expect(container.textContent).toContain("1 个账号待应用");
    expect(container.textContent).toContain("Grok");
    await act(async () =>
      [...container.querySelectorAll<HTMLButtonElement>(".group-draft-toolbar button")]
        .find((node) => node.textContent === "放弃全部")!
        .click(),
    );
    expect(container.textContent).not.toContain("个账号待应用");
  });

  it("blocks applying a stale draft and allows dropping the conflict explicitly", async () => {
    const initial = account(1, "Alpha", [1]);
    await act(async () => render([initial]));
    await act(async () => accountButton("Alpha").click());
    await act(async () =>
      [...container.querySelectorAll<HTMLButtonElement>(".group-destination-bar button")]
        .find((node) => node.textContent === "右组")!
        .click(),
    );

    await act(async () => render([{ ...initial, version: "version-changed" }]));
    expect(container.textContent).toContain("冲突");
    expect(draftApplyButton().disabled).toBe(true);
    await act(async () =>
      [...container.querySelectorAll<HTMLButtonElement>(".group-draft-toolbar button")]
        .find((node) => node.textContent?.includes("1 个账号待应用"))!
        .click(),
    );
    await act(async () =>
      [...container.querySelectorAll<HTMLButtonElement>(".group-draft-list button")]
        .find((node) => node.textContent === "放弃冲突项")!
        .click(),
    );
    expect(container.textContent).not.toContain("个账号待应用");
    expect(vi.mocked(api)).not.toHaveBeenCalled();
  });
});
