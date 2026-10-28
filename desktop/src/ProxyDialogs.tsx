import DiagnosticText from "./DiagnosticText";
import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { Eye, EyeOff, LoaderCircle, X } from "lucide-react";
import { api } from "./bridge";
import { useDialogFocus } from "./dialogFocus";
import { useBackAction } from "./mobile";
import { fullTime } from "./types";
import {
  proxyAddress,
  parseProxyLines,
  type ProxyInput,
  type ProxyRow,
  type QualityResult,
} from "./proxyData";
export function ProxyDialog({
  title,
  close,
  children,
  wide = false,
  busy = false,
}: {
  title: string;
  close: () => void;
  children: ReactNode;
  wide?: boolean;
  busy?: boolean;
}) {
  const ref = useRef<HTMLElement>(null);
  useDialogFocus(ref);
  useBackAction(true, () => {
    if (!busy) close();
  });
  return (
    <div
      className="modal-backdrop"
      onClick={() => {
        if (!busy) close();
      }}
    >
      <section
        ref={ref}
        className={`proxy-dialog${wide ? " wide" : ""}`}
        role="dialog"
        aria-modal="true"
        aria-label={title}
        onClick={(e) => e.stopPropagation()}
        onKeyDown={(e) => {
          if (e.key === "Escape" && !busy) {
            e.stopPropagation();
            close();
          }
        }}
      >
        <header>
          <h2>{title}</h2>
          <button
            className="icon-button"
            aria-label="关闭"
            disabled={busy}
            onClick={close}
          >
            <X size={19} />
          </button>
        </header>
        {children}
      </section>
    </div>
  );
}
function dateInput(value?: string | null) {
  if (!value) return "";
  const date = new Date(value);
  if (!Number.isFinite(date.getTime())) return "";
  return new Date(date.getTime() - date.getTimezoneOffset() * 60000)
    .toISOString()
    .slice(0, 16);
}
export function ProxyEditor({
  row,
  online,
  close,
  saved,
}: {
  row: ProxyRow | null;
  online: boolean;
  close: () => void;
  saved: () => void;
}) {
  const [form, setForm] = useState({
    name: row?.name ?? "",
    protocol: row?.protocol ?? "socks5",
    host: row?.host ?? "",
    port: String(row?.port ?? 1080),
    username: "",
    password: "",
    status: row?.status === "active" ? "active" : "inactive",
    expiry: dateInput(row?.expires_at),
    fallback: row?.fallback_mode ?? "none",
    backup: row?.backup_proxy_id ? String(row.backup_proxy_id) : "",
    warning: String(row?.expiry_warn_days ?? 7),
  });
  const [usernameDirty, setUsernameDirty] = useState(false),
    [expiryDirty, setExpiryDirty] = useState(false),
    [clearAuth, setClearAuth] = useState(false),
    [visible, setVisible] = useState(false);
  const [options, setOptions] = useState<ProxyRow[]>([]),
    [backupError, setBackupError] = useState("");
  const [busy, setBusy] = useState(false),
    [error, setError] = useState("");
  const alive = useRef(true),
    lock = useRef(false),
    connected = useRef(online);
  connected.current = online;
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  useEffect(() => {
    let current = true;
    if (form.fallback !== "proxy" || !online) return;
    setBackupError("");
    void (async () => {
      let page = 1,
        pages = 1;
      const items: ProxyRow[] = [];
      do {
        const data = await api<{ items: ProxyRow[]; pages: number }>(
          "GET",
          `/proxy-management?page=${page}&page_size=200&status=active`,
        );
        if (!current) return;
        items.push(...data.items);
        pages = data.pages;
        page++;
      } while (page <= pages);
      setOptions(items.filter((p) => p.id !== row?.id));
    })().catch(() => {
      if (current)
        setBackupError("备用代理读取失败，请重新选择到期处理方式后重试");
    });
    return () => {
      current = false;
    };
  }, [form.fallback, online, row?.id]);
  const field = (key: keyof typeof form, value: string) =>
    setForm((old) => ({ ...old, [key]: value }));
  async function submit(e: React.FormEvent) {
    e.preventDefault();
    if (!online || lock.current) return;
    const port = Number(form.port),
      warning = Number(form.warning);
    if (
      !form.name.trim() ||
      !form.host.trim() ||
      !Number.isInteger(port) ||
      port < 1 ||
      port > 65535 ||
      !Number.isInteger(warning) ||
      warning < 0
    ) {
      setError("请填写名称、地址、有效端口和非负提醒天数");
      return;
    }
    if (
      form.fallback === "proxy" &&
      (!form.backup || Number(form.backup) === row?.id)
    ) {
      setError("请选择备用代理");
      return;
    }
    const body: ProxyInput = {
      name: form.name.trim(),
      protocol: form.protocol,
      host: form.host.trim().replace(/^\[|\]$/g, ""),
      port,
      fallback_mode: form.fallback,
      backup_proxy_id: form.fallback === "proxy" ? Number(form.backup) : null,
      expiry_warn_days: warning,
    };
    if (row) body.status = form.status;
    if (!row || expiryDirty)
      body.expires_at = form.expiry
        ? Math.floor(Date.parse(form.expiry) / 1000)
        : null;
    if (clearAuth) {
      body.username = "";
      body.password = "";
    } else {
      if (!row || usernameDirty) body.username = form.username;
      if (!row || form.password) body.password = form.password;
    }
    lock.current = true;
    setBusy(true);
    setError("");
    try {
      await api(
        row ? "PUT" : "POST",
        row ? `/proxy-management/${row.id}` : "/proxy-management",
        body,
      );
      if (alive.current) saved();
    } catch (e) {
      if (alive.current) setError(String(e).replace(/^Error: /, ""));
    } finally {
      lock.current = false;
      if (alive.current) setBusy(false);
    }
  }
  return (
    <ProxyDialog title={row ? "编辑 IP" : "新增 IP"} close={close} busy={busy}>
      <form onSubmit={(e) => void submit(e)}>
        <fieldset disabled={!online || busy} className="proxy-form">
          <label className="field span-2">
            <span>名称</span>
            <input
              autoFocus
              required
              maxLength={160}
              value={form.name}
              onChange={(e) => field("name", e.target.value)}
            />
          </label>
          <label className="field">
            <span>协议</span>
            <select
              value={form.protocol}
              onChange={(e) => field("protocol", e.target.value)}
            >
              {["http", "https", "socks5", "socks5h"].map((v) => (
                <option key={v} value={v}>
                  {v.toUpperCase()}
                </option>
              ))}
            </select>
          </label>
          {row && (
            <label className="field">
              <span>状态</span>
              <select
                value={form.status}
                onChange={(e) => field("status", e.target.value)}
              >
                <option value="active">启用</option>
                <option value="inactive">停用</option>
              </select>
            </label>
          )}
          <label className="field proxy-host">
            <span>主机地址</span>
            <input
              required
              placeholder="proxy.example.invalid"
              value={form.host}
              onChange={(e) => field("host", e.target.value)}
            />
          </label>
          <label className="field">
            <span>端口</span>
            <input
              required
              type="number"
              min={1}
              max={65535}
              value={form.port}
              onChange={(e) => field("port", e.target.value)}
            />
          </label>
          <div className="proxy-form-divider span-2">认证</div>
          <label className="field">
            <span>用户名</span>
            <input
              autoComplete="off"
              disabled={clearAuth}
              value={form.username}
              placeholder={row?.username_set ? "已保存，未修改则保留" : "可选"}
              onChange={(e) => {
                setUsernameDirty(true);
                field("username", e.target.value);
              }}
            />
          </label>
          <label className="field">
            <span>密码</span>
            <div className="proxy-password">
              <input
                autoComplete="new-password"
                disabled={clearAuth}
                type={visible ? "text" : "password"}
                value={form.password}
                placeholder={row?.password_set ? "已保存，留空保留" : "可选"}
                onChange={(e) => field("password", e.target.value)}
              />
              <button
                type="button"
                className="icon-button"
                aria-label={visible ? "隐藏密码" : "显示密码"}
                onClick={() => setVisible((v) => !v)}
              >
                {visible ? <EyeOff size={15} /> : <Eye size={15} />}
              </button>
            </div>
          </label>
          {row && (
            <label className="proxy-check span-2">
              <input
                type="checkbox"
                checked={clearAuth}
                onChange={(e) => setClearAuth(e.target.checked)}
              />
              清除已保存的用户名和密码
            </label>
          )}
          <div className="proxy-form-divider span-2">有效期与到期处理</div>
          <label className="field">
            <span>到期时间</span>
            <input
              type="datetime-local"
              value={form.expiry}
              onChange={(e) => {
                setExpiryDirty(true);
                field("expiry", e.target.value);
              }}
            />
            <small className="muted">留空为长期有效</small>
          </label>
          <label className="field">
            <span>提前提醒（天）</span>
            <input
              type="number"
              min={0}
              value={form.warning}
              onChange={(e) => field("warning", e.target.value)}
            />
          </label>
          <label className="field">
            <span>到期处理</span>
            <select
              value={form.fallback}
              onChange={(e) => field("fallback", e.target.value)}
            >
              <option value="none">不切换</option>
              <option value="proxy">切换备用代理</option>
              <option value="direct">改为直连</option>
            </select>
          </label>
          {form.fallback === "proxy" && (
            <label className="field">
              <span>备用代理</span>
              <select
                value={form.backup}
                onChange={(e) => field("backup", e.target.value)}
              >
                <option value="">选择备用代理</option>
                {form.backup &&
                  !options.some((p) => String(p.id) === form.backup) && (
                    <option value={form.backup}>当前代理 #{form.backup}</option>
                  )}
                {options.map((p) => (
                  <option key={p.id} value={p.id}>
                    {p.name}
                  </option>
                ))}
              </select>
              {backupError && <small role="alert">{backupError}</small>}
            </label>
          )}
        </fieldset>
        {error && (
          <p className="bad-text" role="alert">
            {error}
          </p>
        )}
        {!online && (
          <p role="status" className="muted">
            连接已断开，草稿已保留
          </p>
        )}
        <footer>
          <button type="button" disabled={busy} onClick={close}>
            取消
          </button>
          <button type="submit" className="primary" disabled={!online || busy}>
            {busy && <LoaderCircle size={14} className="spin" />}
            {row ? "保存" : "新增"}
          </button>
        </footer>
      </form>
    </ProxyDialog>
  );
}
export function ProxyBatchAdd({
  online,
  close,
  saved,
}: {
  online: boolean;
  close: () => void;
  saved: (message: string) => void;
}) {
  const [text, setText] = useState(""),
    [busy, setBusy] = useState(false);
  const [results, setResults] = useState<
    { index: number; address: string; ok: boolean; message: string }[]
  >([]);
  const alive = useRef(true),
    lock = useRef(false),
    connected = useRef(online);
  connected.current = online;
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  const parsed = useMemo(() => parseProxyLines(text), [text]);
  async function submit() {
    if (
      !online ||
      lock.current ||
      results.length ||
      !parsed.proxies.length ||
      parsed.errors.length ||
      parsed.proxies.length > 1000
    )
      return;
    lock.current = true;
    setBusy(true);
    let next = 0,
      succeeded = 0;
    const slots: (typeof results[number] | undefined)[] = new Array(
      parsed.proxies.length,
    );
    const flushSlots = () => {
      if (alive.current)
        setResults(
          slots.filter((item): item is NonNullable<typeof item> => item !== undefined),
        );
    };
    await Promise.all(
      Array.from({ length: Math.min(5, parsed.proxies.length) }, async () => {
        while (
          next < parsed.proxies.length &&
          alive.current &&
          connected.current
        ) {
          const index = next++,
            proxy = parsed.proxies[index];
          let ok = false,
            message = "";
          try {
            const r = await api<{ created: number; skipped: number }>(
              "POST",
              "/proxy-management/batch",
              { proxies: [proxy] },
            );
            ok = r.created === 1;
            if (ok) ++succeeded;
            message = ok ? "已添加" : "已存在或未添加，请核对";
          } catch {
            message = "结果未确认，请刷新核对；未重试";
          }
          slots[index] = { index, address: proxyAddress(proxy), ok, message };
          flushSlots();
        }
      }),
    );
    if (alive.current) {
      for (let i = next; i < parsed.proxies.length; i++) {
        slots[i] = {
          index: i,
          address: proxyAddress(parsed.proxies[i]),
          ok: false,
          message: "连接已断开，未提交",
        };
      }
      flushSlots();
      setBusy(false);
    }
    lock.current = false;
    if (
      alive.current &&
      connected.current &&
      succeeded === parsed.proxies.length
    )
      saved(`已添加 ${succeeded} 个 IP`);
  }
  const done = () =>
    results.length
      ? saved(
          `添加完成：${results.filter((r) => r.ok).length} 个成功，${results.filter((r) => !r.ok).length} 个需核对`,
        )
      : close();
  return (
    <ProxyDialog title="批量添加 IP" close={done} busy={busy}>
      {!results.length && (
        <>
          <label className="field">
            <span>每行一个代理地址</span>
            <textarea
              aria-label="批量代理地址"
              spellCheck={false}
              autoComplete="off"
              rows={9}
              disabled={busy}
              placeholder="socks5://127.0.0.1:1080"
              value={text}
              onChange={(e) => setText(e.target.value)}
            />
          </label>
          <p className="muted">
            有效 {parsed.proxies.length} 个 · 重复 {parsed.duplicates} 个
          </p>
          {!!parsed.errors.length && (
            <p role="alert" className="bad-text">
              第 {parsed.errors.join("、")} 行格式无效
            </p>
          )}
          {parsed.proxies.length > 1000 && (
            <p role="alert">单次最多添加 1000 个</p>
          )}
        </>
      )}
      {!!results.length && (
        <ul className="proxy-result-list">
          {results.map((r) => (
            <li key={r.index}>
              <b>{r.address}</b>
              <div className={r.ok ? "good-text" : "bad-text"}>
                <DiagnosticText text={r.message} />
              </div>
            </li>
          ))}
        </ul>
      )}
      <footer>
        <button disabled={busy} onClick={done}>
          {results.length ? "完成" : "取消"}
        </button>
        {!results.length && (
          <button
            className="primary"
            disabled={
              !online ||
              busy ||
              !parsed.proxies.length ||
              !!parsed.errors.length ||
              parsed.proxies.length > 1000
            }
            onClick={() => void submit()}
          >
            {busy && <LoaderCircle size={14} className="spin" />}添加{" "}
            {parsed.proxies.length} 个
          </button>
        )}
        {busy && (
          <span role="status">
            {results.length} / {parsed.proxies.length}
          </span>
        )}
      </footer>
    </ProxyDialog>
  );
}
export function ProxyQualityReport({
  name,
  result,
  close,
}: {
  name: string;
  result: QualityResult;
  close: () => void;
}) {
  const labels = {
    pass: "通过",
    warn: "警告",
    fail: "失败",
    challenge: "挑战",
  };
  return (
    <ProxyDialog title={`${name} · 质量检测`} close={close} wide>
      <div className="proxy-quality-overview">
        <strong>
          {result.grade} <span>{result.score}/100</span>
        </strong>
        <DiagnosticText text={result.summary} />
        <div className="muted">
          {[
            result.exit_ip,
            result.country,
            result.base_latency_ms == null
              ? ""
              : `${result.base_latency_ms} ms`,
          ]
            .filter(Boolean)
            .join(" · ")}
        </div>
        {result.checked_at && (
          <time className="muted">
            {fullTime(new Date(result.checked_at * 1000).toISOString())}
          </time>
        )}
      </div>
      <div className="proxy-report-table">
        <table>
          <thead>
            <tr>
              <th>目标</th>
              <th>状态</th>
              <th>HTTP</th>
              <th>延迟</th>
              <th>详情</th>
            </tr>
          </thead>
          <tbody>
            {result.items.map((item, i) => (
              <tr key={`${item.target}:${i}`}>
                <td>{item.target}</td>
                <td>
                  <span
                    className={`proxy-badge ${item.status === "pass" ? "good" : "warning"}`}
                  >
                    {labels[item.status]}
                  </span>
                </td>
                <td>{item.http_status ?? "—"}</td>
                <td>
                  {item.latency_ms == null ? "—" : `${item.latency_ms} ms`}
                </td>
                <td>
                  {item.message ? <DiagnosticText text={item.message} /> : "—"}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      <footer>
        <button onClick={close}>关闭</button>
      </footer>
    </ProxyDialog>
  );
}
