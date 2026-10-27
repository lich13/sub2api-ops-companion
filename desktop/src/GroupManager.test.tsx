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
      schedule={() => null}
      changed={vi.fn()}
      {...overrides}
    />,
  );
}

const accountIdentity = (name: string) =>
  [...container.querySelectorAll<HTMLElement>(".group-account-identity")].find(
    (node) => node.querySelector("strong")?.textContent === name,
  )!;

const draftApplyButton = () =>
  [...container.querySelectorAll<HTMLButtonElement>(".group-draft-toolbar button")].find(
    (node) => node.textContent?.includes("应用变更"),
  )!;

const menuTrigger = (name: string) =>
  accountIdentity(name).closest(".group-account")!.querySelector<HTMLButtonElement>(".account-more-trigger")!;

const menu = () => document.body.querySelector<HTMLElement>(".account-action-popup");

async function moveTo(name: string, destination: string, touch = false) {
  const target = container.querySelector<HTMLElement>(`.zone-${({ "左组": "left", "右组": "right", "两组共有": "both", "移出当前分组": "none" } as Record<string, string>)[destination]}`)!;
  const grip = accountIdentity(name).closest(".group-account")!.querySelector<HTMLElement>(".group-grip")!;
  const previous = document.elementFromPoint;
  document.elementFromPoint = () => target;
  Object.defineProperty(grip, "setPointerCapture", { value: vi.fn(), configurable: true });
  const pointer = (type: string, x: number) => {
    const event = new MouseEvent(type, { bubbles: true, button: 0, clientX: x, clientY: 10 });
    Object.defineProperties(event, { pointerId: { value: 1 }, pointerType: { value: touch ? "touch" : "mouse" } });
    grip.dispatchEvent(event);
  };
  try {
    await act(async () => pointer("pointerdown", 10));
    if (touch) await act(async () => { await vi.advanceTimersByTimeAsync(350); });
    await act(async () => pointer("pointermove", 30));
    await act(async () => pointer("pointerup", 30));
  } finally { document.elementFromPoint = previous; }
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
  vi.useRealTimers();
});

describe("group manager drafts", () => {
  it("drags accounts to the left, right, intersection, and unassigned destinations", async () => {
    const accounts = [
      account(1, "Alpha", [1]),
      account(2, "Beta", [2]),
      account(3, "Both", [1, 2]),
      account(4, "Pending", []),
    ];
    await act(async () => render(accounts));
    await moveTo("Alpha", "右组");
    await moveTo("Beta", "左组");
    await moveTo("Both", "两组共有");
    expect(container.textContent).toContain("2 个账号待应用");
    await moveTo("Both", "移出当前分组");
    await moveTo("Pending", "两组共有");

    expect(container.textContent).toContain("4 个账号待应用");
    expect([...container.querySelectorAll(".group-account.dirty strong")].map((node) => node.textContent).sort()).toEqual([
      "Alpha",
      "Beta",
      "Both",
      "Pending",
    ]);
    expect(api).not.toHaveBeenCalled();
  });

  it("does not render the removed independent group selector", async () => {
    await act(async () => render([account(1, "Alpha", [1]), account(2, "Beta", [2]), account(5, "Third", [3])]));

    expect(container.querySelector(".group-pair-selectors")).toBeNull();
    expect(container.textContent).not.toContain("左侧分组");
    expect(container.textContent).not.toContain("右侧分组");
  });

  it("keeps moves local until apply, submits one groups batch, and preserves failed drafts", async () => {
    const accounts = [
      account(1, "Alpha", [1]),
      account(2, "Beta", [2]),
      account(3, "Gamma", [1]),
    ];
    const batchId = "f".repeat(32);
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method === "POST" && path === "/account-operations/batch") {
        return {
          batch_id: batchId,
          pending: 0,
          items: [
            {
              id: "1".repeat(32), batch_id: batchId, account_id: 1, account_name: "Alpha", action: "groups",
              status: "completed", reason: "", requested: { scope_group_ids: [1, 2], group_ids: [2] }, result: { verified: true },
            },
            {
              id: "2".repeat(32), batch_id: batchId, account_id: 2, account_name: "Beta", action: "groups",
              status: "failed", reason: "第二个账号写入失败", requested: { scope_group_ids: [1, 2], group_ids: [1] },
            },
          ],
        } as never;
      }
      throw new Error(`Unexpected request: ${method} ${path}`);
    });

    await act(async () => render(accounts));
    await moveTo("Alpha", "右组");
    await moveTo("Beta", "左组");

    expect(vi.mocked(api).mock.calls).toEqual([]);
    expect(container.textContent).toContain("2 个账号待应用");

    await act(async () => draftApplyButton().click());
    expect(api).toHaveBeenCalledExactlyOnceWith("POST", "/account-operations/batch", {
      action: "groups",
      payload: {},
      accounts: [
        { account_id: 1, expected_version: "version-1", payload: { scope_group_ids: [1, 2], group_ids: [2] } },
        { account_id: 2, expected_version: "version-2", payload: { scope_group_ids: [1, 2], group_ids: [1] } },
      ],
      request_id: expect.any(String),
      client_id: expect.any(String),
    });
    expect(container.textContent).toContain("已保存 1 个账号");
    expect(container.textContent).toContain("第二个账号写入失败");
    expect(container.textContent).toContain("1 个账号待应用");
    expect(command).toHaveBeenCalledWith("refresh");
  });

  it("supports touch dragging, undo, discard, and preserves drafts across platforms", async () => {
    const accounts = [account(1, "Alpha", [1]), account(2, "Grok", [3], "grok")];
    await act(async () => render(accounts));
    vi.useFakeTimers();
    await moveTo("Alpha", "右组", true);
    expect(container.textContent).toContain("1 个账号待应用");
    expect(accountIdentity("Alpha").closest(".group-account")?.className).toContain("dirty");

    await act(async () =>
      [...container.querySelectorAll<HTMLButtonElement>(".group-draft-toolbar button")]
        .find((node) => node.textContent?.includes("撤销"))!
        .click(),
    );
    expect(container.textContent).not.toContain("个账号待应用");

    await moveTo("Alpha", "右组");
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
    await moveTo("Alpha", "右组");

    await act(async () => render([{ ...initial, version: "version-changed" }], { active: false }));
    expect(container.querySelector(".group-account")).toBeNull();
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

  it("does not move accounts when clicking names, zones, or another account's portal menu", async () => {
    const accounts = [account(1, "Alpha", [1]), account(2, "Beta", [2])];
    const run = vi.fn();
    const accountActions = vi.fn((value: Account) => [
      { id: "test", label: "测试连接", run: () => run(value.id) },
    ]);
    await act(async () => render(accounts, { accountActions }));
    await act(async () => accountIdentity("Alpha").click());
    await act(async () => container.querySelector<HTMLElement>(".zone-right")!.click());
    await act(async () => menuTrigger("Beta").click());

    expect(menu()?.parentElement).toBe(document.body);
    expect(accountIdentity("Alpha").tagName).toBe("DIV");
    expect(accountIdentity("Alpha").hasAttribute("aria-pressed")).toBe(false);
    expect(container.querySelectorAll(".group-account.dirty")).toHaveLength(0);

    const option = menu()!.querySelector<HTMLButtonElement>("button")!;
    await act(async () => option.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true })));
    await act(async () => option.dispatchEvent(new KeyboardEvent("keydown", { key: " ", bubbles: true })));
    await act(async () => option.click());

    expect(run).toHaveBeenCalledExactlyOnceWith(2);
    expect(menu()).toBeNull();
    expect(accountIdentity("Alpha").tagName).toBe("DIV");
    expect(accountIdentity("Alpha").hasAttribute("aria-pressed")).toBe(false);
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

  it("keeps a draft through hiding and accepts an apply result while hidden", async () => {
    const accounts = [account(1, "Alpha", [1])], changed = vi.fn();
    await act(async () => render(accounts, { changed }));
    await moveTo("Alpha", "右组");
    await act(async () => render(accounts, { changed, active: false }));
    expect(container.querySelector(".group-account")).toBeNull();
    expect(changed).toHaveBeenLastCalledWith(1, false);
    await act(async () => render(accounts, { changed }));
    expect(container.querySelector(".zone-right .group-account.dirty strong")?.textContent).toBe("Alpha");
    let finish!: (value: never) => void;
    vi.mocked(api).mockReturnValue(new Promise(resolve => { finish = resolve; }));
    await act(async () => draftApplyButton().click());
    await act(async () => render(accounts, { changed, active: false }));
    await act(async () => finish({ batch_id: "a".repeat(32), pending: 0, items: [{ id: "b".repeat(32), account_id: 1, action: "groups", status: "completed", requested: { scope_group_ids: [1, 2], group_ids: [2] }, result: { verified: true } }] } as never));
    expect(changed).toHaveBeenLastCalledWith(0, false);
    await act(async () => render([{ ...accounts[0], group_ids: [2] }], { changed }));
    expect(container.textContent).not.toContain("个账号待应用");
    expect(container.querySelector(".zone-right .group-account strong")?.textContent).toBe("Alpha");
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
    vi.mocked(api).mockResolvedValue({
      batch_id: "g".repeat(32), pending: 0,
      items: [{
        id: "3".repeat(32), batch_id: "g".repeat(32), account_id: 2, account_name: "Beta", action: "groups",
        status: "completed", reason: "", requested: { scope_group_ids: [1, 2], group_ids: [1] }, result: { verified: true },
      }],
    } as never);
    await act(async () => draftApplyButton().click());

    expect(api).toHaveBeenCalledExactlyOnceWith("POST", "/account-operations/batch", {
      action: "groups",
      payload: {},
      accounts: [{
        account_id: 2,
        expected_version: "version-2",
        payload: { scope_group_ids: [1, 2], group_ids: [1] },
      }],
      request_id: expect.any(String),
      client_id: expect.any(String),
    });
    expect(container.textContent).not.toContain("个账号待应用");
  });
});

it("offers every actual platform and preserves account isolation", async () => {
  await act(async () => render([account(21, "Claude fixture", [21], "anthropic"), account(31, "Gemini fixture", [31], "gemini"), account(41, "Custom fixture", [], "custom")], { groups: [group(21, "Claude 分组", "anthropic"), group(31, "Gemini 分组", "gemini")] }));
  expect([...container.querySelectorAll('[role="tab"]')].map(tab => tab.textContent)).toEqual(["Claude", "Gemini", "custom"]);
  expect(container.textContent).toContain("Claude fixture");
  expect(container.textContent).not.toContain("Gemini fixture");
  await act(async () => [...container.querySelectorAll<HTMLButtonElement>('[role="tab"]')].find(tab => tab.textContent === "Gemini")!.click());
  expect(container.textContent).toContain("Gemini fixture");
  expect(container.textContent).not.toContain("Claude fixture");
});
