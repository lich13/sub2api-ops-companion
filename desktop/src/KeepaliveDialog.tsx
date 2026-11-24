import { useEffectEvent } from "react";
import { useEffect, useRef, useState } from "react";
import { LoaderCircle, X } from "lucide-react";
import { api, command } from "./bridge";
import { ignoredFailure } from "./bridgeError";
import { useDialogFocus } from "./dialogFocus";
import { useBackAction } from "./mobile";
import { fullTime, type Account, type AccountKeepalive } from "./types";

const labels: Record<string, string> = {
  disabled: "未启用",
  waiting: "等待检测",
  paused: "已暂停",
  running: "恢复中",
  needs_confirmation: "待核对",
};
const message = (error: unknown) =>
  error instanceof Error ? error.message : String(error);

export default function KeepaliveDialog({
  account,
  online,
  connectionKey,
  close,
  report,
}: {
  account: Account;
  online: boolean;
  connectionKey: string;
  close: () => void;
  report: (error: unknown) => void;
}) {
  const [value, setValue] = useState<AccountKeepalive | null>(null);
  const [draft, setDraft] = useState<AccountKeepalive | null>(null);
  const [loading, setLoading] = useState(online),
    [busy, setBusy] = useState(false),
    [error, setError] = useState("");
  const epoch = useRef(0),
    revision = useRef(0),
    saving = useRef(false);
  const identity = useRef(""),
    closeAction = useRef(close),
    dialogRef = useRef<HTMLElement>(null);
  closeAction.current = close;
  const dismiss = () => {
    epoch.current = epoch.current + 1;
    revision.current = revision.current + 1;
    closeAction.current();
  };
  useDialogFocus(dialogRef);
  useBackAction(true, dismiss);

  async function read() {
    if (!online || saving.current) return;
    const current = epoch.current,
      request = ++revision.current;
    setLoading(true);
    setError("");
    try {
      const result = await api<AccountKeepalive>(
        "GET",
        `/accounts/${account.id}/keepalive`,
      );
      if (current === epoch.current && request === revision.current) {
        setValue(result);
        setDraft(result);
      }
    } catch (e) {
      if (current === epoch.current && request === revision.current) {
        setError(message(e));
        setValue(null);
        setDraft(null);
      }
    } finally {
      if (current === epoch.current && request === revision.current)
        setLoading(false);
    }
  }
  const readOnChange = useEffectEvent(read);
  useEffect(() => {
    epoch.current = epoch.current + 1;
    revision.current = revision.current + 1;
    saving.current = false;
    const nextIdentity = `${connectionKey}:${account.id}`;
    if (identity.current !== nextIdentity) {
      setValue(null);
      setDraft(null);
      identity.current = nextIdentity;
    }
    setBusy(false);
    setError("");
    setLoading(online);
    if (online) void readOnChange();
    return () => {
      epoch.current = epoch.current + 1;
      revision.current = revision.current + 1;
    };
  }, [account.id, connectionKey, online]);
  const loaded = !!value;
  useEffect(() => {
    if (!online || !loaded) return;
    let alive = true,
      hadPollError = false,
      timer: ReturnType<typeof setTimeout>;
    const current = epoch.current;
    const poll = async () => {
      const request = revision.current;
      if (!saving.current) {
        try {
          const result = await api<AccountKeepalive>(
            "GET",
            `/accounts/${account.id}/keepalive`,
          );
          if (
            alive &&
            current === epoch.current &&
            request === revision.current &&
            !saving.current
          ) {
            setValue(result);
            if (hadPollError) {
              hadPollError = false;
              setError("");
            }
          }
        } catch (err) {
          if (
            alive &&
            current === epoch.current &&
            request === revision.current &&
            !saving.current &&
            !ignoredFailure(err)
          ) {
            hadPollError = true;
            setError(message(err));
          }
        }
      }
      if (alive) timer = setTimeout(() => void poll(), 2000);
    };
    timer = setTimeout(() => void poll(), 2000);
    return () => {
      alive = false;
      clearTimeout(timer);
    };
  }, [account.id, connectionKey, online, loaded]);
  async function save() {
    if (!draft || !value || !online || saving.current) return;
    saving.current = true;
    setBusy(true);
    setError("");
    const current = epoch.current,
      request = ++revision.current;
    try {
      const result = await api<AccountKeepalive>(
        "PUT",
        `/accounts/${account.id}/keepalive`,
        {
          enabled: draft.enabled,
          expected_version: draft.version,
        },
      );
      if (current === epoch.current && request === revision.current) {
        setValue(result);
        setDraft(result);
        dismiss();
        void command("refresh").catch(report);
      }
    } catch (e) {
      if (current === epoch.current && request === revision.current) {
        setError(message(e));
        report(e);
      }
    } finally {
      if (current === epoch.current && request === revision.current) {
        saving.current = false;
        setBusy(false);
      }
    }
  }
  const disabled = loading || busy || !online || !value;
  return (
    <div className="modal-backdrop" onClick={dismiss}>
      <section
        ref={dialogRef}
        className="model-detection-dialog"
        role="dialog"
        aria-modal="true"
        aria-label="账号保活"
        tabIndex={-1}
        onClick={(event) => event.stopPropagation()}
        onKeyDown={(event) => {
          if (event.key === "Escape" && !event.nativeEvent.isComposing) {
            event.preventDefault();
            event.stopPropagation();
            dismiss();
          }
        }}
      >
        <header>
          <div>
            <span className="eyebrow">
              {account.name} #{account.id}
            </span>
            <h2>账号保活</h2>
          </div>
          <button
            type="button"
            className="icon-button"
            aria-label="关闭账号保活"
            onClick={dismiss}
          >
            <X size={19} />
          </button>
        </header>
        <p className="muted">
          检测到符合条件的临时不可调度时尝试恢复。原生恢复也会清除模型级限制；不会打开已关闭的调度或解除降智标记。
        </p>
        {loading && (
          <LoaderCircle className="spin" aria-label="正在读取保活设置" />
        )}
        {!online && <p className="muted">连接离线，恢复连接后可查看和修改。</p>}
        {draft && value && (
          <>
            <label className="switch-row">
              <span>启用账号保活</span>
              <input
                type="checkbox"
                checked={draft.enabled}
                disabled={disabled}
                onChange={(event) =>
                  setDraft({ ...draft, enabled: event.target.checked })
                }
              />
            </label>
            <dl className="detection-meta">
              <dt>状态</dt>
              <dd>{labels[value.status]}</dd>
              <dt>最近恢复</dt>
              <dd>
                {value.last_restore_at
                  ? fullTime(value.last_restore_at)
                  : "暂无记录"}
              </dd>
              <dt>近一小时</dt>
              <dd>{value.restore_count}/5 次已确认恢复</dd>
              {(value.reason || value.paused_reason) && (
                <>
                  <dt>原因</dt>
                  <dd>{value.reason || value.paused_reason}</dd>
                </>
              )}
              {value.next_allowed_at && (
                <>
                  <dt>等待至</dt>
                  <dd>{fullTime(value.next_allowed_at)}</dd>
                </>
              )}
            </dl>
            <p className="muted">
              两次恢复至少间隔 60 秒，一小时最多 5
              次。达到上限后暂停，需检查账号并手动重新开启。
            </p>
          </>
        )}
        {error && (
          <p className="bad-text" role="alert">
            {error}
          </p>
        )}
        <footer>
          <button
            type="button"
            disabled={busy || loading || !online}
            onClick={() => void read()}
          >
            重新读取
          </button>
          <button
            type="button"
            className="primary"
            disabled={disabled || !draft}
            onClick={() => void save()}
          >
            {busy && <LoaderCircle size={15} className="spin" />}保存
          </button>
        </footer>
      </section>
    </div>
  );
}
