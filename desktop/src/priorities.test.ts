import { describe, expect, it } from "vitest";
import {
  groupPriorityAccounts,
  movePriority,
  priorityOrder,
  priorityPending,
  priorityPlatformName,
  samePriorityAccounts,
  sortPriorityPlatforms,
  type PriorityAccount,
} from "./priorities";

const account = (id: number, platform: string, priority: number): PriorityAccount => ({
  id,
  name: `${platform}-${id}`,
  platform,
  type: "oauth",
  priority,
  version: `v-${id}`,
  editable: true,
  reason: "",
  status: "active",
  schedulable: true,
});

describe("priority data helpers", () => {
  it("orders by the server priority and uses the account id as a stable tie breaker", () => {
    expect(priorityOrder([
      account(2, "openai", 2),
      account(3, "openai", 1),
      account(1, "openai", 1),
    ])).toEqual([1, 3, 2]);
  });

  it("moves exactly one id and rejects invalid or duplicate targets", () => {
    expect(movePriority([1, 2, 3], 1, 2)).toEqual([2, 3, 1]);
    expect(movePriority([1, 2, 3], 2, 0)).toEqual([2, 1, 3]);
    expect(movePriority([1, 2, 3], 99, 0)).toEqual([1, 2, 3]);
    expect(movePriority([1, 2, 3], 1, 3)).toEqual([1, 2, 3]);
  });

  it("checks membership as well as order length before accepting a draft", () => {
    const rows = [account(1, "openai", 1), account(2, "openai", 2)];
    expect(samePriorityAccounts([2, 1], rows)).toBe(true);
    expect(samePriorityAccounts([1, 1], rows)).toBe(false);
    expect(samePriorityAccounts([1], rows)).toBe(false);
    expect(samePriorityAccounts([1, 3], rows)).toBe(false);
  });

  it("keeps preferred platform order and groups accounts without sharing input arrays", () => {
    const rows = [account(5, "other", 1), account(2, "grok", 2), account(1, "openai", 1), account(3, "grok", 1)];
    const grouped = groupPriorityAccounts(rows, "fixture-version");
    expect(grouped.map(group => group.platform)).toEqual(["openai", "grok", "other"]);
    expect(grouped.find(group => group.platform === "grok")?.items.map(item => item.id)).toEqual([2, 3]);
    expect(grouped.every(group => group.version === "fixture-version" && group.last_task === null)).toBe(true);
    expect(rows.map(item => item.id)).toEqual([5, 2, 1, 3]);
  });

  it("sorts unknown platforms alphabetically after preferred platforms and maps labels", () => {
    expect(sortPriorityPlatforms([{ platform: "zeta" }, { platform: "anthropic" }, { platform: "alpha" }, { platform: "openai" }, { platform: "grok" }]).map(item => item.platform)).toEqual([
      "openai", "grok", "anthropic", "alpha", "zeta",
    ]);
    expect(priorityPlatformName("openai")).toBe("Codex");
    expect(priorityPlatformName("grok")).toBe("Grok");
    expect(priorityPlatformName("custom")).toBe("custom");
  });

  it("only treats active execution states as pending", () => {
    expect(priorityPending({ status: "queued" } as never)).toBe(true);
    expect(priorityPending({ status: "running" } as never)).toBe(true);
    expect(priorityPending({ status: "checking" } as never)).toBe(true);
    expect(priorityPending({ status: "completed" } as never)).toBe(false);
    expect(priorityPending(null)).toBe(false);
  });
});
