import { useEffect, useRef, useState } from "react";
import { ArrowLeft, ArrowRight, Check, ChevronDown, GripVertical, Layers3, MoreHorizontal, RefreshCw, Search, Undo2, X } from "lucide-react";
import type { Account, Group } from "./types";
import { api, command } from "./bridge";
import DegradationAction, { DegradationBadge } from "./DegradationMark";
import { draftConflict, membershipZone, moveAccount, platformGroups, type Drafts, type Zone } from "./groupDraft";
import { useBackAction } from "./mobile";
import "./group-manager.css";

type Props = { accounts: Account[]; groups: Group[]; active: boolean; mobile: boolean; online: boolean; connectionKey: string; back: () => void; report: (error: unknown) => void; changed: (count: number, busy: boolean) => void };
type Drag = { id: number; x: number; y: number; zone?: Zone };
export default function GroupManager(props: Props) {
  const { accounts, groups, online, mobile, active } = props;
  const [platform, setPlatform] = useState("openai"), [query, setQuery] = useState("");
  const [pairs, setPairs] = useState<Record<string, number[]>>({});
  const [drafts, setDrafts] = useState<Drafts>({}), [history, setHistory] = useState<Drafts[]>([]);
  const [selected, setSelected] = useState<number | null>(null), [drag, setDrag] = useState<Drag | null>(null);
  const [busy, setBusy] = useState(false), [expanded, setExpanded] = useState(false), [message, setMessage] = useState("");
  const mounted = useRef(true), latest = useRef(props), draftRef = useRef(drafts), dragRef = useRef<Drag | null>(null);
  const pointer = useRef<{ id: number; x: number; y: number; ready: boolean; timer?: ReturnType<typeof setTimeout> } | null>(null);
  latest.current = props; draftRef.current = drafts; dragRef.current = drag;
  const available = platformGroups(groups, platform);
  const valid = (pairs[platform] ?? []).filter((id) => available.some((g) => g.id === id));
  const pair = [...valid, ...available.map((g) => g.id).filter((id) => !valid.includes(id))].slice(0, 2);
  const pairRef = useRef(pair); pairRef.current = pair;
  const pending = Object.values(drafts), conflicts = pending.filter((d) => draftConflict(d, accounts, groups));
  const candidates = accounts.filter((a) => a.platform === platform).sort((a, b) => a.name.localeCompare(b.name, "zh-CN") || a.id - b.id);
  const members = (a: Account) => drafts[a.id]?.target ?? a.group_ids;
  const matches = (a: Account) => !query.trim() || `${a.name} #${a.id}`.toLocaleLowerCase().includes(query.trim().toLocaleLowerCase());
  useEffect(() => { props.changed(pending.length, busy); }, [pending.length, busy]);
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; clearTimeout(pointer.current?.timer); }; }, []);
  useEffect(() => { if (!active) { setSelected(null); setDrag(null); } }, [active]);
  useBackAction(active && selected !== null, () => setSelected(null));

  function move(id: number, zone: Zone) {
    const account = latest.current.accounts.find((a) => a.id === id);
    if (!account || !latest.current.online || busy) return;
    const before = draftRef.current, next = moveAccount(before, account, pairRef.current, zone);
    if (JSON.stringify(before) !== JSON.stringify(next)) { setHistory((h) => [...h, before]); setDrafts(next); }
    setSelected(null); setMessage("");
  }
  useEffect(() => {
    const escape = (event: KeyboardEvent) => { if (active && event.key === "Escape") { clearTimeout(pointer.current?.timer); pointer.current = null; setDrag(null); setSelected(null); } };
    window.addEventListener("keydown", escape); return () => window.removeEventListener("keydown", escape);
  }, [active]);
  useEffect(() => {
    if (!drag) return;
    let frame = 0;
    const scroll = () => {
      const current = dragRef.current, surface = document.querySelector(".content") as HTMLElement | null;
      if (current && surface) {
        const bounds = surface.getBoundingClientRect();
        if (current.y < bounds.top + 60) surface.scrollTop -= 9;
        if (current.y > bounds.bottom - 70) surface.scrollTop += 9;
      }
      frame = requestAnimationFrame(scroll);
    };
    frame = requestAnimationFrame(scroll); return () => cancelAnimationFrame(frame);
  }, [!!drag]);
  function pointerDown(event: React.PointerEvent, id: number) {
    if (!online || busy || event.button !== 0) return;
    event.stopPropagation(); event.currentTarget.setPointerCapture(event.pointerId);
    const item = { id, x: event.clientX, y: event.clientY, ready: event.pointerType !== "touch", timer: undefined as ReturnType<typeof setTimeout> | undefined };
    pointer.current = item;
    if (!item.ready) item.timer = setTimeout(() => { item.ready = true; setDrag({ id, x: item.x, y: item.y }); }, 350);
  }
  function pointerMove(event: React.PointerEvent) {
    const item = pointer.current;
    if (!item) return;
    if (!item.ready && Math.hypot(event.clientX - item.x, event.clientY - item.y) > 9) { clearTimeout(item.timer); pointer.current = null; return; }
    if (!item.ready || Math.hypot(event.clientX - item.x, event.clientY - item.y) < 5 && !dragRef.current) return;
    const zone = document.elementFromPoint(event.clientX, event.clientY)?.closest<HTMLElement>("[data-drop-zone]")?.dataset.dropZone as Zone | undefined;
    setDrag({ id: item.id, x: event.clientX, y: event.clientY, zone });
  }
  function pointerUp(event: React.PointerEvent) {
    const item = pointer.current; clearTimeout(item?.timer); pointer.current = null;
    const current = dragRef.current;
    const zone = document.elementFromPoint(event.clientX, event.clientY)?.closest<HTMLElement>("[data-drop-zone]")?.dataset.dropZone as Zone | undefined;
    if (current && item && zone) move(item.id, zone);
    else if (item && !current) setSelected(item.id);
    setDrag(null);
  }
  async function apply() {
    if (busy || !online || !pending.length) return;
    if (conflicts.length) { setMessage("账号或分组已变化，请先处理冲突"); setExpanded(true); return; }
    setBusy(true); setMessage(""); let saved = 0;
    try {
      for (const draft of pending) {
        if (!mounted.current || !latest.current.online) throw new Error("连接已变化，剩余修改未提交");
        if (draftConflict(draft, latest.current.accounts, latest.current.groups)) throw new Error(`${draft.name} 已变化，请刷新后处理冲突`);
        const result = await api<{ verified: boolean; group_ids: number[]; version: string }>("PUT", `/accounts/${draft.id}/groups`, { expected_version: draft.version, scope_group_ids: draft.scope, group_ids: draft.target.filter((id) => draft.scope.includes(id)) });
        if (!mounted.current) return;
        if (!result.verified) throw new Error(`${draft.name} 写入未确认`);
        saved++; setDrafts((all) => { const next = { ...all }; delete next[draft.id]; return next; }); setHistory([]);
      }
      setMessage(`已保存 ${saved} 个账号`); setExpanded(false);
    } catch (error) {
      if (mounted.current) setMessage(`${saved ? `已保存 ${saved} 个账号；` : ""}${String(error instanceof Error ? error.message : error)}`);
    } finally {
      if (mounted.current) { setBusy(false); void command("refresh").catch(props.report); }
    }
  }
  function card(a: Account) {
    const dirty = drafts[a.id], conflict = dirty && draftConflict(dirty, accounts, groups);
    return <article key={a.id} className={`group-account${selected === a.id ? " selected" : ""}${dirty ? " dirty" : ""}${drag?.id === a.id ? " dragging" : ""}`}>
      <button className="group-grip" aria-label={`拖动 ${a.name} #${a.id}`} disabled={!online || busy} onPointerDown={(e) => pointerDown(e, a.id)} onPointerMove={pointerMove} onPointerUp={pointerUp} onPointerCancel={() => { clearTimeout(pointer.current?.timer); pointer.current = null; setDrag(null); }} onClick={(e) => e.stopPropagation()}><GripVertical size={16}/></button>
      <button className="group-account-select" disabled={!online || busy} aria-pressed={selected === a.id} onClick={(e) => { e.stopPropagation(); setSelected(selected === a.id ? null : a.id); }}>
        <strong>{a.name}</strong><span className="group-account-meta"><span>#{a.id}</span><span>{a.type === "oauth" ? "OAuth" : a.type === "apikey" ? "Key" : a.type}</span><DegradationBadge account={a}/>{!a.available && <span className="group-account-status">{a.blockers[0]?.label || "不可调度"}</span>}{dirty && <span className={conflict ? "bad-text" : "group-dirty-label"}>{conflict ? "冲突" : "待应用"}</span>}</span>
      </button>
      {a.platform === "openai" && a.type === "oauth" && <details className="group-account-menu" onClick={(e) => e.stopPropagation()}><summary aria-label={`${a.name}操作`}><MoreHorizontal size={17}/></summary><div><DegradationAction account={a} online={online && !busy} report={props.report}/></div></details>}
    </article>;
  }
  function zone(zone: Zone, label: string, values: Account[], extra = "") {
    return <section className={`membership-zone zone-${zone} ${extra}${drag?.zone === zone ? " drop-over" : ""}${selected !== null ? " can-drop" : ""}`} data-drop-zone={pair.length ? zone : undefined} tabIndex={pair.length && selected !== null ? 0 : -1} role="group" aria-label={label}
      onClick={() => { if (selected !== null) move(selected, zone); }} onKeyDown={(e) => { if (selected !== null && (e.key === "Enter" || e.key === " ")) { e.preventDefault(); move(selected, zone); } }}>
      <header><h3>{label}</h3><span>{values.length}</span></header>
      <div className="membership-accounts">{values.filter(matches).map(card)}{!values.filter(matches).length && <span className="group-empty">{query ? "无匹配账号" : "暂无账号"}</span>}</div>
    </section>;
  }
  const groupName = (id: number) => groups.find((g) => g.id === id)?.name || `#${id}`;
  const region = (name: Zone) => candidates.filter((a) => membershipZone(members(a), pair) === name);
  const unassigned = region("none").filter((a) => !members(a).some((id) => groups.some((g) => g.id === id)));
  const others = region("none").filter((a) => !unassigned.includes(a));
  return <section className="group-manager" hidden={!active}>
    <div className="group-manager-toolbar">
      {mobile && <button className="icon-button" aria-label="返回账号" onClick={props.back}><ArrowLeft size={20}/></button>}
      <div className="group-platforms" role="tablist" aria-label="分组平台">{[["openai", "Codex"], ["grok", "Grok"]].map(([id, label]) => <button key={id} role="tab" aria-selected={platform === id} className={platform === id ? "active" : ""} disabled={busy || !!drag} onClick={() => { setPlatform(id); setSelected(null); setQuery(""); }}>{label}</button>)}</div>
      <label className="group-search"><Search size={16}/><input value={query} placeholder="搜索名称或账号 ID" aria-label="搜索分组账号" onChange={(e) => setQuery(e.target.value)}/></label>
      <button className="icon-button" aria-label="刷新分组" disabled={!online || busy} onClick={() => void command("refresh").catch(props.report)}><RefreshCw size={17}/></button>
    </div>
    {available.length > 2 && <div className="group-pair-selectors">{[0, 1].map((side) => <label key={side}>{side ? "右侧分组" : "左侧分组"}<select value={pair[side]} disabled={busy} onChange={(e) => { const next = [...pair]; next[side] = Number(e.target.value); setPairs({ ...pairs, [platform]: next }); setSelected(null); }}>{available.filter((g) => g.id !== pair[1 - side]).map((g) => <option key={g.id} value={g.id}>{g.name} #{g.id}</option>)}</select></label>)}</div>}
    {selected !== null && <div className="group-destination-bar"><span>{accounts.find((a) => a.id === selected)?.name}</span>{pair.length > 0 && <button onClick={() => move(selected, "left")}>{groupName(pair[0])}</button>}{pair.length > 1 && <><button onClick={() => move(selected, "both")}>两组共有</button><button onClick={() => move(selected, "right")}>{groupName(pair[1])}</button></>}<button onClick={() => move(selected, "none")}>移出当前分组</button><button className="icon-button" aria-label="取消选择" onClick={() => setSelected(null)}><X size={16}/></button></div>}
    {available.length > 0 && <><div className="group-map-heading">{pair.map((id, index) => <div key={id} className={index ? "group-heading-right" : "group-heading-left"}><i/><div><strong>{groupName(id)}</strong><span>#{id}</span></div><b>{candidates.filter((a) => members(a).includes(id)).length}</b></div>)}</div>
      <div className={`group-map${pair.length === 1 ? " single" : ""}`}><div className="group-outline outline-left"/>{pair.length > 1 && <div className="group-outline outline-right"/>}{zone("left", pair.length > 1 ? `仅 ${groupName(pair[0])}` : groupName(pair[0]), region("left"))}{pair.length > 1 && <>{zone("both", "两组共有", region("both"))}{zone("right", `仅 ${groupName(pair[1])}`, region("right"))}</>}</div></>}
    {!available.length && <div className="group-no-groups"><Layers3 size={24}/><span>此平台没有分组</span></div>}
    <div className="group-pool">{zone("none", "待分组", unassigned, "unassigned")}{others.length > 0 && zone("none", "其他分组", others, "other-groups")}</div>
    {!!pending.length && <div className="group-draft-panel"><div className="group-draft-toolbar"><button className="text-button" onClick={() => setExpanded(!expanded)}><span>{pending.length} 个账号待应用</span><ChevronDown size={15}/></button><div><button disabled={busy || !history.length} onClick={() => { const before = history.at(-1); if (before) { setDrafts(before); setHistory(history.slice(0, -1)); setMessage(""); } }}><Undo2 size={15}/>撤销</button><button disabled={busy} onClick={() => { setDrafts({}); setHistory([]); setMessage(""); }}>放弃全部</button><button className="primary" disabled={!online || busy || !!conflicts.length} onClick={() => void apply()}><Check size={16}/>{busy ? "正在保存" : `应用变更 ${pending.length}`}</button></div></div>
      {expanded && <ul className="group-draft-list">{pending.map((d) => <li key={d.id}><strong>{d.name} #{d.id}</strong><span>{d.original.map(groupName).join("、") || "待分组"}<ArrowRight size={14}/>{d.target.map(groupName).join("、") || "待分组"}</span>{draftConflict(d, accounts, groups) && <button onClick={() => { setDrafts((all) => { const next = { ...all }; delete next[d.id]; return next; }); setHistory([]); }}>放弃冲突项</button>}</li>)}</ul>}
    </div>}
    {message && <div className="group-result" role="status">{message}<button className="icon-button" aria-label="关闭结果" onClick={() => setMessage("")}><X size={14}/></button></div>}
    {drag && <div className="group-drag-ghost" style={{ left: drag.x + 12, top: drag.y + 12 }}>{accounts.find((a) => a.id === drag.id)?.name}<span>#{drag.id}</span></div>}
  </section>;
}
