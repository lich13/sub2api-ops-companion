import type { Account, Group } from "./types";

type GroupDraft = {
  id: number;
  name: string;
  version: string;
  original: number[];
  target: number[];
  scope: number[];
};
export type Drafts = Record<number, GroupDraft>;
export type Zone = "left" | "both" | "right" | "none";
export const ids = (values: number[]) =>
  [...new Set(values)].sort((a, b) => a - b);
const sameSortedIds = (a: number[], b: number[]) =>
  a.length === b.length && a.every((value, index) => value === b[index]);
export function platformGroups(groups: Group[], platform: string) {
  return groups
    .filter((g) => g.platform === platform)
    .sort((a, b) => (a.sort_order ?? 0) - (b.sort_order ?? 0) || a.id - b.id);
}
export function moveAccount(
  drafts: Drafts,
  account: Account,
  pair: number[],
  zone: Zone,
): Drafts {
  if (!pair.length) return drafts;
  const existing = drafts[account.id];
  const targetPair =
    zone === "both"
      ? pair
      : zone === "left"
        ? pair.slice(0, 1)
        : zone === "right"
          ? pair.slice(1, 2)
          : [];
  const target = ids([
    ...(existing?.target ?? account.group_ids).filter(
      (id) => !pair.includes(id),
    ),
    ...targetPair,
  ]);
  const original = existing?.original ?? ids(account.group_ids);
  if (sameSortedIds(original, target)) {
    if (!existing) return drafts;
    const next = { ...drafts };
    delete next[account.id];
    return next;
  }
  const scope = ids([...(existing?.scope ?? []), ...pair]);
  if (
    existing &&
    existing.name === account.name &&
    sameSortedIds(existing.target, target) &&
    sameSortedIds(existing.scope, scope)
  ) {
    return drafts;
  }
  return {
    ...drafts,
    [account.id]: {
      id: account.id,
      name: account.name,
      version: existing?.version ?? account.operation_versions.groups,
      original,
      target,
      scope,
    },
  };
}
export function membershipZone(memberships: number[], pair: number[]): Zone {
  const left = memberships.includes(pair[0]),
    right = pair.length > 1 && memberships.includes(pair[1]);
  return left && right ? "both" : left ? "left" : right ? "right" : "none";
}
export function draftConflict(
  draft: GroupDraft,
  accounts: Account[],
  groups: Group[],
) {
  const account = accounts.find((a) => a.id === draft.id);
  return (
    !account ||
    account.operation_versions.groups !== draft.version ||
    draft.scope.some(
      (id) =>
        !groups.some((g) => g.id === id && g.platform === account.platform),
    )
  );
}
