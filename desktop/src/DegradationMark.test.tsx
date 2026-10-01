// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import DegradationAction, { DegradationBadge } from "./DegradationMark";
import { api, command } from "./bridge";
import type { Account } from "./types";

vi.mock("./bridge", () => ({
  api: vi.fn(),
  command: vi.fn(),
  subscribe: vi.fn(),
  updates: vi.fn(async () => () => {}),
  preview: true,
}));

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

let container: HTMLDivElement;
let root: Root;

const account = (overrides: Partial<Account> = {}): Account => ({
  id: 11,
  name: "OAuth 账号",
  priority: 1,
  platform: "openai",
  type: "oauth",
  status: "active",
  schedulable: true,
  available: true,
  group_ids: [],
  blockers: [],
  managed: false,
  version: "account-version",
  last_success_at: null,
  last_error_at: null,
  last_error_id: null,
  last_error_code: null,
  last_error_status: null,
  error_message: "",
  success_after_error: false,
  usage_windows: [],
  degradation_mark: { marked: false, version: "mark-version" },
  ...overrides,
});

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

describe("degradation mark", () => {
  it("keeps the main badge text while compact badges expose only the icon", async () => {
    await act(async () =>
      root.render(
        <>
          <DegradationBadge account={account({ degradation_mark: { marked: true, version: "mark-version" } })} />
          <DegradationBadge compact account={account({ degradation_mark: { marked: true, version: "mark-version" } })} />
        </>,
      ),
    );
    const badges = [...container.querySelectorAll<HTMLElement>(".degradation-badge")];
    expect(badges).toHaveLength(2);
    expect(badges[0].textContent).toContain("降智");
    expect(badges[1].textContent).toBe("");
    expect(badges[1].getAttribute("aria-label")).toBe("降智");
    expect(badges[1].getAttribute("role")).toBe("img");
    expect(badges[1].querySelector("svg")?.getAttribute("aria-hidden")).toBe("true");
  });

  it("renders only when marked and sends an independent verified PUT", async () => {
    const onError = vi.fn();
    vi.mocked(api).mockResolvedValue({
      verified: true,
      degradation_mark: { marked: true, version: "mark-version-2" },
    } as never);
    await act(async () =>
      root.render(
        <>
          <DegradationBadge account={account({ degradation_mark: { marked: true, version: "mark-version" } })} />
          <DegradationAction account={account()} online report={onError} />
        </>,
      ),
    );
    expect(container.querySelector(".degradation-badge")?.textContent).toContain("降智");
    const button = container.querySelector<HTMLButtonElement>(".degradation-action")!;
    expect(button.textContent).toContain("标记降智");
    expect(api).not.toHaveBeenCalled();

    await act(async () => button.click());
    expect(api).toHaveBeenCalledExactlyOnceWith("PUT", "/accounts/11/degradation-mark", {
      marked: true,
      expected_mark_version: "mark-version",
    });
    expect(command).toHaveBeenCalledExactlyOnceWith("refresh");
    expect(onError).not.toHaveBeenCalled();
    expect(button.textContent).toContain("取消标记");
  });

  it("reports an unverified write without refreshing or showing success", async () => {
    const onError = vi.fn();
    vi.mocked(api).mockResolvedValue({
      verified: false,
      degradation_mark: { marked: true, version: "mark-version-2" },
    } as never);
    await act(async () => root.render(<DegradationAction account={account()} online report={onError} />));
    await act(async () => container.querySelector<HTMLButtonElement>(".degradation-action")!.click());
    expect(onError).toHaveBeenCalledOnce();
    expect(String(onError.mock.calls[0][0])).toContain("降智标记保存未确认");
    expect(command).not.toHaveBeenCalled();
    expect(container.querySelector<HTMLButtonElement>(".degradation-action")?.textContent).toContain("标记降智");
  });

  it("disables unreadable marks and supports Key while excluding Grok", async () => {
    const onError = vi.fn();
    await act(async () =>
      root.render(
        <>
          <DegradationAction account={account({ degradation_mark: { error: "读取失败" } })} online report={onError} />
          <DegradationAction account={account({ platform: "grok" })} online report={onError} />
          <DegradationAction account={account({ type: "apikey" })} online report={onError} />
        </>,
      ),
    );
    const buttons = [...container.querySelectorAll<HTMLButtonElement>(".degradation-action")];
    expect(buttons).toHaveLength(2);
    expect(buttons[1].textContent).toContain("标记降智");
    expect(buttons[0].disabled).toBe(true);
    expect(buttons[0].textContent).toContain("标记读取失败");
    await act(async () => buttons[0].click());
    expect(api).not.toHaveBeenCalled();
  });
});
