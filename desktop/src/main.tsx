import React, { useEffect, useMemo, useRef, useState } from "react";
import {
  Activity,
  ArrowUpRight,
  Bell,
  Check,
  ChevronLeft,
  ChevronRight,
  CircleAlert,
  Clock3,
  Command,
  ExternalLink,
  Layers3,
  ListOrdered,
  LayoutDashboard,
  LoaderCircle,
  MoreHorizontal,
  Pin,
  Plug,
  RefreshCw,
  Search,
  Settings2,
  ShieldCheck,
  SlidersHorizontal,
  Unplug,
  Users,
  X,
} from "lucide-react";
import { api, command, preview, subscribe, updates } from "./bridge";
import {
  filterAccounts,
  currentGroups,
  sortPriority,
  sortQuality,
  fullTime,
  initialState,
  type Account,
  type Config,
  type ConfigSection,
  type Group,
  type OpsError,
  type Preferences,
  type ViewState,
} from "./types";
import "./style.css";
import "./responsive.css";
import UsageCell from "./UsageCell";
import {
  MiniUsage,
  PriorityEditor,
  QuotaRefresh,
  RecoveryHistory,
} from "./AccountControls";
import TestDialog from "./TestDialog";
import ModelTestDialog from "./ModelTestDialog";
import { DeleteAccountsDialog, RecoverStateButton } from "./AccountManagement";
import { useQuickHeight } from "./useQuickHeight";
import QualityDialog, { QualityBadge, SlowWarningBadge } from "./AccountQuality";
import ModelConfig from "./ModelConfig";
import AccountTemplates from "./AccountTemplates";
import ModelDetectionDialog from "./ModelDetectionDialog";
import AccountOperations, { AccountOperationStatus } from "./OperationPanel";
import { accountOperation, bindOperationConnection } from "./accountOperations";
import UsageRecords from "./UsageRecords";
import MobileAccounts from "./MobileAccounts";
import GroupManager from "./GroupManager";
import DegradationAction, { DegradationBadge } from "./DegradationMark";
import { listenBack, useBackAction } from "./mobile";
import { version as appVersion } from "../package.json";
import { getVersion as getRuntimeVersion } from "@tauri-apps/api/app";
import "./mobile-layout.css";

type Page = "accounts" | "groups" | "records" | "events" | "features" | "settings";
const quick = new URLSearchParams(location.search).get("panel") === "quick";
const pages: { id: Page; label: string; icon: typeof Activity }[] = [
  { id: "accounts", label: "账号", icon: Users },
  { id: "groups", label: "分组", icon: Layers3 },
  { id: "records", label: "记录", icon: ListOrdered },
  { id: "events", label: "事件", icon: Bell },
  { id: "features", label: "功能", icon: SlidersHorizontal },
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
  return <time>{fullTime(at)}</time>;
}
export default function App() {
  const [state, setState] = useState<ViewState>(initialState),
    [ready, setReady] = useState(false),
    [page, setPage] = useState<Page>("accounts"),
    [ascending, setAscending] = useState(true),
    [sortBy, setSortBy] = useState<"priority" | "quality">("priority"),
    [qualityFilter, setQualityFilter] = useState(""),
    [qualityAccount, setQualityAccount] = useState<Account | null>(null),
    [testAccount, setTestAccount] = useState<Account | null>(null),
    [modelTestAccount, setModelTestAccount] = useState<Account | null>(null),
    [modelDetectionAccount, setModelDetectionAccount] = useState<Account | null>(null),
    [templatesOpen, setTemplatesOpen] = useState(false),
    [templateAccount, setTemplateAccount] = useState<Account | null>(null),
    [quickTab, setQuickTab] = useState<"groups" | "errors">("groups"),
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
    [runtimeVersion, setRuntimeVersion] = useState<string | null>(null),
    [history, setHistory] = useState<OpsError[] | null>(null),
    [cursor, setCursor] = useState<number | null>(null),
    [eventTab, setEventTab] = useState<"errors" | "recoveries">("errors"),
    [selected, setSelected] = useState<Set<number>>(new Set()),
    [removedIds, setRemovedIds] = useState<Set<number>>(new Set()),
    [deleteAccounts, setDeleteAccounts] = useState<Account[] | null>(null),
    [filtersOpen, setFiltersOpen] = useState(false),
    [groupsOpen, setGroupsOpen] = useState(false),
    [groupChanges, setGroupChanges] = useState({ count: 0, busy: false }),
    [leaveGroups, setLeaveGroups] = useState<((proceed: boolean) => void) | null>(null),
    eventGeneration = useRef(0),
    eventAbort = useRef<AbortController | null>(null),
    eventQueue = useRef<Promise<unknown>>(Promise.resolve()),
    detailGeneration = useRef(0),
    detailAbort = useRef<AbortController | null>(null);
  function closeDetail() {
    detailAbort.current?.abort();
    detailAbort.current = null;
    ++detailGeneration.current;
    setDetail(null);
    setDetailBusy(false);
  }
  async function beforeConnectionChange() {
    if (groupChanges.busy) { setToast("分组正在保存，请等待结果"); return false; }
    if (!groupChanges.count) return true;
    return new Promise<boolean>((resolve) => setLeaveGroups(() => resolve));
  }
  const mobile = state.platform === "android" || (preview && new URLSearchParams(location.search).has("mobile"));
  useBackAction(groupsOpen, () => setGroupsOpen(false));
  useBackAction(!!leaveGroups, () => { leaveGroups?.(false); setLeaveGroups(null); });
  useBackAction(filtersOpen, () => setFiltersOpen(false));
  useBackAction(!!detail || detailBusy, closeDetail);
  useBackAction(!!confirm, () => setConfirm(null));
  useEffect(() => {
    if (!mobile) return;
    let disposed = false, off = () => {};
    void listenBack(() => {
      if (page !== "accounts") setPage("accounts");
      else void command("background").catch(() => {});
    }).then((unsubscribe) => { if (disposed) unsubscribe(); else off = unsubscribe; });
    return () => { disposed = true; off(); };
  }, [mobile, page]);
  const quickBody = useQuickHeight(quick, !!detail || detailBusy || !!confirm);
  const connectionKey = `${state.preferences.base_url}:${state.connected}:${state.connection_revision ?? 0}`;
  useEffect(() => bindOperationConnection(connectionKey), [connectionKey]);
  const filterKey = JSON.stringify([
    query,
    group,
    platform,
    filter,
    type,
    qualityFilter,
  ]);
  useEffect(() => {
    setSelected(new Set());
  }, [filterKey, connectionKey]);
  useEffect(() => {
    closeDetail();
  }, [page, connectionKey]);
  useEffect(() => {
    setRemovedIds(new Set());
    setDeleteAccounts(null);
    setHistory(null);
    setCursor(null);
    closeDetail();
    setTestAccount(null);
    setModelTestAccount(null);
    setQualityAccount(null);
    setConfirm(null);
    setGroupsOpen(false);
    setFiltersOpen(false);
    setConfig(null);
    setGroupChanges({ count: 0, busy: false });
  }, [connectionKey]);
  const report = (e: unknown) => { if (!String(e).includes("连接已切换；请求保留在原连接")) setToast(String(e).replace(/^Error: /, "")); };
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
    if (!("__TAURI_INTERNALS__" in window)) return;
    void getRuntimeVersion().then(setRuntimeVersion).catch(() => {});
  }, []);
  useEffect(() => {
    const escape = (event: KeyboardEvent) => {
      if (event.key !== "Escape" || event.isComposing) return;
      if (testAccount) setTestAccount(null);
      else if (confirm) setConfirm(null);
      else if (detail || detailBusy) {
        closeDetail();
      } else if (quick) void command("hide_quick").catch(report);
    };
    window.addEventListener("keydown", escape);
    return () => window.removeEventListener("keydown", escape);
  }, [confirm, detail, detailBusy, testAccount]);
  useEffect(() => {
    let disposed = false;
    if (state.online && (page === "settings" || page === "features"))
      void api<Config>("GET", "/config").then((value) => { if (!disposed) setConfig(value); }).catch((error) => { if (!disposed) report(error); });
    return () => { disposed = true; };
  }, [page, state.online, connectionKey]);
  function queueEventRequest<T>(task: () => Promise<T>) {
    const next = eventQueue.current.then(task, task);
    eventQueue.current = next.catch(() => {});
    return next;
  }
  function loadEvents(mode: "replace" | "more" = "replace") {
    if (page !== "events" || !state.online) return Promise.resolve(false);
    const generation = ++eventGeneration.current;
    eventAbort.current?.abort();
    const controller = new AbortController();
    eventAbort.current = controller;
    const before = mode === "more" ? cursor : null;
    if (mode === "replace") {
      setHistory(null);
      setCursor(null);
    }
    return queueEventRequest(async () => {
      const valid = () =>
        page === "events" &&
        state.online &&
        eventGeneration.current === generation &&
        !controller.signal.aborted;
      if (!valid() || (mode === "more" && !before)) return false;
      try {
        const response = await api<{
          items: OpsError[];
          next_cursor: number | null;
        }>(
          "GET",
          mode === "more" ? `/errors?before_id=${before}` : "/errors",
        );
        if (!valid()) return false;
        setHistory((old) =>
          mode === "more"
            ? [
                ...new Map(
                  [...(old ?? []), ...response.items].map((item) => [
                    item.id,
                    item,
                  ]),
                ).values(),
              ]
            : response.items,
        );
        setCursor(response.next_cursor);
        return true;
      } catch (error) {
        if (valid()) report(error);
        return false;
      } finally {
        if (eventAbort.current === controller) eventAbort.current = null;
      }
    });
  }
  useEffect(() => {
    if (page !== "events" || !state.online) {
      eventAbort.current?.abort();
      eventAbort.current = null;
      ++eventGeneration.current;
      return;
    }
    void loadEvents();
    return () => {
      eventAbort.current?.abort();
      eventAbort.current = null;
      ++eventGeneration.current;
    };
  }, [page, state.online, connectionKey]);
  const snap = state.snapshot,
    groups = snap?.groups ?? [],
    errors = snap?.errors ?? [];
  const accounts = useMemo(
    () => (snap?.accounts ?? []).filter((a) => !removedIds.has(a.id)),
    [snap?.accounts, removedIds],
  );
  const filteredAccounts = useMemo(
    () =>
      (sortBy === "quality" ? sortQuality : sortPriority)(
        filterAccounts(accounts, query, group, platform, filter, type).filter(
          (a) => !qualityFilter || a.quality?.grade === qualityFilter,
        ),
        ascending,
      ),
    [accounts, filterKey, ascending, sortBy],
  );
  useEffect(() => {
    const live = new Set(accounts.map((a) => a.id));
    setQualityAccount((old) => (old && !live.has(old.id) ? null : old));
    setSelected((old) =>
      [...old].every((id) => live.has(id))
        ? old
        : new Set([...old].filter((id) => live.has(id))),
    );
  }, [accounts]);
  const eventRows = (quick ? errors : history ?? []).slice().sort((a, b) => b.id - a.id);
  async function prefs(patch: Partial<Preferences>) {
    try {
      await command("preferences", {
        ...state.preferences,
        ...patch,
        launchAtLogin:
          patch.launch_at_login ?? state.preferences.launch_at_login,
        recordColumns: patch.record_columns ?? state.preferences.record_columns,
        modelTestConcurrency: patch.model_test_concurrency ?? state.preferences.model_test_concurrency,
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
      await accountOperation(a, "schedulable", {
        schedulable: !a.schedulable,
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
    const generation = ++detailGeneration.current;
    detailAbort.current?.abort();
    const controller = new AbortController();
    detailAbort.current = controller;
    setDetailBusy(true);
    try {
      const value = await api<OpsError>("GET", `/errors/${id}`);
      if (generation === detailGeneration.current && !controller.signal.aborted)
        setDetail(value);
    } catch (e) {
      if (generation === detailGeneration.current && !controller.signal.aborted)
        report(e);
    } finally {
      if (generation === detailGeneration.current) {
        detailAbort.current = null;
        setDetailBusy(false);
      }
    }
  }
  function errorState(error: OpsError) {
    const account = accounts.find((item) => item.id === error.account_id);
    if (account?.last_error_id === error.id && !error.resolved && !account.success_after_error) return "当前";
    return error.resolved ? "已解决" : "历史";
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
    const recent = g.recent_accounts ?? [];
    return (
      <article className={`group-row ${compact ? "compact" : ""}`} key={g.id}>
        <div className="group-heading">
          <div className="group-symbol">
            <Layers3 size={16} />
          </div>
          <strong title={g.name}>{g.name}</strong>
          <span className="platform">{g.platform}</span>
        </div>
        <div className="recent-accounts">
          {recent.map((call) => {
            const a = accounts.find((item) => item.id === call.account_id);
            return (
              <div className="recent-account" key={call.account_id}>
                {compact ? (
                  <>
                    <div className="compact-identity">
                      <strong title={call.account_name}>
                        {call.account_name}
                        {a && <><DegradationBadge account={a} compact /><SlowWarningBadge value={a.quality} compact /></>}
                      </strong>
                      {a && <MiniUsage account={a} />}
                    </div>
                    <div className="compact-times">
                      <div>
                        <span>最近调用</span>
                        <Time at={call.called_at} />
                      </div>
                      {a?.last_error_id ? (
                        <button
                          onClick={() => void openError(a.last_error_id!)}
                        >
                          <span>上次错误</span>
                          <Time at={a.last_error_at} />
                        </button>
                      ) : (
                        <div>
                          <span>上次错误</span>
                          <Time at={a?.last_error_at} />
                        </div>
                      )}
                    </div>
                    {a && schedule(a)}
                  </>
                ) : (
                  <>
                    <div className="group-call">
                      {!compact && (
                        <span
                          className={`status-dot ${a?.available ? "good" : "muted"}`}
                        />
                      )}
                      <div className="call-info">
                        <strong title={call.account_name}>
                          {call.account_name || `账号 #${call.account_id}`}
                        </strong>
                        {!compact && (
                          <span>
                            #{call.account_id} · {call.model}
                          </span>
                        )}
                      </div>
                      {a ? schedule(a) : null}
                    </div>
                    <div className="call-time">
                      {compact && <span>最近调用</span>}
                      <Time at={call.called_at} />
                    </div>
                    {a?.last_error_id ? (
                      <button
                        className="error-link"
                        onClick={() => void openError(a.last_error_id!)}
                      >
                        {!compact && <CircleAlert size={12} />}
                        <span>{compact ? "上次错误" : "报错"}</span>
                        <Time at={a.last_error_at} />
                      </button>
                    ) : compact ? (
                      <div className="call-time">
                        <span>上次错误</span>
                        <Time at={a?.last_error_at} />
                      </div>
                    ) : null}
                  </>
                )}
              </div>
            );
          })}
          {!recent.length && <div className="quiet">暂无成功调用</div>}
        </div>
      </article>
    );
  }
  function errorRow(e: OpsError, compact = false) {
    const stateLabel = errorState(e);
    const account = accounts.find((item) => item.id === e.account_id);
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
          <strong>
            {e.account_name || `账号 #${e.account_id}`}
            {compact && account && <DegradationBadge account={account} compact />}
          </strong>
          <span>
            <em className={`error-state ${stateLabel === "当前" ? "active" : ""}`}>
              {stateLabel}
            </em>
            {e.provider_error_code || e.message || "查看错误详情"}
          </span>
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
  const visibleGroups = currentGroups(groups, accounts);
  return (
    <div className={`app${quick ? " quick" : ""}${mobile ? " mobile" : ""}`}>
      {!quick && !mobile && (
        <aside className="sidebar" data-tauri-drag-region>
          <div className="brand">
            <span className="brand-icon">
              <Activity size={21} />
            </span>
            <strong>Sub2Ops</strong>
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
                {p.id === "events" && eventRows.length > 0 ? (
                  <span className="nav-count">{eventRows.length}</span>
                ) : null}
              </button>
            ))}
          </nav>
        </aside>
      )}
      <main className="main">
        <header className="topbar" data-tauri-drag-region>
          {quick ? (
            <nav className="quick-tabs" aria-label="快捷面板">
              <button
                className={quickTab === "groups" ? "active" : ""}
                onClick={() => setQuickTab("groups")}
              >
                分组
              </button>
              <button
                className={quickTab === "errors" ? "active" : ""}
                onClick={() => setQuickTab("errors")}
              >
                异常{eventRows.length > 0 && <span>{eventRows.length}</span>}
              </button>
            </nav>
          ) : (
            <div className="breadcrumb">
              <strong>{pages.find((p) => p.id === page)?.label}</strong>
            </div>
          )}
          <div className="top-actions">
            {preview && !quick && <span className="preview-label">预览</span>}
            {status()}
            <AccountOperations accounts={accounts} online={state.online} connectionKey={connectionKey} report={report} modelTest={setModelTestAccount}/>
            {!quick && !preview && !mobile && (
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
            {quick && (
              <button
                className="icon-button"
                title="关闭快捷面板"
                onClick={() => void command("hide_quick").catch(report)}
              >
                <X size={16} />
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
        <div
          key={mobile ? connectionKey : "content"}
          className={`content ${!quick && page === "events" ? "events-content" : ""} ${!quick && page === "records" ? "records-content" : ""}`}
        >
          <div ref={quickBody} className="content-inner">
            {!ready || state.initializing ? (
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
                {quickTab === "groups" ? (
                  <div className="quick-groups">
                    {visibleGroups.map((g) => groupRow(g, true))}
                    {!groups.length && <Empty text="没有分组记录" />}
                  </div>
                ) : (
                  <div className="error-list">
                    {eventRows.map((error) => errorRow(error, true))}
                    {!eventRows.length && (
                      <div className="quiet">
                        <Check size={15} />
                        暂无账号错误
                      </div>
                    )}
                  </div>
                )}
              </>
            ) : (
              <>
                <GroupManager key={connectionKey} connectionKey={connectionKey} active={page === "groups"} accounts={accounts} groups={groups} mobile={mobile} online={state.online} back={() => setPage("accounts")} report={report} modelTest={(account) => setModelTestAccount(account)} changed={(count, saving) => setGroupChanges({ count, busy: saving })}/>
                <div key={`${page}:${connectionKey}`} className="page-surface" data-page={page}>
                  <div className="page-heading">
                    <div>
                      <h1>{pages.find((p) => p.id === page)?.label}</h1>
                    </div>
                    {page !== "records" && <span className="updated">
                      <Clock3 size={13} />
                      更新于 <Time at={snap.observed_at} />
                    </span>}
                  </div>
                {page === "records" && <UsageRecords key={connectionKey} mobile={mobile} online={state.online} foreground={state.foreground !== false} desktop={state.platform === "macos"} accounts={accounts} columns={state.preferences.record_columns} saveColumns={async (columns) => {
                  await command("preferences", { ...state.preferences, launchAtLogin: state.preferences.launch_at_login, recordColumns: columns });
                }}/>}
                {page === "accounts" && (
                  <>
                    {!mobile && <div className="account-toolbar"><QuotaRefresh online={state.online} active={state.foreground !== false} report={report} /><button onClick={() => { setTemplateAccount(null); setTemplatesOpen(true); }}>账号模板</button></div>}
                    {mobile && <div className="mobile-group-entry"><button onClick={() => setPage("groups")}><Layers3 size={18}/>分组管理<ChevronRight size={16}/></button></div>}
                    <div className="filters">
                      <label className="search">
                        <Search size={15} />
                        <input
                          placeholder="搜索名称或账号 ID"
                          value={query}
                          onChange={(e) => setQuery(e.target.value)}
                        />
                      </label>
                      {mobile && <button aria-label="筛选账号" onClick={() => setFiltersOpen(true)}><SlidersHorizontal size={18}/>筛选</button>}
                      {mobile && filtersOpen && <div className="filter-shade" onClick={() => setFiltersOpen(false)}/>}
                      <div className={`filter-options${filtersOpen ? " open" : ""}`} role={mobile && filtersOpen ? "dialog" : undefined} aria-label={mobile ? "账号筛选" : undefined}>
                      {mobile && <header><h2>筛选账号</h2><button className="icon-button" aria-label="关闭筛选" onClick={() => setFiltersOpen(false)}><X size={20}/></button></header>}
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
                      <select
                        aria-label="质量筛选"
                        value={qualityFilter}
                        onChange={(e) => setQualityFilter(e.target.value)}
                      >
                        <option value="">全部质量</option>
                        <option value="green">绿色 · 良好</option>
                        <option value="yellow">黄色 · 关注</option>
                        <option value="red">红色 · 异常</option>
                      </select>
                      {mobile && <button className="primary" onClick={() => setFiltersOpen(false)}>完成</button>}
                      </div>
                    </div>
                    {mobile && <div className="account-toolbar"><QuotaRefresh online={state.online} active={state.foreground !== false} report={report}/><button onClick={() => { setTemplateAccount(null); setTemplatesOpen(true); }}>账号模板</button><button onClick={() => setGroupsOpen(true)}><Layers3 size={17}/>分组动态</button></div>}
                    {mobile && <div className="mobile-sort"><span>{filteredAccounts.length} 个账号</span><select aria-label="账号排序" value={`${sortBy}:${ascending ? "asc" : "desc"}`} onChange={(e) => { const [by, order] = e.target.value.split(":"); setSortBy(by as "priority" | "quality"); setAscending(order === "asc"); }}><option value="priority:asc">优先级 ↑</option><option value="priority:desc">优先级 ↓</option><option value="quality:desc">质量 ↓</option><option value="quality:asc">质量 ↑</option></select></div>}
                    <div className="table-wrap">
                      <div className="selection-bar">
                        {mobile && <label className="mobile-select-all"><input type="checkbox" aria-label="全选当前筛选账号" checked={filteredAccounts.length > 0 && filteredAccounts.every((a) => selected.has(a.id))} disabled={!state.online} onChange={(e) => setSelected(e.target.checked ? new Set(filteredAccounts.map((a) => a.id)) : new Set())}/>全选</label>}
                        <span>已选 {selected.size} 个账号</span>
                        <button
                          className="danger-text"
                          disabled={!state.online || selected.size === 0}
                          onClick={() =>
                            setDeleteAccounts(
                              accounts.filter((a) => selected.has(a.id)),
                            )
                          }
                        >
                          删除所选
                        </button>
                      </div>
                      {mobile ? <MobileAccounts accounts={filteredAccounts} online={state.online} selected={selected} select={(id, checked) => setSelected((old) => { const next = new Set(old); if (checked) next.add(id); else next.delete(id); return next; })} schedule={schedule} quality={setQualityAccount} test={setTestAccount} remove={(a) => setDeleteAccounts([a])} error={(id) => void openError(id)} modelTest={(a) => setModelTestAccount(a)} modelDetection={(a) => setModelDetectionAccount(a)} template={(a) => { setTemplateAccount(a); setTemplatesOpen(true); }} report={report}/> : <table className="accounts-table">
                        <colgroup><col className="col-select"/><col className="col-name"/><col className="col-priority"/><col className="col-quality"/><col className="col-status"/><col className="col-usage"/><col className="col-error"/><col className="col-schedule"/><col className="col-actions"/></colgroup>
                        <thead>
                          <tr>
                            <th className="select-cell">
                              <input
                                type="checkbox"
                                aria-label="全选当前筛选账号"
                                checked={
                                  filteredAccounts.length > 0 &&
                                  filteredAccounts.every((a) =>
                                    selected.has(a.id),
                                  )
                                }
                                ref={(element) => {
                                  if (element)
                                    element.indeterminate =
                                      filteredAccounts.some((a) =>
                                        selected.has(a.id),
                                      ) &&
                                      !filteredAccounts.every((a) =>
                                        selected.has(a.id),
                                      );
                                }}
                                disabled={
                                  !state.online || !filteredAccounts.length
                                }
                                onChange={(e) =>
                                  setSelected(
                                    e.target.checked
                                      ? new Set(
                                          filteredAccounts.map((a) => a.id),
                                        )
                                      : new Set(),
                                  )
                                }
                              />
                            </th>
                            <th>账号</th>
                            <th
                              aria-sort={
                                sortBy === "priority"
                                  ? ascending
                                    ? "ascending"
                                    : "descending"
                                  : "none"
                              }
                            >
                              <button
                                className="sort-button"
                                onClick={() => {
                                  setSortBy("priority");
                                  setAscending(
                                    sortBy === "priority" ? !ascending : true,
                                  );
                                }}
                              >
                                优先级{" "}
                                {sortBy === "priority"
                                  ? ascending
                                    ? "↑"
                                    : "↓"
                                  : ""}
                              </button>
                            </th>
                            <th
                              aria-sort={
                                sortBy === "quality"
                                  ? ascending
                                    ? "ascending"
                                    : "descending"
                                  : "none"
                              }
                            >
                              <button
                                className="sort-button"
                                onClick={() => {
                                  setSortBy("quality");
                                  setAscending(
                                    sortBy === "quality" ? !ascending : false,
                                  );
                                }}
                              >
                                质量{" "}
                                {sortBy === "quality"
                                  ? ascending
                                    ? "↑"
                                    : "↓"
                                  : ""}
                              </button>
                            </th>
                            <th>当前状态</th>
                            <th>用量窗口</th>
                            <th>最近报错</th>
                            <th>允许调度</th>
                            <th>操作</th>
                          </tr>
                        </thead>
                        <tbody>
                          {filteredAccounts.map((a) => (
                            <tr key={`${connectionKey}:${a.id}`}>
                              <td className="select-cell">
                                <input
                                  type="checkbox"
                                  aria-label={`选择 ${a.name}`}
                                  checked={selected.has(a.id)}
                                  disabled={!state.online}
                                  onChange={(e) =>
                                    setSelected((old) => {
                                      const next = new Set(old);
                                      if (e.target.checked) next.add(a.id);
                                      else next.delete(a.id);
                                      return next;
                                    })
                                  }
                                />
                              </td>
                              <td className="account-name">
                                <strong title={a.name}>{a.name}</strong>
                                <small className="account-identity">
                                  {a.platform === "openai"
                                    ? "OpenAI"
                                    : a.platform === "grok"
                                      ? "Grok"
                                      : a.platform}{" "}
                                  ·{" "}
                                  {a.type === "oauth"
                                    ? "OAuth"
                                    : a.type === "apikey"
                                      ? "Key"
                                      : a.type}
                                  {a.managed && (
                                    <span className="managed-tag">
                                      回退托管
                                    </span>
                                  )}
                                </small>
                                <DegradationBadge account={a}/><AccountOperationStatus id={a.id}/>
                              </td>
                              <td>
                                <PriorityEditor
                                  account={a}
                                  online={state.online}
                                  report={report}
                                />
                              </td>
                              <td className="quality-cell">
                                {["openai", "grok"].includes(a.platform) &&
                                ["oauth", "apikey"].includes(a.type) ? (
                                  <QualityBadge
                                    value={a.quality}
                                    onClick={() => setQualityAccount(a)}
                                  />
                                ) : (
                                  "—"
                                )}
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
                                    : a.blockers
                                        .map((b) => b.label)
                                        .join(" / ")}
                                </span>
                                {a.blockers
                                  .filter(
                                    (b) => b.code === "rate_limit_reset_at",
                                  )
                                  .map((b) => (
                                    <small key={b.code} className="limit-until">
                                      预计解除{" "}
                                      {b.until ? (
                                        <Time at={b.until} />
                                      ) : (
                                        "时间未知"
                                      )}
                                    </small>
                                  ))}
                              </td>
                              <td>
                                <UsageCell
                                  account={a}
                                  online={state.online}
                                  refresh={() => command("refresh")}
                                  report={report}
                                />
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
                                  <span className="muted">暂无记录</span>
                                )}
                              </td>
                              <td>{schedule(a)}</td>
                              <td>
                                <div className="account-actions">
                                  {a.platform === "openai" && ["oauth", "apikey"].includes(a.type) && <>
                                    <button className="degradation-action" disabled={!state.online} onClick={() => setModelTestAccount(a)}>模型测试</button>
                                    <DegradationAction account={a} online={state.online} report={report}/>
                                    <button className="detection-action" disabled={!state.online} onClick={() => setModelDetectionAccount(a)}>定时检测</button>
                                  </>}
                                  <details className="group-account-menu"><summary aria-label={`${a.name}更多操作`}><MoreHorizontal size={16}/>更多</summary><div>
                                    <button className="test-button" disabled={!state.online || !["openai", "grok"].includes(a.platform) || !["oauth", "apikey"].includes(a.type)} onClick={() => setTestAccount(a)}>测试连接</button>
                                    <RecoverStateButton account={a} online={state.online} report={report}/>
                                    {a.platform === "openai" && ["oauth", "apikey"].includes(a.type) && <button disabled={!state.online} onClick={() => { setTemplateAccount(a); setTemplatesOpen(true); }}>应用模板</button>}
                                    <button className="danger-text" disabled={!state.online} onClick={() => setDeleteAccounts([a])}>删除</button>
                                  </div></details>
                                </div>
                              </td>
                            </tr>
                          ))}
                        </tbody>
                      </table>}
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
                  <div className="events-view">
                    <nav
                      className="event-tabs"
                      role="tablist"
                      aria-label="事件类型"
                    >
                      <button
                        role="tab"
                        id="errors-tab"
                        aria-selected={eventTab === "errors"}
                        aria-controls="errors-panel"
                        onClick={() => setEventTab("errors")}
                      >
                        上游与认证错误
                      </button>
                      <button
                        role="tab"
                        id="recoveries-tab"
                        aria-selected={eventTab === "recoveries"}
                        aria-controls="recoveries-panel"
                        onClick={() => setEventTab("recoveries")}
                      >
                        恢复成功
                      </button>
                    </nav>
                    <div className="event-panels">
                      <section
                        id="errors-panel"
                        className="event-panel"
                        role="tabpanel"
                        aria-labelledby="errors-tab"
                        hidden={eventTab !== "errors"}
                      >
                        <div className="section-heading">
                          <span>{eventRows.length} 条记录</span>
                          <button
                            className="text-button"
                            disabled={!state.online}
                            onClick={() => void loadEvents()}
                          >
                            刷新记录
                          </button>
                        </div>
                        <div className="error-list">
                          {eventRows.map((error) => errorRow(error))}
                          {!eventRows.length && <Empty text="暂无账号错误" />}
                        </div>
                        {cursor && (
                          <button
                            className="load-more"
                            disabled={!state.online}
                            onClick={() => void loadEvents("more")}
                          >
                            加载更早记录
                          </button>
                        )}
                      </section>
                      <section
                        id="recoveries-panel"
                        className="event-panel"
                        role="tabpanel"
                        aria-labelledby="recoveries-tab"
                        hidden={eventTab !== "recoveries"}
                      >
                        <RecoveryHistory
                          key={connectionKey}
                          latest={snap.recoveries ?? []}
                          accounts={accounts}
                          online={state.online}
                          report={report}
                        />
                      </section>
                    </div>
                  </div>
                )}
                {(page === "features" || page === "settings") &&
                  (config ? (
                    <SettingsPage
                      page={page}
                      config={config}
                      accounts={accounts}
                      state={state}
                      runtimeVersion={runtimeVersion}
                      prefs={prefs}
                      onChange={(key, value) =>
                        setConfig({ ...config, [key]: value })
                      }
                      onMessage={setToast}
                      onError={report}
                      beforeConnectionChange={beforeConnectionChange}
                    />
                  ) : (
                    <Empty text="正在读取设置" />
                  ))}
                </div>
              </>
            )}
          </div>
        </div>
        {quick && (
          <footer className="quick-bottom">
            <span>
              <Time at={snap?.observed_at} />
            </span>
            <button onClick={() => void command("show_main").catch(report)}>
              打开主窗口 <ArrowUpRight size={14} />
            </button>
          </footer>
        )}
      </main>
      {mobile && <nav className="bottom-nav" aria-label="主导航">{pages.filter((p) => p.id !== "groups").map((p) => <button key={p.id} className={page === p.id || page === "groups" && p.id === "accounts" ? "active" : ""} aria-current={page === p.id ? "page" : undefined} onClick={() => setPage(p.id)}><p.icon size={21}/><span>{p.label}</span></button>)}</nav>}
      {leaveGroups && <div className="modal-backdrop"><section className="discard-groups-dialog" role="dialog" aria-modal="true" aria-label="未应用的分组修改"><h2>放弃分组草稿并更换连接？</h2><div><button onClick={() => { leaveGroups(false); setLeaveGroups(null); }}>保留草稿</button><button className="primary" onClick={() => { leaveGroups(true); setLeaveGroups(null); }}>放弃并继续</button></div></section></div>}
      {mobile && groupsOpen && <div className="mobile-groups-surface" role="dialog" aria-modal="true" aria-label="分组动态"><header><h2>分组动态</h2><button className="icon-button" aria-label="关闭分组动态" onClick={() => setGroupsOpen(false)}><X size={20}/></button></header><div className="quick-groups">{visibleGroups.map((g) => groupRow(g, true))}{!groups.length && <Empty text="没有分组记录"/>}</div></div>}
      {deleteAccounts && (
        <DeleteAccountsDialog
          accounts={deleteAccounts}
          online={state.online}
          removed={(id) => {
            setRemovedIds((old) => new Set([...old, id]));
            setSelected(
              (old) => new Set([...old].filter((value) => value !== id)),
            );
          }}
          finished={(results) =>
            setSelected(
              new Set(
                results
                  .filter(
                    (r) =>
                      r.status === "failed" &&
                      accounts.some((a) => a.id === r.id),
                  )
                  .map((r) => r.id),
              ),
            )
          }
          close={() => setDeleteAccounts(null)}
        />
      )}
      {(detail || detailBusy) && (
        <div className="drawer-backdrop" onClick={closeDetail}>
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
                  closeDetail();
                }}
              >
                {quick ? (
                  <>
                    <ChevronLeft size={17} />
                    返回
                  </>
                ) : (
                  <X size={19} />
                )}
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
                    {detail.notification && <><dt>降智报警</dt><dd>{detail.notification.status === "suppressed" ? detail.notification.reason === "degradation_mark" ? "降智标记静默" : "已抑制" : ({ delivered: "已推送", queued: "待推送", retry: "待重试", unavailable: "投递状态暂不可读取" } as Record<string, string>)[detail.notification.status] ?? "—"}</dd></>}
                  </dl>
                  <h3>错误内容</h3>
                  {accounts.find((a) => a.id === detail.account_id) && <DegradationAction account={accounts.find((a) => a.id === detail.account_id)!} online={state.online} report={report}/>}
                  <pre>
                    {detail.content || detail.message || "没有额外错误内容"}
                  </pre>
                  {detail.content_limited && (
                    <p className="content-truncated">内容已截断</p>
                  )}
                  <div className="section-heading">
                    <h3>账号近期错误</h3>
                      <button
                        className="text-button"
                      onClick={() => {
                        setPage("events");
                        setQuickTab("errors");
                        closeDetail();
                      }}
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
      {testAccount && (
        <TestDialog
          account={accounts.find((a) => a.id === testAccount.id) ?? testAccount}
          online={state.online}
          close={() => setTestAccount(null)}
        />
      )}
      {modelTestAccount && (
        <ModelTestDialog
          key={`${connectionKey}:${modelTestAccount.id}`}
          account={accounts.find((a) => a.id === modelTestAccount.id) ?? modelTestAccount}
          online={state.online}
          report={report}
          close={() => setModelTestAccount(null)}
          concurrency={state.preferences.model_test_concurrency ?? 1}
          saveConcurrency={(value) => void prefs({ model_test_concurrency: value })}
        />
      )}
      {templatesOpen && <AccountTemplates key={connectionKey} accounts={accounts} online={state.online} initialAccount={templateAccount} close={() => setTemplatesOpen(false)} report={report}/>}
      {modelDetectionAccount && <ModelDetectionDialog key={`${connectionKey}:${modelDetectionAccount.id}`} account={accounts.find((a) => a.id === modelDetectionAccount.id) ?? modelDetectionAccount} online={state.online} close={() => setModelDetectionAccount(null)} showResult={() => { setModelTestAccount(modelDetectionAccount); setModelDetectionAccount(null); }} report={report}/>}
      {qualityAccount && (
        <QualityDialog
          key={`${connectionKey}:${qualityAccount.id}`}
          account={qualityAccount}
          onClose={() => setQualityAccount(null)}
        />
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
  beforeChange = async () => true,
}: {
  state: ViewState;
  onError: (e: unknown) => void;
  beforeChange?: () => Promise<boolean>;
}) {
  const [url, setUrl] = useState(
      state.preferences.base_url || "",
    ),
    [key, setKey] = useState(""),
    [busy, setBusy] = useState(false);
  return (
    <form
      className="connection-form"
      onSubmit={async (e) => {
        e.preventDefault();
        if (!await beforeChange()) return;
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
  oauth_recovery_monitor_enabled: "额度恢复监控",
  oauth_auto_reset_credit_enabled: "7d 100% 且 429 时自动用卡",
  oauth_daily_test_enabled: "每日定时测活",
  oauth_daily_test_time: "测活时间（北京时间）",
  oauth_usage_refresh_concurrency: "额度查询并发",
  oauth_recovery_test_concurrency: "测活并发",
  oauth_early_probe_batch_size: "单轮账号上限",
  oauth_recovery_test_model_id: "测活模型",
};
function SettingsPage({
  page,
  config,
  accounts,
  state,
  runtimeVersion,
  prefs,
  onChange,
  onMessage,
  onError,
  beforeConnectionChange,
}: {
  page: Page;
  config: Config;
  accounts: Account[];
  state: ViewState;
  runtimeVersion: string | null;
  prefs: (p: Partial<Preferences>) => Promise<void>;
  onChange: (k: string, c: ConfigSection) => void;
  onMessage: (m: string) => void;
  onError: (e: unknown) => void;
  beforeConnectionChange: () => Promise<boolean>;
}) {
  async function action(name: string) {
    try {
      const result = await api<{ message?: string }>(
        "POST",
        `/actions/${name}`,
      );
      onMessage(result.message || "操作完成");
    } catch (e) {
      onError(e);
    }
  }
  return (
    <div className={`settings-stack ${page === "features" ? "features-stack" : ""}`}>
      {page === "features" ? (
        <>
          <ConfigForm
            section="oauth"
            title="OAuth 恢复与测活"
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
          <section className="feature-models">
            <h2>模型配置</h2>
            <ModelConfig key={`${state.preferences.base_url}:${state.connection_revision ?? 0}`} online={state.online}/>
          </section>
        </>
      ) : (
        <>
          <section className="settings-card">
            <h2>客户端</h2>
            {state.platform !== "android" && <Field
              label="开机启动"
              value={state.preferences.launch_at_login}
              set={(v) => void prefs({ launch_at_login: Boolean(v) })}
            />}
            <div className="field">
              <span>当前版本</span>
              <button
                onClick={() =>
                  void command<string>("check_updates")
                    .then(onMessage)
                    .catch(onError)
                }
              >
                {runtimeVersion ?? appVersion} · 检查更新 <ExternalLink size={13} />
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
          <section className="settings-card">
            <Connection state={state} onError={onError} beforeChange={beforeConnectionChange} />
            <button
              className="danger-text"
              onClick={() => void beforeConnectionChange().then((allowed) => { if (allowed) return command("disconnect"); }).catch(onError)}
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
  value,
  online,
  onChange,
  onMessage,
  onError,
  children,
}: {
  section: string;
  title: string;
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
