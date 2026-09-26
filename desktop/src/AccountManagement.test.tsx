// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { api, command, subscribe } from "./bridge";
import { DeleteAccountsDialog, RecoverStateButton } from "./AccountManagement";
import { RecoveryHistory } from "./AccountControls";
import App from "./main";
import type { Account, Recovery, ViewState } from "./types";

vi.mock("./bridge", () => ({
  api: vi.fn(),
  command: vi.fn(),
  subscribe: vi.fn(),
  updates: vi.fn(async () => () => {}),
  preview: true,
}));
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
let container: HTMLDivElement, root: Root;
beforeEach(() => {
  vi.clearAllMocks();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});
afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
});
const button = (text: string) =>
  [...container.querySelectorAll("button")].find(
    (b) => b.textContent === text,
  )!;
const account = (id: number): Account => ({
  id,
  name: `Account ${id}`,
  priority: id,
  platform: id > 3 ? "grok" : "openai",
  type: "apikey",
  status: "active",
  schedulable: false,
  available: false,
  blockers: [],
  managed: id === 1,
  version: "a".repeat(64),
  group_ids: [],
  last_success_at: null,
  last_error_at: null,
  last_error_id: null,
  last_error_code: null,
  last_error_status: null,
  error_message: "",
  success_after_error: false,
  usage_windows: [],
});

it("confirms names and IDs, bounds deletion to three, and keeps individual failures without retry", async () => {
  const pending: {
    resolve: (value: unknown) => void;
    reject: (error: Error) => void;
  }[] = [];
  let active = 0,
    maximum = 0;
  vi.mocked(api).mockImplementation(async () => {
    active++;
    maximum = Math.max(maximum, active);
    try {
      return await new Promise((resolve, reject) =>
        pending.push({ resolve, reject }),
      );
    } finally {
      active--;
    }
  });
  const removed = vi.fn(),
    finished = vi.fn();
  await act(async () =>
    root.render(
      <DeleteAccountsDialog
        accounts={[1, 2, 3, 4, 5].map(account)}
        online
        removed={removed}
        finished={finished}
        close={vi.fn()}
      />,
    ),
  );
  expect(container.textContent).toContain("删除 5 个账号");
  expect(container.textContent).toContain("#1 · 回退托管");
  expect(api).not.toHaveBeenCalled();
  await act(async () => {
    button("确认删除").click();
    button("确认删除")?.click();
  });
  expect(api).toHaveBeenCalledTimes(3);
  expect(vi.mocked(api).mock.calls[0]).toEqual([
    "DELETE",
    "/accounts/1",
    { expected_version: "a".repeat(64), detach_managed: true },
  ]);
  await act(async () => {
    pending[0].resolve({ deleted: true, verified: true, detached: true });
    pending[1].reject(new Error("已解除托管；删除未确认"));
  });
  expect(api).toHaveBeenCalledTimes(5);
  await act(async () => {
    for (const p of pending.slice(2))
      p.resolve({ deleted: true, verified: true, detached: false });
  });
  expect(maximum).toBe(3);
  expect(removed.mock.calls.flat()).toEqual([1, 3, 4, 5]);
  expect(
    finished.mock.calls[0][0]
      .filter((r: { status: string }) => r.status === "failed")
      .map((r: { id: number }) => r.id),
  ).toEqual([2]);
  expect(container.textContent).toContain("4 个已删除，1 个失败");
  expect(container.textContent).toContain("已解除托管；删除未确认");
  expect(api).toHaveBeenCalledTimes(5);
});

it("stops undispatched deletions when the connection's dialog is removed", async () => {
  const resolvers: ((value: unknown) => void)[] = [];
  vi.mocked(api).mockImplementation(
    () => new Promise((resolve) => resolvers.push(resolve)),
  );
  await act(async () =>
    root.render(
      <DeleteAccountsDialog
        accounts={[1, 2, 3, 4].map(account)}
        online
        removed={vi.fn()}
        finished={vi.fn()}
        close={vi.fn()}
      />,
    ),
  );
  await act(async () => button("确认删除").click());
  await act(async () => root.render(<div />));
  await act(async () =>
    resolvers.forEach((resolve) => resolve({ deleted: true, verified: true })),
  );
  expect(api).toHaveBeenCalledTimes(3);
});

it("restores directly once, without quota or test requests", async () => {
  let resolve: (value: unknown) => void = () => {};
  vi.mocked(api).mockImplementation(
    () =>
      new Promise((r) => {
        resolve = r;
      }),
  );
  await act(async () =>
    root.render(
      <RecoverStateButton
        account={{ ...account(2), recoverable: true }}
        online
        report={vi.fn()}
      />,
    ),
  );
  await act(async () => {
    button("恢复状态").click();
    button("恢复状态").click();
  });
  expect(api).toHaveBeenCalledExactlyOnceWith(
    "POST",
    "/accounts/2/recover-state",
    { expected_version: "a".repeat(64) },
  );
  await act(async () => resolve({ verified: true }));
});

it("filters already loaded recoveries again after deletion and a late page response", async () => {
  const records = [1, 2, 3].map(
    (id): Recovery => ({
      id,
      account_id: id,
      account_name: `History ${id}`,
      model_id: "model",
      test_completed_at: null,
      recovered_at: null,
      legacy: true,
    }),
  );
  vi.mocked(api).mockResolvedValue({ items: records.slice(1), next_cursor: 2 });
  const render = (ids: number[]) =>
    root.render(
      <RecoveryHistory
        accounts={ids.map(account)}
        latest={[records[2]]}
        online
        report={vi.fn()}
      />,
    );
  await act(async () => render([1, 2, 3]));
  expect(container.textContent).toContain("History 3");
  await act(async () => render([1, 2]));
  expect(container.textContent).not.toContain("History 3");
  vi.mocked(api).mockResolvedValue({
    items: [records[0], records[2]],
    next_cursor: null,
  });
  await act(async () => button("加载更早恢复记录").click());
  expect(container.textContent).toContain("History 1");
  expect(container.textContent).not.toContain("History 3");
});

it("selects current filters, preserves live selections on polling and clears on filter or connection changes", async () => {
  let receive: (state: ViewState) => void = () => {};
  const state: ViewState = {
    connected: true,
    online: true,
    error: "",
    preferences: {
      base_url: "https://qa.invalid",
      favorites: [],
      pinned: false,
      launch_at_login: false,
    },
    snapshot: {
      observed_at: "2026-09-26T00:00:00Z",
      accounts: [1, 2, 4].map(account),
      groups: [],
      errors: [],
      recoveries: [],
    },
  };
  vi.mocked(subscribe).mockImplementation(async (cb) => {
    receive = cb;
    return () => {};
  });
  vi.mocked(command).mockResolvedValue(state);
  vi.mocked(api).mockResolvedValue({
    status: "idle",
    items: [],
    next_cursor: null,
  });
  await act(async () => root.render(<App />));
  await act(async () =>
    (
      container.querySelector(
        '[aria-label="全选当前筛选账号"]',
      ) as HTMLInputElement
    ).click(),
  );
  expect(container.textContent).toContain("已选 3 个账号");
  await act(async () =>
    receive({
      ...state,
      snapshot: { ...state.snapshot!, accounts: [1, 4].map(account) },
    }),
  );
  expect(container.textContent).toContain("已选 2 个账号");
  const filter = container.querySelector(
    '[aria-label="平台筛选"]',
  ) as HTMLSelectElement;
  await act(async () => {
    filter.value = "grok";
    filter.dispatchEvent(new Event("change", { bubbles: true }));
  });
  expect(container.textContent).toContain("已选 0 个账号");
  await act(async () =>
    (
      container.querySelector(
        '[aria-label="全选当前筛选账号"]',
      ) as HTMLInputElement
    ).click(),
  );
  expect(container.textContent).toContain("已选 1 个账号");
  await act(async () => receive({ ...state, connection_revision: 1 }));
  expect(container.textContent).toContain("已选 0 个账号");
  await act(async () => button("事件").click());
  expect(container.querySelector("#errors-panel")?.hasAttribute("hidden")).toBe(
    false,
  );
  const errors = container.querySelector("#errors-panel") as HTMLElement;
  errors.scrollTop = 50;
  await act(async () => button("恢复成功").click());
  expect(container.querySelector("#errors-panel")?.hasAttribute("hidden")).toBe(
    true,
  );
  await act(async () => button("上游与认证错误").click());
  expect(errors.scrollTop).toBe(50);
});
