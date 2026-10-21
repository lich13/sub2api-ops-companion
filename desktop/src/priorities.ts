export type PriorityAccount = {
  id: number; name: string; platform: string; type: string; priority: number; version: string;
  editable: boolean; reason: string; status: string; schedulable: boolean;
};
export type PriorityItem = {
  account_id: number; account_name: string; platform?: string; original_priority: number; target_priority: number;
  observed_priority: number | null; status: "pending" | "unchanged" | "dispatched" | "verified";
};
export type PriorityTask = {
  id: string; status: string; cancel_requested: boolean; reason: string; total: number;
  change_count: number; verified_count: number; created_at: string; updated_at: string;
  completed_at: string | null; items?: PriorityItem[];
  platform: string | null; base: 0 | 1; waiting_reason: string; waiting_account_ids: number[];
};
export type PriorityPlatform = { platform: string; items: PriorityAccount[]; version: string; last_task: PriorityTask | null };
export type PriorityView = { platforms: PriorityPlatform[]; active_task: PriorityTask | null; legacy_last_task: PriorityTask | null };
export type LegacyPriorityView = { items: PriorityAccount[]; version: string };
export type PriorityDraft = { ids: number[]; version: string; accounts: PriorityAccount[] };
export const priorityPending = (task: PriorityTask | null | undefined) => !!task && ["queued", "running", "checking"].includes(task.status);
export const priorityOrder = (accounts: PriorityAccount[]) => [...accounts].sort((a, b) => a.priority - b.priority || a.id - b.id).map(a => a.id);
export function samePriorityAccounts(ids: number[], accounts: PriorityAccount[]) {
  const unique = new Set(ids);
  return ids.length === accounts.length && unique.size === ids.length && accounts.every(a => unique.has(a.id));
}
export function movePriority(ids: number[], id: number, target: number) {
  const next = ids.filter(value => value !== id);
  if (next.length !== ids.length - 1 || target < 0 || target >= ids.length) return ids;
  next.splice(target, 0, id);
  return next;
}
const preferredPlatforms = ["openai", "grok", "anthropic"];
export const priorityPlatformName = (platform: string) => ({ openai: "Codex", grok: "Grok", anthropic: "Claude" }[platform] ?? platform);
export function sortPriorityPlatforms<T extends { platform: string }>(platforms: T[]): T[] {
  const rank = (p: string) => { const i = preferredPlatforms.indexOf(p); return i < 0 ? preferredPlatforms.length : i; };
  return [...platforms].sort((a, b) => rank(a.platform) - rank(b.platform) || (a.platform < b.platform ? -1 : a.platform > b.platform ? 1 : 0));
}
export function groupPriorityAccounts(items: PriorityAccount[], version = ""): PriorityPlatform[] {
  const groups = new Map<string, PriorityAccount[]>();
  for (const item of items) { const group = groups.get(item.platform) ?? []; group.push(item); groups.set(item.platform, group); }
  return sortPriorityPlatforms([...groups].map(([platform, accounts]) => ({ platform, items: accounts, version, last_task: null })));
}
