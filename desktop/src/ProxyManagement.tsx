import { useCallback, useEffect, useRef, useState } from "react";
import {
  ArrowDown,
  ArrowUp,
  Check,
  Copy,
  Download,
  Eye,
  LoaderCircle,
  MoreHorizontal,
  Pencil,
  Plus,
  RefreshCw,
  Search,
  ShieldCheck,
  Trash2,
  Upload,
} from "lucide-react";
import { api, command } from "./bridge";
import { fullTime } from "./types";
import {
  ProxyBatchAdd,
  ProxyDialog,
  ProxyEditor,
  ProxyQualityReport,
} from "./ProxyDialogs";
import {
  exportProxyFile,
  parseImport,
  proxyAddress,
  proxyUrl,
  readProxyFile,
  type ConnectionResult,
  type ImportData,
  type ImportResult,
  type ProxyAccount,
  type ProxyCredentials,
  type ProxyPage,
  type ProxyRow,
  type QualityResult,
} from "./proxyData";
import "./proxy-management.css";

type Dialog =
  | { kind: "edit"; row: ProxyRow | null }
  | { kind: "batch" }
  | { kind: "delete"; rows: ProxyRow[] }
  | { kind: "export" }
  | { kind: "import"; data: ImportData }
  | { kind: "accounts"; row: ProxyRow; accounts: ProxyAccount[] }
  | { kind: "credentials"; row: ProxyRow; value: ProxyCredentials }
  | { kind: "quality"; row: ProxyRow; value: QualityResult }
  | {
      kind: "results";
      title: string;
      rows: { name: string; ok: boolean; message: string }[];
    };
const emptyPage: ProxyPage = {
  items: [],
  page: 1,
  page_size: 20,
  total: 0,
  pages: 0,
};
const states: Record<string, string> = {
  active: "启用",
  inactive: "停用",
  expired: "已到期",
  error: "异常",
};
const qualityLabels: Record<string, string> = {
  healthy: "优质",
  warn: "警告",
  challenge: "挑战",
  failed: "失败",
  fail: "失败",
};
const errorText = (e: unknown) => String(e).replace(/^Error: /, "");
export default function ProxyManagement({
  active,
  online,
  connectionKey,
}: {
  active: boolean;
  online: boolean;
  connectionKey: string;
}) {
  const [data, setData] = useState<ProxyPage>(emptyPage),
    [search, setSearch] = useState(""),
    [protocol, setProtocol] = useState(""),
    [status, setStatus] = useState("");
  const [page, setPage] = useState(1),
    [size, setSize] = useState(20),
    [sort, setSort] = useState("id"),
    [order, setOrder] = useState("desc"),
    [selected, setSelected] = useState(new Set<number>());
  const [loading, setLoading] = useState(false),
    [error, setError] = useState(""),
    [message, setMessage] = useState(""),
    [busy, setBusy] = useState(""),
    [dialog, setDialog] = useState<Dialog | null>(null),
    [menu, setMenu] = useState(false);
  const [diagnostics, setDiagnostics] = useState<
    Record<number, { connection?: ConnectionResult; quality?: QualityResult }>
  >({});
  const menuRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!menu) return;
    const dismiss = (e: PointerEvent) => {
      if (!menuRef.current?.contains(e.target as Node)) setMenu(false);
    };
    const escape = (e: KeyboardEvent) => {
      if (e.key === "Escape") setMenu(false);
    };
    document.addEventListener("pointerdown", dismiss);
    document.addEventListener("keydown", escape);
    return () => {
      document.removeEventListener("pointerdown", dismiss);
      document.removeEventListener("keydown", escape);
    };
  }, [menu]);
  const alive = useRef(true),
    generation = useRef(0),
    readGeneration = useRef(0),
    operationLock = useRef(false),
    current = useRef({ online, connectionKey });
  current.current = { online, connectionKey };
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
      ++generation.current;
      ++readGeneration.current;
    };
  }, []);
  useEffect(() => {
    ++generation.current;
    ++readGeneration.current;
    setData(emptyPage);
    setSelected(new Set());
    setDialog(null);
    setDiagnostics({});
    setBusy("");
    setError("");
    operationLock.current = false;
  }, [connectionKey]);
  useEffect(() => {
    if (!online) {
      ++generation.current;
      ++readGeneration.current;
      setLoading(false);
      setMenu(false);
      setBusy("");
      operationLock.current = false;
      setDiagnostics({});
      setDialog((d) => (d?.kind === "credentials" ? null : d));
    }
  }, [online]);
  const query = useCallback(
    (exporting = false) => {
      const q = new URLSearchParams({ sort_by: sort, sort_order: order });
      if (!exporting) {
        q.set("page", String(page));
        q.set("page_size", String(size));
      }
      if (search.trim()) q.set("search", search.trim());
      if (protocol) q.set("protocol", protocol);
      if (status) q.set("status", status);
      return q.toString();
    },
    [page, size, search, protocol, status, sort, order],
  );
  const load = useCallback(async () => {
    if (!active || !online) return;
    const token = ++readGeneration.current;
    setLoading(true);
    setError("");
    try {
      const r = await api<ProxyPage>("GET", `/proxy-management?${query()}`);
      if (!alive.current || token !== readGeneration.current) return;
      if (r.pages > 0 && page > r.pages) {
        setPage(r.pages);
        return;
      }
      setData(r);
      setSelected(
        (old) =>
          new Set([...old].filter((id) => r.items.some((p) => p.id === id))),
      );
    } catch (e) {
      if (alive.current && token === readGeneration.current)
        setError(errorText(e));
    } finally {
      if (alive.current && token === readGeneration.current) setLoading(false);
    }
  }, [active, online, query, page]);
  useEffect(() => {
    if (!active || !online) return;
    const timer = window.setTimeout(() => void load(), 180);
    return () => {
      window.clearTimeout(timer);
      ++readGeneration.current;
    };
  }, [active, online, load]);
  function filter(set: (value: string) => void, value: string) {
    set(value);
    setPage(1);
    setSelected(new Set());
  }
  function sortBy(key: string) {
    setSort(key);
    setOrder(sort === key && order === "asc" ? "desc" : "asc");
    setPage(1);
  }
  async function operate(
    label: string,
    fn: (valid: () => boolean) => Promise<void>,
  ) {
    if (!online || operationLock.current) return;
    operationLock.current = true;
    const token = generation.current,
      key = connectionKey;
    setBusy(label);
    setError("");
    setMessage("");
    setMenu(false);
    const valid = () =>
      alive.current &&
      token === generation.current &&
      current.current.online &&
      current.current.connectionKey === key;
    try {
      await fn(valid);
    } catch (e) {
      if (valid()) setError(errorText(e));
    } finally {
      if (valid()) {
        operationLock.current = false;
        setBusy("");
      }
    }
  }
  const selectedRows = data.items.filter((p) => selected.has(p.id));
  function toggle(id: number) {
    setSelected((old) => {
      const next = new Set(old);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }
  async function diagnose(rows: ProxyRow[], action: "test" | "quality-check") {
    await operate(
      action === "test" ? "测试连接" : "质量检测",
      async (valid) => {
        const results: { name: string; ok: boolean; message: string }[] = [];
        let next = 0;
        await Promise.all(
          Array.from({ length: Math.min(5, rows.length) }, async () => {
            while (next < rows.length && valid()) {
              const row = rows[next++];
              try {
                if (action === "test") {
                  const result = await api<ConnectionResult>(
                    "POST",
                    `/proxies/${row.id}/test`,
                  );
                  if (!valid()) return;
                  setDiagnostics((old) => ({
                    ...old,
                    [row.id]: { ...old[row.id], connection: result },
                  }));
                  results.push({
                    name: row.name,
                    ok: result.success,
                    message: result.success
                      ? `连接成功${result.latency_ms == null ? "" : ` · ${result.latency_ms} ms`}`
                      : result.message || "连接失败",
                  });
                } else {
                  const result = await api<QualityResult>(
                    "POST",
                    `/proxies/${row.id}/quality-check`,
                  );
                  if (!valid()) return;
                  setDiagnostics((old) => ({
                    ...old,
                    [row.id]: { ...old[row.id], quality: result },
                  }));
                  results.push({
                    name: row.name,
                    ok: true,
                    message: `${result.grade} · ${result.score}/100`,
                  });
                  if (rows.length === 1)
                    setDialog({ kind: "quality", row, value: result });
                }
              } catch (e) {
                results.push({
                  name: row.name,
                  ok: false,
                  message: errorText(e),
                });
              }
            }
          }),
        );
        if (!valid()) return;
        if (rows.length > 1 || results.some((r) => !r.ok))
          setDialog({
            kind: "results",
            title: `${action === "test" ? "连接测试" : "质量检测"}结果`,
            rows: results,
          });
        else if (action === "test")
          setMessage(results[0]?.message ?? "测试完成");
        await load();
      },
    );
  }
  async function copy(row: ProxyRow, credentials: boolean) {
    await operate("复制", async (valid) => {
      const value = credentials
        ? await api<ProxyCredentials>(
            "GET",
            `/proxy-management/${row.id}/credentials`,
          )
        : undefined;
      if (!valid()) return;
      await navigator.clipboard.writeText(
        credentials ? proxyUrl(row, value) : proxyAddress(row),
      );
      if (valid()) setMessage("已复制");
    });
  }
  async function remove(rows: ProxyRow[]) {
    await operate("删除", async (valid) => {
      if (rows.length === 1) {
        await api("DELETE", `/proxy-management/${rows[0].id}`);
        if (valid()) {
          setDialog(null);
          setSelected((old) => {
            const next = new Set(old);
            next.delete(rows[0].id);
            return next;
          });
          setMessage("已删除");
        }
      } else {
        const result = await api<{
          deleted_ids: number[];
          skipped: { id: number; reason: string }[];
        }>("POST", "/proxy-management/batch-delete", {
          ids: rows.map((p) => p.id),
        });
        if (!valid()) return;
        setSelected(new Set(result.skipped.map((p) => p.id)));
        setDialog({
          kind: "results",
          title: "批量删除结果",
          rows: rows.map((p) => ({
            name: p.name,
            ok: result.deleted_ids.includes(p.id),
            message: result.deleted_ids.includes(p.id)
              ? "已删除"
              : result.skipped.find((s) => s.id === p.id)?.reason ||
                "删除未确认",
          })),
        });
      }
      if (valid()) {
        await load();
        void command("refresh").catch(() => {});
      }
    });
  }
  async function importFile() {
    await operate("读取文件", async (valid) => {
      const text = await readProxyFile();
      if (text && valid())
        setDialog({ kind: "import", data: parseImport(text) });
    });
  }
  async function importData(value: ImportData) {
    await operate("导入", async (valid) => {
      const r = await api<ImportResult>("POST", "/proxy-management/data", {
        data: value,
      });
      if (!valid()) return;
      setMessage(
        `新增 ${r.proxy_created} 个，复用 ${r.proxy_reused} 个，失败 ${r.proxy_failed} 个`,
      );
      setDialog(
        r.errors.length
          ? {
              kind: "results",
              title: "导入结果",
              rows: r.errors.map((e) => ({
                name: e.name || "代理",
                ok: false,
                message: e.message,
              })),
            }
          : null,
      );
      await load();
    });
  }
  function saved(message = "已保存") {
    setDialog(null);
    setMessage(message);
    void load();
    void command("refresh").catch(() => {});
  }
  const blocked = !online || !!busy;
  const sortTitle = (label: string, key: string) => (
    <button
      className="proxy-sort"
      disabled={!!busy}
      onClick={() => sortBy(key)}
    >
      {label}
      {sort === key &&
        (order === "asc" ? <ArrowUp size={12} /> : <ArrowDown size={12} />)}
    </button>
  );
  function quality(row: ProxyRow) {
    const q = diagnostics[row.id]?.quality;
    const c = diagnostics[row.id]?.connection;
    const latency = c?.latency_ms ?? row.latency_ms;
    const grade = q?.grade ?? row.quality_grade;
    const score = q?.score ?? row.quality_score;
    const qualityStatus = q
      ? q.items.some((i) => i.status === "fail")
        ? "failed"
        : q.items.some((i) => i.status === "challenge")
          ? "challenge"
          : q.items.some((i) => i.status === "warn")
            ? "warn"
            : "healthy"
      : (row.quality_status ?? "");
    return (
      <div className="proxy-quality-cell">
        <span
          className={`proxy-badge ${c?.success === false || row.latency_status === "failed" ? "bad" : latency == null ? "muted" : latency > 1000 ? "warning" : "good"}`}
        >
          {c?.success === false
            ? "连接失败"
            : latency == null
              ? "未测试"
              : `${latency} ms`}
        </span>
        {grade && (
          <button
            className="proxy-quality-link"
            disabled={!q}
            onClick={() => {
              if (q) setDialog({ kind: "quality", row, value: q });
            }}
          >
            质量 {grade}/{score ?? "—"}
            <span
              className={
                qualityStatus === "healthy"
                  ? "good"
                  : qualityStatus === "failed" || qualityStatus === "fail"
                    ? "bad"
                    : "warning"
              }
            >
              {qualityLabels[qualityStatus] ?? ""}
            </span>
          </button>
        )}
      </div>
    );
  }
  function location(row: ProxyRow) {
    const c = diagnostics[row.id]?.connection;
    return (
      [c?.country || row.country, c?.region || row.region, c?.city || row.city]
        .filter((v, i, a) => v && a.indexOf(v) === i)
        .join(" · ") || "—"
    );
  }
  function rowActions(row: ProxyRow) {
    return (
      <div className="proxy-row-actions">
        <button title="测试连接" aria-label="测试连接" disabled={blocked} onClick={() => void diagnose([row], "test")}>
          <Check size={15} />
          测试
        </button>
        <button
          disabled={blocked}
          onClick={() => void diagnose([row], "quality-check")}
          title="质量检测" aria-label="质量检测"
        >
          <ShieldCheck size={15} />
          质量
        </button>
        <button
          disabled={blocked}
          onClick={() => setDialog({ kind: "edit", row })}
        >
          <Pencil size={15} />
          编辑
        </button>
        <button
          disabled={blocked}
          className="danger-text"
          onClick={() => setDialog({ kind: "delete", rows: [row] })}
        >
          <Trash2 size={15} />
          删除
        </button>
      </div>
    );
  }
  function accounts(row: ProxyRow) {
    void operate("关联账号", async (valid) => {
      const value = await api<ProxyAccount[]>(
        "GET",
        `/proxy-management/${row.id}/accounts`,
      );
      if (valid()) setDialog({ kind: "accounts", row, accounts: value });
    });
  }
  function auth(row: ProxyRow) {
    return row.username_set || row.password_set ? (
      <button
        className="proxy-auth"
        disabled={blocked}
        aria-label={`查看 ${row.name} 认证`}
        onClick={() =>
          void operate("读取认证", async (valid) => {
            const value = await api<ProxyCredentials>(
              "GET",
              `/proxy-management/${row.id}/credentials`,
            );
            if (valid()) setDialog({ kind: "credentials", row, value });
          })
        }
      >
        •••••• <Eye size={14} />
      </button>
    ) : (
      <span className="muted">—</span>
    );
  }
  function statusButton(row: ProxyRow) {
    return (
      <button
        className={`proxy-status ${row.status === "active" ? "good" : "muted"}`}
        disabled={blocked}
        aria-label={`${row.status === "active" ? "停用" : "启用"} ${row.name}`}
        onClick={() =>
          void operate("修改状态", async (valid) => {
            await api("PUT", `/proxy-management/${row.id}`, {
              status: row.status === "active" ? "inactive" : "active",
            });
            if (valid()) {
              setMessage("状态已更新");
              await load();
            }
          })
        }
      >
        {states[row.status] || row.status}
      </button>
    );
  }
  return (
    <section className="proxy-management" aria-label="IP 管理">
      <div className="proxy-filterbar">
        <label className="proxy-search">
          <Search size={15} />
          <input
            type="search"
            aria-label="搜索 IP"
            disabled={!!busy}
            placeholder="搜索名称或地址"
            maxLength={100}
            value={search}
            onChange={(e) => filter(setSearch, e.target.value)}
          />
        </label>
        <select
          aria-label="筛选协议"
          disabled={!!busy}
          value={protocol}
          onChange={(e) => filter(setProtocol, e.target.value)}
        >
          <option value="">全部协议</option>
          {["http", "https", "socks5", "socks5h"].map((v) => (
            <option key={v} value={v}>
              {v.toUpperCase()}
            </option>
          ))}
        </select>
        <select
          aria-label="筛选状态"
          disabled={!!busy}
          value={status}
          onChange={(e) => filter(setStatus, e.target.value)}
        >
          <option value="">全部状态</option>
          {["active", "inactive", "expired"].map((v) => (
            <option key={v} value={v}>
              {states[v]}
            </option>
          ))}
        </select>
        <select
          className="proxy-mobile-sort"
          aria-label="IP 排序"
          disabled={!!busy}
          value={`${sort}:${order}`}
          onChange={(e) => {
            const [key, direction] = e.target.value.split(":");
            setSort(key);
            setOrder(direction);
            setPage(1);
            setSelected(new Set());
          }}
        >
          <option value="id:desc">最新添加</option>
          <option value="id:asc">最早添加</option>
          <option value="name:asc">名称 ↑</option>
          <option value="name:desc">名称 ↓</option>
          <option value="protocol:asc">协议 ↑</option>
          <option value="protocol:desc">协议 ↓</option>
          <option value="account_count:desc">账号数 ↓</option>
          <option value="account_count:asc">账号数 ↑</option>
        </select>
        <span className="proxy-total">{data.total} 个 IP</span>
      </div>
      <div className="proxy-toolbar">
        <div className="proxy-selection">
          <label className="proxy-mobile-select">
            <input
              type="checkbox"
              aria-label="全选当前页 IP"
              checked={
                data.items.length > 0 && selected.size === data.items.length
              }
              onChange={(e) =>
                setSelected(
                  new Set(e.target.checked ? data.items.map((p) => p.id) : []),
                )
              }
            />
            全选
          </label>
          {selected.size > 0 && (
            <>
              <span>已选 {selected.size}</span>
              <button
                disabled={blocked}
                onClick={() => void diagnose(selectedRows, "test")}
              >
                测试连接
              </button>
              <button
                disabled={blocked}
                onClick={() => void diagnose(selectedRows, "quality-check")}
              >
                质量检测
              </button>
              <button
                disabled={blocked}
                className="danger-text"
                onClick={() =>
                  setDialog({ kind: "delete", rows: selectedRows })
                }
              >
                删除
              </button>
              <button
                className="text-button"
                onClick={() => setSelected(new Set())}
              >
                取消选择
              </button>
            </>
          )}
        </div>
        <div className="proxy-primary-actions">
          <button
            className="icon-button"
            aria-label="刷新 IP"
            disabled={!online || loading || !!busy}
            onClick={() => void load()}
          >
            <RefreshCw size={15} className={loading ? "spin" : ""} />
          </button>
          <div className="proxy-more" ref={menuRef}>
            <button
              disabled={blocked}
              aria-label="更多 IP 操作"
              aria-expanded={menu}
              onClick={() => setMenu((v) => !v)}
            >
              <MoreHorizontal size={17} />
              <span>更多</span>
            </button>
            {menu && (
              <div className="proxy-menu">
                <button
                  onClick={() => {
                    setMenu(false);
                    setDialog({ kind: "batch" });
                  }}
                >
                  <Plus size={14} />
                  批量添加
                </button>
                <button onClick={() => void importFile()}>
                  <Upload size={14} />
                  导入 JSON
                </button>
                <button
                  onClick={() => {
                    setMenu(false);
                    setDialog({ kind: "export" });
                  }}
                >
                  <Download size={14} />
                  {selected.size ? "导出所选" : "导出筛选结果"}
                </button>
              </div>
            )}
          </div>
          <button
            className="primary"
            disabled={blocked}
            onClick={() => setDialog({ kind: "edit", row: null })}
          >
            <Plus size={15} />
            新增 IP
          </button>
        </div>
      </div>
      {(error || message || busy || !online) && (
        <div className="proxy-feedback" role={error ? "alert" : "status"}>
          {error ? (
            <span className="bad-text">
              {error}{" "}
              <button
                disabled={!online || loading || !!busy}
                onClick={() => void load()}
              >
                刷新核对
              </button>
            </span>
          ) : busy ? (
            <>
              <LoaderCircle size={14} className="spin" />
              {busy}中
            </>
          ) : !online ? (
            "连接已断开"
          ) : (
            message
          )}
        </div>
      )}
      <div
        className="proxy-table-scroll"
        role="region"
        aria-label="IP 列表"
        tabIndex={0}
      >
        <table className="proxy-table">
          <colgroup>{Array.from({ length: 11 }, (_, i) => <col key={i}/>)}</colgroup>
          <thead>
            <tr>
              <th className="proxy-pin-select">
                <input
                  type="checkbox"
                  aria-label="全选 IP"
                  checked={
                    data.items.length > 0 && selected.size === data.items.length
                  }
                  onChange={(e) =>
                    setSelected(
                      new Set(
                        e.target.checked ? data.items.map((p) => p.id) : [],
                      ),
                    )
                  }
                />
              </th>
              <th className="proxy-pin-name">{sortTitle("名称", "name")}</th>
              <th>{sortTitle("协议", "protocol")}</th>
              <th>地址</th>
              <th>认证</th>
              <th>地理位置</th>
              <th>{sortTitle("账号数", "account_count")}</th>
              <th>延迟与质量</th>
              <th>有效期</th>
              <th>状态</th>
              <th className="proxy-pin-actions">操作</th>
            </tr>
          </thead>
          <tbody>
            {data.items.map((row) => (
              <tr
                key={row.id}
                className={selected.has(row.id) ? "selected" : ""}
              >
                <td className="proxy-pin-select">
                  <input
                    type="checkbox"
                    aria-label={`选择 ${row.name}`}
                    checked={selected.has(row.id)}
                    onChange={() => toggle(row.id)}
                  />
                </td>
                <td className="proxy-pin-name">
                  <b title={row.name}>{row.name}</b>
                </td>
                <td>
                  <span className="proxy-badge protocol">
                    {row.protocol.toUpperCase()}
                  </span>
                </td>
                <td>
                  <div className="proxy-address">
                    <code title={proxyAddress(row)}>{proxyAddress(row)}</code>
                    <button
                      className="icon-button"
                      aria-label={`复制 ${row.name} 地址`}
                      disabled={blocked}
                      onClick={() => void copy(row, false)}
                    >
                      <Copy size={13} />
                    </button>
                  </div>
                </td>
                <td>{auth(row)}</td>
                <td>
                  <span className="proxy-location" title={location(row)}>
                    {location(row)}
                  </span>
                </td>
                <td>
                  <button
                    className="proxy-count"
                    disabled={blocked}
                    onClick={() => accounts(row)}
                  >
                    {row.account_count ?? 0} 个账号
                  </button>
                </td>
                <td>{quality(row)}</td>
                <td>
                  <time
                    className={row.status === "expired" ? "bad-text" : "muted"}
                  >
                    {row.expires_at ? fullTime(row.expires_at) : "长期有效"}
                  </time>
                </td>
                <td>{statusButton(row)}</td>
                <td className="proxy-pin-actions">{rowActions(row)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      <div className="proxy-mobile-list">
        {data.items.map((row) => (
          <article
            key={row.id}
            className={selected.has(row.id) ? "selected" : ""}
          >
            <header>
              <label>
                <input
                  type="checkbox"
                  aria-label={`选择 ${row.name}`}
                  checked={selected.has(row.id)}
                  onChange={() => toggle(row.id)}
                />
                <b>{row.name}</b>
              </label>
              {statusButton(row)}
            </header>
            <div className="proxy-mobile-address">
              <span className="proxy-badge protocol">
                {row.protocol.toUpperCase()}
              </span>
              <code title={proxyAddress(row)}>{proxyAddress(row)}</code>
              <button
                aria-label={`复制 ${row.name} 地址`}
                disabled={blocked}
                onClick={() => void copy(row, false)}
              >
                <Copy size={15} />
              </button>
            </div>
            <div className="proxy-mobile-facts">
              <span>{location(row)}</span>
              <button disabled={blocked} onClick={() => accounts(row)}>
                {row.account_count ?? 0} 个账号
              </button>
              <span>
                {row.expires_at ? fullTime(row.expires_at) : "长期有效"}
              </span>
              {auth(row)}
            </div>
            {quality(row)}
            {rowActions(row)}
          </article>
        ))}
      </div>
      {!data.items.length && (
        <div className="proxy-empty">
          {loading
            ? "正在读取 IP"
            : error
              ? "IP 列表暂不可用"
              : search || protocol || status
                ? "没有符合条件的 IP"
                : "暂无 IP"}
        </div>
      )}
      <footer className="proxy-pagination">
        <label>
          每页{" "}
          <select
            aria-label="每页 IP 数"
            disabled={!!busy}
            value={size}
            onChange={(e) => {
              setSize(Number(e.target.value));
              setPage(1);
              setSelected(new Set());
            }}
          >
            {[20, 50, 100, 200].map((n) => (
              <option key={n}>{n}</option>
            ))}
          </select>
        </label>
        <span>
          第 {page} / {Math.max(1, data.pages)} 页
        </span>
        <button
          disabled={page <= 1 || loading || !!busy}
          onClick={() => {
            setPage((p) => p - 1);
            setSelected(new Set());
          }}
        >
          上一页
        </button>
        <button
          disabled={page >= data.pages || loading || !!busy}
          onClick={() => {
            setPage((p) => p + 1);
            setSelected(new Set());
          }}
        >
          下一页
        </button>
      </footer>
      {dialog?.kind === "edit" && (
        <ProxyEditor
          row={dialog.row}
          online={online}
          close={() => setDialog(null)}
          saved={() => saved()}
        />
      )}
      {dialog?.kind === "batch" && (
        <ProxyBatchAdd
          online={online}
          close={() => setDialog(null)}
          saved={saved}
        />
      )}
      {dialog?.kind === "quality" && (
        <ProxyQualityReport
          name={dialog.row.name}
          result={dialog.value}
          close={() => setDialog(null)}
        />
      )}
      {dialog?.kind === "delete" && (
        <ProxyDialog
          title={`删除 ${dialog.rows.length} 个 IP`}
          close={() => setDialog(null)}
          busy={!!busy}
        >
          <p>
            确认删除以下 IP？有关联账号或被用作备用代理的项目将由上游拒绝删除。
          </p>
          <ul className="proxy-confirm-list">
            {dialog.rows.map((r) => (
              <li key={r.id}>
                {r.name} <span className="muted">#{r.id}</span>
              </li>
            ))}
          </ul>
          {error && (
            <p role="alert" className="bad-text">
              {error}
            </p>
          )}
          <footer>
            <button disabled={!!busy} onClick={() => setDialog(null)}>
              取消
            </button>
            <button
              className="danger"
              disabled={blocked}
              onClick={() => void remove(dialog.rows)}
            >
              删除
            </button>
          </footer>
        </ProxyDialog>
      )}
      {dialog?.kind === "export" && (
        <ProxyDialog
          title="导出 IP"
          close={() => setDialog(null)}
          busy={!!busy}
        >
          <p>
            导出{selected.size ? `所选 ${selected.size} 个 IP` : "当前筛选结果"}
            。文件包含代理认证信息。
          </p>
          {error && (
            <p role="alert" className="bad-text">
              {error}
            </p>
          )}
          <footer>
            <button disabled={!!busy} onClick={() => setDialog(null)}>
              取消
            </button>
            <button
              className="primary"
              disabled={blocked}
              onClick={() =>
                void operate("导出", async (valid) => {
                  const q = selected.size
                    ? `ids=${[...selected].join(",")}`
                    : query(true);
                  const ok = await exportProxyFile(
                    `/proxy-management/data?${q}`,
                    valid,
                  );
                  if (valid()) {
                    setDialog(null);
                    if (ok) setMessage("已导出");
                  }
                })
              }
            >
              选择保存位置
            </button>
          </footer>
        </ProxyDialog>
      )}
      {dialog?.kind === "import" && (
        <ProxyDialog
          title="导入 IP"
          close={() => setDialog(null)}
          busy={!!busy}
        >
          <p>
            文件包含 {dialog.data.proxies.length}{" "}
            个代理，将按网页端规则新增或复用已有代理。
          </p>
          {error && (
            <p role="alert" className="bad-text">
              {error}
            </p>
          )}
          <footer>
            <button disabled={!!busy} onClick={() => setDialog(null)}>
              取消
            </button>
            <button
              className="primary"
              disabled={blocked}
              onClick={() => void importData(dialog.data)}
            >
              导入
            </button>
          </footer>
        </ProxyDialog>
      )}
      {dialog?.kind === "credentials" && (
        <ProxyDialog
          title={`${dialog.row.name} · 认证`}
          close={() => setDialog(null)}
        >
          <dl className="proxy-credentials">
            <dt>用户名</dt>
            <dd>{dialog.value.username || "—"}</dd>
            <dt>密码</dt>
            <dd>{dialog.value.password || "—"}</dd>
          </dl>
          <footer>
            <button
              disabled={blocked}
              onClick={() => void copy(dialog.row, true)}
            >
              <Copy size={14} />
              复制完整代理地址
            </button>
            <button onClick={() => setDialog(null)}>关闭</button>
          </footer>
        </ProxyDialog>
      )}
      {dialog?.kind === "accounts" && (
        <ProxyDialog
          title={`${dialog.row.name} · 关联账号`}
          close={() => setDialog(null)}
        >
          <ul className="proxy-account-list">
            {dialog.accounts.map((a) => (
              <li key={a.id}>
                <b>{a.name}</b>
                <span>
                  #{a.id} · {a.platform} · {a.type}
                </span>
              </li>
            ))}
          </ul>
          {!dialog.accounts.length && <p className="muted">暂无关联账号</p>}
          <footer>
            <button onClick={() => setDialog(null)}>关闭</button>
          </footer>
        </ProxyDialog>
      )}
      {dialog?.kind === "results" && (
        <ProxyDialog title={dialog.title} close={() => setDialog(null)}>
          <ul className="proxy-result-list">
            {dialog.rows.map((r, i) => (
              <li key={i}>
                <b>{r.name}</b>
                <span className={r.ok ? "good-text" : "bad-text"}>
                  {r.message}
                </span>
              </li>
            ))}
          </ul>
          <footer>
            <button onClick={() => setDialog(null)}>关闭</button>
          </footer>
        </ProxyDialog>
      )}
    </section>
  );
}
