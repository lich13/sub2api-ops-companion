// @vitest-environment jsdom
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { AccountActionItem } from "./AccountActions";
import {
  accountRestriction,
  QuickRecover,
  QuickRestriction,
} from "./QuickAccountStatus";
import type { Account } from "./types";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

const account = (overrides: Partial<Account> = {}): Account => ({
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
  recoverable: false,
  version: "fixture-version",
  last_success_at: null,
  last_error_at: null,
  last_error_id: null,
  last_error_code: null,
  last_error_status: null,
  error_message: "",
  success_after_error: false,
  usage_windows: [],
  ...overrides,
});

let container: HTMLDivElement;
let root: Root;

async function render(element: ReactNode) {
  await act(async () => root.render(element));
}

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

describe("quick account status", () => {
  const restrictions: [string, Partial<Account>, string][] = [
    ["available without a restriction", { available: true, blockers: [], recoverable: false }, ""],
    ["current blocker labels", { available: true, blockers: [{ code: "cooldown", label: "上游限流" }], recoverable: true }, "上游限流"],
    ["recoverable state without a blocker", { available: false, blockers: [], recoverable: true }, "存在可恢复的状态限制"],
    ["unavailable state without a blocker", { available: false, blockers: [], recoverable: false }, "暂不可调度"],
  ];
  it.each(restrictions)("uses %s to describe the current restriction", (_label, overrides, reason) => {
    const current = account(overrides);
    expect(accountRestriction(current)).toBe(reason);
    const icon = document.createElement("div");
    const iconRoot = createRoot(icon);
    act(() => iconRoot.render(<QuickRestriction reason={accountRestriction(current)} />));
    expect(icon.querySelector('[role="img"]')?.getAttribute("aria-label") ?? "").toBe(
      reason ? `调度受限：${reason}` : "",
    );
    act(() => iconRoot.unmount());
  });

  it("renders no recovery control when there is no action", async () => {
    await render(<QuickRecover account={account()} />);
    expect(container.querySelector("button")).toBeNull();
  });

  it("exposes pending state, honors disabled actions, and invokes an enabled action", async () => {
    const blockedRun = vi.fn();
    const blocked: AccountActionItem = {
      id: "recover",
      label: "恢复状态",
      disabled: true,
      pending: true,
      run: blockedRun,
    };
    await render(<QuickRecover account={account()} action={blocked} />);

    const busyButton = container.querySelector<HTMLButtonElement>("button")!;
    expect(busyButton.disabled).toBe(true);
    expect(busyButton.getAttribute("aria-busy")).toBe("true");
    await act(async () => busyButton.click());
    expect(blockedRun).not.toHaveBeenCalled();

    const run = vi.fn();
    await render(
      <QuickRecover
        account={account()}
        action={{ id: "recover", label: "恢复状态", run }}
      />,
    );
    const enabledButton = container.querySelector<HTMLButtonElement>("button")!;
    await act(async () => enabledButton.click());
    expect(run).toHaveBeenCalledOnce();
  });
});
