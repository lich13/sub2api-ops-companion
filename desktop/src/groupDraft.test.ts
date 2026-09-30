import { describe, it, expect } from "vitest";
import { draftConflict, membershipZone, moveAccount, platformGroups } from "./groupDraft";
import type { Account, Group } from "./types";
const account = { id: 7, name: "同名", platform: "openai", version: "v1", group_ids: [1, 9] } as Account;
const groups = [1, 2, 9].map((id) => ({ id, name: "组", platform: "openai" } as Group));
describe("membership drafts", () => {
  it("moves among exact overlap regions and preserves memberships outside the pair", () => {
    let draft = moveAccount({}, account, [1, 2], "both");
    expect(draft[7].target).toEqual([1, 2, 9]);
    draft = moveAccount(draft, account, [1, 2], "right");
    expect(draft[7].target).toEqual([2, 9]);
    draft = moveAccount(draft, account, [1, 2], "none");
    expect(draft[7].target).toEqual([9]);
    expect(moveAccount(draft, account, [1, 2], "left")).toEqual({});
    expect(account.group_ids).toEqual([1, 9]);
  });
  it("keeps baseline and scope when editing more than two groups", () => {
    const one = moveAccount({}, account, [1, 2], "both");
    const next = moveAccount(one, { ...account, version: "v2" }, [2, 9], "left");
    expect(next[7]).toMatchObject({ original: [1, 9], target: [1, 2], scope: [1, 2, 9], version: "v1" });
  });
  it("covers unassigned, single groups and intersection only once", () => {
    expect(membershipZone([], [1, 2])).toBe("none");
    expect(membershipZone([1, 2, 9], [1, 2])).toBe("both");
    expect(membershipZone([2], [1, 2])).toBe("right");
    expect(membershipZone([1], [1])).toBe("left");
    expect(moveAccount({}, { ...account, group_ids: [] }, [1], "left")[7].target).toEqual([1]);
    expect(moveAccount({}, account, [], "none")).toEqual({});
  });
  it("detects stale accounts, missing accounts and missing or changed platform groups", () => {
    const draft = moveAccount({}, account, [1, 2], "right")[7];
    expect(draftConflict(draft, [account], groups)).toBe(false);
    expect(draftConflict(draft, [], groups)).toBe(true);
    expect(draftConflict(draft, [{ ...account, version: "v2" }], groups)).toBe(true);
    expect(draftConflict(draft, [account], groups.slice(0, 1))).toBe(true);
    expect(draftConflict(draft, [account], groups.map((g) => ({ ...g, platform: "grok" })))).toBe(true);
  });
  it("keeps region order stable instead of following recent calls", () => {
    const unordered = [{ ...groups[0], sort_order: 2 }, { ...groups[1], sort_order: 0 }, { ...groups[2], sort_order: 0 }, { ...groups[0], id: 99, platform: "grok" }];
    expect(platformGroups(unordered, "openai").map((g) => g.id)).toEqual([2, 9, 1]);
  });
});
