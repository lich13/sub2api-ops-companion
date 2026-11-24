import type { Account } from "./types";
import type { SettingsSection } from "./AccountSettingsDialog";

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
