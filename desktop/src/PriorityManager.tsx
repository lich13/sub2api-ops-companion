import { useEffect, useRef, useState } from "react";
import { ArrowDown, ArrowLeft, ArrowUp, Check, GripVertical, RefreshCw, Search, Undo2 } from "lucide-react";
import { api, command } from "./bridge";
import { clientId } from "./accountOperations";
import { movePriority, priorityOrder, priorityPending, samePriorityAccounts, type PriorityAccount, type PriorityDraft, type PriorityTask, type PriorityView } from "./priorities";
import type { Account } from "./types";
import "./priority-manager.css";

type Props = { accounts: Account[]; active: boolean; online: boolean; mobile: boolean; connectionKey: string; back: () => void; changed: (count: number) => void };
type Drag = { id: number; x: number; y: number; target: number | null };
const taskLabels: Record<string, string> = { queued: "等待执行", running: "正在保存", checking: "正在核对", completed: "全部已确认", needs_confirmation: "需要重新核对", failed: "保存失败", cancelled: "已停止" };
const itemLabels: Record<string, string> = { pending: "尚未派发", unchanged: "无需修改", dispatched: "已派发，待核对", verified: "已确认" };
const cleanError = (error: unknown) => String(error).replace(/^Error: /, "");
export default function PriorityManager(props: Props) {
  const { active, online, mobile, connectionKey } = props;
  const [view, setView] = useState<PriorityView | null>(null), [supported, setSupported] = useState<boolean | null>(null);
  const [draft, setDraft] = useState<PriorityDraft | null>(null), [history, setHistory] = useState<(PriorityDraft | null)[]>([]);
  const [task, setTask] = useState<PriorityTask | null>(null), [query, setQuery] = useState(""), [platform, setPlatform] = useState("");
  const [loading, setLoading] = useState(false), [submitting, setSubmitting] = useState(false), [error, setError] = useState("");
  const [readRevision, setReadRevision] = useState(0);
  const [drag, setDrag] = useState<Drag | null>(null), [expanded, setExpanded] = useState(false), [notice, setNotice] = useState("");
  const generation = useRef(0), alive = useRef(true), submittingRef = useRef(false), latest = useRef(props);
  const draftRef = useRef(draft), taskRef = useRef(task), dragRef = useRef(drag), section = useRef<HTMLElement>(null);
  const pointer = useRef<{ id: number; x: number; y: number; ready: boolean; timer?: ReturnType<typeof setTimeout> } | null>(null);
  const request = useRef<{ signature: string; body: Record<string, unknown> } | null>(null);
  latest.current = props; draftRef.current = draft; taskRef.current = task; dragRef.current = drag;
  const fallback: PriorityAccount[] = props.accounts.map(a => ({ id: a.id, name: a.name, platform: a.platform, type: a.type, priority: a.priority, version: a.operation_versions?.priority ?? "", editable: false, reason: "请升级服务端以管理全局优先级", status: a.status, schedulable: a.schedulable }));
  const accounts = draft?.accounts ?? view?.items ?? fallback;
  const ids = draft?.ids ?? priorityOrder(accounts);
  const rows = ids.map(id => accounts.find(a => a.id === id)).filter((a): a is PriorityAccount => !!a);
  const changes = rows.filter((a, index) => a.priority !== index);
  const filtered = !!query.trim() || !!platform, busy = submitting || priorityPending(task);
  const conflict = !!draft && !!view && (draft.version !== view.version || !samePriorityAccounts(draft.ids, view.items));
  const blocked = rows.some((a, index) => a.priority !== index && !a.editable);
  const writable = online && supported === true && !!view && !loading && !busy && !filtered && !conflict;
  const current = (epoch: number) => alive.current && epoch === generation.current && latest.current.connectionKey === connectionKey && latest.current.active && latest.current.online;
  useEffect(() => { props.changed(draft && changes.length ? changes.length : 0); }, [draft, changes.length]);
  useEffect(() => { alive.current = true; return () => { alive.current = false; ++generation.current; clearTimeout(pointer.current?.timer); }; }, []);
  function stopDrag() { clearTimeout(pointer.current?.timer); pointer.current = null; setDrag(null); }
  useEffect(() => { if (!active || !online) stopDrag(); }, [active, online]);
  useEffect(() => { if (conflict) stopDrag(); }, [conflict]);
  async function read(reprepare = false) {
    if (!latest.current.active || !latest.current.online) return;
    const epoch = ++generation.current; setLoading(true); setError("");
    try {
      const caps = await api<{ features?: { account_priority_ordering?: boolean } }>("GET", "/capabilities");
      if (!current(epoch)) return;
      const available = caps.features?.account_priority_ordering === true; setSupported(available);
      if (!available) return;
      const next = await api<PriorityView>("GET", "/account-priorities");
      if (!current(epoch)) return;
      const summary = next.active_task ?? next.last_task;
      const detail = summary ? await api<PriorityTask>("GET", `/account-priority-orders/${summary.id}`) : null;
      if (!current(epoch)) return;
      setView(next); setTask(detail); setReadRevision(revision => revision + 1);
      if (reprepare) {
        const target = draftRef.current?.ids ?? taskRef.current?.items?.map(item => item.account_id);
        setDraft(target && samePriorityAccounts(target, next.items) ? { ids: target, version: next.version, accounts: next.items } : null);
        setHistory([]); request.current = null;
        setNotice(target && !samePriorityAccounts(target, next.items) ? "账号集合已变化，已按当前优先级重新准备" : "已重新读取实际优先级，请核对后应用");
      }
    } catch (error) { if (current(epoch)) setError(cleanError(error)); }
    finally { if (current(epoch)) setLoading(false); }
  }
  useEffect(() => {
    if (active && online) void read(); else setLoading(false);
    return () => { ++generation.current; };
  }, [active, online, connectionKey]);
  useEffect(() => {
    if (!active || !online || !priorityPending(task)) return;
    let disposed = false, timer: ReturnType<typeof setTimeout>;
    const epoch = generation.current, id = task!.id;
    const poll = async () => {
      try {
        const next = await api<PriorityTask>("GET", `/account-priority-orders/${id}`);
        if (disposed || !current(epoch)) return;
        setTask(next); setError("");
        if (!priorityPending(next)) { void command("refresh"); await read(); return; }
      } catch (error) { if (!disposed && current(epoch)) setError(cleanError(error)); }
      if (!disposed && current(epoch)) timer = setTimeout(() => void poll(), 1000);
    };
    timer = setTimeout(() => void poll(), 1000);
    return () => { disposed = true; clearTimeout(timer); };
  }, [task?.id, task?.status, active, online, readRevision]);
  function move(id: number, target: number) {
    if (!writable || !view || !accounts.find(a => a.id === id)?.editable) return;
    const next = movePriority(ids, id, target);
    if (next.every((id, index) => ids[index] === id)) return;
    setHistory(old => [...old, draft]); setDraft({ ids: next, version: draft?.version ?? view.version, accounts: draft?.accounts ?? view.items });
    setNotice(""); request.current = null;
  }
  function targetAt(x: number, y: number, id: number) {
    const row = document.elementFromPoint(x, y)?.closest<HTMLElement>("[data-priority-id]");
    if (!row || !section.current?.contains(row)) return null;
    const targetId = Number(row.dataset.priorityId), original = ids.indexOf(id), over = ids.indexOf(targetId);
    if (original < 0 || over < 0) return null;
    const bounds = row.getBoundingClientRect();
    const gap = over + (y >= bounds.top + bounds.height / 2 ? 1 : 0);
    return gap > original ? gap - 1 : gap;
  }
  function pointerDown(event: React.PointerEvent, id: number) {
    if (!writable || event.button !== 0 || !accounts.find(a => a.id === id)?.editable) return;
    event.currentTarget.setPointerCapture(event.pointerId);
    const item = { id, x: event.clientX, y: event.clientY, ready: event.pointerType !== "touch", timer: undefined as ReturnType<typeof setTimeout> | undefined };
    pointer.current = item;
    if (!item.ready) item.timer = setTimeout(() => { item.ready = true; setDrag({ id, x: item.x, y: item.y, target: null }); }, 350);
  }
  function pointerMove(event: React.PointerEvent) {
    const item = pointer.current;
    if (!item) return;
    const distance = Math.hypot(event.clientX - item.x, event.clientY - item.y);
    if (!item.ready && distance > 9) { stopDrag(); return; }
    if (!item.ready || distance < 5 && !dragRef.current) return;
    setDrag({ id: item.id, x: event.clientX, y: event.clientY, target: targetAt(event.clientX, event.clientY, item.id) });
  }
  function pointerUp(event: React.PointerEvent) {
    const item = pointer.current, dragging = dragRef.current;
    const target = item && dragging ? targetAt(event.clientX, event.clientY, item.id) : null;
    if (item && target !== null) move(item.id, target);
    stopDrag();
  }
  useEffect(() => {
    if (!drag) return;
    let frame = 0;
    const scroll = () => {
      const point = dragRef.current, surface = section.current?.closest<HTMLElement>(".content");
      if (point && surface) { const bounds = surface.getBoundingClientRect(); if (point.y < bounds.top + 60) surface.scrollTop -= 9; if (point.y > bounds.bottom - 70) surface.scrollTop += 9; }
      frame = requestAnimationFrame(scroll);
    };
    frame = requestAnimationFrame(scroll); return () => cancelAnimationFrame(frame);
  }, [!!drag]);
  useEffect(() => { const cancel = (event: KeyboardEvent) => { if (active && event.key === "Escape") stopDrag(); }; window.addEventListener("keydown", cancel); return () => window.removeEventListener("keydown", cancel); }, [active]);
  useEffect(() => { const hidden = () => { if (document.hidden) stopDrag(); }; document.addEventListener("visibilitychange", hidden); return () => document.removeEventListener("visibilitychange", hidden); }, []);
  async function apply() {
    if (!writable || !view || blocked || !changes.length || submittingRef.current) return;
    submittingRef.current = true; setSubmitting(true); setError(""); stopDrag();
    const connected = () => alive.current && latest.current.connectionKey === connectionKey;
    try {
      const fields = { ordered_account_ids: ids, expected_version: draft?.version ?? view.version };
      const signature = JSON.stringify(fields);
      if (request.current?.signature !== signature) request.current = { signature, body: { ...fields, request_id: crypto.randomUUID(), client_id: clientId() } };
      const next = await api<PriorityTask>("POST", "/account-priority-orders", request.current.body);
      if (!connected()) return;
      ++generation.current; setLoading(false); setReadRevision(revision => revision + 1);
      setTask(next); setDraft(null); setHistory([]); request.current = null; setExpanded(true); setNotice("任务已提交，离开页面或关闭客户端后仍会继续");
      if (!priorityPending(next)) { void command("refresh"); await read(); }
    } catch (error) { if (connected()) setError(cleanError(error)); }
    finally { submittingRef.current = false; if (alive.current) setSubmitting(false); }
  }
  async function cancel() {
    if (!task || !online || !priorityPending(task)) return;
    const epoch = generation.current;
    try { const next = await api<PriorityTask>("POST", `/account-priority-orders/${task.id}/cancel`, {}); if (current(epoch)) { setTask(next); if (!priorityPending(next)) { void command("refresh"); await read(); } } }
    catch (error) { if (current(epoch)) setError(cleanError(error)); }
  }
  if (!active) return null;
  return <section ref={section} className="priority-manager" aria-label="全局优先级管理">
    <header className="priority-heading">{mobile && <button className="icon-button" aria-label="返回账号" onClick={props.back}><ArrowLeft size={20}/></button>}<div><h1>优先级</h1><p>全部平台统一排序，越靠上越优先；应用后编号从 0 开始。</p></div><button className="icon-button" aria-label="刷新优先级" disabled={!online || loading || submitting} onClick={() => void read()}><RefreshCw size={17}/></button></header>
    <div className="priority-filters"><label><Search size={16}/><input aria-label="搜索优先级账号" placeholder="搜索名称或账号 ID" value={query} onChange={e => { stopDrag(); setQuery(e.target.value); }}/></label><select aria-label="优先级平台" value={platform} onChange={e => { stopDrag(); setPlatform(e.target.value); }}><option value="">全部平台</option>{[...new Set(accounts.map(a => a.platform))].sort().map(p => <option key={p} value={p}>{p}</option>)}</select></div>
    {supported === false && <p role="alert" className="bad-text">请升级服务端以管理全局优先级。</p>}
    {!online && <p role="status" className="muted">当前离线，已保留草稿；连接恢复后请核对实际优先级。</p>}
    {filtered && <p className="muted">筛选结果仅供查看，清除筛选后可调整全局顺序。</p>}
    {conflict && <p role="alert" className="bad-text">账号集合或优先级已变化，草稿已保留，请重新核对。</p>}
    {supported === true && blocked && <p role="alert" className="bad-text">重编号会修改不可编辑账号：{rows.filter((a, index) => !a.editable && a.priority !== index).map(a => `${a.name} #${a.id}`).join("、")}，当前不能应用。</p>}
    {error && <p role="alert" className="bad-text">{error}</p>}
    {loading && <p role="status">正在读取优先级</p>}
    <ol className="priority-list">{rows.map((a, index) => {
      if (platform && a.platform !== platform || query.trim() && !`${a.name} #${a.id}`.toLocaleLowerCase().includes(query.trim().toLocaleLowerCase())) return null;
      const oldIndex = drag ? ids.indexOf(drag.id) : -1;
      const insertion = drag?.target === null || !drag ? -1 : drag.target >= oldIndex ? drag.target + 1 : drag.target;
      const blocker = props.accounts.find(item => item.id === a.id)?.blockers[0]?.label;
      return <li key={a.id} data-priority-id={a.id} className={`${drag?.id === a.id ? "dragging" : ""}${insertion === index ? " insert-before" : ""}${insertion === rows.length && index === rows.length - 1 ? " insert-after" : ""}`}>
        <button className="priority-grip" aria-label={`拖动 ${a.name} #${a.id}`} disabled={!writable || !a.editable} onPointerDown={e => pointerDown(e, a.id)} onPointerMove={pointerMove} onPointerUp={pointerUp} onPointerCancel={stopDrag} onKeyDown={e => { if (e.key === "ArrowUp" || e.key === "ArrowDown") { e.preventDefault(); move(a.id, index + (e.key === "ArrowUp" ? -1 : 1)); } }}><GripVertical size={17}/></button>
        <div className="priority-identity"><strong title={a.name}>{a.name}</strong><small>#{a.id}<span className="priority-platform">{a.platform}</span>{!a.editable ? a.reason || "不可编辑" : blocker || (!a.schedulable ? "不可调度" : a.status !== "active" ? a.status : "")}</small></div>
        <div className="priority-number"><span>当前 <b>{a.priority}</b></span>{a.priority !== index && <span>目标 <b>{index}</b></span>}</div>
        <div className="priority-moves"><button className="icon-button" aria-label={`上移 ${a.name}`} disabled={!writable || !a.editable || index === 0} onClick={() => move(a.id, index - 1)}><ArrowUp size={16}/></button><button className="icon-button" aria-label={`下移 ${a.name}`} disabled={!writable || !a.editable || index === rows.length - 1} onClick={() => move(a.id, index + 1)}><ArrowDown size={16}/></button></div>
      </li>;
    })}</ol>
    {!rows.length && !loading && <p className="muted">暂无账号</p>}
    {(!!draft || !!changes.length) && <div className="priority-draft"><p>{draft ? `${changes.length} 个账号待应用` : "当前编号存在重复或间隔，应用后将统一重编号"}</p><div><button disabled={!writable || !history.length} onClick={() => { setDraft(history.at(-1) ?? null); setHistory(h => h.slice(0, -1)); request.current = null; }}><Undo2 size={15}/>撤销</button><button disabled={busy || !draft} onClick={() => { setDraft(null); setHistory([]); request.current = null; setNotice(""); }}>放弃全部</button><button className="primary" disabled={!writable || blocked || !changes.length} onClick={() => void apply()}><Check size={16}/>{submitting ? "正在提交" : `应用变更 ${changes.length}`}</button></div></div>}
    {(conflict || task && !priorityPending(task) && task.status !== "completed") && <button disabled={!online || loading || busy} onClick={() => void read(true)}>重新核对</button>}
    {notice && <p role="status">{notice}</p>}
    {task && <section className="priority-progress" aria-label="优先级排序进度"><header><strong>{taskLabels[task.status] || "状态待核对"} · {task.verified_count}/{task.total}</strong><button className="text-button" onClick={() => setExpanded(!expanded)}>{expanded ? "收起进度" : "查看进度"}</button>{priorityPending(task) && <button disabled={!online || task.cancel_requested} onClick={() => void cancel()}>{task.cancel_requested ? "正在停止" : "停止剩余修改"}</button>}</header><p>{task.reason}</p>{expanded && <ul>{task.items?.map(item => <li key={item.account_id}><span>{item.account_name} #{item.account_id}</span><span>{item.original_priority} → {item.target_priority}</span><span>核验 {item.observed_priority ?? "—"}</span><span>{itemLabels[item.status] || "待核对"}</span></li>)}</ul>}</section>}
    {drag && <div className="priority-drag-ghost" style={{ left: drag.x + 12, top: drag.y + 12 }}>{accounts.find(a => a.id === drag.id)?.name}</div>}
  </section>;
}
