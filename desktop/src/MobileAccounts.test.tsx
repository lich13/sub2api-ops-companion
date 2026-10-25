// @vitest-environment jsdom
import { act, type ComponentProps } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { AccountActionItem } from "./AccountActions";
import MobileAccounts from "./MobileAccounts";
import type { Account } from "./types";

vi.mock("./bridge", () => ({ api: vi.fn(), command: vi.fn() }));
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

const account: Account = {
  id: 1,
  name: "Fixture account",
  priority: 50,
  platform: "openai",
  type: "oauth",
  status: "active",
  schedulable: true,
  available: true,
  group_ids: [],
  blockers: [],
  recoverable: true,
  version: "fixture-version",
  last_success_at: null,
  last_error_at: null,
  last_error_id: null,
  last_error_code: null,
  last_error_status: null,
  error_message: "",
  success_after_error: false,
  usage_windows: [],
};

let container: HTMLDivElement;
let root: Root;

const props = (overrides: Partial<ComponentProps<typeof MobileAccounts>> = {}) => ({
  accounts: [account],
  online: true,
  selected: new Set<number>(),
  select: vi.fn(),
  schedule: () => null,
  error: vi.fn(),
  report: vi.fn(),
  accountActions: () => [] as AccountActionItem[],
  ...overrides,
});

async function render(nextProps: ComponentProps<typeof MobileAccounts>) {
  await act(async () => root.render(<MobileAccounts {...nextProps} />));
}

async function click(label: string) {
  const button = [...container.querySelectorAll<HTMLButtonElement>("button")].find(
    (node) => node.getAttribute("aria-label") === label || node.textContent?.trim() === label,
  );
  if (!button) throw new Error(`Missing button: ${label}`);
  await act(async () => button.click());
}

const actionSheet = () => container.querySelector('[role="dialog"][aria-label="账号操作"]');

beforeEach(() => {
  vi.resetAllMocks();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
});

describe("mobile accounts", () => {
  it.each([
    ["success after error", "2026-10-01T04:05:06Z", 10, true, "10-01 12:05:06"],
    ["success before error", "2026-10-01T02:00:00Z", 10, false, "10-01 10:00:00"],
    ["success at error time", "2026-10-01T03:00:00Z", 10, false, "10-01 11:00:00"],
    ["no error", "2026-10-01T04:05:06Z", null, false, "10-01 12:05:06"],
    ["no successful call", null, 10, false, "暂无记录"],
    ["empty history", null, null, false, "暂无记录"],
    ["invalid success time", "invalid", 10, false, "时间未知"],
  ] as const)("shows the last successful call for %s", async (_label, success, errorId, recovered, expected) => {
    const openError = vi.fn();
    await render(props({ accounts: [{ ...account, last_success_at: success, last_error_at: errorId ? "2026-10-01T03:00:00Z" : null, last_error_id: errorId, last_error_status: errorId ? 502 : null, success_after_error: recovered }], error: openError }));

    const summary = container.querySelector<HTMLElement>(".account-last-success")!;
    expect(summary.textContent).toBe(`最后成功${expected}`);
    expect(summary.querySelector("time")!.classList.contains("good-text")).toBe(recovered);
    const errorButton = container.querySelector<HTMLButtonElement>(".mobile-error");
    expect(!!errorButton).toBe(errorId !== null);
    expect(errorButton?.textContent?.includes("之后已有成功调用") ?? false).toBe(recovered);
    if (errorButton) {
      await act(async () => errorButton.click());
      expect(openError).toHaveBeenCalledExactlyOnceWith(errorId);
    }
  });

  it("disables account selection while offline", async () => {
    const select = vi.fn();
    await render(props({ online: false, select }));

    const checkbox = container.querySelector<HTMLInputElement>('input[type="checkbox"]')!;
    expect(checkbox.disabled).toBe(true);
    await act(async () => checkbox.click());
    expect(select).not.toHaveBeenCalled();
  });

  it("keeps the footer priority value read-only", async () => {
    await render(props());
    const footer = container.querySelector<HTMLElement>(".mobile-account footer")!;
    expect(footer.textContent).toContain("优先级 50");
    expect([...footer.querySelectorAll("button")].some((button) => button.textContent?.includes("优先级"))).toBe(false);
    expect(actionSheet()).toBeNull();
  });

  it("closes the action sheet after an operation and when its account disappears", async () => {
    const run = vi.fn();
    const action: AccountActionItem = {
      id: "recover",
      label: "恢复状态",
      run,
    };
    const accountActions = vi.fn(() => [action]);
    const current = props({ accountActions });
    await render(current);
    await click("Fixture account操作");
    expect(actionSheet()).not.toBeNull();

    await click("恢复状态");
    expect(run).toHaveBeenCalledOnce();
    expect(actionSheet()).toBeNull();

    await click("Fixture account操作");
    expect(actionSheet()).not.toBeNull();
    await render({ ...current, accounts: [] });
    expect(actionSheet()).toBeNull();
  });
});
