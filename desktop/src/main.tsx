import React, { useEffect, useState } from "react";
import {
  Activity,
  ArrowUpRight,
  Bell,
  Check,
  ChevronRight,
  CircleAlert,
  Clock3,
  Command,
  ExternalLink,
  Layers3,
  LayoutDashboard,
  LoaderCircle,
  Pin,
  Plug,
  RefreshCw,
  Search,
  Settings2,
  ShieldCheck,
  SlidersHorizontal,
  Star,
  Unplug,
  Users,
  X,
} from "lucide-react";
import { api, command, preview, subscribe, updates } from "./bridge";
import {
  filterAccounts,
  fullTime,
  initialState,
  relativeTime,
  type Account,
  type Config,
  type ConfigSection,
  type Group,
  type OpsError,
  type Preferences,
  type ViewState,
} from "./types";
import "./style.css";

type Page = "overview" | "accounts" | "events" | "automation" | "settings";
const quick = new URLSearchParams(location.search).get("panel") === "quick";
const pages: { id: Page; label: string; icon: typeof Activity }[] = [
  { id: "overview", label: "总览", icon: LayoutDashboard },
  { id: "accounts", label: "账号", icon: Users },
  { id: "events", label: "事件", icon: Bell },
  { id: "automation", label: "自动化", icon: SlidersHorizontal },
  { id: "settings", label: "设置", icon: Settings2 },
];
function Switch({
  on,
  disabled,
  label,
  onChange,
}: {
  on: boolean;
  disabled?: boolean;
  label: string;
  onChange: () => void;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={on}
      aria-label={label}
      title={label}
      disabled={disabled}
      className={`switch ${on ? "on" : ""}`}
      onClick={(e) => {
        e.stopPropagation();
        onChange();
      }}
    >
      <span />
    </button>
  );
}
function Time({ at }: { at: string | null | undefined }) {
  return <time title={fullTime(at)}>{relativeTime(at)}</time>;
}
export default function App() {
  const [state, setState] = useState<ViewState>(initialState),
    [ready, setReady] = useState(false),
    [page, setPage] = useState<Page>("overview"),
    [toast, setToast] = useState(""),
    [busy, setBusy] = useState<number | null>(null),
    [detail, setDetail] = useState<OpsError | null>(null),
    [detailBusy, setDetailBusy] = useState(false),
    [confirm, setConfirm] = useState<Account | null>(null),
    [query, setQuery] = useState(""),
    [group, setGroup] = useState(""),
    [platform, setPlatform] = useState(""),
    [filter, setFilter] = useState(""),
    [type, setType] = useState(""),
    [config, setConfig] = useState<Config | null>(null),
    [history, setHistory] = useState<OpsError[] | null>(null),
    [cursor, setCursor] = useState<number | null>(null);
  const report = (e: unknown) => setToast(String(e).replace(/^Error: /, ""));
  useEffect(() => {
    let disposed = false;
    let un: () => void = () => {},
      unUpdate = () => {};
    void (async () => {
      un = await subscribe((s) => {
        if (!disposed) setState(s);
      });
      unUpdate = await updates(setToast);
      const s = await command<ViewState>("get_state");
      if (!disposed) {
        setState(s);
        setReady(true);
      } else {
        un();
        unUpdate();
      }
    })().catch(report);
    const online = () => void command("refresh").catch(report);
    window.addEventListener("online", online);
    return () => {
      disposed = true;
      un();
      unUpdate();
      window.removeEventListener("online", online);
    };
  }, []);
  useEffect(() => {
    if (toast) {
      const id = setTimeout(() => setToast(""), 6000);
      return () => clearTimeout(id);
    }
  }, [toast]);
  useEffect(() => {
    if (state.online && (page === "settings" || page === "automation"))
      void api<Config>("GET", "/config").then(setConfig).catch(report);
  }, [page, state.online]);
  useEffect(() => {
    if (page === "events" && state.online)
      void api<{ items: OpsError[]; next_cursor: number | null }>(
        "GET",
        "/errors",
      )
        .then((r) => {
          setHistory(r.items);
          setCursor(r.next_cursor);
        })
        .catch(report);
  }, [page, state.online]);
  const snap = state.snapshot,
    accounts = snap?.accounts ?? [],
    groups = snap?.groups ?? [],
    errors = snap?.errors ?? [];
  const eventRows = [
    ...new Map(
      [...(history ?? []), ...errors].map((item) => [item.id, item]),
    ).values(),
  ].sort((a, b) => b.id - a.id);
  async function prefs(patch: Partial<Preferences>) {
    try {
      await command("preferences", {
        ...state.preferences,
        ...patch,
        launchAtLogin:
          patch.launch_at_login ?? state.preferences.launch_at_login,
      });
    } catch (e) {
      report(e);
    }
  }
  async function toggle(a: Account, detach = false) {
    if (a.managed && !detach) {
      setConfirm(a);
      return;
    }
    setConfirm(null);
    setBusy(a.id);
    try {
      await api("POST", `/accounts/${a.id}/schedulable`, {
        schedulable: !a.schedulable,
        expected_version: a.version,
        detach_managed: detach,
      });
      setToast(`${a.name}：${a.schedulable ? "已关闭" : "已打开"}调度`);
    } catch (e) {
      report(e);
    } finally {
      setBusy(null);
    }
  }
  async function openError(id: number) {
    setDetailBusy(true);
    try {
      setDetail(await api<OpsError>("GET", `/errors/${id}`));
    } catch (e) {
      report(e);
    } finally {
      setDetailBusy(false);
    }
  }
  function schedule(a: Account) {
    return (
      <div className="switch-cell">
        {busy === a.id ? <LoaderCircle size={14} className="spin" /> : null}
        <Switch
          on={a.schedulable}
          disabled={!state.online || busy !== null}
          label={`${a.name}调度`}
          onChange={() => void toggle(a)}
        />
      </div>
    );
  }
  function groupRow(g: Group, compact = false) {
    const a = accounts.find((a) => a.id === g.account_id),
      favorite = state.preferences.favorites.includes(g.id);
    return (
      <article className={`group-row ${compact ? "compact" : ""}`} key={g.id}>
        <div className="group-heading">
          <div className="group-symbol">
            <Layers3 size={16} />
          </div>
          <strong>{g.name}</strong>
          <span className="platform">{g.platform}</span>
          <button
            className={`icon-button favorite ${favorite ? "selected" : ""}`}
            title={favorite ? "取消收藏" : "收藏分组"}
            onClick={() =>
              void prefs({
                favorites: favorite
                  ? state.preferences.favorites.filter((id) => id !== g.id)
                  : [...state.preferences.favorites, g.id],
              })
            }
          >
            <Star size={15} fill={favorite ? "currentColor" : "none"} />
          </button>
        </div>
        <div className="group-call">
          <span className={`status-dot ${a?.available ? "good" : "muted"}`} />
          <div className="call-info">
            <strong>
              {g.account_id
                ? g.account_name || `账号 #${g.account_id}`
                : "暂无成功调用"}
            </strong>
            <span>
              {g.account_id
                ? `#${g.account_id} · ${g.model}`
                : "等待该分组的首条使用记录"}
            </span>
          </div>
          {a ? schedule(a) : null}
        </div>
        <div className="group-footer">
          <span>{g.called_at ? "最近成功调用" : "尚无记录"}</span>
          {g.called_at ? <Time at={g.called_at} /> : null}
          {a?.last_error_id ? (
            <button
              className="error-link"
              onClick={() => void openError(a.last_error_id!)}
            >
              <CircleAlert size={12} /> 最近报错 <Time at={a.last_error_at} />
            </button>
          ) : null}
        </div>
      </article>
    );
  }
  function errorRow(e: OpsError) {
    return (
      <button
        key={e.id}
        className="error-row"
        onClick={() => void openError(e.id)}
      >
        <span
          className={`code ${e.upstream_status_code === 429 ? "warning" : ""}`}
        >
          {e.upstream_status_code || e.status_code || "ERR"}
        </span>
        <div>
          <strong>{e.account_name || `账号 #${e.account_id}`}</strong>
          <span>{e.provider_error_code || e.message || "查看错误详情"}</span>
        </div>
        <Time at={e.created_at} />
        <ChevronRight size={15} />
      </button>
    );
  }
  function status() {
    return (
      <span className={`connection-status ${state.online ? "online" : ""}`}>
        <i />
        {state.online ? "已连接" : state.connected ? "连接中断" : "未连接"}
      </span>
    );
  }
  const visibleGroups = [...groups].sort(
    (a, b) =>
      Number(state.preferences.favorites.includes(b.id)) -
      Number(state.preferences.favorites.includes(a.id)),
  );
  return (
    <div className={quick ? "app quick" : "app"}>
      {!quick && (
        <aside className="sidebar" data-tauri-drag-region>
          <div className="brand">
            <span className="brand-icon">
              <Activity size={21} />
            </span>
            <strong>Sub2Ops</strong>
          </div>
          <div className="workspace">
            <span className="workspace-letter">S</span>
            <div>
              <strong>运维工作台</strong>
              <span>
                {state.preferences.base_url
                  ? new URL(state.preferences.base_url).hostname
                  : "尚未连接"}
              </span>
            </div>
          </div>
          <nav>
            {pages.map((p) => (
              <button
                key={p.id}
                className={page === p.id ? "active" : ""}
                onClick={() => setPage(p.id)}
              >
                <p.icon size={17} />
                {p.label}
                {p.id === "events" && errors.length > 0 ? (
                  <span className="nav-count">{errors.length}</span>
                ) : null}
              </button>
            ))}
          </nav>
          <div className="sidebar-bottom">
            <ShieldCheck size={15} />
            <span>云端持续守护</span>
            <small>0.1.0</small>
          </div>
        </aside>
      )}
      <main className="main">
        <header className="topbar" data-tauri-drag-region>
          {quick ? (
            <div className="quick-title">
              <Activity size={19} />
              <strong>Sub2Ops</strong>
            </div>
          ) : (
            <div className="breadcrumb">
              工作台 <ChevronRight size={12} />{" "}
              <strong>{pages.find((p) => p.id === page)?.label}</strong>
            </div>
          )}
          <div className="top-actions">
            {preview && <span className="preview-label">预览</span>}
            {status()}
            {!quick && !preview && (
              <button
                className="icon-button"
                title="打开快捷面板"
                onClick={() => void command("show_quick").catch(report)}
              >
                <LayoutDashboard size={15} />
              </button>
            )}
            <button
              className="icon-button"
              title="立即刷新"
              onClick={() => void command("refresh").catch(report)}
            >
              <RefreshCw size={15} />
            </button>
            {quick && (
              <button
                className={`icon-button ${state.preferences.pinned ? "selected" : ""}`}
                aria-pressed={state.preferences.pinned}
                title={state.preferences.pinned ? "取消固定" : "固定面板"}
                onClick={() =>
                  void prefs({ pinned: !state.preferences.pinned })
                }
              >
                <Pin size={15} />
              </button>
            )}
          </div>
        </header>
        {state.connected && !state.online && (
          <div className="offline">
            <Unplug size={15} />
            <span>
              {state.error || "正在读取云端状态"}
              {snap && (
                <>
                  {" "}
                  · 上次更新 <Time at={snap.observed_at} />
                </>
              )}
            </span>
          </div>
        )}
        <div className="content">
          {!ready ? (
            <div className="empty">
              <LoaderCircle className="spin" />
              正在加载客户端
            </div>
          ) : !state.connected ? (
            <Connection state={state} onError={report} />
          ) : !snap ? (
            <div className="empty">
              <Plug size={28} />
              <h2>等待云端数据</h2>
              <p>{state.error || "正在连接运维服务"}</p>
              <button onClick={() => setPage("settings")}>连接设置</button>
              {page === "settings" && (
                <Connection state={state} onError={report} />
              )}
            </div>
          ) : quick ? (
            <>
              <div className="section-heading">
                <h2>最近成功调用</h2>
                <span>{groups.length} 个分组</span>
              </div>
              <div className="quick-groups">
                {visibleGroups.map((g) => groupRow(g, true))}
              </div>
              <div className="section-heading">
                <h2>最近错误</h2>
                <span>{errors.length ? `${errors.length} 条` : "暂无"}</span>
              </div>
              <div className="error-list">
                {errors.slice(0, 5).map(errorRow)}
                {!errors.length && (
                  <div className="quiet">
                    <Check size={15} />
                    暂无账号错误
                  </div>
                )}
              </div>
            </>
          ) : (
            <>
              <div className="page-heading">
                <div>
                  <h1>{pages.find((p) => p.id === page)?.label}</h1>
                  <p>
                    {
                      {
                        overview: "每个分组的最近成功调用与账号状态",
                        accounts: "按账号控制调度，保留真实可用性与错误证据",
                        events: "查看上游错误与模型降级记录",
                        automation: "云端执行，修改即时生效",
                        settings: "连接服务、消息通道与客户端偏好",
                      }[page]
                    }
                  </p>
                </div>
                <span className="updated">
                  <Clock3 size={13} />
                  更新于 <Time at={snap.observed_at} />
                </span>
              </div>
              {page === "overview" && (
                <>
                  <div className="summary-strip">
                    <div>
                      <span className="status-dot good" />
                      <strong>
                        {accounts.filter((a) => a.available).length}
                      </strong>
                      可调度
                    </div>
                    <div>
                      <span className="status-dot warning" />
                      <strong>
                        {accounts.filter((a) => !a.available).length}
                      </strong>
                      受限或关闭
                    </div>
                    <div>
                      <Layers3 size={14} />
                      <strong>{groups.length}</strong>分组
                    </div>
                    <button onClick={() => setPage("accounts")}>
                      查看全部账号 <ArrowUpRight size={14} />
                    </button>
                  </div>
                  <div className="section-heading">
                    <h2>分组动态</h2>
                    <span>以成功完成的使用记录为准</span>
                  </div>
                  <div className="groups-grid">
                    {visibleGroups.map((g) => groupRow(g))}
                  </div>
                  {!groups.length && <Empty text="没有分组记录" />}
                  <div className="section-heading">
                    <h2>最近错误</h2>
                    <button
                      className="text-button"
                      onClick={() => setPage("events")}
                    >
                      全部事件 <ChevronRight size={13} />
                    </button>
                  </div>
                  <div className="error-list">
                    {errors.slice(0, 6).map(errorRow)}
                    {!errors.length && <Empty text="暂无账号错误" />}
                  </div>
                </>
              )}
              {page === "accounts" && (
                <>
                  <div className="filters">
                    <label className="search">
                      <Search size={15} />
                      <input
                        placeholder="搜索名称或账号 ID"
                        value={query}
                        onChange={(e) => setQuery(e.target.value)}
                      />
                    </label>
                    <select
                      aria-label="分组筛选"
                      value={group}
                      onChange={(e) => setGroup(e.target.value)}
                    >
                      <option value="">全部分组</option>
                      {groups.map((g) => (
                        <option value={g.id} key={g.id}>
                          {g.name}
                        </option>
                      ))}
                    </select>
                    <select
                      aria-label="平台筛选"
                      value={platform}
                      onChange={(e) => setPlatform(e.target.value)}
                    >
                      <option value="">全部平台</option>
                      {[...new Set(accounts.map((a) => a.platform))].map(
                        (p) => (
                          <option key={p}>{p}</option>
                        ),
                      )}
                    </select>
                    <select
                      aria-label="类型筛选"
                      value={type}
                      onChange={(e) => setType(e.target.value)}
                    >
                      <option value="">全部类型</option>
                      {[...new Set(accounts.map((a) => a.type))].map((p) => (
                        <option key={p}>{p}</option>
                      ))}
                    </select>
                    <select
                      aria-label="状态筛选"
                      value={filter}
                      onChange={(e) => setFilter(e.target.value)}
                    >
                      <option value="">全部状态</option>
                      <option value="ready">可调度</option>
                      <option value="off">调度关闭</option>
                      <option value="managed">回退托管</option>
                      <option value="error">曾有错误</option>
                    </select>
                  </div>
                  <div className="table-wrap">
                    <table>
                      <thead>
                        <tr>
                          <th>账号</th>
                          <th>平台 / 类型</th>
                          <th>当前状态</th>
                          <th>最近报错</th>
                          <th>允许调度</th>
                        </tr>
                      </thead>
                      <tbody>
                        {filterAccounts(
                          accounts,
                          query,
                          group,
                          platform,
                          filter,
                          type,
                        ).map((a) => (
                          <tr key={a.id}>
                            <td>
                              <strong>{a.name}</strong>
                              <small>
                                #{a.id}
                                {a.managed && (
                                  <span className="managed-tag">回退托管</span>
                                )}
                              </small>
                            </td>
                            <td>
                              <span>{a.platform}</span>
                              <small>{a.type}</small>
                            </td>
                            <td>
                              <span
                                className={`account-status ${a.available ? "good-text" : ""}`}
                              >
                                <i
                                  className={`status-dot ${a.available ? "good" : "warning"}`}
                                />
                                {a.available
                                  ? "可调度"
                                  : a.blockers.map((b) => b.label).join(" / ")}
                              </span>
                            </td>
                            <td>
                              {a.last_error_id ? (
                                <button
                                  className="error-time"
                                  onClick={() =>
                                    void openError(a.last_error_id!)
                                  }
                                >
                                  <span>
                                    {a.last_error_code ||
                                      a.last_error_status ||
                                      "上游错误"}
                                  </span>
                                  <Time at={a.last_error_at} />
                                  {a.success_after_error && (
                                    <small className="good-text">
                                      之后已有成功调用
                                    </small>
                                  )}
                                </button>
                              ) : a.error_message ? (
                                <span title={a.error_message}>
                                  错误时间未知
                                </span>
                              ) : (
                                <span className="muted">—</span>
                              )}
                            </td>
                            <td>{schedule(a)}</td>
                          </tr>
                        ))}
                      </tbody>
                    </table>
                  </div>
                  {!filterAccounts(
                    accounts,
                    query,
                    group,
                    platform,
                    filter,
                    type,
                  ).length && <Empty text="没有符合条件的账号" />}
                </>
              )}
              {page === "events" && (
                <>
                  <div className="section-heading">
                    <h2>上游与认证错误</h2>
                    <button
                      className="text-button"
                      disabled={!state.online}
                      onClick={() =>
                        void api<{
                          items: OpsError[];
                          next_cursor: number | null;
                        }>("GET", "/errors")
                          .then((r) => {
                            setHistory(r.items);
                            setCursor(r.next_cursor);
                          })
                          .catch(report)
                      }
                    >
                      刷新记录
                    </button>
                  </div>
                  <div className="error-list">
                    {eventRows.map(errorRow)}
                    {!eventRows.length && <Empty text="暂无账号错误" />}
                  </div>
                  {cursor && (
                    <button
                      className="load-more"
                      onClick={() =>
                        void api<{
                          items: OpsError[];
                          next_cursor: number | null;
                        }>("GET", `/errors?before_id=${cursor}`)
                          .then((r) => {
                            setHistory([...(history ?? []), ...r.items]);
                            setCursor(r.next_cursor);
                          })
                          .catch(report)
                      }
                    >
                      加载更早记录
                    </button>
                  )}
                  <div className="section-heading">
                    <h2>模型降级保护</h2>
                    <span>{snap.incidents.length} 条记录</span>
                  </div>
                  <div className="incident-list">
                    {snap.incidents.map((i, n) => (
                      <article key={`${i.account_id}-${n}`}>
                        <div>
                          <strong>
                            {i.account_name}{" "}
                            <span className="muted">#{i.account_id}</span>
                          </strong>
                          <span
                            className={`pill ${i.status === "confirmed" ? "danger" : "warning"}`}
                          >
                            {i.status === "confirmed" ? "确认降级" : "待核实"}
                          </span>
                        </div>
                        <p className="model-chain">
                          {i.requested_model} <ChevronRight size={12} />
                          {i.upstream_model} <ChevronRight size={12} />
                          <strong>{i.response_model}</strong>
                        </p>
                        <p>
                          {i.action} · {i.reason}
                        </p>
                        <small>
                          {i.history ? "历史记录" : "实时记录"} · {i.count} 次 ·
                          最近 <Time at={i.latest_at} />
                        </small>
                      </article>
                    ))}
                    {!snap.incidents.length && (
                      <Empty text="暂无模型异常记录" />
                    )}
                  </div>
                </>
              )}
              {(page === "automation" || page === "settings") &&
                (config ? (
                  <SettingsPage
                    page={page}
                    config={config}
                    accounts={accounts}
                    state={state}
                    prefs={prefs}
                    onChange={(key, value) =>
                      setConfig({ ...config, [key]: value })
                    }
                    onMessage={setToast}
                    onError={report}
                  />
                ) : (
                  <Empty text="正在读取设置" />
                ))}
            </>
          )}
        </div>
        {quick && (
          <footer className="quick-bottom">
            <span>
              <Time at={snap?.observed_at} />
            </span>
            <button onClick={() => void command("show_main").catch(report)}>
              打开工作台 <ArrowUpRight size={14} />
            </button>
          </footer>
        )}
      </main>
      {(detail || detailBusy) && (
        <div className="drawer-backdrop" onClick={() => setDetail(null)}>
          <aside className="drawer" onClick={(e) => e.stopPropagation()}>
            <header>
              <div>
                <span className="eyebrow">错误详情</span>
                <h2>{detail?.account_name || "正在加载"}</h2>
              </div>
              <button
                className="icon-button"
                aria-label="关闭错误详情"
                onClick={() => {
                  setDetail(null);
                  setDetailBusy(false);
                }}
              >
                <X size={19} />
              </button>
            </header>
            {detailBusy ? (
              <LoaderCircle className="spin" />
            ) : (
              detail && (
                <>
                  <div className="error-banner">
                    <CircleAlert size={20} />
                    <div>
                      <strong>
                        {detail.provider_error_code ||
                          detail.upstream_status_code ||
                          detail.status_code}
                      </strong>
                      <p>{detail.message}</p>
                    </div>
                  </div>
                  <dl className="detail-meta">
                    <dt>账号</dt>
                    <dd>#{detail.account_id}</dd>
                    <dt>分组</dt>
                    <dd>{detail.group_name || "无分组记录"}</dd>
                    <dt>发生时间</dt>
                    <dd>{fullTime(detail.created_at)} 北京时间</dd>
                    <dt>请求模型</dt>
                    <dd>{detail.requested_model || detail.model}</dd>
                    <dt>上游模型</dt>
                    <dd>{detail.upstream_model || "未知"}</dd>
                    <dt>HTTP 状态</dt>
                    <dd>
                      {detail.upstream_status_code ||
                        detail.status_code ||
                        "未知"}
                    </dd>
                    <dt>请求 ID</dt>
                    <dd>{detail.request_id || "未知"}</dd>
                  </dl>
                  <h3>错误内容</h3>
                  <pre>
                    {detail.content || detail.message || "没有额外错误内容"}
                  </pre>
                  <p className="hint">
                    仅展示脱敏后的错误摘要，长内容可能已截断。
                  </p>
                  <div className="section-heading">
                    <h3>账号近期错误</h3>
                    <button
                      className="text-button"
                      onClick={() =>
                        void api<{ items: OpsError[] }>(
                          "GET",
                          `/errors?account_id=${detail.account_id}&limit=10`,
                        )
                          .then((r) => {
                            setHistory(r.items);
                            setPage("events");
                            setDetail(null);
                          })
                          .catch(report)
                      }
                    >
                      查看记录 <ChevronRight size={13} />
                    </button>
                  </div>
                </>
              )
            )}
          </aside>
        </div>
      )}
      {confirm && (
        <div className="modal-backdrop">
          <section
            className="modal"
            role="alertdialog"
            aria-labelledby="confirm-title"
          >
            <ShieldCheck size={26} />
            <h2 id="confirm-title">解除该账号的回退托管？</h2>
            <p>
              「{confirm.name}」正在由 Key
              回退管理。继续后将取消此账号的托管选择，并
              {confirm.schedulable ? "关闭" : "打开"}调度。
            </p>
            <p className="hint">其他账号与平台开关保持原状态。</p>
            <footer>
              <button onClick={() => setConfirm(null)}>取消</button>
              <button
                className="primary"
                onClick={() => void toggle(confirm, true)}
              >
                解除托管并{confirm.schedulable ? "关闭" : "打开"}
              </button>
            </footer>
          </section>
        </div>
      )}
      {toast && (
        <div className="toast" role="status">
          <span>{toast}</span>
          <button aria-label="关闭提示" onClick={() => setToast("")}>
            <X size={14} />
          </button>
        </div>
      )}
    </div>
  );
}
function Empty({ text }: { text: string }) {
  return (
    <div className="empty">
      <Check size={23} />
      <p>{text}</p>
    </div>
  );
}
function Connection({
  state,
  onError,
}: {
  state: ViewState;
  onError: (e: unknown) => void;
}) {
  const [url, setUrl] = useState(
      state.preferences.base_url || "https://661313.xyz/sub2ops",
    ),
    [key, setKey] = useState(""),
    [busy, setBusy] = useState(false);
  return (
    <form
      className="connection-form"
      onSubmit={(e) => {
        e.preventDefault();
        setBusy(true);
        void command("connect", { baseUrl: url, apiKey: key })
          .then(() => setKey(""))
          .catch(onError)
          .finally(() => setBusy(false));
      }}
    >
      <span className="connection-icon">
        <Plug size={26} />
      </span>
      <h2>{state.connected ? "更换连接" : "连接你的运维服务"}</h2>
      <p>使用现有 Sub2API 管理员 API Key。</p>
      <label>
        服务地址
        <input
          type="url"
          required
          value={url}
          onChange={(e) => setUrl(e.target.value)}
          placeholder="https://example.com/sub2ops"
          autoComplete="url"
        />
      </label>
      <label>
        管理员 API Key
        <input
          type="password"
          required
          value={key}
          onChange={(e) => setKey(e.target.value)}
          placeholder="粘贴已有的管理员 Key"
          autoComplete="off"
          spellCheck={false}
        />
      </label>
      <p className="hint">Key 保存在 macOS 钥匙串，后台任务继续在云端运行。</p>
      <button className="primary" disabled={busy}>
        {busy ? (
          <LoaderCircle className="spin" size={15} />
        ) : (
          <Plug size={15} />
        )}
        验证并连接
      </button>
    </form>
  );
}
const oauthLabels: Record<string, string> = {
  oauth_usage_refresh_enabled: "后台额度刷新",
  oauth_recovery_monitor_enabled: "额度恢复监控",
  oauth_daily_test_enabled: "每日定时测活",
  oauth_daily_test_time: "测活时间（北京时间）",
  oauth_usage_refresh_concurrency: "额度刷新并发",
  oauth_recovery_test_concurrency: "测活并发",
  oauth_early_probe_batch_size: "单轮账号上限",
  oauth_regular_refresh_interval_seconds: "常规刷新间隔（秒）",
  oauth_7d_probe_interval_seconds: "7d 探测间隔（秒）",
  oauth_recovery_test_model_id: "测活模型",
};
function SettingsPage({
  page,
  config,
  accounts,
  state,
  prefs,
  onChange,
  onMessage,
  onError,
}: {
  page: Page;
  config: Config;
  accounts: Account[];
  state: ViewState;
  prefs: (p: Partial<Preferences>) => Promise<void>;
  onChange: (k: string, c: ConfigSection) => void;
  onMessage: (m: string) => void;
  onError: (e: unknown) => void;
}) {
  async function action(name: string) {
    try {
      const result = await api<{ message?: string; pairing_code?: string }>(
        "POST",
        `/actions/${name}`,
      );
      onMessage(result.message || "配对码已更新");
      if (result.pairing_code)
        onChange("telegram", {
          ...config.telegram,
          pairing_code: result.pairing_code,
        });
    } catch (e) {
      onError(e);
    }
  }
  return (
    <div className="settings-stack">
      {page === "automation" ? (
        <>
          <ConfigForm
            section="oauth"
            title="OAuth 恢复与测活"
            description="即时恢复全天运行；每日测活仅覆盖 OpenAI OAuth。"
            value={config.oauth}
            online={state.online}
            onChange={onChange}
            onMessage={onMessage}
            onError={onError}
          >
            {(draft, set) => (
              <>
                {Object.entries(oauthLabels).map(([key, label]) => (
                  <Field
                    key={key}
                    label={label}
                    value={draft[key]}
                    type={key === "oauth_daily_test_time" ? "time" : undefined}
                    set={(v) => set(key, v)}
                  />
                ))}
              </>
            )}
          </ConfigForm>
          <ConfigForm
            section="key_fallback"
            title="Key 调度回退"
            description="各平台独立判断 OAuth 可用性，仅控制已选择的 Key 账号。"
            value={config.key_fallback}
            online={state.online}
            onChange={onChange}
            onMessage={onMessage}
            onError={onError}
          >
            {(draft, set) => (
              <>
                {["openai", "grok"].map((p) => (
                  <div className="platform-settings" key={p}>
                    <Field
                      label={`${p === "openai" ? "OpenAI" : "Grok"} 自动回退`}
                      value={draft[`${p}_enabled`]}
                      set={(v) => set(`${p}_enabled`, v)}
                    />
                    {accounts
                      .filter((a) => a.platform === p && a.type === "apikey")
                      .map((a) => (
                        <label className="check-account" key={a.id}>
                          <input
                            type="checkbox"
                            checked={(
                              (draft.managed_account_ids as number[]) ?? []
                            ).includes(a.id)}
                            onChange={(e) =>
                              set(
                                "managed_account_ids",
                                e.target.checked
                                  ? [
                                      ...((draft.managed_account_ids as number[]) ??
                                        []),
                                      a.id,
                                    ]
                                  : (
                                      (draft.managed_account_ids as number[]) ??
                                      []
                                    ).filter((id) => id !== a.id),
                              )
                            }
                          />
                          <span>
                            {a.name}
                            <small>
                              #{a.id} · {a.platform}
                            </small>
                          </span>
                        </label>
                      ))}
                    {!accounts.some(
                      (a) => a.platform === p && a.type === "apikey",
                    ) && <p className="hint">没有可选择的 Key 账号</p>}
                  </div>
                ))}
              </>
            )}
          </ConfigForm>
          <ConfigForm
            section="model_guard"
            title="模型降级保护"
            description="读取真实使用日志，保留模型映射语义；历史异常只告警。"
            value={config.model_guard}
            online={state.online}
            onChange={onChange}
            onMessage={onMessage}
            onError={onError}
          >
            {(draft, set) => (
              <>
                <Field
                  label="OpenAI 监控"
                  value={draft.openai_enabled}
                  set={(v) => set("openai_enabled", v)}
                />
                <Field
                  label="Grok 监控"
                  value={draft.grok_enabled}
                  set={(v) => set("grok_enabled", v)}
                />
                <Field
                  label="确认降级后自动移除精确模型入口"
                  value={draft.auto_remove}
                  set={(v) => set("auto_remove", v)}
                />
              </>
            )}
          </ConfigForm>
        </>
      ) : (
        <>
          <section className="settings-card">
            <h2>客户端</h2>
            <Field
              label="开机启动"
              value={state.preferences.launch_at_login}
              set={(v) => void prefs({ launch_at_login: Boolean(v) })}
            />
            <div className="field">
              <span>当前版本</span>
              <button
                onClick={() =>
                  void command<string>("check_updates")
                    .then(onMessage)
                    .catch(onError)
                }
              >
                0.1.0 · 检查更新 <ExternalLink size={13} />
              </button>
            </div>
          </section>
          <ConfigForm
            section="bark"
            title="Bark 事件推送"
            value={config.bark}
            online={state.online}
            onChange={onChange}
            onMessage={onMessage}
            onError={onError}
          >
            {(draft, set) => (
              <>
                <Field
                  label="启用推送"
                  value={draft.enabled}
                  set={(v) => set("enabled", v)}
                />
                <Field
                  label={`Device Key${config.bark.device_key_set ? "（已保存，留空保留）" : ""}`}
                  value={draft.device_key ?? ""}
                  type="password"
                  set={(v) => set("device_key", v)}
                />
                <button
                  type="button"
                  disabled={!state.online}
                  onClick={() => void action("bark-test")}
                >
                  发送测试消息
                </button>
              </>
            )}
          </ConfigForm>
          <ConfigForm
            section="telegram"
            title="Telegram 查询与配对"
            description="仅保留额度查询和配对，不提供账号操作。"
            value={config.telegram}
            online={state.online}
            onChange={onChange}
            onMessage={onMessage}
            onError={onError}
          >
            {(draft, set) => (
              <>
                <Field
                  label={`Bot Token${config.telegram.bot_token_set ? "（已保存，留空保留）" : ""}`}
                  value={draft.bot_token ?? ""}
                  type="password"
                  set={(v) => set("bot_token", v)}
                />
                <div className="field">
                  <span>配对码</span>
                  <code>
                    {String(config.telegram.pairing_code || "尚未生成")}
                  </code>
                </div>
                <div className="field">
                  <span>授权用户</span>
                  <strong>
                    {Number(config.telegram.paired_user_count || 0)}
                  </strong>
                </div>
                <div className="button-row">
                  <button
                    type="button"
                    disabled={!state.online}
                    onClick={() => void action("telegram-pairing")}
                  >
                    重新生成配对码
                  </button>
                  <button
                    type="button"
                    disabled={!state.online}
                    onClick={() => void action("telegram-test")}
                  >
                    Bot 消息测试
                  </button>
                </div>
              </>
            )}
          </ConfigForm>
          <section className="settings-card">
            <Connection state={state} onError={onError} />
            <button
              className="danger-text"
              onClick={() => void command("disconnect").catch(onError)}
            >
              <Unplug size={14} />
              断开连接并删除本机 Key
            </button>
          </section>
        </>
      )}
    </div>
  );
}
function Field({
  label,
  value,
  type,
  set,
}: {
  label: string;
  value: unknown;
  type?: string;
  set: (v: unknown) => void;
}) {
  return (
    <label className="field">
      <span>{label}</span>
      {typeof value === "boolean" ? (
        <Switch label={label} on={value} onChange={() => set(!value)} />
      ) : (
        <input
          type={type || (typeof value === "number" ? "number" : "text")}
          value={String(value ?? "")}
          autoComplete="off"
          onInput={(e) =>
            set(
              typeof value === "number"
                ? Number(e.currentTarget.value)
                : e.currentTarget.value,
            )
          }
        />
      )}
    </label>
  );
}
function ConfigForm({
  section,
  title,
  description,
  value,
  online,
  onChange,
  onMessage,
  onError,
  children,
}: {
  section: string;
  title: string;
  description?: string;
  value: ConfigSection;
  online: boolean;
  onChange: (k: string, c: ConfigSection) => void;
  onMessage: (m: string) => void;
  onError: (e: unknown) => void;
  children: (
    draft: ConfigSection,
    set: (key: string, v: unknown) => void,
  ) => React.ReactNode;
}) {
  const [draft, setDraft] = useState(value),
    [changes, setChanges] = useState<Record<string, unknown>>({}),
    [busy, setBusy] = useState(false),
    [conflict, setConflict] = useState(false);
  useEffect(() => {
    if (!Object.keys(changes).length) {
      setDraft(value);
      setConflict(false);
    } else if (draft.revision !== value.revision) setConflict(true);
  }, [value]);
  const set = (key: string, v: unknown) => {
    setDraft((d) => ({ ...d, [key]: v }));
    setChanges((c) => ({ ...c, [key]: v }));
  };
  return (
    <form
      className="settings-card"
      onSubmit={(e) => {
        e.preventDefault();
        setBusy(true);
        void api<ConfigSection>("PUT", `/config/${section}`, {
          expected_revision: draft.revision,
          changes,
        })
          .then((c) => {
            setDraft(c);
            setChanges({});
            setConflict(false);
            onChange(section, c);
            onMessage("设置已保存");
          })
          .catch((e) => {
            onError(e);
            if (String(e).includes("409")) setConflict(true);
          })
          .finally(() => setBusy(false));
      }}
    >
      <div className="settings-heading">
        <div>
          <h2>{title}</h2>
          {description && <p>{description}</p>}
        </div>
        <button
          className="primary small"
          disabled={!online || busy || !Object.keys(changes).length || conflict}
        >
          {busy ? (
            <LoaderCircle size={13} className="spin" />
          ) : (
            <Check size={13} />
          )}
          保存
        </button>
      </div>
      <fieldset disabled={!online || busy}>{children(draft, set)}</fieldset>
      {conflict && (
        <div className="conflict">
          云端配置已变化。
          <button
            type="button"
            onClick={() =>
              void api<Config>("GET", "/config")
                .then((c) => {
                  setChanges({});
                  setDraft(c[section]);
                  setConflict(false);
                  onChange(section, c[section]);
                })
                .catch(onError)
            }
          >
            放弃本地修改并刷新
          </button>
        </div>
      )}
    </form>
  );
}
