import {
  createContext,
  useContext,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { Settings2, X } from "lucide-react";
import { api } from "./bridge";
import { useBackAction } from "./mobile";
import { useDialogFocus } from "./dialogFocus";
import type { Account } from "./types";

type Settings = { seconds: number; revision: string };
type TokenSettings = {
  account_id: number;
  has_access_token: boolean;
  revision: string;
};
const Context = createContext({
  accounts: [] as Account[],
  seconds: 60,
  ready: true,
  visible: true,
  revision: "",
  error: "",
  reload: () => {},
  saved: (_value: Settings) => {},
});
export const useKeyQuotaSettings = () => useContext(Context);

export function KeyQuotaProvider({
  online,
  active,
  enabled,
  accounts = [],
  children,
}: {
  online: boolean;
  active: boolean;
  enabled: boolean;
  accounts?: Account[];
  children: ReactNode;
}) {
  const [value, setValue] = useState<Settings | null>(null),
    [error, setError] = useState("");
  const [visible, setVisible] = useState(document.visibilityState !== "hidden"),
    [retry, setRetry] = useState(0);
  useEffect(() => {
    const changed = () => setVisible(document.visibilityState !== "hidden");
    document.addEventListener("visibilitychange", changed);
    return () => document.removeEventListener("visibilitychange", changed);
  }, []);
  useEffect(() => {
    if (!online || !active || !visible || !enabled) return;
    let alive = true;
    void api<Settings>("GET", "/config/key-quota")
      .then((v) => {
        if (alive) {
          setValue(v);
          setError("");
        }
      })
      .catch((e) => {
        if (alive) setError(String(e).replace(/^Error: /, ""));
      });
    return () => {
      alive = false;
    };
  }, [online, active, visible, enabled, retry]);
  return (
    <Context.Provider
      value={{
        accounts,
        seconds: value?.seconds ?? 60,
        revision: value?.revision ?? "",
        ready: !!value && !error,
        visible: visible && active,
        error,
        reload: () => setRetry((n) => n + 1),
        saved: (v) => {
          setValue(v);
          setError("");
        },
      }}
    >
      {children}
    </Context.Provider>
  );
}

export function KeyQuotaRefreshSettings({ online }: { online: boolean }) {
  const context = useKeyQuotaSettings();
  const [open, setOpen] = useState(false);
  return (
    <>
      <button disabled={!online} onClick={() => setOpen(true)}>
        <Settings2 size={14} />
        余额刷新
      </button>
      {context.error && (
        <span role="status" className="bad-text">
          余额设置读取失败 <button onClick={context.reload}>重试</button>
        </span>
      )}
      {open && (
        <KeyQuotaSettingsDialog
          online={online}
          close={() => setOpen(false)}
          saved={(v) => context.saved(v as Settings)}
        />
      )}
    </>
  );
}

export function KeyQuotaSettingsDialog({
  account,
  online,
  close,
  saved,
}: {
  account?: Account;
  online: boolean;
  close: () => void;
  saved: (value: Settings | TokenSettings) => void;
}) {
  const [value, setValue] = useState<Settings | TokenSettings | null>(null),
    [seconds, setSeconds] = useState("60"),
    [enabled, setEnabled] = useState(true);
  const [token, setToken] = useState(""),
    [clear, setClear] = useState(false),
    [busy, setBusy] = useState(false),
    [error, setError] = useState("");
  const alive = useRef(true),
    guard = useRef(false),
    dialog = useRef<HTMLElement>(null);
  useDialogFocus(dialog);
  const dismiss = () => {
    if (!guard.current) close();
  };
  useBackAction(true, dismiss);
  const path = account
    ? `/accounts/${account.id}/key-quota-settings`
    : "/config/key-quota";
  async function read() {
    setError("");
    try {
      const v = await api<Settings | TokenSettings>("GET", path);
      if (alive.current) {
        setValue(v);
        if ("seconds" in v) {
          setEnabled(v.seconds !== 0);
          setSeconds(String(v.seconds || 60));
        }
      }
    } catch (e) {
      if (alive.current) setError(String(e).replace(/^Error: /, ""));
    }
  }
  useEffect(() => {
    alive.current = true;
    void read();
    return () => {
      alive.current = false;
    };
  }, [path]);
  async function save() {
    if (!value || !online || guard.current) return;
    const n = enabled ? Number(seconds) : 0;
    if (!account && enabled && (!Number.isInteger(n) || n < 10 || n > 86400)) {
      setError("余额刷新间隔需为 10–86400 秒");
      return;
    }
    guard.current = true;
    setBusy(true);
    setError("");
    try {
      const v = await api<Settings | TokenSettings>("PUT", path, {
        expected_revision: value.revision,
        ...(account
          ? clear
            ? { access_token: "" }
            : token.trim()
              ? { access_token: token.trim() }
              : {}
          : { seconds: n }),
      });
      if (alive.current) {
        setToken("");
        close();
        saved(v);
      }
    } catch (e) {
      if (alive.current) setError(String(e).replace(/^Error: /, ""));
    } finally {
      guard.current = false;
      if (alive.current) setBusy(false);
    }
  }
  return (
    <div className="modal-backdrop" onClick={dismiss}>
      <section
        ref={dialog}
        className="modal key-quota-settings"
        role="dialog"
        aria-modal="true"
        aria-label={account ? "余额查询设置" : "余额刷新设置"}
        onClick={(e) => e.stopPropagation()}
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            e.stopPropagation();
            dismiss();
          }
        }}
      >
        <header>
          <h2>{account ? "余额查询设置" : "余额刷新设置"}</h2>
          <button
            className="icon-button"
            aria-label="关闭余额设置"
            disabled={busy}
            onClick={dismiss}
          >
            <X size={18} />
          </button>
        </header>
        <form
          onSubmit={(e) => {
            e.preventDefault();
            void save();
          }}
        >
          {account ? (
            <>
              <p>{account.name}</p>
              <label className="field">
                <span>New API 账户访问令牌</span>
                <input
                  aria-label="账户访问令牌"
                  type="password"
                  autoComplete="off"
                  value={token}
                  disabled={busy || !online}
                  placeholder={
                    clear
                      ? "保存后清除"
                      : value &&
                          "has_access_token" in value &&
                          value.has_access_token
                        ? "已配置，留空保留"
                        : "可选，用于查询账户余额"
                  }
                  onChange={(e) => {
                    setToken(e.target.value);
                    setClear(false);
                  }}
                />
              </label>
              <button
                type="button"
                disabled={busy || !online}
                onClick={() => {
                  setToken("");
                  setClear(true);
                }}
              >
                清除账户访问令牌
              </button>
            </>
          ) : (
            <>
              <label className="switch-row">
                <span>余额自动刷新</span>
                <input
                  type="checkbox"
                  checked={enabled}
                  disabled={busy || !online}
                  onChange={(e) => setEnabled(e.target.checked)}
                />
              </label>
              {enabled && (
                <label className="field">
                  <span>间隔 / 秒</span>
                  <input
                    aria-label="余额刷新间隔"
                    type="number"
                    min={10}
                    max={86400}
                    step={1}
                    value={seconds}
                    disabled={busy || !online}
                    onChange={(e) => setSeconds(e.target.value)}
                  />
                </label>
              )}
            </>
          )}
          {error && (
            <p role="alert" className="bad-text">
              {error}{" "}
              <button type="button" disabled={busy} onClick={() => void read()}>
                重新读取
              </button>
            </p>
          )}
          <footer>
            <button type="button" disabled={busy} onClick={dismiss}>
              取消
            </button>
            <button className="primary" disabled={busy || !online || !value}>
              {busy ? "保存中…" : "保存"}
            </button>
          </footer>
        </form>
      </section>
    </div>
  );
}
