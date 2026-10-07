import { describe, it, expect } from "vitest";
import {
  fullTime,
  filterAccounts,
  sortPriority,
  sortRecentCall,
  sortQuality,
  currentGroups,
  type Account,
  type Group,
  type RecentAccount,
} from "./types";

const recentCall = (
  account_id: number,
  log_id: number,
  called_at: string | null,
): RecentAccount => ({
  account_id,
  log_id,
  account_name: `账号 ${account_id}`,
  model: "gpt-6-sol",
  upstream_model: "gpt-6-sol",
  called_at,
});

const group = (id: number, fields: Partial<Group> = {}): Group => ({
  id,
  name: `分组 ${id}`,
  platform: "openai",
  account_id: null,
  account_name: "",
  model: "",
  upstream_model: "",
  upstream_response_model: "",
  called_at: null,
  ...fields,
});

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
  it("sorts recent calls by last_called_at with a last_success_at fallback", () => {
    const data = [
      { id: 4, last_called_at: null, last_success_at: null },
      { id: 2, last_called_at: "2026-10-07T00:00:00Z", last_success_at: "2026-10-04T00:00:00Z" },
      { id: 1, last_called_at: "2026-10-07T00:00:00Z", last_success_at: null },
      { id: 3, last_called_at: null, last_success_at: "2026-10-05T00:00:00Z" },
      { id: 5, last_called_at: "invalid", last_success_at: "2026-10-06T00:00:00Z" },
    ] as Account[];
    expect(sortRecentCall(data).map((a) => a.id)).toEqual([2, 1, 5, 3, 4]);
    expect(sortRecentCall(data, true).map((a) => a.id)).toEqual([3, 5, 2, 1, 4]);
  });
});

describe("current group activity", () => {
  it("filters stale calls, deduplicates by latest time and ID, and sorts groups deterministically", () => {
    const at = "2026-09-30T00:00:00Z";
    const groups = [
      group(20, {
        called_at: "2026-09-29T23:00:00Z",
        recent_accounts: [recentCall(2, 20, "2026-09-29T23:00:00Z")],
      }),
      group(11, {
        called_at: at,
        recent_accounts: [recentCall(1, 15, at)],
      }),
      group(5, {
        called_at: at,
        recent_accounts: [
          recentCall(1, 9, at),
          recentCall(1, 11, at),
          recentCall(1, 900, "2026-09-29T00:00:00Z"),
          recentCall(2, 12, at),
          recentCall(3, 1000, "2099-01-01T00:00:00Z"),
        ],
      }),
      group(2, {
        account_id: 1,
        called_at: "2099-01-01T00:00:00Z",
        recent_accounts: [recentCall(1, 2000, "2099-01-01T00:00:00Z")],
      }),
      group(3, {
        account_id: 2,
        called_at: "2099-01-01T00:00:00Z",
        recent_accounts: [],
      }),
      group(8, {
        account_id: 1,
        called_at: "2099-01-01T00:00:00Z",
        recent_accounts: [recentCall(1, 3000, "not-a-time")],
      }),
    ];
    const accounts = [
      { id: 1, group_ids: [5, 8, 11] },
      { id: 2, group_ids: [5, 20] },
      { id: 3, group_ids: [3] },
    ] as Account[];
    const before = JSON.stringify({ groups, accounts });

    const sorted = currentGroups(groups, accounts);

    expect(sorted.map((item) => item.id)).toEqual([5, 11, 20, 2, 3, 8]);
    expect(sorted[0].recent_accounts?.map((call) => [call.account_id, call.log_id])).toEqual([
      [2, 12],
      [1, 11],
    ]);
    expect(sorted.find((item) => item.id === 2)?.recent_accounts).toEqual([]);
    expect(sorted.find((item) => item.id === 2)?.called_at).toBeNull();
    expect(sorted.find((item) => item.id === 3)?.recent_accounts).toEqual([]);
    expect(sorted.find((item) => item.id === 8)?.called_at).toBe("not-a-time");
    expect(JSON.stringify({ groups, accounts })).toBe(before);
    expect(sorted[0]).not.toBe(groups[2]);
    expect(sorted[0].recent_accounts).not.toBe(groups[2].recent_accounts);
  });

  it("supports old single-account responses while keeping empty recent lists authoritative", () => {
    const oldResponse = group(22, {
      account_id: 22,
      account_name: "旧账号",
      model: "legacy-model",
      upstream_model: "legacy-upstream",
      called_at: "2026-09-30T00:00:00Z",
    });
    const sorted = currentGroups([oldResponse], [
      { id: 22, group_ids: [22] },
    ] as Account[]);

    expect(sorted[0].recent_accounts).toEqual([
      {
        log_id: 0,
        account_id: 22,
        account_name: "旧账号",
        model: "legacy-model",
        upstream_model: "legacy-upstream",
        called_at: "2026-09-30T00:00:00Z",
      },
    ]);
    expect(currentGroups([], [])).toEqual([]);
  });
});
