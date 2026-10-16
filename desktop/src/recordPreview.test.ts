// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import type { UsageRecord } from "./records";
import { presetRange } from "./recordDates";

type Preview = typeof import("./recordPreview")["recordPreview"];
type PreviewPage = { items: UsageRecord[]; next_cursor: string | null };

const scenarios = [
  {
    label: "Beijing 23:59:59",
    instant: "2026-10-01T23:59:59+08:00",
    today: ["2026-10-01", "2026-10-01"],
    recent: ["2026-09-30", "2026-10-01"],
    todayCount: 75,
    reason: "all 75 generated rows are still on the current Beijing date",
  },
  {
    label: "Beijing 00:00:00",
    instant: "2026-10-02T00:00:00+08:00",
    today: ["2026-10-02", "2026-10-02"],
    recent: ["2026-10-01", "2026-10-02"],
    todayCount: 1,
    reason: "only the newest row is midnight; the other 74 are yesterday",
  },
  {
    label: "Beijing 00:10:00",
    instant: "2026-10-02T00:10:00+08:00",
    today: ["2026-10-02", "2026-10-02"],
    recent: ["2026-10-01", "2026-10-02"],
    todayCount: 20,
    reason: "20 rows are after midnight and the remaining 55 are yesterday",
  },
  {
    label: "Beijing 12:00:00",
    instant: "2026-10-02T12:00:00+08:00",
    today: ["2026-10-02", "2026-10-02"],
    recent: ["2026-10-01", "2026-10-02"],
    todayCount: 75,
    reason: "all 75 generated rows fit within the current Beijing date",
  },
] as const;

function readPage(
  preview: Preview,
  range: ReturnType<typeof presetRange>,
  cursor?: string,
): PreviewPage {
  const query = new URLSearchParams({
    limit: "50",
    start_date: range.start,
    end_date: range.end,
  });
  if (cursor) query.set("cursor", cursor);

  const result = preview(`/usage-records?${query}`);
  if (
    typeof result !== "object" ||
    result === null ||
    !("items" in result) ||
    !Array.isArray(result.items)
  ) {
    throw new Error("Record preview did not return a usage-record page");
  }
  return result as PreviewPage;
}

function readAll(preview: Preview, range: ReturnType<typeof presetRange>) {
  const items: UsageRecord[] = [];
  let cursor: string | undefined;
  const maxPages = 10;
  for (let pageCount = 0; pageCount < maxPages; pageCount += 1) {
    const page = readPage(preview, range, cursor);
    items.push(...page.items);
    cursor = page.next_cursor ?? undefined;
    if (!cursor) return items;
  }
  throw new Error(`Record preview exceeded ${maxPages} pages`);
}

function beijingDate(timestamp: string) {
  return new Date(Date.parse(timestamp) + 8 * 60 * 60 * 1000)
    .toISOString()
    .slice(0, 10);
}

afterEach(() => {
  vi.useRealTimers();
  vi.resetModules();
  window.history.replaceState({}, "", "/");
});

describe("record preview date filtering across Beijing midnight", () => {
  for (const scenario of scenarios) {
    it(`${scenario.label}: today returns ${scenario.todayCount} rows because ${scenario.reason}`, async () => {
      vi.useFakeTimers();
      vi.setSystemTime(new Date(scenario.instant));
      vi.resetModules();
      window.history.replaceState({}, "", "/");
      const { recordPreview } = await import("./recordPreview");

      const today = presetRange("today");
      const recent = presetRange("24h");
      expect(today).toEqual({
        preset: "today",
        start: scenario.today[0],
        end: scenario.today[1],
      });
      expect(recent).toEqual({
        preset: "24h",
        start: scenario.recent[0],
        end: scenario.recent[1],
      });

      const todayRows = readAll(recordPreview, today);
      expect(todayRows).toHaveLength(scenario.todayCount);
      expect(todayRows.map((row) => beijingDate(row.created_at))).toEqual(
        Array.from({ length: scenario.todayCount }, () => today.start),
      );

      const recentFirst = readPage(recordPreview, recent);
      expect(recentFirst.items).toHaveLength(50);
      expect(recentFirst.next_cursor).toBe("151");
      const recentSecond = readPage(recordPreview, recent, recentFirst.next_cursor!);
      expect(recentSecond.items).toHaveLength(25);
      expect(recentSecond.next_cursor).toBeNull();
      const recentRows = [...recentFirst.items, ...recentSecond.items];
      expect(recentRows).toHaveLength(75);
      expect(recentRows.map((row) => row.id)).toEqual(
        Array.from({ length: 75 }, (_, index) => 200 - index),
      );
      expect(new Set(recentRows.map((row) => row.id)).size).toBe(75);
      expect(
        recentRows.every((row) => {
          const day = beijingDate(row.created_at);
          return day >= recent.start && day <= recent.end;
        }),
      ).toBe(true);
    });
  }
});
