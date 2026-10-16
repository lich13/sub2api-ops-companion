import type { Account, AccountKeepalive } from "./types";

const controls = new Map<number, AccountKeepalive>();
export function keepalivePreview(method: string, path: string, body: Record<string, unknown>, accounts: Account[]) {
  const id = Number(path.split("/")[2]), account = accounts.find((item) => item.id === id);
  if (!account) throw new Error("账号不存在");
  let value = controls.get(id) ?? {
    account_id: id, enabled: false, generation: 0, version: "a".repeat(64), status: "disabled",
    reason: "", paused_reason: "", last_restore_at: null, restore_count: 0, budget_used: 0,
    next_allowed_at: null, read_only: false,
  };
  if (method === "PUT") {
    if (typeof body.enabled !== "boolean" || body.expected_version !== value.version) throw new Error("保活设置已变化，请重新读取");
    value = { ...value, enabled: body.enabled, generation: value.generation + 1,
      version: crypto.randomUUID().replaceAll("-", "").repeat(2), paused_reason: "" };
    controls.set(id, value);
  }
  value = { ...value, status: !value.enabled ? "disabled" : account.degradation_mark?.marked || !account.schedulable ? "paused" : "waiting",
    reason: !value.enabled ? value.paused_reason : account.degradation_mark?.marked ? "已判定降智，保活暂停" : !account.schedulable ? "调度已关闭" : "等待临时不可调度" };
  return structuredClone(value);
}
