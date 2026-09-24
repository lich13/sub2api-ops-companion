import { describe, it, expect } from "vitest";
import { fullTime, filterAccounts, type Account } from "./types";
describe("account evidence", () => {
  it("never invents timestamps", () => {
    expect(fullTime(null)).toBe("暂无记录");
    expect(fullTime("bad")).toBe("时间未知");
    expect(fullTime("2026-09-24T00:00:00Z")).toBe("2026-09-24 08:00:00");
    expect(fullTime("2026-09-24T16:00:00Z")).toBe("2026-09-25 00:00:00");
    expect(fullTime("2026-09-25T00:00:01+08:00")).toBe("2026-09-25 00:00:01");
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
});
