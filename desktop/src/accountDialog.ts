import type { Operation } from "./accountOperations";
import type { AccountSettingsReply } from "./protocol";
import type { Account } from "./types";

export type SettingsSection = "whitelist" | "mappings" | "proxy";
export type AccountSettings = Omit<
  AccountSettingsReply,
  "concurrency" | "load_factor" | "proxy_id"
> & {
  concurrency: number | null;
  load_factor: number | null;
  proxy_id: number | null;
};

export const operationLabel = (job: Operation) =>
  job.status === "queued"
    ? job.reason || "已排队，等待执行"
    : (
        {
          running: "正在保存",
          checking: "正在核对实际值",
          completed: "已确认生效",
          needs_confirmation: "相关字段已变化，请在待办中核对",
          failed: "保存失败",
          cancelled: "已取消",
          superseded: "已由新请求替代",
        } as Record<string, string>
      )[job.status] || job.status;

type Single = "test" | "modelTest" | "detection" | "keepalive";
type Batch = "delete" | "reset" | "proxy";

type AccountDialog =
  | { kind: "closed" }
  | { kind: Single; account: Account }
  | { kind: Batch; accounts: Account[] }
  | {
      kind: "settings";
      account: Account;
      section: SettingsSection;
      connection: string;
    }
  | { kind: "reauthorize"; account: Account; connection: string }
  | { kind: "templates"; account: Account | null; accounts?: Account[] };

type Action = AccountDialog | { kind: "accountsChanged"; ids: Set<number> };
export const closedDialog: AccountDialog = { kind: "closed" };

export function accountDialog(
  state: AccountDialog,
  action: Action,
): AccountDialog {
  if (action.kind === "accountsChanged") {
    return state.kind === "keepalive" && !action.ids.has(state.account.id)
      ? closedDialog
      : state;
  }
  return action;
}
