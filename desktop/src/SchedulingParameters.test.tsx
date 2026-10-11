// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import SchedulingParameters from "./SchedulingParameters";
import { command } from "./bridge";
import { followOperation, operationEpoch, submitOperation, type Operation } from "./accountOperations";
import type { Account } from "./types";

vi.mock("./bridge", () => ({ command: vi.fn() }));
vi.mock("./mobile", () => ({ useBackAction: vi.fn() }));
vi.mock("./accountOperations", () => ({
  followOperation: vi.fn(),
  operationEpoch: vi.fn(),
  submitOperation: vi.fn(),
}));

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

const account = (overrides: Partial<Account> = {}) => ({
  id: 101,
  name: "Fixture account",
  priority: 12,
  concurrency: 4,
  load_factor: 80,
  platform: "openai",
  type: "oauth",
  status: "active",
  schedulable: true,
  available: true,
  group_ids: [],
  blockers: [],
  managed: false,
  recoverable: true,
  version: "account-version",
  operation_versions: {
    priority: "priority-version",
    concurrency: "concurrency-version",
    load_factor: "load-factor-version",
  },
  last_success_at: null,
  last_error_at: null,
  last_error_id: null,
  last_error_code: null,
  last_error_status: null,
  error_message: "",
  success_after_error: false,
  usage_windows: [],
  ...overrides,
} as Account);

const job = (action: string, status = "queued"): Operation => ({
  id: `job-${action}`,
  account_id: 101,
  account_name: "Fixture account",
  action,
  status,
  requested: {},
});

let container: HTMLDivElement;
let root: Root;

const click = async (element: Element) => {
  await act(async () => (element as HTMLElement).click());
};

const change = async (element: HTMLInputElement, value: string) => {
  await act(async () => {
    const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
    setter?.call(element, value);
    element.dispatchEvent(new Event("input", { bubbles: true }));
  });
};

const editor = () => document.body.querySelector<HTMLElement>('[role="dialog"][aria-label="编辑优先级"]');

beforeEach(() => {
  vi.clearAllMocks();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  vi.mocked(operationEpoch).mockReturnValue(1);
  vi.mocked(command).mockResolvedValue(undefined as never);
  vi.mocked(submitOperation).mockImplementation(async (_account, action, payload) => ({
    ...job(action),
    requested: payload,
  }));
  vi.mocked(followOperation).mockImplementation(async (initial: Operation, _onEvent, onUpdate) => {
    onUpdate?.(initial);
    return { verified: true } as never;
  });
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  document.body.querySelectorAll(".numeric-editor-backdrop").forEach((node) => node.remove());
});

describe("scheduling parameters", () => {
  it("opens a full numeric value, allows transient clearing, and applies step buttons", async () => {
    await act(async () => root.render(<SchedulingParameters account={account()} online />));
    const value = container.querySelector<HTMLButtonElement>('[aria-label="编辑Fixture account优先级"]')!;
    expect(value.textContent).toBe("12");

    await click(value);
    const form = editor()!;
    const input = form.querySelector<HTMLInputElement>('input[aria-label="优先级"]')!;
    expect(input.value).toBe("12");

    await change(input, "");
    expect(input.value).toBe("");
    await click(form.querySelector<HTMLButtonElement>('[aria-label="增加优先级"]')!);
    expect(input.value).toBe("1");
    await click(form.querySelector<HTMLButtonElement>('[aria-label="减少优先级"]')!);
    expect(input.value).toBe("0");

    await change(input, "");
    await act(async () => form.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true })));
    expect(form.querySelector('[role="alert"]')?.textContent).toContain("优先级必须");
    expect(submitOperation).not.toHaveBeenCalled();

    await act(async () => form.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true })));
    expect(editor()).toBeNull();
  });

  it("shows queued feedback until the operation is confirmed and then refreshes", async () => {
    let resolveFollow!: (value: { verified: boolean }) => void;
    const pending = new Promise<{ verified: boolean }>((resolve) => { resolveFollow = resolve; });
    vi.mocked(followOperation).mockImplementation(async (initial: Operation, _onEvent, onUpdate) => {
      onUpdate?.(initial);
      return pending as never;
    });
    await act(async () => root.render(<SchedulingParameters account={account()} online />));
    await click(container.querySelector<HTMLButtonElement>('[aria-label="编辑Fixture account优先级"]')!);
    const form = editor()!;
    await click(form.querySelector<HTMLButtonElement>('button[type="submit"]')!);
    expect(form.querySelector('[role="status"]')?.textContent).toContain("已排队");
    expect(command).not.toHaveBeenCalledWith("refresh");

    await act(async () => resolveFollow({ verified: true }));
    expect(form.querySelector('[role="status"]')?.textContent).toBe("已确认生效");
    expect(command).toHaveBeenCalledWith("refresh");
    expect(submitOperation).toHaveBeenCalledWith(
      expect.objectContaining({ id: 101 }),
      "priority",
      { priority: 12 },
      "priority-version",
    );
  });

  it("retains the entered value and exposes a failed save", async () => {
    vi.mocked(followOperation).mockRejectedValue(new Error("fixture save failed"));
    await act(async () => root.render(<SchedulingParameters account={account()} online />));
    await click(container.querySelector<HTMLButtonElement>('[aria-label="编辑Fixture account优先级"]')!);
    const form = editor()!;
    const input = form.querySelector<HTMLInputElement>('input[aria-label="优先级"]')!;
    await change(input, "99");
    await click(form.querySelector<HTMLButtonElement>('button[type="submit"]')!);

    expect(input.value).toBe("99");
    expect(form.querySelector('[role="alert"]')?.textContent).toBe("fixture save failed");
    expect(command).not.toHaveBeenCalledWith("refresh");
  });

  it("uses automatic load factor until explicitly disabled", async () => {
    const automatic = account({ load_factor: null, concurrency: 7 });
    await act(async () => root.render(<SchedulingParameters account={automatic} online />));
    await click(container.querySelector<HTMLButtonElement>('[aria-label="编辑Fixture account负载因子"]')!);
    const form = document.body.querySelector<HTMLElement>('[role="dialog"][aria-label="编辑负载因子"]')!;
    const checkbox = form.querySelector<HTMLInputElement>('input[type="checkbox"]')!;
    const input = form.querySelector<HTMLInputElement>('input[aria-label="负载因子"]')!;
    expect(checkbox.checked).toBe(true);
    expect(input.disabled).toBe(true);

    await click(checkbox);
    expect(checkbox.checked).toBe(false);
    expect(input.disabled).toBe(false);
    expect(input.value).toBe("7");
    await click(form.querySelector<HTMLButtonElement>('button[type="submit"]')!);
    expect(submitOperation).toHaveBeenCalledWith(
      expect.objectContaining({ id: 101 }),
      "load_factor",
      { load_factor: 7 },
      "load-factor-version",
    );
  });

  it("submits null when automatic mode is explicitly saved", async () => {
    await act(async () => root.render(<SchedulingParameters account={account({ load_factor: null })} online />));
    await click(container.querySelector<HTMLButtonElement>('[aria-label="编辑Fixture account负载因子"]')!);
    const form = document.body.querySelector<HTMLElement>('[role="dialog"][aria-label="编辑负载因子"]')!;
    await click(form.querySelector<HTMLButtonElement>('button[type="submit"]')!);
    expect(submitOperation).toHaveBeenCalledWith(
      expect.objectContaining({ id: 101 }),
      "load_factor",
      { load_factor: null },
      "load-factor-version",
    );
  });
});
