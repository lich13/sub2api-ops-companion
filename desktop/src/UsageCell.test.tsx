import { beforeEach, describe, it, expect, vi } from "vitest";
vi.mock("./bridge", () => ({ api: vi.fn() }));
import { api } from "./bridge";
import { renderToStaticMarkup } from "react-dom/server";
import UsageCell, { usageColor } from "./UsageCell";
import type { Account } from "./types";
beforeEach(() => vi.clearAllMocks());
describe("Sub2API usage cell", () => {
  it("uses 75 and 90 percent boundaries", () => {
    expect([74.9, 75, 89.9, 90, 100].map(usageColor)).toEqual([
      "green",
      "amber",
      "amber",
      "red",
      "red",
    ]);
  });
  it("renders stats, absolute reset time and only supported actions", () => {
    const account = {
      id: 1,
      name: "test",
      usage: {
        branch: "openai_oauth",
        today: null,
        windows: [
          {
            key: "codex_7d",
            label: "7d",
            used_percent: 99,
            reset_at: "2026-09-25T16:05:02Z",
            observed_at: "2026-09-24T00:00:00Z",
            status: "stale",
            source: "passive",
            stats: {
              requests: 313,
              tokens: 46900000,
              cost: 76.8,
              standard_cost: 50,
              user_cost: 25,
            },
            estimated_total_cost: 77.58,
          },
        ],
        actions: ["query_usage", "query_reset_credits", "reset_quota"],
        reset_credits: {
          available: 1,
          expires_at: ["2026-10-23T02:52:00+08:00"],
          observed_at: null,
        },
      },
    } as Account;
    const html = renderToStaticMarkup(
      <UsageCell
        account={account}
        online
        refresh={async () => {}}
        report={() => {}}
      />,
    );
    for (const text of [
      "313",
      "46.9M",
      "76.80",
      "25.00",
      "77.58",
      "09-26 00:05:02",
      "10-23 02:52:00",
      "历史",
      "次数",
    ])
      expect(html).toContain(text);
    for (const text of ["2026-", "点数", "可邀请", "邀请用户", "探测"])
      expect(html).not.toContain(text);
  });
  it("does not invent a percentage or OAuth actions for a Key without quota", () => {
    const account = {
      usage: {
        branch: "apikey",
        windows: [],
        today: { requests: 0, tokens: 0, cost: 0, user_cost: 0 },
        actions: [],
      },
    } as unknown as Account;
    const html = renderToStaticMarkup(
      <UsageCell
        account={account}
        online
        refresh={async () => {}}
        report={() => {}}
      />,
    );
    expect(html).toContain("req");
    expect(html).not.toContain("progressbar");
    expect(html).not.toContain("查询");
  });

  it.each([undefined, null])(
    "omits automatic reset status when no task is supplied (%s)",
    (status) => {
      const html = renderToStaticMarkup(
        <UsageCell
          account={
            {
              id: 1,
              usage_windows: [],
              auto_reset_credit: status,
            } as unknown as Account
          }
          online
          refresh={async () => {}}
          report={vi.fn()}
        />,
      );
      expect(html).not.toContain("auto-reset-status");
      expect(html).not.toContain("下次");
      expect(api).not.toHaveBeenCalled();
    },
  );

  it("keeps failure and retry information in a closed disclosure with Beijing timestamps", () => {
    const html = renderToStaticMarkup(
      <UsageCell
        account={
          {
            id: 1,
            usage_windows: [],
            auto_reset_credit: {
              stage: "retry",
              label: "等待重试",
              error: "暂时无法确认重置结果",
              next_at: "2026-09-30T16:05:02Z",
              attempt_at: "2026-09-30T15:00:00Z",
              test_completed_at: null,
              recovered_at: null,
            },
          } as unknown as Account
        }
        online
        refresh={async () => {}}
        report={vi.fn()}
      />,
    );
    expect(html).toContain(
      '<details class="auto-reset-status"><summary>等待重试</summary>',
    );
    expect(html).toContain('<span role="status">暂时无法确认重置结果</span>');
    expect(html).toContain("下次 10-01 00:05:02");
    expect(html).toContain("用卡 09-30 23:00:00");
    expect(html).not.toContain("<time>测活");
    expect(html).not.toContain("<time>恢复");
    expect(html).not.toContain(" open=");
    expect(html).not.toContain(">retry<");
    expect(api).not.toHaveBeenCalled();
  });

  it("shows completed recovery evidence without inventing a retry time or an error", () => {
    const html = renderToStaticMarkup(
      <UsageCell
        account={
          {
            id: 1,
            usage_windows: [],
            auto_reset_credit: {
              stage: "recovered",
              label: "已恢复",
              error: "",
              next_at: null,
              attempt_at: "2026-09-30T15:00:00Z",
              test_completed_at: "2026-09-30T15:00:10Z",
              recovered_at: "2026-09-30T15:00:20Z",
            },
          } as unknown as Account
        }
        online={false}
        refresh={async () => {}}
        report={vi.fn()}
      />,
    );
    expect(html).toContain("<summary>已恢复</summary>");
    expect(html).toContain("用卡 09-30 23:00:00");
    expect(html).toContain("测活 09-30 23:00:10");
    expect(html).toContain("恢复 09-30 23:00:20");
    expect(html).not.toContain('role="status"');
    expect(html).not.toContain("下次");
    expect(html).not.toContain("暂无记录");
    expect(api).not.toHaveBeenCalled();
  });
});
