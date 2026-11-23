import type {
  PriorityAccountReply as PriorityAccount,
  PriorityItemReply,
  PriorityOverviewReply,
  PriorityPlatformReply,
  PriorityTaskReply,
} from "./protocol";
export type { PriorityAccountReply as PriorityAccount } from "./protocol";
export type PriorityItem = Omit<
  PriorityItemReply,
  "platform" | "observed_priority" | "status"
> & {
  platform?: string;
  observed_priority: number | null;
  status: "pending" | "unchanged" | "dispatched" | "verified";
};
export type PriorityTask = Omit<PriorityTaskReply, "completed_at" | "items"> & {
  completed_at: string | null;
  items?: PriorityItem[];
};
export type PriorityPlatform = Omit<
  PriorityPlatformReply,
  "items" | "last_task"
> & {
  items: PriorityAccount[];
  last_task: PriorityTask | null;
};
export type PriorityView = Omit<
  PriorityOverviewReply,
  "platforms" | "active_task"
> & {
  platforms: PriorityPlatform[];
  active_task: PriorityTask | null;
};
export type PrioritySlot = { priority: number; id: number | null };
export type PriorityDraft = {
  ids: number[];
  version: string;
  accounts: PriorityAccount[];
  parallelism?: number;
  slots?: PrioritySlot[];
};
export const priorityPending = (task: PriorityTask | null | undefined) =>
  !!task && ["queued", "running", "checking"].includes(task.status);
export function samePriorityAccounts(
  ids: number[],
  accounts: Pick<PriorityAccount, "id">[],
) {
  const unique = new Set(ids);
  return (
    ids.length === accounts.length &&
    unique.size === ids.length &&
    accounts.every((a) => unique.has(a.id))
  );
}
export function movePriority(ids: number[], id: number, target: number) {
  const next = ids.filter((value) => value !== id);
  if (next.length !== ids.length - 1 || target < 0 || target > next.length)
    return ids;
  next.splice(target, 0, id);
  return next;
}
export function prioritySlots(
  accounts: PriorityAccount[],
  width: number,
): PrioritySlot[] {
  const ordered = [...accounts].sort(
    (a, b) => a.priority - b.priority || a.id - b.id,
  );
  if (width === 1)
    return ordered.map((account, index) => ({
      id: account.id,
      priority: index + 1,
    }));
  const groups = new Map<number, number[]>();
  for (const account of ordered) {
    const group = groups.get(account.priority);
    if (group) group.push(account.id);
    else groups.set(account.priority, [account.id]);
  }
  const slots = [...groups].flatMap(([priority, ids]) =>
    Array.from({ length: Math.max(width, ids.length) }, (_, i) => ({
      priority,
      id: ids[i] ?? null,
    })),
  );
  const next = Math.max(0, ...groups.keys()) + 1;
  return [
    ...slots,
    ...Array.from({ length: width }, () => ({ priority: next, id: null })),
  ];
}
export function movePrioritySlot(
  slots: PrioritySlot[],
  id: number,
  target: number,
  width: number,
): PrioritySlot[] {
  const source = slots.findIndex((slot) => slot.id === id);
  if (source < 0 || target < 0 || target >= slots.length || source === target)
    return slots;
  const next = slots.map((slot) => ({ ...slot }));
  next[source].id = next[target].id;
  next[target].id = id;
  const last = next.at(-1)!.priority;
  if (next.some((slot) => slot.priority === last && slot.id !== null)) {
    next.push(
      ...Array.from({ length: width }, () => ({
        priority: last + 1,
        id: null,
      })),
    );
  }
  return next;
}
export const preferredPlatforms = [
  "openai",
  "grok",
  "anthropic",
  "gemini",
  "antigravity",
  "deepseek",
  "kimi",
  "zhipu",
  "minimax",
  "opencode_go",
  "typesafe",
  "command_code",
  "cline",
];
export const priorityPlatformName = (platform: string) =>
  ({
    openai: "Codex",
    grok: "Grok",
    anthropic: "Claude",
    gemini: "Gemini",
    antigravity: "Antigravity",
    deepseek: "DeepSeek",
    kimi: "Kimi",
    zhipu: "智谱",
    minimax: "MiniMax",
    opencode_go: "OpenCode Go",
    typesafe: "TypeSafe / Jev",
    command_code: "Command Code",
    cline: "Cline",
  })[platform] ?? platform;
export function sortPriorityPlatforms<T extends { platform: string }>(
  platforms: T[],
): T[] {
  const rank = (p: string) => {
    const i = preferredPlatforms.indexOf(p);
    return i < 0 ? preferredPlatforms.length : i;
  };
  return [...platforms].sort(
    (a, b) =>
      rank(a.platform) - rank(b.platform) ||
      (a.platform < b.platform ? -1 : a.platform > b.platform ? 1 : 0),
  );
}
export function groupPriorityAccounts(
  items: PriorityAccount[],
  version = "",
): PriorityPlatform[] {
  const groups = new Map<string, PriorityAccount[]>();
  for (const item of items) {
    const group = groups.get(item.platform) ?? [];
    group.push(item);
    groups.set(item.platform, group);
  }
  return sortPriorityPlatforms(
    [...groups].map(([platform, accounts]) => ({
      platform,
      items: accounts,
      version,
      last_task: null,
      parallelism: 1,
    })),
  );
}
