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
  parallelism?: number;
};
export type PriorityPlatform = { platform: string; items: PriorityAccount[]; version: string; last_task: PriorityTask | null; parallelism?: number };
export type PriorityView = { platforms: PriorityPlatform[]; active_task: PriorityTask | null; legacy_last_task: PriorityTask | null };
export type LegacyPriorityView = { items: PriorityAccount[]; version: string };
export type PrioritySlot = { priority: number; id: number | null };
export type PriorityDraft = { ids: number[]; version: string; accounts: PriorityAccount[]; parallelism?: number; slots?: PrioritySlot[] };
export const priorityPending = (task: PriorityTask | null | undefined) => !!task && ["queued", "running", "checking"].includes(task.status);
export const priorityOrder = (accounts: PriorityAccount[]) => [...accounts].sort((a, b) => a.priority - b.priority || a.id - b.id).map(a => a.id);
export function samePriorityAccounts(ids: number[], accounts: PriorityAccount[]) {
  const unique = new Set(ids);
  return ids.length === accounts.length && unique.size === ids.length && accounts.every(a => unique.has(a.id));
}
export function movePriority(ids: number[], id: number, target: number) {
  const next = ids.filter(value => value !== id);
  if (next.length !== ids.length - 1 || target < 0 || target > next.length) return ids;
  next.splice(target, 0, id);
  return next;
}
export function prioritySlots(accounts: PriorityAccount[], width: number): PrioritySlot[] {
  const ordered = [...accounts].sort((a, b) => a.priority - b.priority || a.id - b.id);
  if (width === 1) return ordered.map((account, index) => ({ id: account.id, priority: index + 1 }));
  const groups = new Map<number, number[]>();
  for (const account of ordered) {
    const group = groups.get(account.priority);
    if (group) group.push(account.id); else groups.set(account.priority, [account.id]);
  }
  const slots = [...groups].flatMap(([priority, ids]) => Array.from({ length: Math.max(width, ids.length) }, (_, i) => ({ priority, id: ids[i] ?? null })));
  const next = Math.max(0, ...groups.keys()) + 1;
  return [...slots, ...Array.from({ length: width }, () => ({ priority: next, id: null }))];
}
export function movePrioritySlot(slots: PrioritySlot[], id: number, target: number, width: number): PrioritySlot[] {
  const source = slots.findIndex(slot => slot.id === id);
  if (source < 0 || target < 0 || target >= slots.length || source === target) return slots;
  const next = slots.map(slot => ({ ...slot }));
  next[source].id = next[target].id;
  next[target].id = id;
  const last = next.at(-1)!.priority;
  if (next.some(slot => slot.priority === last && slot.id !== null)) {
    next.push(...Array.from({ length: width }, () => ({ priority: last + 1, id: null })));
  }
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
  return sortPriorityPlatforms([...groups].map(([platform, accounts]) => ({ platform, items: accounts, version, last_task: null, parallelism: 1 })));
}
