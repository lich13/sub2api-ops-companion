import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import {
  Activity,
  Link2,
  BellOff,
  Clock3,
  LoaderCircle,
  ListFilter,
  Network,
  Route,
  CircleCheck,
  RotateCcw,
  LayoutTemplate,
  Trash2,
} from "lucide-react";
import { accountOperation } from "./accountOperations";
import { command } from "./bridge";
import { supportsModelDetection } from "./modelTestTypes";
import AccountActionMenu from "./AccountActionMenu";
import type { Account, DegradationMark } from "./types";

export type AccountActionItem = {
  id: string;
  label: string;
  icon?: ReactNode;
  disabled?: boolean;
  pending?: boolean;
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
  keepalive?: (account: Account) => void;
  reauthorize?: (account: Account) => void;
  test: (account: Account) => void;
  template: (account: Account) => void;
  remove: (account: Account) => void;
  settings?: (
    account: Account,
    section: "whitelist" | "mappings" | "proxy",
  ) => void;
};

// Owned by the main window, so closing a popup or changing pages does not
// discard an accepted operation's refresh and feedback.
export function useAccountActions(
  options: Options,
): (account: Account) => AccountActionItem[] {
  const current = useRef(options);
  current.current = options;
  const alive = useRef(true);
  const running = useRef(new Set<string>());
  const [runningSnapshot, setRunningSnapshot] = useState<ReadonlySet<string>>(
    () => new Set(),
  );
  const [marks, setMarks] = useState<
    Record<number, { base?: string; value: DegradationMark }>
  >({});
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  useEffect(() => {
    setMarks((prev) => (Object.keys(prev).length === 0 ? prev : {}));
  }, [options.connectionKey]);

  const execute = useCallback(
    async (
      account: Account,
      action: "recover" | "degradation_mark",
      mark?: DegradationMark,
    ) => {
      const connection = current.current.connectionKey;
      const key = `${connection}:${account.id}:${action}`;
      if (!current.current.online || running.current.has(key)) return;
      if (action === "degradation_mark" && !mark?.version) return;
      const valid = () =>
        alive.current && current.current.connectionKey === connection;
      running.current.add(key);
      setRunningSnapshot(new Set(running.current));
      try {
        if (action === "degradation_mark") {
          const result = await accountOperation<{
            verified: boolean;
            degradation_mark: DegradationMark;
          }>(account, action, { marked: !mark!.marked }, mark!.version);
          if (!valid()) return;
          if (!result.verified) throw new Error("降智标记保存未确认");
          setMarks((saved) => ({
            ...saved,
            [account.id]: {
              base: account.degradation_mark?.version,
              value: result.degradation_mark,
            },
          }));
          await command("refresh");
        } else {
          const result = await accountOperation<{ verified: boolean }>(
            account,
            action,
            {},
          );
          if (valid())
            current.current.report(
              result.verified
                ? `${account.name}：状态已恢复`
                : "恢复状态未确认，请刷新",
            );
        }
      } catch (error) {
        if (valid()) current.current.report(error);
      } finally {
        running.current.delete(key);
        if (valid()) {
          setRunningSnapshot(new Set(running.current));
          if (action === "recover")
            void command("refresh").catch((error) => {
              if (valid()) current.current.report(error);
            });
        }
      }
    },
    [],
  );

  const { online, connectionKey } = options;
  const hasKeepalive = Boolean(options.keepalive);
  const hasReauthorize = Boolean(options.reauthorize);
  const hasSettings = Boolean(options.settings);

  return useCallback(
    (account: Account) => {
      const openai =
        account.platform === "openai" &&
        ["oauth", "apikey"].includes(account.type);
      const pending = (action: string) =>
        runningSnapshot.has(`${connectionKey}:${account.id}:${action}`);
      const mark =
        marks[account.id]?.base === account.degradation_mark?.version
          ? (marks[account.id]?.value ?? account.degradation_mark)
          : account.degradation_mark;
      const items: AccountActionItem[] = [];
      if (supportsModelDetection(account))
        items.push(
          {
            id: "model_test",
            label: "模型测试",
            icon: <Activity size={12} />,
            className: "degradation-action",
            disabled: !online,
            run: () => current.current.modelTest(account),
          },
          {
            id: "degradation_mark",
            label: mark?.error
              ? "标记读取失败"
              : mark?.marked
                ? "取消标记"
                : "标记降智",
            icon: pending("degradation_mark") ? (
              <LoaderCircle size={12} className="spin" />
            ) : (
              <BellOff size={12} />
            ),
            className: "degradation-action",
            title: mark?.error,
            disabled: !online || pending("degradation_mark") || !mark?.version,
            run: () => {
              void execute(account, "degradation_mark", mark);
            },
          },
          {
            id: "model_detection",
            label: "定时检测",
            icon: <Clock3 size={12} />,
            className: "detection-action",
            disabled: !online,
            run: () => current.current.modelDetection(account),
          },
        );
      items.push({
        id: "test",
        label: "测试连接",
        icon: <CircleCheck size={12} />,
        className: "test-button",
        disabled: !online,
        run: () => current.current.test(account),
      });
      if (hasKeepalive)
        items.push({
          id: "keepalive",
          label: "账号保活",
          icon: <Activity size={12} />,
          disabled: !online,
          run: () => current.current.keepalive?.(account),
        });
      if (
        account.platform === "openai" &&
        account.type === "oauth" &&
        hasReauthorize
      )
        items.push({
          id: "reauthorize",
          label: "重新授权",
          icon: <Link2 size={12} />,
          disabled: !online,
          run: () => current.current.reauthorize?.(account),
        });
      if (account.recoverable)
        items.push({
          id: "recover",
          label: "恢复状态",
          disabled: !online || pending("recover"),
          pending: pending("recover"),
          icon: pending("recover") ? (
            <LoaderCircle size={12} className="spin" />
          ) : (
            <RotateCcw size={12} />
          ),
          run: () => {
            void execute(account, "recover");
          },
        });
      if (openai)
        items.push({
          id: "template",
          label: "应用模板",
          icon: <LayoutTemplate size={12} />,
          disabled: !online,
          run: () => current.current.template(account),
        });
      if (hasSettings)
        items.push(
          {
            id: "model_whitelist",
            label: "模型白名单",
            icon: <ListFilter size={12} />,
            disabled: !online,
            run: () => current.current.settings?.(account, "whitelist"),
          },
          {
            id: "model_mapping",
            label: "模型映射",
            icon: <Route size={12} />,
            disabled: !online,
            run: () => current.current.settings?.(account, "mappings"),
          },
          {
            id: "proxy",
            label: "选择代理",
            icon: <Network size={12} />,
            disabled: !online,
            run: () => current.current.settings?.(account, "proxy"),
          },
        );
      items.push({
        id: "delete",
        label: "删除",
        icon: <Trash2 size={12} />,
        className: "danger-text",
        disabled: !online,
        run: () => current.current.remove(account),
      });
      return items;
    },
    [
      online,
      connectionKey,
      hasKeepalive,
      hasReauthorize,
      hasSettings,
      marks,
      runningSnapshot,
      execute,
    ],
  );
}

const primaryActions = new Set([
  "model_test",
  "degradation_mark",
  "model_detection",
]);
export const isPrimaryAction = (item: AccountActionItem) =>
  primaryActions.has(item.id);

export function AccountActionButton({
  item,
  disabled = false,
  after,
}: {
  item: AccountActionItem;
  disabled?: boolean;
  after?: () => void;
}) {
  return (
    <button
      type="button"
      className={item.className}
      title={item.title}
      disabled={disabled || item.disabled}
      onClick={() => {
        item.run();
        after?.();
      }}
    >
      <span className="account-action-icon" aria-hidden="true">
        {item.icon}
      </span>
      <span>{item.label}</span>
    </button>
  );
}

export default function AccountActions({
  items,
  label,
  active = true,
  contextKey,
  disabled = false,
}: {
  items: AccountActionItem[];
  label: string;
  active?: boolean;
  contextKey?: string;
  disabled?: boolean;
}) {
  return (
    <div className="group-account-menu">
      <AccountActionMenu
        label={label}
        iconOnly
        active={active}
        contextKey={contextKey}
        disabled={disabled}
      >
        {items.map((item) => (
          <AccountActionButton key={item.id} item={item} disabled={disabled} />
        ))}
      </AccountActionMenu>
    </div>
  );
}
