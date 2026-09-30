// @vitest-environment jsdom
import React, { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import RecordDatePicker from "./RecordDatePicker";
import { handleBack } from "./mobile";
import {
  beijingDate,
  dateRangeLabel,
  presetRange,
  shiftDate,
  validDateRange,
  type DatePreset,
  type RecordDateRange,
} from "./recordDates";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

describe("Beijing record dates", () => {
  it("changes the calendar date at Beijing midnight independently of UTC midnight", () => {
    expect(beijingDate(Date.parse("2026-09-30T15:59:59.999Z"))).toBe(
      "2026-09-30",
    );
    expect(beijingDate(Date.parse("2026-09-30T16:00:00.000Z"))).toBe(
      "2026-10-01",
    );
    expect(beijingDate(Date.parse("2026-12-31T16:00:00.000Z"))).toBe(
      "2027-01-01",
    );
  });

  it.each<[DatePreset, string, string]>([
    ["today", "2026-03-01", "2026-03-01"],
    ["yesterday", "2026-02-28", "2026-02-28"],
    ["24h", "2026-02-28", "2026-03-01"],
    ["7d", "2026-02-23", "2026-03-01"],
    ["14d", "2026-02-16", "2026-03-01"],
    ["30d", "2026-01-31", "2026-03-01"],
    ["month", "2026-03-01", "2026-03-01"],
    ["lastMonth", "2026-02-01", "2026-02-28"],
  ])(
    "resolves %s to inclusive calendar dates %s through %s",
    (preset, start, end) => {
      expect(presetRange(preset, Date.parse("2026-03-01T02:34:56Z"))).toEqual({
        preset,
        start,
        end,
      });
    },
  );

  it("handles leap February and the previous calendar year", () => {
    expect(
      presetRange("lastMonth", Date.parse("2024-03-05T04:00:00Z")),
    ).toEqual({ preset: "lastMonth", start: "2024-02-01", end: "2024-02-29" });
    expect(
      presetRange("lastMonth", Date.parse("2026-01-05T04:00:00Z")),
    ).toEqual({ preset: "lastMonth", start: "2025-12-01", end: "2025-12-31" });
    expect(shiftDate("2024-03-01", -1)).toBe("2024-02-29");
    expect(shiftDate("2026-12-31", 1)).toBe("2027-01-01");
  });

  it("accepts an inclusive single day and rejects reversed, missing, or nonexistent dates", () => {
    expect(validDateRange("2026-09-30", "2026-09-30")).toBe(true);
    expect(validDateRange("2024-02-29", "2024-03-01")).toBe(true);
    for (const [start, end] of [
      ["2026-09-30", "2026-09-29"],
      ["", "2026-09-30"],
      ["2026-02-29", "2026-03-01"],
      ["2026-04-31", "2026-05-01"],
      ["2026-9-1", "2026-09-30"],
    ])
      expect(validDateRange(start, end), `${start} through ${end}`).toBe(false);
  });

  it("labels applied presets and custom date ranges without time-of-day values", () => {
    expect(
      dateRangeLabel({
        preset: "today",
        start: "2026-09-30",
        end: "2026-09-30",
      }),
    ).toBe("今天");
    expect(
      dateRangeLabel({ preset: null, start: "2026-09-30", end: "2026-09-30" }),
    ).toBe("2026-09-30");
    expect(
      dateRangeLabel({ preset: null, start: "2026-09-29", end: "2026-09-30" }),
    ).toBe("2026-09-29 — 2026-09-30");
  });
});

describe("record date picker", () => {
  let container: HTMLDivElement, root: Root;
  const changed = vi.fn<(value: RecordDateRange) => void>();
  const initial: RecordDateRange = {
    preset: "today",
    start: "2026-09-30",
    end: "2026-09-30",
  };
  const input = (label: string) =>
    container.querySelector<HTMLInputElement>(`[aria-label="${label}"]`)!;
  const dialog = () => container.querySelector('[role="dialog"]');
  const trigger = () =>
    container.querySelector<HTMLButtonElement>(
      'button[aria-label="时间范围"]',
    )!;
  async function render(disabled = false) {
    await act(async () =>
      root.render(
        <React.StrictMode>
          <RecordDatePicker
            value={initial}
            disabled={disabled}
            onChange={changed}
          />
        </React.StrictMode>,
      ),
    );
  }
  async function click(label: string) {
    const button = [
      ...container.querySelectorAll<HTMLButtonElement>("button"),
    ].find(
      (node) =>
        node.getAttribute("aria-label") === label || node.textContent === label,
    );
    if (!button) throw new Error(`Missing button: ${label}`);
    await act(async () => button.click());
  }
  async function setDate(label: string, value: string) {
    await act(async () => {
      Object.getOwnPropertyDescriptor(
        HTMLInputElement.prototype,
        "value",
      )!.set!.call(input(label), value);
      input(label).dispatchEvent(new Event("input", { bubbles: true }));
    });
  }
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-09-30T04:00:00Z"));
    changed.mockReset();
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
  });
  afterEach(async () => {
    await act(async () => root.unmount());
    container.remove();
    vi.useRealTimers();
  });

  it("keeps a selected preset as a draft until Apply", async () => {
    await render();
    await click("时间范围");
    await click("近7天");
    expect(changed).not.toHaveBeenCalled();
    expect(trigger().textContent).toBe("今天");
    expect(input("开始日期").value).toBe("2026-09-24");
    expect(input("结束日期").value).toBe("2026-09-30");
    await click("应用");
    expect(changed).toHaveBeenCalledExactlyOnceWith({
      preset: "7d",
      start: "2026-09-24",
      end: "2026-09-30",
    });
    expect(dialog()).toBeNull();
  });

  it.each(["close", "Escape", "Back", "outside"])(
    "discards an uncommitted range when closed with %s",
    async (action) => {
      await render();
      await click("时间范围");
      await click("上月");
      const fallback = vi.fn();
      if (action === "close") await click("关闭时间范围");
      else if (action === "Escape")
        await act(async () =>
          dialog()!.dispatchEvent(
            new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
          ),
        );
      else if (action === "Back") await act(async () => handleBack(fallback));
      else
        await act(async () =>
          document.body.dispatchEvent(
            new Event("pointerdown", { bubbles: true }),
          ),
        );
      expect(dialog()).toBeNull();
      expect(changed).not.toHaveBeenCalled();
      expect(fallback).not.toHaveBeenCalled();
      await act(async () => handleBack(fallback));
      expect(fallback).toHaveBeenCalledTimes(1);
      await click("时间范围");
      expect(input("开始日期").value).toBe("2026-09-30");
      expect(input("结束日期").value).toBe("2026-09-30");
    },
  );

  it("validates custom dates and applies the chosen end date inclusively", async () => {
    await render();
    await click("时间范围");
    await setDate("开始日期", "2026-10-02");
    await click("应用");
    expect(changed).not.toHaveBeenCalled();
    expect(container.querySelector('[role="alert"]')?.textContent).toContain(
      "有效的日期范围",
    );
    await setDate("结束日期", "2026-10-03");
    await click("应用");
    expect(changed).toHaveBeenCalledExactlyOnceWith({
      preset: null,
      start: "2026-10-02",
      end: "2026-10-03",
    });
    expect(dialog()).toBeNull();
  });

  it("re-evaluates a relative preset if midnight passes before Apply", async () => {
    vi.setSystemTime(new Date("2026-09-30T15:59:59Z"));
    await render();
    await click("时间范围");
    await click("近7天");
    vi.setSystemTime(new Date("2026-09-30T16:00:00Z"));
    await click("应用");
    expect(changed).toHaveBeenCalledExactlyOnceWith({
      preset: "7d",
      start: "2026-09-25",
      end: "2026-10-01",
    });
  });

  it("closes and releases Back handling when the picker is disabled", async () => {
    await render();
    await click("时间范围");
    await click("昨天");
    await render(true);
    expect(dialog()).toBeNull();
    expect(trigger().disabled).toBe(true);
    expect(changed).not.toHaveBeenCalled();
    const fallback = vi.fn();
    await act(async () => handleBack(fallback));
    expect(fallback).toHaveBeenCalledTimes(1);
    await render();
    await click("时间范围");
    expect(input("开始日期").value).toBe("2026-09-30");
  });
});
