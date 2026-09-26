import { describe, it, expect } from "vitest";
import {
  fullTime,
  filterAccounts,
  sortPriority,
  sortQuality,
  type Account,
} from "./types";
describe("account evidence", () => {
  it("sorts quality in both directions with unknowns last and stable ID ties", () => {
    const data = [
      { id: 4, quality: { score: null } },
      { id: 3, quality: { score: 90 } },
      { id: 2, quality: { score: 35 } },
      { id: 1, quality: { score: 90 } },
      { id: 5 },
    ] as Account[];
    expect(sortQuality(data).map((a) => a.id)).toEqual([2, 1, 3, 4, 5]);
    expect(sortQuality(data, false).map((a) => a.id)).toEqual([1, 3, 2, 4, 5]);
  });
  it("never invents timestamps", () => {
    expect(fullTime(null)).toBe("暂无记录");
    expect(fullTime("bad")).toBe("时间未知");
    expect(fullTime("2026-09-24T00:00:00Z")).toBe("09-24 08:00:00");
    expect(fullTime("2026-09-24T16:00:00Z")).toBe("09-25 00:00:00");
    expect(fullTime("2026-09-25T00:00:01+08:00")).toBe("09-25 00:00:01");
    expect(fullTime("2026-12-31T16:00:00Z")).toBe("01-01 00:00:00");
  });
  it("combines group and state filters without assuming enabled means available", () => {
    const data = [
      {
        id: 1,
        name: "主力",
        platform: "openai",
        type: "oauth",
        group_ids: [1, 2],
        schedulable: true,
        available: false,
      },
      {
        id: 2,
        name: "备用",
        platform: "grok",
        type: "apikey",
        group_ids: [2],
        schedulable: true,
        available: true,
      },
    ] as Account[];
    expect(filterAccounts(data, "", "2", "", "ready").map((a) => a.id)).toEqual(
      [2],
    );
    expect(
      filterAccounts(data, "主", "1", "openai", "").map((a) => a.id),
    ).toEqual([1]);
  });
  it("sorts priority stably with smaller values first", () => {
    const data = [
      { id: 2, priority: 10 },
      { id: 1, priority: 2 },
      { id: 3, priority: 2 },
    ] as Account[];
    expect(sortPriority(data).map((a) => a.id)).toEqual([1, 3, 2]);
    expect(sortPriority(data, false).map((a) => a.id)).toEqual([2, 1, 3]);
  });
});
