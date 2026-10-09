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
      accountActions={() => [{ id: "test", label: "测试连接", run: vi.fn() }]}
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

const menuTrigger = (name: string) =>
  accountButton(name).closest(".group-account")!.querySelector<HTMLButtonElement>(".account-more-trigger")!;

const menu = () => document.body.querySelector<HTMLElement>(".account-action-popup");

async function moveTo(name: string, destination: string) {
  await act(async () => accountButton(name).click());
  await act(async () =>
    [...container.querySelectorAll<HTMLButtonElement>(".group-destination-bar button")]
      .find((node) => node.textContent === destination)!
      .click(),
  );
}

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

  it("shows the compact slow first-token warning only for an active account", async () => {
    const flagged = {
      ...account(1, "慢账号", [1]),
      quality: {
        score: 55,
        grade: "yellow" as const,
        reasons: ["首字偏慢"],
        sample_status: "complete" as const,
        data_status: "fresh" as const,
        computed_at: null,
        warnings: [{ kind: "slow_ttft" as const, sample_count: 10, slow_count: 8, threshold_ms: 10000, active: true }],
      },
    };
    await act(async () => render([flagged, account(2, "快账号", [1])]));
    expect(container.querySelector('[aria-label="首字慢"]')).toBeTruthy();
  });

  it("keeps an existing selection intact when opening and using another account's portal menu", async () => {
    const accounts = [account(1, "Alpha", [1]), account(2, "Beta", [2])];
    const run = vi.fn();
    const accountActions = vi.fn((value: Account) => [
      { id: "test", label: "测试连接", run: () => run(value.id) },
    ]);
    await act(async () => render(accounts, { accountActions }));
    await act(async () => accountButton("Alpha").click());
    await act(async () => menuTrigger("Beta").click());

    expect(menu()?.parentElement).toBe(document.body);
    expect(accountButton("Alpha").getAttribute("aria-pressed")).toBe("true");
    expect(container.querySelectorAll(".group-account.dirty")).toHaveLength(0);

    const option = menu()!.querySelector<HTMLButtonElement>("button")!;
    await act(async () => option.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true })));
    await act(async () => option.dispatchEvent(new KeyboardEvent("keydown", { key: " ", bubbles: true })));
    await act(async () => option.click());

    expect(run).toHaveBeenCalledExactlyOnceWith(2);
    expect(menu()).toBeNull();
    expect(accountButton("Alpha").getAttribute("aria-pressed")).toBe("true");
    expect(container.querySelectorAll(".group-account.dirty")).toHaveLength(0);
    expect(api).not.toHaveBeenCalled();
  });

  it("closes its menu across platform changes and page deactivation", async () => {
    const accounts = [account(1, "Alpha", [1]), account(2, "Grok", [3], "grok")];
    await act(async () => render(accounts));
    await act(async () => menuTrigger("Alpha").click());
    expect(menu()).not.toBeNull();
    await act(async () =>
      [...container.querySelectorAll<HTMLButtonElement>('[role="tab"]')]
        .find((node) => node.textContent === "Grok")!.click(),
    );
    expect(menu()).toBeNull();

    await act(async () => menuTrigger("Grok").click());
    expect(menu()).not.toBeNull();
    await act(async () => render(accounts, { active: false }));
    expect(menu()).toBeNull();
    await act(async () => render(accounts));
    expect(menu()).toBeNull();
  });

  it("drops only a deleted account's draft and never restores it through undo", async () => {
    const alpha = account(1, "Alpha", [1]);
    const beta = account(2, "Beta", [2]);
    await act(async () => render([alpha, beta]));
    await moveTo("Alpha", "右组");
    await moveTo("Beta", "左组");
    expect(container.textContent).toContain("2 个账号待应用");

    await act(async () => render([beta]));

    expect(container.textContent).toContain("1 个账号待应用");
    expect(container.textContent).not.toContain("冲突");
    expect(draftApplyButton().disabled).toBe(false);
    expect([...container.querySelectorAll(".group-account.dirty strong")].map((node) => node.textContent)).toEqual(["Beta"]);
    const undo = [...container.querySelectorAll<HTMLButtonElement>(".group-draft-toolbar button")]
      .find((node) => node.textContent?.includes("撤销"))!;
    if (!undo.disabled) {
      await act(async () => undo.click());
      expect(container.textContent).not.toContain("个账号待应用");
      expect(container.textContent).not.toContain("Alpha");
      await moveTo("Beta", "左组");
    }
    vi.mocked(api).mockResolvedValue({ verified: true } as never);
    await act(async () => draftApplyButton().click());

    expect(api).toHaveBeenCalledExactlyOnceWith("PUT", "/accounts/2/groups", {
      expected_version: "version-2",
      scope_group_ids: [1, 2],
      group_ids: [1],
    });
    expect(container.textContent).not.toContain("个账号待应用");
  });
});
