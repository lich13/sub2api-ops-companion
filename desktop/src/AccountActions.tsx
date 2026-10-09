import { useEffect, useRef, useState, type ReactNode } from "react";
import { Activity, BellOff, Clock3, LoaderCircle } from "lucide-react";
import { accountOperation } from "./accountOperations";
import { command } from "./bridge";
import AccountActionMenu from "./AccountActionMenu";
import type { Account, DegradationMark } from "./types";

export type AccountActionItem = {
  id: string;
  label: string;
  icon?: ReactNode;
  disabled?: boolean;
  className?: string;
  title?: string;
  run: () => void;
};
type Options = {
  online: boolean;
  connectionKey: string;
  report: (error: unknown) => void;
  modelTest: (account: Account) => void;
  modelDetection: (account: Account) => void;
  test: (account: Account) => void;
  template: (account: Account) => void;
  remove: (account: Account) => void;
};

// Owned by the main window, so closing a popup or changing pages does not
// discard an accepted operation's refresh and feedback.
export function useAccountActions(options: Options): (account: Account) => AccountActionItem[] {
  const current = useRef(options);
  current.current = options;
  const alive = useRef(true);
  const running = useRef(new Set<string>());
  const [, redraw] = useState(0);
  const [marks, setMarks] = useState<Record<number, { base?: string; value: DegradationMark }>>({});
  useEffect(() => { alive.current = true; return () => { alive.current = false; }; }, []);
  useEffect(() => { setMarks({}); }, [options.connectionKey]);

  async function execute(account: Account, action: "recover" | "degradation_mark", mark?: DegradationMark) {
    const connection = current.current.connectionKey;
    const key = `${connection}:${account.id}:${action}`;
    if (!current.current.online || running.current.has(key)) return;
    if (action === "degradation_mark" && !mark?.version) return;
    const valid = () => alive.current && current.current.connectionKey === connection;
    running.current.add(key);
    redraw((value) => value + 1);
    try {
      if (action === "degradation_mark") {
        const result = await accountOperation<{ verified: boolean; degradation_mark: DegradationMark }>(
          account, action, { marked: !mark!.marked }, mark!.version);
        if (!valid()) return;
        if (!result.verified) throw new Error("降智标记保存未确认");
        setMarks((saved) => ({ ...saved, [account.id]: { base: account.degradation_mark?.version, value: result.degradation_mark } }));
        await command("refresh");
      } else {
        const result = await accountOperation<{ verified: boolean }>(account, action, {});
        if (valid()) current.current.report(result.verified ? `${account.name}：状态已恢复` : "恢复状态未确认，请刷新");
      }
    } catch (error) {
      if (valid()) current.current.report(error);
    } finally {
      running.current.delete(key);
      if (valid()) {
        redraw((value) => value + 1);
        if (action === "recover") void command("refresh").catch((error) => { if (valid()) current.current.report(error); });
      }
    }
  }

  return (account) => {
    const openai = account.platform === "openai" && ["oauth", "apikey"].includes(account.type);
    const testable = ["openai", "grok"].includes(account.platform) && ["oauth", "apikey"].includes(account.type);
    const pending = (action: string) => running.current.has(`${options.connectionKey}:${account.id}:${action}`);
    const mark = marks[account.id]?.base === account.degradation_mark?.version ? marks[account.id]?.value ?? account.degradation_mark : account.degradation_mark;
    const items: AccountActionItem[] = [];
    if (openai) items.push(
      { id: "model_test", label: "模型测试", icon: <Activity size={12}/>, className: "degradation-action", disabled: !options.online, run: () => options.modelTest(account) },
      { id: "degradation_mark", label: mark?.error ? "标记读取失败" : mark?.marked ? "取消标记" : "标记降智",
        icon: pending("degradation_mark") ? <LoaderCircle size={12} className="spin"/> : <BellOff size={12}/>, className: "degradation-action",
        title: mark?.error, disabled: !options.online || pending("degradation_mark") || !mark?.version,
        run: () => { void execute(account, "degradation_mark", mark); } },
      { id: "model_detection", label: "定时检测", icon: <Clock3 size={12}/>, className: "detection-action", disabled: !options.online, run: () => options.modelDetection(account) },
    );
    items.push({ id: "test", label: "测试连接", className: "test-button", disabled: !options.online || !testable, run: () => options.test(account) });
    if (account.recoverable) items.push({ id: "recover", label: "恢复状态", disabled: !options.online || pending("recover"),
      icon: pending("recover") ? <LoaderCircle size={12} className="spin"/> : undefined, run: () => { void execute(account, "recover"); } });
    if (openai) items.push({ id: "template", label: "应用模板", disabled: !options.online, run: () => options.template(account) });
    items.push({ id: "delete", label: "删除", className: "danger-text", disabled: !options.online, run: () => options.remove(account) });
    return items;
  };
}

const primaryActions = new Set(["model_test", "degradation_mark", "model_detection"]);
export const isPrimaryAction = (item: AccountActionItem) => primaryActions.has(item.id);

export function AccountActionButton({ item, disabled = false, after }: { item: AccountActionItem; disabled?: boolean; after?: () => void }) {
  return <button type="button" className={item.className} title={item.title} disabled={disabled || item.disabled}
    onClick={() => { item.run(); after?.(); }}>{item.icon}{item.label}</button>;
}

export default function AccountActions({ items, label, mode = "panel", active = true, contextKey, disabled = false }: {
  items: AccountActionItem[]; label: string; mode?: "panel" | "group"; active?: boolean; contextKey?: string; disabled?: boolean;
}) {
  const primary = mode === "panel" ? items.filter(isPrimaryAction) : [];
  const rest = mode === "panel" ? items.filter((item) => !isPrimaryAction(item)) : items;
  return <div className={mode === "panel" ? "account-actions" : "group-account-menu"}>
    {primary.map((item) => <AccountActionButton key={item.id} item={item} disabled={disabled}/>)}
    <AccountActionMenu label={label} iconOnly={mode === "group"} active={active} contextKey={contextKey} disabled={disabled}>
      {rest.map((item) => <AccountActionButton key={item.id} item={item} disabled={disabled}/>)}
    </AccountActionMenu>
  </div>;
}
