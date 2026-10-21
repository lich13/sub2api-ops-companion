export type PriorityAccount = {
  id: number; name: string; platform: string; type: string; priority: number; version: string;
  editable: boolean; reason: string; status: string; schedulable: boolean;
};
export type PriorityItem = {
  account_id: number; account_name: string; original_priority: number; target_priority: number;
  observed_priority: number | null; status: "pending" | "unchanged" | "dispatched" | "verified";
};
export type PriorityTask = {
  id: string; status: string; cancel_requested: boolean; reason: string; total: number;
  change_count: number; verified_count: number; created_at: string; updated_at: string;
  completed_at: string | null; items?: PriorityItem[];
};
export type PriorityView = { items: PriorityAccount[]; version: string; active_task: PriorityTask | null; last_task: PriorityTask | null };
export type PriorityDraft = { ids: number[]; version: string; accounts: PriorityAccount[] };
export const priorityPending = (task: PriorityTask | null) => !!task && ["queued", "running", "checking"].includes(task.status);
export const priorityOrder = (accounts: PriorityAccount[]) => [...accounts].sort((a, b) => a.priority - b.priority || a.id - b.id).map(a => a.id);
export const samePriorityAccounts = (ids: number[], accounts: PriorityAccount[]) => ids.length === accounts.length && new Set(ids).size === ids.length && accounts.every(a => ids.includes(a.id));
export function movePriority(ids: number[], id: number, target: number) {
  const next = ids.filter(value => value !== id);
  if (next.length !== ids.length - 1) return ids;
  next.splice(Math.max(0, Math.min(next.length, target)), 0, id);
  return next;
}
