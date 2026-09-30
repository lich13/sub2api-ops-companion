import type { Account, Group } from "./types";

export type GroupDraft = { id: number; name: string; version: string; original: number[]; target: number[]; scope: number[] };
export type Drafts = Record<number, GroupDraft>;
export type Zone = "left" | "both" | "right" | "none";
export const ids = (values: number[]) => [...new Set(values)].sort((a, b) => a - b);
export const sameIds = (a: number[], b: number[]) => JSON.stringify(ids(a)) === JSON.stringify(ids(b));
export function platformGroups(groups: Group[], platform: string) {
  return groups.filter((g) => g.platform === platform).sort((a, b) => (a.sort_order ?? 0) - (b.sort_order ?? 0) || a.id - b.id);
}
export function moveAccount(drafts: Drafts, account: Account, pair: number[], zone: Zone): Drafts {
  if (!pair.length) return drafts;
  const existing = drafts[account.id];
  const targetPair = zone === "both" ? pair : zone === "left" ? pair.slice(0, 1) : zone === "right" ? pair.slice(1, 2) : [];
  const target = ids([...(existing?.target ?? account.group_ids).filter((id) => !pair.includes(id)), ...targetPair]);
  const original = existing?.original ?? ids(account.group_ids);
  const next = { ...drafts };
  if (sameIds(original, target)) delete next[account.id];
  else next[account.id] = { id: account.id, name: account.name, version: existing?.version ?? account.version, original, target, scope: ids([...(existing?.scope ?? []), ...pair]) };
  return next;
}
export function membershipZone(memberships: number[], pair: number[]): Zone {
  const left = memberships.includes(pair[0]), right = pair.length > 1 && memberships.includes(pair[1]);
  return left && right ? "both" : left ? "left" : right ? "right" : "none";
}
export function draftConflict(draft: GroupDraft, accounts: Account[], groups: Group[]) {
  const account = accounts.find((a) => a.id === draft.id);
  return !account || account.version !== draft.version || draft.scope.some((id) => !groups.some((g) => g.id === id && g.platform === account.platform));
}
