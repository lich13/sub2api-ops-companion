import { useEffect, useMemo, useRef, useState, type CSSProperties, type ReactNode } from "react";
import { ArrowDown, ArrowLeft, ArrowUp, Check, GripVertical, Minus, Plus, RefreshCw, Search, Undo2 } from "lucide-react";
import { api, command } from "./bridge";
import { clientId } from "./accountOperations";
import { groupPriorityAccounts, movePriority, movePrioritySlot, prioritySlots, priorityPending, priorityPlatformName, samePriorityAccounts, sortPriorityPlatforms, type LegacyPriorityView, type PriorityAccount, type PriorityDraft, type PriorityPlatform, type PriorityTask, type PriorityView } from "./priorities";
import type { Account } from "./types";
import "./priority-manager.css";

type Props = { accounts: Account[]; active: boolean; online: boolean; mobile: boolean; connectionKey: string; back: () => void; schedule: (account: Account, disabled?: boolean) => ReactNode; changed: (count: number) => void };
type Drag = { id: number; platform: string; x: number; y: number; target: number | null };
type Drafts = Record<string, PriorityDraft | undefined>;
type Caps = { account_priority_ordering?: boolean; account_priority_per_platform?: boolean };
const taskLabels: Record<string, string> = { queued: "等待执行", running: "正在保存", checking: "正在核对", completed: "全部已确认", needs_confirmation: "需要重新核对", failed: "保存失败", cancelled: "已停止" };
const cleanError = (error: unknown) => String(error).replace(/^Error: /, "");
const taskPath = (id: string, summary = false) => `/account-priority-orders/${id}?schema=2${summary ? "&details=0" : ""}`;
const fallbackAccount = (a: Account): PriorityAccount => ({ id: a.id, name: a.name, platform: a.platform, type: a.type, priority: a.priority, version: a.operation_versions?.priority ?? "", editable: false, reason: "请升级服务端以按平台管理优先级", status: a.status, schedulable: a.schedulable });
export default function PriorityManager(props: Props) {
  const { active, online, mobile, connectionKey } = props;
  const [view, setView] = useState<PriorityView | null>(null), [supported, setSupported] = useState<boolean | null>(null);
  const [drafts, setDrafts] = useState<Drafts>({}), [history, setHistory] = useState<Record<string, (PriorityDraft | undefined)[]>>({});
  const [results, setResults] = useState<Record<string, string>>({});
  const resultTimers = useRef(new Map<string, ReturnType<typeof setTimeout>>());
  const [query, setQuery] = useState(""), [selected, setSelected] = useState("");
  const [loading, setLoading] = useState(false), [submitting, setSubmitting] = useState(""), [setting, setSetting] = useState("");
  const [error, setError] = useState(""), [notices, setNotices] = useState<Record<string, string>>({});
  const [readRevision, setReadRevision] = useState(0), [width, setWidth] = useState(0), [visible, setVisible] = useState(!document.hidden);
  const [drag, setDrag] = useState<Drag | null>(null);
  const generation = useRef(0), alive = useRef(true), submittingRef = useRef(false), latest = useRef(props), caps = useRef<Caps | null>(null);
  const draftsRef = useRef(drafts), viewRef = useRef(view), dragRef = useRef(drag), section = useRef<HTMLElement>(null);
  const lists = useRef(new Map<string, HTMLOListElement>());
  const pointer = useRef<(Drag & { ready: boolean; timer?: ReturnType<typeof setTimeout> }) | null>(null);
  const requests = useRef<Record<string, { signature: string; body: Record<string, unknown> }>>({});
  const completed = useRef(new Set<string>());
  latest.current = props; draftsRef.current = drafts; viewRef.current = view; dragRef.current = drag;
  const liveSource = active ? props.accounts : undefined;
  const fallbackSource = active && (view === null || supported === false) ? props.accounts : undefined;
  const liveAccounts = useMemo(() => new Map(liveSource?.map(account => [account.id, account])), [liveSource]);
  const fallback = useMemo(() => fallbackSource?.map(fallbackAccount) ?? [], [fallbackSource]);
  const platforms = useMemo(() => {
    if (!active) return [];
    const groups = [...(view?.platforms ?? groupPriorityAccounts(fallback))];
    for (const [platform, draft] of Object.entries(drafts)) if (draft && !groups.some(group => group.platform === platform)) groups.push({ platform, items: [], version: "", last_task: null });
    return sortPriorityPlatforms(groups);
  }, [active, view, fallback, drafts]);
  const states = useMemo(() => new Map(platforms.map(group => {
    const draft = drafts[group.platform], accounts = draft?.accounts ?? group.items;
    const byId = new Map(accounts.map(account => [account.id, account]));
    const parallelism = Math.max(1, Math.min(8, draft?.parallelism ?? group.parallelism ?? 1));
    const slots = draft?.slots ?? (parallelism === 1 && draft ? draft.ids.map((id, index) => ({ id, priority: index + 1 })) : prioritySlots(accounts, parallelism));
    const occupied = slots.filter(slot => slot.id !== null), ids = occupied.map(slot => slot.id!), targets = occupied.map(slot => slot.priority);
    const rows = ids.map(id => byId.get(id)!).filter(Boolean), changes = rows.filter((a, index) => a.priority !== targets[index]);
    const conflict = !!draft && (draft.version !== group.version || !samePriorityAccounts(draft.ids, group.items));
    const blocked = changes.filter(a => !a.editable);
    const lanes = new Map<number, { priority: number; slots: ((typeof slots)[number] & { index: number })[] }>();
    slots.forEach((slot, index) => {
      let lane = lanes.get(slot.priority);
      if (!lane) { lane = { priority: slot.priority, slots: [] }; lanes.set(slot.priority, lane); }
      lane.slots.push({ ...slot, index });
    });
    return [group.platform, { draft, accounts, ids, rows, slots, targets, byId, changes, conflict, blocked, parallelism, lanes: [...lanes.values()] }] as const;
  })), [platforms, drafts]);
  const columns = !mobile && platforms.length > 1 && platforms.length <= 3 && width >= Math.max(960, platforms.reduce((sum, group) => sum + Math.max(320, (group.parallelism ?? 1) * 220 + 72), 0) + (platforms.length - 1) * 12);
  const selectedPlatform = platforms.some(p => p.platform === selected) ? selected : platforms[0]?.platform;
  const activeTask = view?.active_task ?? null, busy = !!submitting || priorityPending(activeTask), filtered = !!query.trim();
  const changedCount = useMemo(() => Object.values(drafts).reduce((count, draft) => {
    if (!draft) return count;
    const priorities = new Map(draft.accounts.map(account => [account.id, account.priority]));
    const parallelism = Math.max(1, Math.min(8, draft.parallelism ?? 1));
    const slots = draft.slots ?? (parallelism === 1 ? draft.ids.map((id, index) => ({ id, priority: index + 1 })) : prioritySlots(draft.accounts, parallelism));
    return count + slots.filter(slot => slot.id !== null && priorities.has(slot.id) && priorities.get(slot.id) !== slot.priority).length;
  }, 0), [drafts]);
  const connected = () => alive.current && latest.current.connectionKey === connectionKey;
  const current = (epoch: number) => connected() && epoch === generation.current && latest.current.active && latest.current.online && !document.hidden;
  useEffect(() => { props.changed(changedCount); }, [changedCount]);
  useEffect(() => { alive.current = true; return () => { alive.current = false; ++generation.current; clearTimeout(pointer.current?.timer); resultTimers.current.forEach(clearTimeout); }; }, []);
  function stopDrag() { clearTimeout(pointer.current?.timer); pointer.current = null; setDrag(null); }
  useEffect(() => { stopDrag(); }, [active, online, columns, selectedPlatform]);
  useEffect(() => {
    const point = dragRef.current;
    if (point) { const group = platforms.find(group => group.platform === point.platform); if (!group || state(group).conflict || !state(group).editing) stopDrag(); }
  }, [view, filtered, loading]);
  useEffect(() => {
    if (!active || !section.current) return;
    const node = section.current;
    setWidth(node.getBoundingClientRect().width);
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(entries => setWidth(entries[0].contentRect.width));
    observer.observe(node); return () => observer.disconnect();
  }, [active]);
  useEffect(() => {
    ++generation.current; caps.current = null; completed.current.clear(); requests.current = {};
    setView(null); setSupported(null); setSubmitting(""); submittingRef.current = false; setDrafts({}); setHistory({}); resultTimers.current.forEach(clearTimeout); resultTimers.current.clear(); setResults({}); setNotices({}); setError(""); setSelected(""); stopDrag();
  }, [connectionKey]);
  useEffect(() => {
    const hidden = () => { setVisible(!document.hidden); if (document.hidden) { ++generation.current; stopDrag(); } };
    document.addEventListener("visibilitychange", hidden); return () => document.removeEventListener("visibilitychange", hidden);
  }, []);
  useEffect(() => { const cancel = (event: KeyboardEvent) => { if (active && event.key === "Escape") stopDrag(); }; window.addEventListener("keydown", cancel); return () => window.removeEventListener("keydown", cancel); }, [active]);
  function showResult(platform: string, message: string) {
    clearTimeout(resultTimers.current.get(platform));
    setResults(old => ({ ...old, [platform]: message }));
    resultTimers.current.set(platform, setTimeout(() => {
      resultTimers.current.delete(platform);
      setResults(old => { const next = { ...old }; delete next[platform]; return next; });
    }, 3000));
  }
  async function read(reprepare?: string, refreshCaps = false) {
    if (!active || !online || document.hidden) return;
    const previousTask = viewRef.current?.active_task;
    const epoch = ++generation.current; setLoading(true); setError("");
    try {
      let features = caps.current;
      if (!features || refreshCaps) {
        const response = await api<{ features?: Caps }>("GET", "/capabilities");
        if (!current(epoch)) return;
        features = response.features ?? {}; caps.current = features;
      }
      const available = features.account_priority_per_platform === true; setSupported(available);
      let next: PriorityView;
      if (available) next = await api<PriorityView>("GET", "/account-priorities?schema=2");
      else if (features.account_priority_ordering) {
        const old = await api<LegacyPriorityView>("GET", "/account-priorities");
        next = { platforms: groupPriorityAccounts(old.items.map(item => ({ ...item, editable: false })), old.version), active_task: null, legacy_last_task: null };
      } else next = { platforms: groupPriorityAccounts(props.accounts.map(fallbackAccount)), active_task: null, legacy_last_task: null };
      if (!current(epoch)) return;
      const finished = previousTask && next.platforms.find(group => group.last_task?.id === previousTask.id)?.last_task;
      if (priorityPending(previousTask) && finished && !priorityPending(finished)) await finishTask(finished, epoch, next);
      if (!current(epoch)) return;
      setView(next);
      if (reprepare) {
        const group = next.platforms.find(group => group.platform === reprepare);
        const previous = draftsRef.current[reprepare];
        let task = group?.last_task;
        if (!previous && task) task = await api<PriorityTask>("GET", taskPath(task.id));
        if (!current(epoch)) return;
        const target = previous?.ids ?? task?.items?.map(item => item.account_id);
        const same = !!group && !!target && samePriorityAccounts(target, group.items);
        setDrafts(old => ({ ...old, [reprepare]: same ? { ids: target!, slots: previous?.slots ?? ((group!.parallelism ?? 1) > 1 && task?.items ? prioritySlots(group!.items.map(account => ({ ...account, priority: task!.items!.find(item => item.account_id === account.id)?.target_priority ?? account.priority })), group!.parallelism ?? 1) : undefined), parallelism: group!.parallelism ?? 1, version: group!.version, accounts: group!.items } : undefined }));
        setHistory(old => ({ ...old, [reprepare]: [] })); delete requests.current[reprepare];
        setNotices(old => ({ ...old, [reprepare]: target && !same ? "账号集合已变化，已按当前优先级重新准备" : "已重新读取实际优先级，请核对后应用" }));
      }
    } catch (error) { if (current(epoch)) setError(cleanError(error)); }
    finally {
      if (current(epoch)) {
        setLoading(false);
        // A failed list read also invalidated the old poll's generation.
        setReadRevision(revision => revision + 1);
      }
    }
  }
  useEffect(() => {
    if (active && online && visible) void read(); else { setLoading(false); if (!online) caps.current = null; }
    return () => { ++generation.current; };
  }, [active, online, visible, connectionKey]);
  function updateTask(task: PriorityTask) {
    setView(old => old ? { ...old, active_task: priorityPending(task) ? task : old.active_task?.id === task.id ? null : old.active_task,
      platforms: old.platforms.map(group => group.platform === task.platform ? { ...group, last_task: task } : group) } : old);
  }
  async function finishTask(task: PriorityTask, epoch: number, loaded?: PriorityView) {
    if (completed.current.has(task.id) || !current(epoch)) return;
    const next = loaded ?? await api<PriorityView>("GET", "/account-priorities?schema=2");
    if (!current(epoch)) return;
    completed.current.add(task.id); setView(next);
    const label = taskLabels[task.status] ?? "待核对";
    if (task.platform) showResult(task.platform, `${label} · ${task.verified_count}/${task.total} 已确认${task.reason && task.reason !== label ? ` · ${task.reason}` : ""}`);
    void command("refresh").catch(error => { if (current(epoch)) setError(cleanError(error)); });
  }
  useEffect(() => {
    if (!active || !online || !visible || !priorityPending(activeTask)) return;
    let disposed = false, timer: ReturnType<typeof setTimeout>;
    const epoch = generation.current, id = activeTask!.id;
    const poll = async () => {
      try {
        const next = await api<PriorityTask>("GET", taskPath(id, true));
        if (disposed || !current(epoch)) return;
        if (!priorityPending(next)) { await finishTask(next, epoch); return; }
        updateTask(next); setError("");
      } catch (error) { if (!disposed && current(epoch)) setError(cleanError(error)); }
      if (!disposed && current(epoch)) timer = setTimeout(() => void poll(), 1000);
    };
    timer = setTimeout(() => void poll(), 1000);
    return () => { disposed = true; clearTimeout(timer); };
  }, [activeTask?.id, active, online, visible, readRevision]);
  function state(group: PriorityPlatform) {
    const s = states.get(group.platform)!;
    const editing = online && supported === true && view !== null && !loading && !setting && !filtered && !s.conflict && submitting !== group.platform && !(priorityPending(activeTask) && activeTask?.platform === group.platform);
    return { ...s, editing };
  }
  function move(platform: string, id: number, target: number) {
    const group = platforms.find(group => group.platform === platform);
    if (!group) return;
    const s = state(group);
    if (!s.editing || !s.rows.find(account => account.id === id)?.editable) return;
    const slots = s.parallelism > 1 ? movePrioritySlot(s.slots, id, target, s.parallelism) : undefined;
    if (slots && slots.some(slot => slot.id !== null && !s.byId.get(slot.id)?.editable && s.byId.get(slot.id)?.priority !== slot.priority)) return;
    const next = slots ? slots.filter(slot => slot.id !== null).map(slot => slot.id!) : movePriority(s.ids, id, target);
    if (slots ? JSON.stringify(slots) === JSON.stringify(s.slots) : next.every((id, index) => s.ids[index] === id)) return;
    setHistory(old => ({ ...old, [platform]: [...(old[platform] ?? []), s.draft] }));
    setDrafts(old => ({ ...old, [platform]: { ids: next, slots, version: s.draft?.version ?? group.version, accounts: s.accounts, parallelism: s.parallelism } }));
    setNotices(old => ({ ...old, [platform]: "" })); delete requests.current[platform];
  }
  function targetAt(x: number, y: number, platform: string, id: number) {
    const row = document.elementFromPoint(x, y)?.closest<HTMLElement>("[data-priority-id],[data-priority-slot]");
    if (!row || !section.current?.contains(row) || row.dataset.platform && row.dataset.platform !== platform) return null;
    const group = platforms.find(group => group.platform === platform);
    if (!group) return null;
    const { ids, slots, parallelism } = state(group), original = ids.indexOf(id);
    if (parallelism > 1) return row.dataset.prioritySlot !== undefined ? Number(row.dataset.priorityIndex) : slots.findIndex(slot => slot.id === Number(row.dataset.priorityId));
    if (row.dataset.prioritySlot !== undefined) return Math.min(Number(row.dataset.priorityIndex), ids.length - 1);
    const over = ids.indexOf(Number(row.dataset.priorityId));
    if (original < 0 || over < 0) return null;
    const bounds = row.getBoundingClientRect(), gap = over + (y >= bounds.top + bounds.height / 2 ? 1 : 0);
    return gap > original ? gap - 1 : gap;
  }
  function pointerDown(event: React.PointerEvent, platform: string, id: number) {
    const group = platforms.find(group => group.platform === platform);
    if (!group || !state(group).editing || event.button !== 0 || !state(group).rows.find(a => a.id === id)?.editable) return;
    event.currentTarget.setPointerCapture(event.pointerId);
    const item = { id, platform, x: event.clientX, y: event.clientY, target: null, ready: event.pointerType !== "touch", timer: undefined as ReturnType<typeof setTimeout> | undefined };
    pointer.current = item;
    if (!item.ready) item.timer = setTimeout(() => { item.ready = true; setDrag({ ...item }); }, 350);
  }
  function pointerMove(event: React.PointerEvent) {
    const item = pointer.current; if (!item) return;
    const distance = Math.hypot(event.clientX - item.x, event.clientY - item.y);
    if (!item.ready && distance > 9) { stopDrag(); return; }
    if (!item.ready || distance < 5 && !dragRef.current) return;
    const over = document.elementFromPoint(event.clientX, event.clientY)?.closest<HTMLElement>("[data-priority-platform]");
    if (over && over.dataset.priorityPlatform !== item.platform) { stopDrag(); return; }
    setDrag({ id: item.id, platform: item.platform, x: event.clientX, y: event.clientY, target: targetAt(event.clientX, event.clientY, item.platform, item.id) });
  }
  function pointerUp(event: React.PointerEvent) {
    const item = pointer.current;
    const target = item && dragRef.current ? targetAt(event.clientX, event.clientY, item.platform, item.id) : null;
    if (item && target !== null) move(item.platform, item.id, target);
    stopDrag();
  }
  useEffect(() => {
    if (!drag) return;
    let frame = 0;
    const scroll = () => {
      const point = dragRef.current, surface = point && lists.current.get(point.platform);
      if (point && surface) { const bounds = surface.getBoundingClientRect(); if (point.y < bounds.top + 50) surface.scrollTop -= 9; if (point.y > bounds.bottom - 50) surface.scrollTop += 9; }
      frame = requestAnimationFrame(scroll);
    };
    frame = requestAnimationFrame(scroll); return () => cancelAnimationFrame(frame);
  }, [!!drag]);
  async function apply(group: PriorityPlatform) {
    const s = state(group), platform = group.platform;
    if (!s.editing || busy || s.blocked.length || !s.changes.length || submittingRef.current) return;
    submittingRef.current = true; setSubmitting(platform); setError(""); stopDrag();
    try {
      const fields = { platform, ordered_account_ids: s.ids, parallelism: s.parallelism, ...(s.parallelism > 1 ? { target_priorities: s.targets } : {}), expected_version: s.draft?.version ?? group.version }, signature = JSON.stringify(fields);
      if (requests.current[platform]?.signature !== signature) requests.current[platform] = { signature, body: { ...fields, request_id: crypto.randomUUID(), client_id: clientId() } };
      const next = await api<PriorityTask>("POST", "/account-priority-orders?schema=2", requests.current[platform].body);
      if (!connected()) return;
      const epoch = ++generation.current; setLoading(false); setReadRevision(revision => revision + 1);
      updateTask(next); setDrafts(old => ({ ...old, [platform]: undefined })); setHistory(old => ({ ...old, [platform]: [] })); delete requests.current[platform];
      if (!priorityPending(next) && current(epoch)) await finishTask(next, epoch);
    } catch (error) { if (connected()) showResult(platform, cleanError(error)); }
    finally { if (connected()) { submittingRef.current = false; setSubmitting(""); } }
  }
  async function changeParallelism(group: PriorityPlatform, value: number) {
    const next = Math.max(1, Math.min(8, Math.round(value))), currentWidth = group.parallelism ?? 1;
    if (next === currentWidth || !online || loading || busy || filtered || setting || drafts[group.platform]) return;
    setSetting(group.platform); setError("");
    try {
      const result = await api<PriorityView>("PUT", "/account-priorities?schema=2", { platform: group.platform, parallelism: next, expected_version: group.version });
      if (!connected()) return;
      setView(result);
    } catch (error) { if (connected()) setError(cleanError(error)); }
    finally { if (connected()) setSetting(""); }
  }
  async function cancel(task: PriorityTask) {
    if (!online || !priorityPending(task)) return;
    const epoch = generation.current;
    try { const next = await api<PriorityTask>("POST", `/account-priority-orders/${task.id}/cancel?schema=2`, {}); if (current(epoch)) { updateTask(next); if (!priorityPending(next)) await finishTask(next, epoch); } }
    catch (error) { if (current(epoch)) setError(cleanError(error)); }
  }
  function taskState(task: PriorityTask) {
    if (task.base !== 1 || !task.platform || task.status === "completed") return null;
    const known = new Map(platforms.flatMap(group => group.items).map(account => [account.id, account.name]));
    return <div className="priority-task-state">
      {priorityPending(task) ? <>
        <span role="status">{taskLabels[task.status]} · {task.verified_count}/{task.total} 已确认
          {task.waiting_reason && ` · ${task.waiting_reason}${task.waiting_account_ids.length ? `：${task.waiting_account_ids.map(id => `${known.get(id) ?? "账号"} #${id}`).join("、")}` : ""}`}
        </span>
        <button disabled={!online || task.cancel_requested} onClick={() => void cancel(task)}>停止剩余修改</button>
      </> : <button disabled={!online || loading || busy || filtered} onClick={() => void read(task.platform!)}>重新核对</button>}
    </div>;
  }
  if (!active) return null;
  return <section ref={section} className="priority-manager" aria-label="全局优先级管理">
    <header className="priority-heading">{mobile && <button className="icon-button" aria-label="返回账号" onClick={props.back}><ArrowLeft size={20}/></button>}<h1>优先级</h1><button className="icon-button" aria-label="刷新优先级" disabled={!online || loading || !!submitting} onClick={() => void read(undefined, true)}><RefreshCw size={17}/></button></header>
    <div className="priority-filters"><label><Search size={16}/><input aria-label="搜索优先级账号" placeholder="搜索名称或账号 ID" value={query} onChange={e => { stopDrag(); setQuery(e.target.value); }}/></label></div>
    {supported === false && <p role="alert" className="bad-text">请升级服务端以按平台管理优先级。{!caps.current?.account_priority_ordering && "当前仅显示旧服务提供的账号。"}</p>}
    {!online && <p role="status" className="muted">当前离线，已保留草稿；连接恢复后请核对实际优先级。</p>}
    {error && <p role="alert" className="bad-text">{error}</p>}
    {loading && <p role="status">正在读取优先级</p>}
    {!loading && !platforms.length && <p className="priority-empty">暂无账号</p>}
    {!columns && platforms.length > 1 && <div className="priority-tabs" role="tablist" aria-label="优先级平台">{platforms.map(group => <button key={group.platform} id={`priority-tab-${group.platform}`} role="tab" aria-selected={selectedPlatform === group.platform} aria-controls={`priority-platform-${group.platform}`} onClick={() => { stopDrag(); setSelected(group.platform); }}>{priorityPlatformName(group.platform)}<span>{group.items.length}</span>{drafts[group.platform] && <i aria-label="有未应用草稿"/>}</button>)}</div>}
    <div className={`priority-platforms${columns ? " is-columns" : ""}`} style={columns ? { gridTemplateColumns: platforms.map(group => `minmax(0, ${Math.max(320, (group.parallelism ?? 1) * 220 + 72)}fr)`).join(" ") } : undefined}>
      {platforms.map(group => {
        const platform = group.platform, s = state(group), undo = history[platform] ?? [], task = activeTask?.platform === platform ? activeTask : group.last_task;
        const lanes = s.lanes;
        return <section key={platform} id={`priority-platform-${platform}`} data-priority-platform={platform} className="priority-column" hidden={!columns && selectedPlatform !== platform} aria-label={`${priorityPlatformName(platform)} 优先级`}>
          <header className="priority-column-heading"><h2>{priorityPlatformName(platform)}</h2><span>{s.rows.length} 个账号</span><div className="priority-parallelism" aria-label={`${priorityPlatformName(platform)}并列值`}><span>并列</span><button className="icon-button" aria-label={`减少${priorityPlatformName(platform)}并列值`} disabled={s.parallelism <= 1 || !!setting || !!drafts[platform] || !s.editing} onClick={() => void changeParallelism(group, s.parallelism - 1)}><Minus size={14}/></button><output>{s.parallelism}</output><button className="icon-button" aria-label={`增加${priorityPlatformName(platform)}并列值`} disabled={s.parallelism >= 8 || !!setting || !!drafts[platform] || !s.editing} onClick={() => void changeParallelism(group, s.parallelism + 1)}><Plus size={14}/></button></div>{s.changes.length > 0 && <b>{s.changes.length} 项变更</b>}</header>
          {s.conflict && <p role="alert" className="bad-text">该平台账号集合或优先级已变化，草稿已保留，请重新核对。<button disabled={!online || loading || busy} onClick={() => void read(platform)}>重新核对</button></p>}
          {supported === true && view !== null && s.blocked.length > 0 && <p role="alert" className="bad-text">重编号会修改不可编辑账号：{s.blocked.map(a => `${a.name} #${a.id}`).join("、")}，当前不能应用。</p>}
          <ol className={`priority-list${s.parallelism > 1 ? " has-parallel-slots" : ""}`} aria-label={`${priorityPlatformName(platform)} 账号顺序`} ref={node => { if (node) lists.current.set(platform, node); else lists.current.delete(platform); }}>{lanes.filter(lane => !filtered || lane.slots.some(slot => slot.id !== null && `${s.byId.get(slot.id)?.name} #${slot.id}`.toLocaleLowerCase().includes(query.trim().toLocaleLowerCase()))).map(lane => <li key={`${platform}-lane-${lane.priority}`} className="priority-lane"><span className="priority-lane-label">{lane.priority}</span><div className="priority-slot-grid" style={{ "--priority-parallelism": s.parallelism } as CSSProperties}>{lane.slots.map(slot => {
            const index = slot.index, a = slot.id === null ? undefined : s.byId.get(slot.id);
            if (!a) return <div key={`${platform}-slot-${index}`} className={`priority-slot priority-slot-empty${drag?.platform === platform && drag.target === index ? " is-target" : ""}`} data-priority-slot data-priority-index={index} data-platform={platform}><Plus size={15}/></div>;
            if (filtered && !`${a.name} #${a.id}`.toLocaleLowerCase().includes(query.trim().toLocaleLowerCase())) return <div key={a.id} className="priority-slot priority-slot-filtered" data-priority-slot data-priority-index={index} data-platform={platform}/>;
            const dragging = drag?.platform === platform && drag.id === a.id, insertion = drag?.platform === platform && drag.target === index && drag.id !== a.id;
            const before = insertion && s.slots.findIndex(slot => slot.id === drag!.id) > index;
            const live = liveAccounts.get(a.id), status = live?.status ?? a.status, schedulable = live?.schedulable ?? a.schedulable, targetPriority = lane.priority;
            return <div key={a.id} className={`priority-slot${insertion ? s.parallelism > 1 ? " is-target" : before ? " insert-before" : " insert-after" : ""}`}><div data-priority-id={a.id} data-platform={platform} className={`priority-account-row${dragging ? " dragging" : ""}`}>
              <button className="priority-grip" aria-label={`拖动 ${a.name}`} disabled={!s.editing || !a.editable} onPointerDown={e => pointerDown(e, platform, a.id)} onPointerMove={pointerMove} onPointerUp={pointerUp} onPointerCancel={stopDrag} onKeyDown={e => { if (["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight"].includes(e.key)) { e.preventDefault(); move(platform, a.id, index + (["ArrowUp", "ArrowLeft"].includes(e.key) ? -1 : 1)); } }}><GripVertical size={17}/></button>
              <div className="priority-identity"><strong title={a.name}>{a.name}</strong><small><span>#{a.id}</span><span>{a.type}</span></small>{(!schedulable || !a.editable || status !== "active") && <small className="priority-account-reason">{!a.editable ? a.reason : !schedulable ? "不可调度" : status}</small>}</div>
              <div className={`priority-number${a.priority !== targetPriority ? " is-changed" : ""}`}><span>当前 <b>{a.priority}</b></span><span>目标 <b>{targetPriority}</b></span></div>
              <div className="priority-account-controls">{live ? props.schedule(live, !!drag) : <span className="muted">调度状态待刷新</span>}
              <div className="priority-moves"><button className="icon-button" aria-label={`上移 ${a.name}`} disabled={!s.editing || !a.editable || index === 0} onClick={() => move(platform, a.id, index - 1)}><ArrowUp size={15}/></button><button className="icon-button" aria-label={`下移 ${a.name}`} disabled={!s.editing || !a.editable || index === s.slots.length - 1} onClick={() => move(platform, a.id, index + 1)}><ArrowDown size={15}/></button></div></div>
            </div></div>;
          })}</div></li>)}</ol>
          {filtered && !s.rows.some(a => `${a.name} #${a.id}`.toLocaleLowerCase().includes(query.trim().toLocaleLowerCase())) && <p className="priority-empty">无匹配账号</p>}
          {(s.changes.length > 0 || s.draft) && <div className="priority-draft">
            <div><button disabled={!undo.length || !s.editing} onClick={() => { stopDrag(); setDrafts(old => ({ ...old, [platform]: undo[undo.length - 1] })); setHistory(old => ({ ...old, [platform]: undo.slice(0, -1) })); delete requests.current[platform]; }}><Undo2 size={15}/>撤销</button><button disabled={!s.draft || !s.editing} onClick={() => { stopDrag(); setDrafts(old => ({ ...old, [platform]: undefined })); setHistory(old => ({ ...old, [platform]: [] })); delete requests.current[platform]; }}>放弃全部</button><button className="primary" disabled={!s.editing || busy || !!s.blocked.length || !s.changes.length} onClick={() => void apply(group)}><Check size={15}/>{submitting === platform ? "正在提交" : "应用变更"}</button></div>
            {priorityPending(activeTask) && activeTask?.platform !== platform && <p>其他平台排序任务进行中，草稿已保留。</p>}
          </div>}
          {notices[platform] && <p role="status" className="muted">{notices[platform]}</p>}
          {task && taskState(task)}
          {results[platform] && <p role="status" className="priority-result">{results[platform]}</p>}
        </section>;
      })}
    </div>
    {drag && <div className="priority-drag-ghost" style={{ left: drag.x + 12, top: drag.y + 12 }}>{platforms.find(group => group.platform === drag.platform)?.items.find(a => a.id === drag.id)?.name}</div>}
  </section>;
}
