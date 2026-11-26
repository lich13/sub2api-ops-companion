import {
  bridgeFailure,
  credentialsRequired,
  errorMessage,
  ignoredFailure,
} from "./bridgeError";
import { closedDialog, type AccountDialog } from "./accountDialog";
import React, {
  Suspense,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
} from "react";
import {
  Check,
  ChevronRight,
  CircleAlert,
  Clock3,
  Activity,
  Bell,
  Layers3,
  ListOrdered,
  LayoutTemplate,
  Network,
  Plus,
  Trash2,
  LoaderCircle,
  Plug,
  RefreshCw,
  Search,
  Settings2,
  SlidersHorizontal,
  Ticket,
  Unplug,
  Users,
  X,
} from "lucide-react";
import { api, command, preview, subscribe } from "./bridge";
import { subscribeObserved } from "./tauriEvents";
import { isAccountSort, readAccountSort, saveAccountSort } from "./accountSort";
import {
  connectionIdentity,
  filterAccounts,
  currentGroups,
  defaultConnectionName,
  savedConnections,
  sortPriority,
  sortRecentCall,
  fullTime,
  initialState,
  type Account,
  type Config,
  type Group,
  type OpsError,
  type Preferences,
  type SavedConnection,
  type ViewState,
} from "./types";
import "./style.css";
import { KeyQuotaProvider, KeyQuotaRefreshSettings } from "./KeyQuotaSettings";
import { useErrorFeed, type ErrorCategory } from "./useErrorFeed";
import { MiniUsage, QuotaRefresh } from "./AccountControls";
import { useConnectionBatch, ConnectionBatchResults } from "./ConnectionBatch";
import {
  RecoverSelectedButton,
  resetCreditEligible,
} from "./AccountRecoveryButton";
import { useAccountActions } from "./AccountActions";
import AccountOperations from "./OperationPanel";
import {
  accountOperation,
  bindOperationConnection,
  operationEpoch,
} from "./accountOperations";
import { bindAccountModelsConnection } from "./accountModels";
import MobileAccounts from "./MobileAccounts";
import ConnectionStatusCard from "./ConnectionStatusCard";
import ConnectionView from "./ConnectionView";
import ClientUpdateCard from "./ClientUpdateCard";
import SettingsView from "./SettingsView";
import { SwitchControl as Switch } from "./SwitchControl";
import DegradationAction, { DegradationBadge } from "./DegradationMark";
import {
  isCleanEscape,
  useBackdropDismiss,
  useDialogFocus,
} from "./dialogFocus";
import { listenBack, useBackAction } from "./mobile";
import { getVersion as getRuntimeVersion } from "@tauri-apps/api/app";

type PreloadableComponent<P extends object> = React.FC<P> & {
  preload: () => Promise<unknown>;
};

function lazyWithPreload<P extends object>(
  loader: () => Promise<{ default: React.FC<P> }>,
): PreloadableComponent<P> {
  let Resolved: React.FC<P> | null = null;
  let pending: Promise<{ default: React.FC<P> }> | null = null;
  const load = () => {
    if (!pending) {
      pending = loader().then((mod) => {
        Resolved = mod.default;
        return mod;
      });
    }
    return pending;
  };
  const Lazy = React.lazy(load);
  const Component: PreloadableComponent<P> = Object.assign(
    function PreloadableLazy(props: P) {
      if (Resolved) {
        return <Resolved {...props} />;
      }
      return (
        <Suspense fallback={null}>
          <Lazy {...props} />
        </Suspense>
      );
    },
    { preload: load },
  );
  return Component;
}

const LazyAccountCreationDialog = lazyWithPreload(
  () => import("./AccountCreationDialog"),
);
const LazyProxyBatchDialog = lazyWithPreload(
  () => import("./ProxyBatchDialog"),
);
const LazyReauthorizeAccountDialog = lazyWithPreload(
  () => import("./ReauthorizeAccountDialog"),
);
const LazyTestDialog = lazyWithPreload(() => import("./TestDialog"));
const LazyModelTestDialog = lazyWithPreload(() => import("./ModelTestDialog"));
const LazyAccountTemplates = lazyWithPreload(
  () => import("./AccountTemplates"),
);
const LazyAccountSettingsDialog = lazyWithPreload(
  () => import("./AccountSettingsDialog"),
);
const LazyModelDetectionDialog = lazyWithPreload(
  () => import("./ModelDetectionDialog"),
);
const LazyKeepaliveDialog = lazyWithPreload(() => import("./KeepaliveDialog"));
const LazyDeleteAccountsDialog = lazyWithPreload(async () => ({
  default: (await import("./AccountManagement")).DeleteAccountsDialog,
}));
const LazyResetCreditsDialog = lazyWithPreload(async () => ({
  default: (await import("./AccountManagement")).ResetCreditsDialog,
}));
const LazyGroupManager = lazyWithPreload(() => import("./GroupManager"));
const LazyPriorityManager = lazyWithPreload(() => import("./PriorityManager"));
const LazyUsageRecords = lazyWithPreload(() => import("./UsageRecords"));
const LazyDetectionLogs = lazyWithPreload(() => import("./DetectionLogs"));
const LazyRecoveryHistory = lazyWithPreload(() => import("./RecoveryHistory"));

const secondarySurfaces = [
  LazyGroupManager,
  LazyPriorityManager,
  LazyUsageRecords,
  LazyDetectionLogs,
  LazyRecoveryHistory,
  LazyTestDialog,
  LazyModelTestDialog,
  LazyAccountTemplates,
  LazyAccountSettingsDialog,
  LazyModelDetectionDialog,
  LazyKeepaliveDialog,
  LazyDeleteAccountsDialog,
  LazyResetCreditsDialog,
];

let secondaryPrewarmed = false;
function prewarmSecondarySurfaces() {
  if (secondaryPrewarmed) return;
  secondaryPrewarmed = true;
  if (import.meta.env.MODE === "test") {
    for (const surface of secondarySurfaces) {
      void surface.preload().catch(() => {});
    }
    return;
  }
  void (async () => {
    for (const surface of secondarySurfaces) {
      await surface.preload().catch(() => {});
    }
  })();
}

if (typeof window !== "undefined" && import.meta.env.MODE === "test") {
  prewarmSecondarySurfaces();
}

let latestObservedAt = "";
const observedSubscribers = new Set<() => void>();
function publishObservedAt(next: string | null | undefined) {
  const value = next ?? "";
  if (value !== latestObservedAt) {
    latestObservedAt = value;
    for (const cb of observedSubscribers) cb();
  }
}
function subscribeObservedStore(cb: () => void) {
  observedSubscribers.add(cb);
  return () => {
    observedSubscribers.delete(cb);
  };
}
function getObservedSnapshot() {
  return latestObservedAt;
}

function sameStringArray(
  a: readonly string[] | null | undefined,
  b: readonly string[] | null | undefined,
): boolean {
  if (a === b) return true;
  if (!a || !b || a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) {
    if (a[i] !== b[i]) return false;
  }
  return true;
}

function sameConnections(
  a: readonly SavedConnection[] | undefined,
  b: readonly SavedConnection[] | undefined,
): boolean {
  if (a === b) return true;
  const left = a ?? [];
  const right = b ?? [];
  if (left.length !== right.length) return false;
  for (let i = 0; i < left.length; i++) {
    if (
      left[i].name !== right[i].name ||
      left[i].base_url !== right[i].base_url
    ) {
      return false;
    }
  }
  return true;
}

function samePreferences(a: Preferences, b: Preferences): boolean {
  if (a === b) return true;
  if (
    a.base_url !== b.base_url ||
    (a.model_test_concurrency ?? null) !== (b.model_test_concurrency ?? null) ||
    !sameStringArray(a.record_columns ?? null, b.record_columns ?? null) ||
    !sameConnections(a.connections, b.connections)
  ) {
    return false;
  }
  const mpA = a.model_test_preferences;
  const mpB = b.model_test_preferences;
  if (mpA === mpB) return true;
  const keysA = mpA ? Object.keys(mpA) : [];
  const keysB = mpB ? Object.keys(mpB) : [];
  if (keysA.length !== keysB.length) return false;
  for (const key of keysA) {
    const va = mpA![key];
    const vb = mpB?.[key];
    if (!vb || va.engine !== vb.engine || va.sample_count !== vb.sample_count) {
      return false;
    }
  }
  return true;
}

function sameUpstreamConnection(
  a: ViewState["upstream_connection"],
  b: ViewState["upstream_connection"],
): boolean {
  const left = a ?? null;
  const right = b ?? null;
  if (left === right) return true;
  if (!left || !right) return false;
  return (
    left.state === right.state &&
    left.endpoint === right.endpoint &&
    left.http_status === right.http_status &&
    left.message === right.message &&
    left.retryable === right.retryable &&
    left.checked_at === right.checked_at
  );
}

function mergeIncomingState(prev: ViewState, next: ViewState): ViewState {
  publishObservedAt(next.snapshot?.observed_at);
  const prevKey = `${prev.preferences.base_url}:${prev.connected}:${prev.connection_revision ?? 0}`;
  const nextKey = `${next.preferences.base_url}:${next.connected}:${next.connection_revision ?? 0}`;
  if (
    prevKey === nextKey &&
    prev.snapshot &&
    next.snapshot &&
    prev.snapshot_revision &&
    prev.snapshot_revision === next.snapshot_revision
  ) {
    if (
      prev.online === next.online &&
      prev.error === next.error &&
      prev.foreground === next.foreground &&
      prev.initializing === next.initializing &&
      samePreferences(prev.preferences, next.preferences) &&
      sameUpstreamConnection(prev.upstream_connection, next.upstream_connection)
    ) {
      return prev;
    }
    return { ...next, snapshot: prev.snapshot };
  }
  return next;
}

function LazyAccountCreationButton({
  accounts,
  online,
  connectionKey,
  report,
}: {
  accounts: Account[];
  online: boolean;
  connectionKey: string;
  report: (message: string) => void;
}) {
  const [opened, setOpened] = useState<string | null>(null);
  return (
    <>
      <button disabled={!online} onClick={() => setOpened(connectionKey)}>
        <Plus size={15} />
        添加账号
      </button>
      {opened === connectionKey && (
        <Suspense fallback={null}>
          <LazyAccountCreationDialog
            key={connectionKey}
            accounts={accounts}
            online={online}
            close={() => setOpened(null)}
            report={report}
          />
        </Suspense>
      )}
    </>
  );
}

type Page =
  | "accounts"
  | "groups"
  | "priorities"
  | "records"
  | "events"
  | "features"
  | "settings";
const pages: { id: Page; label: string; icon: typeof Activity }[] = [
  { id: "accounts", label: "账号", icon: Users },
  { id: "groups", label: "分组", icon: Layers3 },
  { id: "priorities", label: "优先级", icon: ListOrdered },
  { id: "records", label: "记录", icon: ListOrdered },
  { id: "events", label: "事件", icon: Bell },
  { id: "features", label: "功能", icon: SlidersHorizontal },
  { id: "settings", label: "设置", icon: Settings2 },
];
function Time({ at }: { at: string | null | undefined }) {
  return <time>{fullTime(at)}</time>;
}
function SnapshotObservedTime({ fallback }: { fallback?: string | null }) {
  const observed = useSyncExternalStore(
    subscribeObservedStore,
    getObservedSnapshot,
  );
  return <Time at={observed || fallback} />;
}
export default function App() {
  const [state, setState] = useState<ViewState>(initialState),
    [ready, setReady] = useState(false),
    [page, setPage] = useState<Page>("accounts"),
    [accountSort, setAccountSort] = useState(readAccountSort),
    [toast, setToast] = useState(""),
    [busy, setBusy] = useState<number | null>(null),
    [detail, setDetail] = useState<OpsError | null>(null),
    [detailBusy, setDetailBusy] = useState(false),
    [query, setQuery] = useState(""),
    [group, setGroup] = useState(""),
    [platform, setPlatform] = useState(""),
    [filter, setFilter] = useState(""),
    [type, setType] = useState(""),
    [config, setConfig] = useState<Config | null>(null),
    [runtimeVersion, setRuntimeVersion] = useState<string | null>(null),
    [eventTab, setEventTab] = useState<ErrorCategory | "recoveries" | "logs">(
      "degradation",
    ),
    [selected, setSelected] = useState<Set<number>>(new Set()),
    [removedIds, setRemovedIds] = useState<Set<number>>(new Set()),
    [filtersOpen, setFiltersOpen] = useState(false),
    [groupsOpen, setGroupsOpen] = useState(false),
    [groupChanges, setGroupChanges] = useState({ count: 0, busy: false }),
    [priorityChanges, setPriorityChanges] = useState(0),
    [leaveGroups, setLeaveGroups] = useState<
      ((proceed: boolean) => void) | null
    >(null),
    [switchingUrl, setSwitchingUrl] = useState<string | null>(null),
    [connectionModal, setConnectionModal] = useState<{
      baseUrl: string;
      name: string;
      requireKey?: boolean;
    } | null>(null),
    [switcherOpen, setSwitcherOpen] = useState(false),
    discardRef = useRef<HTMLElement>(null),
    connectionModalRef = useRef<HTMLDivElement>(null),
    groupsSurfaceRef = useRef<HTMLDivElement>(null),
    filtersSheetRef = useRef<HTMLDivElement>(null),
    drawerRef = useRef<HTMLElement>(null),
    schedulePending = useRef(false),
    detailGeneration = useRef(0),
    detailAbort = useRef<AbortController | null>(null);
  const [dialog, showDialog] = useState<AccountDialog>(closedDialog);
  const testAccount = dialog.kind === "test" ? dialog.account : null;
  const modelTestAccount = dialog.kind === "modelTest" ? dialog.account : null;
  const modelDetectionAccount =
    dialog.kind === "detection" ? dialog.account : null;
  const settingsAccount = dialog.kind === "settings" ? dialog : null;
  const reauthorizeAccount = dialog.kind === "reauthorize" ? dialog : null;
  const deleteAccounts = dialog.kind === "delete" ? dialog.accounts : null;
  const resetAccounts = dialog.kind === "reset" ? dialog.accounts : null;
  const proxyAccounts = dialog.kind === "proxy" ? dialog.accounts : null;
  const connections = useMemo(
    () => savedConnections(state.preferences),
    [state.preferences],
  );
  const activeUrl = state.preferences.base_url.trim().replace(/\/+$/, "");
  const activeConnection = useMemo(
    () =>
      connections.find((item) => item.base_url === activeUrl) ??
      (activeUrl
        ? { name: defaultConnectionName(activeUrl), base_url: activeUrl }
        : null),
    [connections, activeUrl],
  );
  const sortBy = accountSort.startsWith("recent:") ? "recent" : "priority";
  const ascending = accountSort.endsWith(":asc");
  function updateAccountSort(value: string) {
    if (!isAccountSort(value)) return;
    setAccountSort(value);
    saveAccountSort(value);
  }
  function closeDetail() {
    detailAbort.current?.abort();
    detailAbort.current = null;
    detailGeneration.current = detailGeneration.current + 1;
    setDetail(null);
    setDetailBusy(false);
  }
  async function beforeConnectionChange() {
    if (groupChanges.busy) {
      setToast("分组正在保存，请等待结果");
      return false;
    }
    if (!groupChanges.count && !priorityChanges) return true;
    return new Promise<boolean>((resolve) => setLeaveGroups(() => resolve));
  }
  async function switchConnection(item: SavedConnection) {
    const targetUrl = item.base_url.trim().replace(/\/+$/, "");
    if (!targetUrl || switchingUrl) return;
    setSwitcherOpen(false);
    if (state.connected && targetUrl === activeUrl) return;
    if (!(await beforeConnectionChange())) return;
    setSwitchingUrl(targetUrl);
    try {
      await command("connect", {
        baseUrl: targetUrl,
        apiKey: "",
        name: item.name,
      });

      setToast(`已切换到 ${item.name}`);
    } catch (error) {
      report(error);
      if (credentialsRequired(error)) {
        setConnectionModal({
          baseUrl: targetUrl,
          name: item.name,
          requireKey: true,
        });
      }
    } finally {
      setSwitchingUrl(null);
    }
  }
  useDialogFocus(discardRef, !!leaveGroups);
  useDialogFocus(connectionModalRef, !!connectionModal);
  useDialogFocus(groupsSurfaceRef, groupsOpen);
  useDialogFocus(filtersSheetRef, filtersOpen);
  useDialogFocus(drawerRef, !!detail || detailBusy);
  const leaveGroupsBackdrop = useBackdropDismiss(() => {
    leaveGroups?.(false);
    setLeaveGroups(null);
  });
  const connectionModalBackdrop = useBackdropDismiss(() =>
    setConnectionModal(null),
  );
  const detailBackdrop = useBackdropDismiss(closeDetail);
  useBackAction(groupsOpen, () => setGroupsOpen(false));
  useBackAction(!!leaveGroups, () => {
    leaveGroups?.(false);
    setLeaveGroups(null);
  });
  useBackAction(filtersOpen, () => setFiltersOpen(false));
  useBackAction(!!detail || detailBusy, closeDetail);
  useEffect(() => {
    let disposed = false,
      off = () => {};
    void listenBack(() => {
      if (page !== "accounts") setPage("accounts");
      else void command("background").catch(() => {});
    }).then((unsubscribe) => {
      if (disposed) unsubscribe();
      else off = unsubscribe;
    });
    return () => {
      disposed = true;
      off();
    };
  }, [page]);
  const contentRef = useRef<HTMLDivElement>(null);
  const retainedScrollRef = useRef<{ groups: number; priorities: number }>({
    groups: 0,
    priorities: 0,
  });
  const previousPage = useRef(page);
  useEffect(() => {
    if (
      previousPage.current !== page &&
      (previousPage.current === "accounts" || page === "accounts")
    ) {
      setQuery("");
      setGroup("");
      setPlatform("");
      setFilter("");
      setType("");
      setSelected(new Set());
      setFiltersOpen(false);
      setGroupsOpen(false);
      showDialog(closedDialog);
      closeDetail();
      if (page === "accounts") {
        setAccountSort(readAccountSort());
        if (contentRef.current) {
          contentRef.current.scrollTop = 0;
          contentRef.current.scrollLeft = 0;
        }
      }
    }
    if (
      previousPage.current !== page &&
      (page === "groups" || page === "priorities") &&
      contentRef.current
    ) {
      contentRef.current.scrollTop = retainedScrollRef.current[page];
      contentRef.current.scrollLeft = 0;
    }
    previousPage.current = page;
  }, [page]);
  useEffect(() => {
    if (!ready || !state.connected || secondaryPrewarmed) return;
    if ("requestIdleCallback" in window) {
      const id = window.requestIdleCallback(prewarmSecondarySurfaces, {
        timeout: 2500,
      });
      return () => window.cancelIdleCallback(id);
    }
    const timer = setTimeout(prewarmSecondarySurfaces, 1500);
    return () => clearTimeout(timer);
  }, [ready, state.connected]);
  const connectionKey = connectionIdentity(state);
  const [visitedRetained, setVisitedRetained] = useState<{
    key: string;
    groups: boolean;
    priorities: boolean;
  }>({ key: connectionKey, groups: false, priorities: false });
  const navigatePage = useCallback(
    (next: (typeof pages)[number]["id"]) => {
      setPage(next);
      if (next === "groups" || next === "priorities") {
        setVisitedRetained((prev) => {
          const sameKey = prev.key === connectionKey;
          const groups = (sameKey && prev.groups) || next === "groups";
          const priorities =
            (sameKey && prev.priorities) || next === "priorities";
          if (
            sameKey &&
            prev.groups === groups &&
            prev.priorities === priorities
          ) {
            return prev;
          }
          return { key: connectionKey, groups, priorities };
        });
      }
    },
    [connectionKey],
  );
  useEffect(() => bindOperationConnection(connectionKey), [connectionKey]);
  useEffect(() => bindAccountModelsConnection(connectionKey), [connectionKey]);
  const connectionBatch = useConnectionBatch({
    connectionKey,
    storageKey: state.preferences.base_url,
    online: state.online,
    active: page === "accounts" && state.foreground !== false,
  });
  useEffect(() => {
    setSelected(new Set());
  }, [query, group, platform, filter, type, connectionKey]);
  useEffect(() => {
    closeDetail();
    setSwitcherOpen(false);
  }, [page, connectionKey, eventTab]);
  useEffect(() => {
    retainedScrollRef.current = { groups: 0, priorities: 0 };
    setRemovedIds(new Set());
    showDialog(closedDialog);
    closeDetail();
    setConnectionModal(null);
    setSwitcherOpen(false);
    schedulePending.current = false;
    setBusy(null);
    setGroupsOpen(false);
    setFiltersOpen(false);
    setConfig(null);
    setGroupChanges({ count: 0, busy: false });
    setPriorityChanges(0);
    setLeaveGroups((pending: ((proceed: boolean) => void) | null) => {
      pending?.(false);
      return null;
    });
  }, [connectionKey]);
  const report = useCallback((e: unknown) => {
    if (!ignoredFailure(e)) setToast(errorMessage(e));
  }, []);
  const accountActions = useAccountActions({
    online: state.online,
    connectionKey,
    report,
    modelTest: (account) => showDialog({ kind: "modelTest", account }),
    modelDetection: (account) => showDialog({ kind: "detection", account }),
    test: (account) => showDialog({ kind: "test", account }),
    keepalive: (account) => showDialog({ kind: "keepalive", account }),
    reauthorize: (account) =>
      showDialog({ kind: "reauthorize", account, connection: connectionKey }),
    template: (account) => {
      showDialog({ kind: "templates", account: account });
    },
    remove: (account) => showDialog({ kind: "delete", accounts: [account] }),
    settings: (account, section) =>
      showDialog({
        kind: "settings",
        account,
        section,
        connection: connectionKey,
      }),
  });
  useEffect(() => {
    let disposed = false;
    let un: () => void = () => {};
    let unObserved: () => void = () => {};
    void (async () => {
      [un, unObserved] = await Promise.all([
        subscribe((s) => {
          if (!disposed) setState((prev) => mergeIncomingState(prev, s));
        }),
        subscribeObserved((obs) => {
          if (!disposed) publishObservedAt(obs);
        }),
      ]);
      const s = await command("get_state");
      if (!disposed) {
        setState((prev) => mergeIncomingState(prev, s));
        setReady(true);
      } else {
        un();
        unObserved();
      }
    })().catch(report);
    const online = () => void command("refresh").catch(report);
    window.addEventListener("online", online);
    return () => {
      disposed = true;
      un();
      unObserved();
      window.removeEventListener("online", online);
    };
  }, [report]);
  useEffect(() => {
    if (toast) {
      const id = setTimeout(() => setToast(""), 6000);
      return () => clearTimeout(id);
    }
  }, [toast]);
  useEffect(() => {
    if (!("__TAURI_INTERNALS__" in window)) return;
    void getRuntimeVersion()
      .then(setRuntimeVersion)
      .catch(() => {});
  }, []);
  const snap = state.snapshot,
    groups = useMemo(() => snap?.groups ?? [], [snap?.groups]);
  const accounts = useMemo(
    () => (snap?.accounts ?? []).filter((a) => !removedIds.has(a.id)),
    [snap?.accounts, removedIds],
  );
  const accountById = useMemo(
    () => new Map(accounts.map((a) => [a.id, a])),
    [accounts],
  );
  const keepaliveAccount =
    dialog.kind === "keepalive"
      ? (accountById.get(dialog.account.id) ?? null)
      : null;
  const liveSelected = useMemo(() => {
    if (!selected.size) return selected;
    return [...selected].every((id) => accountById.has(id))
      ? selected
      : new Set([...selected].filter((id) => accountById.has(id)));
  }, [selected, accountById]);
  useEffect(() => {
    const escape = (event: KeyboardEvent) => {
      if (!isCleanEscape(event)) return;
      if (switcherOpen) setSwitcherOpen(false);
      else if (connectionModal) setConnectionModal(null);
      else if (groupsOpen) setGroupsOpen(false);
      else if (filtersOpen) setFiltersOpen(false);
      else if (testAccount) showDialog(closedDialog);
      else if (detail || detailBusy) {
        closeDetail();
      }
    };
    window.addEventListener("keydown", escape);
    return () => window.removeEventListener("keydown", escape);
  }, [
    switcherOpen,
    connectionModal,
    groupsOpen,
    filtersOpen,
    detail,
    detailBusy,
    testAccount,
  ]);
  useEffect(() => {
    let disposed = false;
    if (state.online && (page === "settings" || page === "features"))
      void api<Config>("GET", "/config")
        .then((value) => {
          if (!disposed) setConfig(value);
        })
        .catch((error) => {
          if (!disposed) report(error);
        });
    return () => {
      disposed = true;
    };
  }, [page, state.online, connectionKey, report]);
  const degradationFeed = useErrorFeed(
    "degradation",
    page === "events" && eventTab === "degradation",
    state.online,
    connectionKey,
  );
  const otherFeed = useErrorFeed(
    "other",
    page === "events" && eventTab === "other",
    state.online,
    connectionKey,
  );
  const eventFeed = eventTab === "other" ? otherFeed : degradationFeed;
  const loadEvents = eventFeed.load;
  const filteredAccounts = useMemo(
    () =>
      (sortBy === "recent" ? sortRecentCall : sortPriority)(
        filterAccounts(accounts, query, group, platform, filter, type),
        ascending,
      ),
    [accounts, query, group, platform, filter, type, ascending, sortBy],
  );
  const eventRows = useMemo(
    () =>
      (page === "events" ? eventFeed.items : [])
        .slice()
        .sort((a, b) => b.id - a.id),
    [page, eventFeed.items],
  );
  const hasApiKey = useMemo(
    () => accounts.some((a) => a.type === "apikey"),
    [accounts],
  );
  const accountPlatforms = useMemo(
    () => [...new Set(accounts.map((a) => a.platform))],
    [accounts],
  );
  const accountTypes = useMemo(
    () => [...new Set(accounts.map((a) => a.type))],
    [accounts],
  );
  const selectedAccounts = useMemo(
    () =>
      liveSelected.size ? accounts.filter((a) => liveSelected.has(a.id)) : [],
    [accounts, liveSelected],
  );
  const hasSelectedResetCredit = useMemo(
    () => selectedAccounts.some(resetCreditEligible),
    [selectedAccounts],
  );
  function savePrefs(patch: Partial<Preferences>) {
    return command("preferences", {
      ...(patch.record_columns !== undefined
        ? { recordColumns: patch.record_columns }
        : {}),
      ...(patch.model_test_concurrency !== undefined
        ? { modelTestConcurrency: patch.model_test_concurrency }
        : {}),
      ...(patch.model_test_preferences !== undefined
        ? { modelTestPreferences: patch.model_test_preferences }
        : {}),
    });
  }
  async function prefs(patch: Partial<Preferences>) {
    try {
      await savePrefs(patch);
    } catch (e) {
      report(e);
    }
  }
  const toggle = useCallback(
    async (a: Account) => {
      if (!state.online || schedulePending.current) return;
      const epoch = operationEpoch();
      schedulePending.current = true;
      setBusy(a.id);
      try {
        const result = await accountOperation<{ verified: boolean }>(
          a,
          "schedulable",
          {
            schedulable: !a.schedulable,
          },
        );
        if (!result.verified)
          throw new Error("调度写入未确认，请刷新查看实际状态");
        if (operationEpoch() === epoch) {
          setToast(`${a.name}：${a.schedulable ? "已关闭" : "已打开"}调度`);
        }
      } catch (e) {
        if (operationEpoch() === epoch) {
          report(e);
          void command("refresh");
        }
      } finally {
        if (operationEpoch() === epoch) {
          schedulePending.current = false;
          setBusy(null);
        }
      }
    },
    [state.online, report],
  );
  const openError = useCallback(
    async (id: number, category?: ErrorCategory) => {
      const generation = ++detailGeneration.current;
      detailAbort.current?.abort();
      const controller = new AbortController();
      detailAbort.current = controller;
      setDetailBusy(true);
      try {
        const value = await api<OpsError>(
          "GET",
          `/errors/${id}${category ? `?category=${category}` : ""}`,
        );
        if (
          generation === detailGeneration.current &&
          !controller.signal.aborted
        )
          setDetail(value);
      } catch (e) {
        if (
          generation === detailGeneration.current &&
          !controller.signal.aborted
        )
          report(e);
      } finally {
        if (generation === detailGeneration.current) {
          detailAbort.current = null;
          setDetailBusy(false);
        }
      }
    },
    [report],
  );
  function errorState(error: OpsError) {
    const account =
      error.account_id != null ? accountById.get(error.account_id) : undefined;
    if (
      account?.last_error_id === error.id &&
      !error.resolved &&
      !account.success_after_error
    )
      return "当前";
    return error.resolved ? "已解决" : "历史";
  }
  const schedule = useCallback(
    function scheduleCell(a: Account, disabled = false) {
      return (
        <div className="switch-cell">
          {busy === a.id ? <LoaderCircle size={14} className="spin" /> : null}
          <Switch
            on={a.schedulable}
            disabled={disabled || !state.online || busy !== null}
            label={`${a.name}调度`}
            onChange={() => void toggle(a)}
          />
        </div>
      );
    },
    [busy, state.online, toggle],
  );
  const mobileSchedule = useCallback(
    function mobileScheduleCell(a: Account, disabled = false, isBusy = false) {
      return (
        <div className="switch-cell">
          {isBusy ? <LoaderCircle size={14} className="spin" /> : null}
          <Switch
            on={a.schedulable}
            disabled={disabled || !state.online || isBusy}
            label={`${a.name}调度`}
            onChange={() => void toggle(a)}
          />
        </div>
      );
    },
    [state.online, toggle],
  );
  const toggleSelect = useCallback((id: number, checked: boolean) => {
    setSelected((old) => {
      const next = new Set(old);
      if (checked) next.add(id);
      else next.delete(id);
      return next;
    });
  }, []);
  const handleAccountError = useCallback(
    (id: number) => {
      void openError(id);
    },
    [openError],
  );
  function groupRow(g: Group) {
    const recent = g.recent_accounts;
    return (
      <article className="group-row compact" key={g.id}>
        <div className="group-heading">
          <div className="group-symbol">
            <Layers3 size={16} />
          </div>
          <strong title={g.name}>{g.name}</strong>
          <span className="platform">{g.platform}</span>
        </div>
        <div className="recent-accounts">
          {recent.map((call) => {
            const a =
              call.account_id != null
                ? accountById.get(call.account_id)
                : undefined;
            return (
              <div className="recent-account" key={call.account_id}>
                {
                  <>
                    <div className="compact-identity">
                      <strong title={call.account_name}>
                        {call.account_name}
                        {a && (
                          <>
                            <DegradationBadge account={a} compact />
                          </>
                        )}
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
                }
              </div>
            );
          })}
          {!recent.length && <div className="quiet">暂无成功调用</div>}
        </div>
      </article>
    );
  }
  function errorRow(e: OpsError, category?: ErrorCategory) {
    const stateLabel = errorState(e);
    const contents = (
      <>
        <span
          className={`code ${e.upstream_status_code === 429 ? "warning" : ""}`}
        >
          {e.upstream_status_code || e.status_code || "ERR"}
        </span>
        <div>
          <strong>
            {e.account_name ||
              (e.account_id ? `账号 #${e.account_id}` : "未关联账号")}
          </strong>
          <span>
            <em
              className={`error-state ${stateLabel === "当前" ? "active" : ""}`}
            >
              {stateLabel}
            </em>
            {
              <span className="event-model">
                {e.requested_model || e.model || "未知模型"} ·{" "}
              </span>
            }
            {e.provider_error_code || e.message || "查看错误详情"}
          </span>
        </div>
        <Time at={e.created_at} />
      </>
    );

    return (
      <button
        key={e.id}
        className="error-row"
        onClick={() => void openError(e.id, category)}
      >
        {contents}
        <ChevronRight size={15} />
      </button>
    );
  }
  function renderSwitcherMenu() {
    return (
      <>
        <div
          className="instance-switcher-backdrop"
          onClick={() => setSwitcherOpen(false)}
        />
        <div
          className="instance-switcher-menu"
          role="menu"
          aria-label="Sub2API 实例列表"
        >
          <div className="instance-switcher-header">
            <span>Sub2API 实例 ({connections.length})</span>
            {
              <button
                type="button"
                className="text-button"
                onClick={() => {
                  setSwitcherOpen(false);
                  setPage("settings");
                }}
              >
                管理实例
              </button>
            }
          </div>
          <div className="instance-switcher-list">
            {connections.map((item) => {
              const isCurrent = state.connected && item.base_url === activeUrl;
              const isSwitching = switchingUrl === item.base_url;
              return (
                <button
                  key={item.base_url}
                  type="button"
                  role="menuitem"
                  className={`instance-switcher-item${isCurrent ? " active" : ""}`}
                  disabled={switchingUrl !== null}
                  onClick={() => void switchConnection(item)}
                >
                  <span
                    className={`status-dot ${isCurrent ? (state.online ? "good" : "warning") : "muted"}`}
                  />
                  <div className="instance-switcher-meta">
                    <strong>{item.name}</strong>
                    <small>{item.base_url}</small>
                  </div>
                  {isSwitching ? (
                    <LoaderCircle size={14} className="spin" />
                  ) : isCurrent ? (
                    <span className="instance-current-badge">当前</span>
                  ) : null}
                </button>
              );
            })}
          </div>
          <div className="instance-switcher-footer">
            <button
              type="button"
              className="instance-add-button"
              onClick={() => {
                setSwitcherOpen(false);
                setConnectionModal({ baseUrl: "", name: "", requireKey: true });
              }}
            >
              <Plus size={14} />
              新增实例
            </button>
          </div>
        </div>
      </>
    );
  }
  function status() {
    return (
      <div className="connection-status-group">
        <span className={`connection-status ${state.online ? "online" : ""}`}>
          <i />
          {state.online ? "已连接" : state.connected ? "连接中断" : "未连接"}
        </span>
        {(connections.length > 0 || state.connected) && (
          <div className="instance-switcher">
            <button
              type="button"
              className="instance-switcher-trigger"
              aria-label="切换 Sub2API 实例"
              aria-expanded={switcherOpen}
              onClick={() => setSwitcherOpen((cur) => !cur)}
            >
              {switchingUrl ? (
                <LoaderCircle size={13} className="spin" />
              ) : (
                <Plug size={13} />
              )}
              <span>{activeConnection?.name ?? "选择实例"}</span>
            </button>
            {switcherOpen && renderSwitcherMenu()}
          </div>
        )}
      </div>
    );
  }
  const visibleGroups = useMemo(
    () => (groupsOpen ? currentGroups(groups, accounts) : []),
    [groupsOpen, groups, accounts],
  );
  const detailAccount =
    detail && detail.account_id != null
      ? accountById.get(detail.account_id)
      : undefined;
  return (
    <div className="app mobile">
      <main className="main">
        <header className="topbar">
          <div className="breadcrumb">
            <strong>{pages.find((p) => p.id === page)?.label}</strong>
          </div>
          <div className="top-actions">
            {preview && <span className="preview-label">预览</span>}
            {status()}
            <AccountOperations
              accounts={accounts}
              online={state.online}
              connectionKey={connectionKey}
              active={state.foreground !== false}
              report={report}
              modelTest={(account) =>
                showDialog({ kind: "modelTest", account })
              }
              priorities={() => navigatePage("priorities")}
              template={(a) => {
                showDialog({ kind: "templates", account: a });
              }}
            />

            <button
              className="icon-button"
              title="立即刷新"
              onClick={() => void command("refresh").catch(report)}
            >
              <RefreshCw size={15} />
            </button>
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
                  · 上次更新{" "}
                  <SnapshotObservedTime fallback={snap.observed_at} />
                </>
              )}
            </span>
          </div>
        )}
        <div
          ref={contentRef}
          key={connectionKey}
          className={`content ${page === "events" ? "events-content" : ""} ${page === "records" ? "records-content" : ""}`}
          onScroll={(e) => {
            if (page === "groups" || page === "priorities") {
              retainedScrollRef.current[page] = e.currentTarget.scrollTop;
            }
          }}
        >
          <div className="content-inner">
            {!ready || state.initializing ? (
              <div className="empty">
                <LoaderCircle className="spin" />
                正在加载客户端
              </div>
            ) : !state.connected ? (
              <div className="unconnected-shell">
                <ConnectionView
                  state={state}
                  onError={report}
                  onSwitchSaved={(item) => void switchConnection(item)}
                  switchingUrl={switchingUrl}
                />
                <ClientUpdateCard
                  runtimeVersion={runtimeVersion}
                  onError={report}
                />
              </div>
            ) : !snap ? (
              <div className="empty">
                <Plug size={28} />
                <h2>等待云端数据</h2>
                <p>{state.error || "正在连接运维服务"}</p>
                <button onClick={() => setPage("settings")}>连接设置</button>
                <ClientUpdateCard
                  runtimeVersion={runtimeVersion}
                  onError={report}
                />
                {page === "settings" && (
                  <>
                    <ConnectionStatusCard connectionKey={connectionKey} />
                    <ConnectionView
                      state={state}
                      onError={report}
                      beforeChange={beforeConnectionChange}
                      onSwitchSaved={(item) => void switchConnection(item)}
                      switchingUrl={switchingUrl}
                    />
                  </>
                )}
              </div>
            ) : (
              <>
                {(page === "groups" ||
                  (visitedRetained.key === connectionKey &&
                    visitedRetained.groups)) && (
                  <LazyGroupManager
                    key={connectionKey}
                    connectionKey={connectionKey}
                    active={page === "groups"}
                    accounts={accounts}
                    groups={groups}
                    online={state.online}
                    back={() => setPage("accounts")}
                    report={report}
                    accountActions={accountActions}
                    schedule={schedule}
                    changed={(count, saving) =>
                      setGroupChanges({ count, busy: saving })
                    }
                  />
                )}
                {(page === "priorities" ||
                  (visitedRetained.key === connectionKey &&
                    visitedRetained.priorities)) && (
                  <LazyPriorityManager
                    key={`priorities:${connectionKey}`}
                    connectionKey={connectionKey}
                    active={page === "priorities"}
                    accounts={accounts}
                    online={state.online}
                    back={() => setPage("accounts")}
                    schedule={schedule}
                    changed={setPriorityChanges}
                  />
                )}
                <div
                  key={`surface:${page}:${connectionKey}`}
                  className="page-surface"
                  data-page={page}
                  hidden={page === "groups" || page === "priorities"}
                >
                  <div className="page-heading">
                    {page !== "records" && (
                      <span className="updated">
                        <Clock3 size={13} />
                        更新于{" "}
                        <SnapshotObservedTime fallback={snap.observed_at} />
                      </span>
                    )}
                  </div>
                  {page === "records" && (
                    <Suspense fallback={null}>
                      <LazyUsageRecords
                        key={connectionKey}
                        online={state.online}
                        foreground={state.foreground !== false}
                        accounts={accounts}
                        columns={state.preferences.record_columns}
                        saveColumns={(columns) =>
                          savePrefs({ record_columns: columns })
                        }
                      />
                    </Suspense>
                  )}
                  {page === "accounts" && (
                    <KeyQuotaProvider
                      key={connectionKey}
                      accounts={accounts}
                      online={state.online}
                      active={state.foreground !== false}
                      enabled={hasApiKey}
                    >
                      <div className="mobile-group-entry">
                        <button onClick={() => navigatePage("groups")}>
                          <Layers3 size={18} />
                          分组管理
                          <ChevronRight size={16} />
                        </button>
                        <button onClick={() => navigatePage("priorities")}>
                          <ListOrdered size={18} />
                          优先级管理
                          <ChevronRight size={16} />
                        </button>
                      </div>
                      <div className="filters">
                        <label className="search">
                          <Search size={15} />
                          <input
                            placeholder="搜索名称或账号 ID"
                            value={query}
                            onChange={(e) => setQuery(e.target.value)}
                          />
                        </label>
                        <button
                          aria-label="筛选账号"
                          onClick={() => setFiltersOpen(true)}
                        >
                          <SlidersHorizontal size={18} />
                          筛选
                        </button>
                        {filtersOpen && (
                          <div
                            className="filter-shade"
                            onClick={() => setFiltersOpen(false)}
                          />
                        )}
                        <div
                          ref={filtersSheetRef}
                          className={`filter-options${filtersOpen ? " open" : ""}`}
                          role={filtersOpen ? "dialog" : undefined}
                          aria-modal={filtersOpen ? "true" : undefined}
                          aria-label="账号筛选"
                        >
                          <header>
                            <h2>筛选账号</h2>
                            <button
                              className="icon-button"
                              aria-label="关闭筛选"
                              onClick={() => setFiltersOpen(false)}
                            >
                              <X size={20} />
                            </button>
                          </header>
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
                            {accountPlatforms.map((p) => (
                              <option key={p}>{p}</option>
                            ))}
                          </select>
                          <select
                            aria-label="类型筛选"
                            value={type}
                            onChange={(e) => setType(e.target.value)}
                          >
                            <option value="">全部类型</option>
                            {accountTypes.map((p) => (
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
                            <option value="error">曾有错误</option>
                          </select>
                          <button
                            className="primary"
                            onClick={() => setFiltersOpen(false)}
                          >
                            完成
                          </button>
                        </div>
                      </div>
                      <div className="account-toolbar">
                        <LazyAccountCreationButton
                          accounts={accounts}
                          online={state.online}
                          connectionKey={connectionKey}
                          report={report}
                        />
                        {hasApiKey && (
                          <KeyQuotaRefreshSettings online={state.online} />
                        )}
                        <QuotaRefresh
                          key={connectionKey}
                          connectionKey={connectionKey}
                          online={state.online}
                          active={state.foreground !== false}
                          report={report}
                        />
                        <button
                          onClick={() => {
                            showDialog({ kind: "templates", account: null });
                          }}
                        >
                          账号模板
                        </button>
                        <button onClick={() => setGroupsOpen(true)}>
                          <Layers3 size={17} />
                          分组动态
                        </button>
                      </div>
                      <div className="mobile-sort">
                        <label className="mobile-select-all">
                          <input
                            type="checkbox"
                            aria-label="全选当前筛选账号"
                            checked={
                              filteredAccounts.length > 0 &&
                              filteredAccounts.every((a) =>
                                liveSelected.has(a.id),
                              )
                            }
                            disabled={!state.online}
                            onChange={(e) =>
                              setSelected(
                                e.target.checked
                                  ? new Set(filteredAccounts.map((a) => a.id))
                                  : new Set(),
                              )
                            }
                          />
                          全选
                        </label>
                        <span>{filteredAccounts.length} 个账号</span>
                        <select
                          aria-label="账号排序"
                          value={accountSort}
                          onChange={(e) => updateAccountSort(e.target.value)}
                        >
                          <option value="recent:desc">最近调用</option>
                          <option value="recent:asc">最早调用</option>
                          <option value="priority:asc">优先级 ↑</option>
                          <option value="priority:desc">优先级 ↓</option>
                        </select>
                      </div>
                      <div className="table-wrap">
                        {liveSelected.size > 0 && (
                          <div className="selection-bar">
                            <div className="selection-summary">
                              <span>已选 {liveSelected.size} 个账号</span>
                            </div>
                            <div
                              className="selection-actions"
                              role="group"
                              aria-label="批量账号操作"
                            >
                              <button
                                id="connection-batch-start"
                                disabled={
                                  !state.online || liveSelected.size === 0
                                }
                                aria-disabled={connectionBatch.busy}
                                onClick={() =>
                                  void connectionBatch.start(selectedAccounts)
                                }
                              >
                                <Activity size={12} />
                                测试连接
                              </button>
                              <RecoverSelectedButton
                                key={connectionKey}
                                accounts={selectedAccounts}
                                online={state.online}
                                report={report}
                              />
                              <button
                                disabled={
                                  !state.online || !hasSelectedResetCredit
                                }
                                onClick={() =>
                                  showDialog({
                                    kind: "reset",
                                    accounts: selectedAccounts,
                                  })
                                }
                              >
                                <Ticket size={12} />
                                使用重置卡
                              </button>
                              <button
                                disabled={
                                  !state.online || liveSelected.size === 0
                                }
                                onClick={() =>
                                  showDialog({
                                    kind: "proxy",
                                    accounts: selectedAccounts,
                                  })
                                }
                              >
                                <Network size={12} />
                                选择代理
                              </button>
                              <button
                                disabled={
                                  !state.online || liveSelected.size === 0
                                }
                                onClick={() => {
                                  showDialog({
                                    kind: "templates",
                                    account: null,
                                    accounts: selectedAccounts,
                                  });
                                }}
                              >
                                <LayoutTemplate size={12} />
                                应用模板
                              </button>
                              <button
                                className="danger-text"
                                disabled={
                                  !state.online || liveSelected.size === 0
                                }
                                onClick={() =>
                                  showDialog({
                                    kind: "delete",
                                    accounts: selectedAccounts,
                                  })
                                }
                              >
                                <Trash2 size={12} />
                                删除所选
                              </button>
                            </div>
                          </div>
                        )}
                        <ConnectionBatchResults control={connectionBatch} />
                        <MobileAccounts
                          active={state.foreground !== false}
                          connectionKey={connectionKey}
                          accounts={filteredAccounts}
                          online={state.online}
                          selected={liveSelected}
                          busyAccountId={busy}
                          select={toggleSelect}
                          schedule={mobileSchedule}
                          accountActions={accountActions}
                          error={handleAccountError}
                          report={report}
                        />
                      </div>
                      {!filteredAccounts.length && (
                        <Empty text="没有符合条件的账号" />
                      )}
                    </KeyQuotaProvider>
                  )}
                  {page === "events" && (
                    <div className="events-view">
                      <nav
                        className="event-tabs"
                        role="tablist"
                        aria-label="事件类型"
                      >
                        {(
                          [
                            ["degradation", "降智错误"],
                            ["other", "其他错误"],
                            ["recoveries", "恢复历史"],
                            ["logs", "日志"],
                          ] as const
                        ).map(([id, label]) => (
                          <button
                            key={id}
                            role="tab"
                            id={`${id}-tab`}
                            aria-selected={eventTab === id}
                            aria-controls={`${id}-panel`}
                            onClick={() => setEventTab(id)}
                          >
                            {label}
                          </button>
                        ))}
                      </nav>
                      <div className="event-panels">
                        {(eventTab === "degradation" ||
                          eventTab === "other") && (
                          <section
                            id={`${eventTab}-panel`}
                            className="event-panel"
                            role="tabpanel"
                            aria-labelledby={`${eventTab}-tab`}
                          >
                            <div className="section-heading">
                              <span>{eventRows.length} 条记录</span>
                              <button
                                className="text-button"
                                disabled={!state.online || eventFeed.loading}
                                onClick={() => void loadEvents()}
                              >
                                刷新记录
                              </button>
                            </div>
                            {eventFeed.error && (
                              <p className="bad-text" role="alert">
                                {eventFeed.error}{" "}
                                <button onClick={() => void loadEvents()}>
                                  重试
                                </button>
                              </p>
                            )}
                            <div className="error-list">
                              {eventRows.map((error) =>
                                errorRow(error, eventTab),
                              )}
                              {!eventRows.length && (
                                <Empty
                                  text={
                                    eventFeed.loading
                                      ? "正在读取"
                                      : eventFeed.error
                                        ? "记录暂不可用"
                                        : "暂无错误记录"
                                  }
                                />
                              )}
                            </div>
                            {eventFeed.cursor !== null && (
                              <button
                                className="load-more"
                                disabled={!state.online || eventFeed.loading}
                                onClick={() => void loadEvents("more")}
                              >
                                加载更早记录
                              </button>
                            )}
                          </section>
                        )}
                        <section
                          id="logs-panel"
                          className="event-panel"
                          role="tabpanel"
                          aria-labelledby="logs-tab"
                          hidden={eventTab !== "logs"}
                        >
                          {eventTab === "logs" && (
                            <Suspense fallback={null}>
                              <LazyDetectionLogs
                                key={connectionKey}
                                active={
                                  page === "events" && eventTab === "logs"
                                }
                                online={state.online}
                                connectionKey={connectionKey}
                              />
                            </Suspense>
                          )}
                        </section>
                        <section
                          id="recoveries-panel"
                          className="event-panel"
                          role="tabpanel"
                          aria-labelledby="recoveries-tab"
                          hidden={eventTab !== "recoveries"}
                        >
                          {eventTab === "recoveries" && (
                            <Suspense fallback={null}>
                              <LazyRecoveryHistory
                                key={connectionKey}
                                latest={snap.recoveries ?? []}
                                accounts={accounts}
                                online={state.online}
                                active={eventTab === "recoveries"}
                                report={report}
                              />
                            </Suspense>
                          )}
                        </section>
                      </div>
                    </div>
                  )}
                  {page === "settings" && (
                    <ConnectionStatusCard connectionKey={connectionKey} />
                  )}
                  {(page === "features" || page === "settings") &&
                    (config ? (
                      <SettingsView
                        page={page}
                        config={config}
                        accounts={accounts}
                        state={state}
                        runtimeVersion={runtimeVersion}
                        onChange={(key, value) =>
                          setConfig({ ...config, [key]: value })
                        }
                        onMessage={setToast}
                        onError={report}
                        beforeConnectionChange={beforeConnectionChange}
                        onSwitchConnection={(item) =>
                          void switchConnection(item)
                        }
                        onOpenConnectionModal={setConnectionModal}
                        switchingUrl={switchingUrl}
                      />
                    ) : page === "settings" ? (
                      <div className="settings-stack">
                        <ClientUpdateCard
                          runtimeVersion={runtimeVersion}
                          onError={report}
                        />
                        <section className="settings-card">
                          <ConnectionView
                            state={state}
                            onError={report}
                            beforeChange={beforeConnectionChange}
                            onSwitchSaved={(item) =>
                              void switchConnection(item)
                            }
                            switchingUrl={switchingUrl}
                          />
                        </section>
                      </div>
                    ) : (
                      <Empty text="正在读取设置" />
                    ))}
                </div>
              </>
            )}
          </div>
        </div>
      </main>
      <nav className="bottom-nav" aria-label="主导航">
        {pages
          .filter((p) => p.id !== "groups" && p.id !== "priorities")
          .map((p) => {
            const selected =
              page === p.id ||
              (["groups", "priorities"].includes(page) && p.id === "accounts");
            return (
              <button
                key={p.id}
                className={selected ? "active" : ""}
                aria-current={selected ? "page" : undefined}
                onPointerDown={() => {
                  if (p.id === "records") {
                    void LazyUsageRecords.preload().catch(() => {});
                  }
                }}
                onClick={() => navigatePage(p.id)}
              >
                <p.icon size={21} />
                <span>{p.label}</span>
              </button>
            );
          })}
      </nav>
      {leaveGroups && (
        <div className="modal-backdrop" {...leaveGroupsBackdrop}>
          <section
            ref={discardRef}
            className="discard-groups-dialog"
            role="dialog"
            aria-modal="true"
            aria-label={
              priorityChanges ? "未应用的优先级修改" : "未应用的分组修改"
            }
            tabIndex={-1}
            onClick={(e) => e.stopPropagation()}
            onKeyDown={(e) => {
              if (isCleanEscape(e)) {
                e.preventDefault();
                e.stopPropagation();
                leaveGroups(false);
                setLeaveGroups(null);
              }
            }}
          >
            <h2>
              放弃{groupChanges.count ? "分组" : ""}
              {groupChanges.count && priorityChanges ? "和" : ""}
              {priorityChanges ? "优先级" : ""}草稿并更换连接？
            </h2>
            <div>
              <button
                onClick={() => {
                  leaveGroups(false);
                  setLeaveGroups(null);
                }}
              >
                保留草稿
              </button>
              <button
                className="primary"
                onClick={() => {
                  leaveGroups(true);
                  setLeaveGroups(null);
                }}
              >
                放弃并继续
              </button>
            </div>
          </section>
        </div>
      )}
      {connectionModal && (
        <div className="modal-backdrop" {...connectionModalBackdrop}>
          <div
            ref={connectionModalRef}
            className="connection-modal"
            role="dialog"
            aria-modal="true"
            aria-label="连接 Sub2API 实例"
            onClick={(e) => e.stopPropagation()}
          >
            <ConnectionView
              state={state}
              initialUrl={connectionModal.baseUrl}
              initialName={connectionModal.name}
              requireKey={Boolean(connectionModal.requireKey)}
              showNameField
              onError={report}
              beforeChange={beforeConnectionChange}
              onConnected={() => setConnectionModal(null)}
              onCancel={() => setConnectionModal(null)}
              onSwitchSaved={(item) => {
                setConnectionModal(null);
                void switchConnection(item);
              }}
              switchingUrl={switchingUrl}
            />
          </div>
        </div>
      )}
      {groupsOpen && (
        <div
          ref={groupsSurfaceRef}
          className="mobile-groups-surface"
          role="dialog"
          aria-modal="true"
          aria-label="分组动态"
        >
          <header>
            <h2>分组动态</h2>
            <button
              className="icon-button"
              aria-label="关闭分组动态"
              onClick={() => setGroupsOpen(false)}
            >
              <X size={20} />
            </button>
          </header>
          <div className="mobile-activity-groups">
            {visibleGroups.map((g) => groupRow(g))}
            {!groups.length && <Empty text="没有分组记录" />}
          </div>
        </div>
      )}
      {resetAccounts && (
        <Suspense fallback={null}>
          <LazyResetCreditsDialog
            accounts={resetAccounts}
            online={state.online}
            report={report}
            close={() => showDialog(closedDialog)}
          />
        </Suspense>
      )}
      {deleteAccounts && (
        <Suspense fallback={null}>
          <LazyDeleteAccountsDialog
            accounts={deleteAccounts}
            allAccounts={accounts}
            online={state.online}
            removed={(id) => {
              setRemovedIds((old) =>
                old.has(id) ? old : new Set([...old, id]),
              );
              setSelected((old) =>
                !old.has(id)
                  ? old
                  : new Set([...old].filter((value) => value !== id)),
              );
            }}
            finished={(results) =>
              setSelected(
                new Set(
                  results
                    .filter(
                      (r) => r.status === "failed" && accountById.has(r.id),
                    )
                    .map((r) => r.id),
                ),
              )
            }
            close={() => showDialog(closedDialog)}
          />
        </Suspense>
      )}
      {(detail || detailBusy) && (
        <div className="drawer-backdrop" {...detailBackdrop}>
          <aside
            ref={drawerRef}
            className="drawer"
            role="dialog"
            aria-modal="true"
            aria-label="错误详情"
            onClick={(e) => e.stopPropagation()}
          >
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
                {<X size={19} />}
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
                    <dd>#{detail.account_id ?? "—"}</dd>
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
                    <dt>来源</dt>
                    <dd>
                      {detail.error_source || detail.error_owner || "未知"}
                    </dd>
                    <dt>阶段</dt>
                    <dd>{detail.error_phase || "未知"}</dd>
                    <dt>请求 ID</dt>
                    <dd>{detail.request_id || "未知"}</dd>
                    {detail.notification && (
                      <>
                        <dt>检测线索</dt>
                        <dd>
                          {detail.notification.status === "suppressed"
                            ? detail.notification.reason === "degradation_mark"
                              ? "降智标记静默"
                              : "已抑制"
                            : ((
                                {
                                  delivered: "历史预警已推送",
                                  retired: "历史预警已停发",
                                  clue: "已采集",
                                  testing: "检测中",
                                  ignored: "已忽略",
                                  failed: "检测未完成",
                                  confirmed: "已确认降智",
                                  unavailable: "状态暂不可读取",
                                } as Record<string, string>
                              )[detail.notification.status] ?? "—")}
                        </dd>
                      </>
                    )}
                  </dl>
                  {detail.disposition_notification && (
                    <p>
                      处置通知：
                      {(
                        {
                          delivered: "已推送",
                          queued: "待推送",
                          retry: "待重试",
                          suppressed: "已取消",
                          failed: "投递失败",
                        } as Record<string, string>
                      )[detail.disposition_notification.status] ?? "—"}
                    </p>
                  )}
                  <h3>错误内容</h3>
                  {detailAccount && (
                    <DegradationAction
                      account={detailAccount}
                      online={state.online}
                      report={report}
                    />
                  )}
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

      {testAccount && (
        <Suspense fallback={null}>
          <LazyTestDialog
            key={`${connectionKey}:${testAccount.id}`}
            account={accountById.get(testAccount.id) ?? testAccount}
            online={state.online}
            close={() => showDialog(closedDialog)}
          />
        </Suspense>
      )}
      {modelTestAccount && (
        <Suspense fallback={null}>
          <LazyModelTestDialog
            key={`${connectionKey}:${modelTestAccount.id}`}
            account={accountById.get(modelTestAccount.id) ?? modelTestAccount}
            online={state.online}
            report={report}
            close={() => showDialog(closedDialog)}
            preference={
              state.preferences.model_test_preferences?.[
                `${state.preferences.base_url}:${modelTestAccount.id}`
              ]
            }
            savePreference={(value) =>
              void prefs({
                model_test_preferences: {
                  ...state.preferences.model_test_preferences,
                  [`${state.preferences.base_url}:${modelTestAccount.id}`]:
                    value,
                },
              })
            }
            concurrency={state.preferences.model_test_concurrency ?? 1}
            saveConcurrency={(value) =>
              void prefs({ model_test_concurrency: value })
            }
          />
        </Suspense>
      )}
      {dialog.kind === "templates" && (
        <Suspense fallback={null}>
          <LazyAccountTemplates
            key={connectionKey}
            accounts={accounts}
            online={state.online}
            initialAccount={dialog.account}
            initialAccounts={dialog.accounts}
            close={() => showDialog(closedDialog)}
            report={report}
          />
        </Suspense>
      )}
      {proxyAccounts && (
        <Suspense fallback={null}>
          <LazyProxyBatchDialog
            key={connectionKey}
            accounts={proxyAccounts}
            online={state.online}
            connectionKey={connectionKey}
            close={() => showDialog(closedDialog)}
            report={report}
          />
        </Suspense>
      )}
      {modelDetectionAccount && (
        <Suspense fallback={null}>
          <LazyModelDetectionDialog
            key={`${connectionKey}:${modelDetectionAccount.id}`}
            account={
              accountById.get(modelDetectionAccount.id) ?? modelDetectionAccount
            }
            online={state.online}
            close={() => showDialog(closedDialog)}
            showResult={() => {
              showDialog({ kind: "modelTest", account: modelDetectionAccount });
            }}
            report={report}
          />
        </Suspense>
      )}
      {keepaliveAccount && (
        <Suspense fallback={null}>
          <LazyKeepaliveDialog
            key={`${connectionKey}:${keepaliveAccount.id}`}
            account={keepaliveAccount}
            online={state.online}
            connectionKey={connectionKey}
            close={() => showDialog(closedDialog)}
            report={report}
          />
        </Suspense>
      )}
      {reauthorizeAccount?.connection === connectionKey && (
        <Suspense fallback={null}>
          <LazyReauthorizeAccountDialog
            key={`${connectionKey}:${reauthorizeAccount.account.id}`}
            account={reauthorizeAccount.account}
            online={state.online}
            close={() => showDialog(closedDialog)}
            report={report}
          />
        </Suspense>
      )}
      {settingsAccount?.connection === connectionKey && (
        <Suspense fallback={null}>
          <LazyAccountSettingsDialog
            key={`${connectionKey}:${settingsAccount.account.id}:${settingsAccount.section}`}
            account={settingsAccount.account}
            initialSection={settingsAccount.section}
            online={state.online}
            close={() => showDialog(closedDialog)}
          />
        </Suspense>
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
