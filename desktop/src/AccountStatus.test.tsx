// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import AccountStatus, { remainingRestriction } from "./AccountStatus";
import type { Account } from "./types";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

const now = Date.parse("2026-10-16T00:00:00.000Z");

const account = (overrides: Partial<Account> = {}): Account => ({
  id: 17,
  name: "Fixture account",
  priority: 50,
  platform: "openai",
  type: "oauth",
  status: "active",
  schedulable: false,
  available: false,
  group_ids: [],
  blockers: [{ code: "rate_limit_reset_at", label: "上游限流" }],
  recoverable: false,
  version: "fixture-version",
  last_success_at: null,
  last_error_at: "2026-10-16T00:00:00.000Z",
  last_error_id: 71,
  last_error_code: "rate_limit_exceeded",
  last_error_status: 429,
  error_message: "Fixture upstream rate limit",
  success_after_error: false,
  usage_windows: [],
  ...overrides,
});

let container: HTMLDivElement;
let root: Root;

async function render(value: Account, onError = vi.fn()) {
  await act(async () => root.render(<AccountStatus account={value} onError={onError} />));
  return onError;
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(now);
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.useRealTimers();
});

describe("remaining restriction", () => {
  it("formats remaining days, hours, minutes, and seconds", () => {
    expect(remainingRestriction("2026-10-27T19:00:00.000Z", now)).toBe(
      "11天19小时",
    );
    expect(remainingRestriction("2026-10-16T05:27:00.000Z", now)).toBe(
      "5小时27分钟",
    );
    expect(remainingRestriction("2026-10-16T00:02:30.000Z", now)).toBe(
      "2分钟",
    );
    expect(remainingRestriction("2026-10-16T00:00:30.000Z", now)).toBe(
      "30秒",
    );
  });

  it("distinguishes an expired deadline from a missing or invalid one", () => {
    expect(remainingRestriction("2026-10-15T23:59:59.000Z", now)).toBe(
      "等待状态刷新",
    );
    expect(remainingRestriction(undefined, now)).toBe("时间未知");
    expect(remainingRestriction("not-a-date", now)).toBe("时间未知");
  });
});

describe("account status display", () => {
  it("releases its countdown interval when removed", async () => {
    await render(
      account({
        blockers: [
          {
            code: "rate_limit_reset_at",
            label: "上游限流",
            until: "2026-10-16T00:01:05.000Z",
          },
        ],
      }),
    );
    expect(vi.getTimerCount()).toBe(1);

    await act(async () => root.render(null));

    expect(vi.getTimerCount()).toBe(0);
  });

  it("shows the current limit, accurate Beijing time, ticking countdown, and current 429 error", async () => {
    const onError = vi.fn();
    await render(
      account({
        blockers: [
          {
            code: "rate_limit_reset_at",
            label: "上游限流",
            until: "2026-10-16T00:01:05.000Z",
          },
        ],
      }),
      onError,
    );

    expect(container.querySelector(".account-status.pill.warning")?.textContent).toBe(
      "上游限流",
    );
    expect(container.querySelector(".limit-until")?.textContent).toBe(
      "1分钟后自动解除",
    );
    expect(container.querySelector(".limit-until")?.getAttribute("title")).toBe(
      "预计解除 10-16 08:01:05",
    );
    expect(
      container
        .querySelector('[aria-label="查看最近上游错误 HTTP 429"]')
        ?.textContent,
    ).toContain("429");
    expect(
      container
        .querySelector('[aria-label^="状态说明："]')
        ?.getAttribute("aria-label"),
    ).toContain("最近上游错误：HTTP 429");

    await act(async () => {
      await vi.advanceTimersByTimeAsync(6_000);
    });
    expect(container.querySelector(".limit-until")?.textContent).toBe(
      "59秒后自动解除",
    );

    await act(async () => {
      container.querySelector<HTMLButtonElement>(
        '[aria-label="查看最近上游错误 HTTP 429"]',
      )!.click();
    });
    expect(onError).toHaveBeenCalledExactlyOnceWith(71);
  });

  it.each([
    ["an invalid deadline", "not-a-date", "时间未知"],
    ["an expired deadline", "2026-10-15T23:59:59.000Z", "等待状态刷新"],
  ])("does not imply recovery for %s", async (_label, until, expected) => {
    await render(
      account({
        blockers: [{ code: "rate_limit_reset_at", label: "上游限流", until }],
      }),
    );

    expect(container.querySelector(".limit-until")?.textContent).toBe(expected);
    expect(container.querySelector(".account-status")?.textContent).toBe("上游限流");
    expect(container.textContent).not.toContain("已解除");
    expect(container.textContent).not.toContain("已恢复");
    expect(container.textContent).not.toContain("可调度");
  });

  it("keeps an available account with only historical 429 data clear", async () => {
    await render(
      account({
        available: true,
        schedulable: true,
        blockers: [],
        success_after_error: false,
      }),
    );

    expect(container.querySelector(".account-status")?.textContent).toBe("可调度");
    expect(container.querySelector(".account-status.warning")).toBeNull();
    expect(
      container.querySelector('[aria-label="最近上游错误 HTTP 429"]'),
    ).toBeNull();
    expect(container.textContent).not.toContain("Fixture upstream rate limit");
  });

  it("hides the current-error badge after a successful request", async () => {
    await render(
      account({
        blockers: [
          {
            code: "rate_limit_reset_at",
            label: "上游限流",
            until: "2026-10-16T00:01:05.000Z",
          },
        ],
        success_after_error: true,
      }),
    );

    expect(
      container.querySelector('[aria-label="最近上游错误 HTTP 429"]'),
    ).toBeNull();
    expect(
      container
        .querySelector('[aria-label^="状态说明："]')
        ?.getAttribute("aria-label"),
    ).not.toContain("最近上游错误");
  });
});
