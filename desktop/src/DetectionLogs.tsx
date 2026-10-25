import { useEffect, useRef, useState } from "react";
import { X } from "lucide-react";
import { api } from "./bridge";
import { engineName } from "./modelTestTypes";
import { fullTime } from "./types";
import { useBackAction } from "./mobile";
import { useDialogFocus } from "./dialogFocus";
import { useDetectionLogFeed, type DetectionLog } from "./useDetectionLogFeed";
import "./detection-logs.css";

const statuses: Record<string, string> = { completed: "已完成", failed: "失败", cancelled: "已取消", interrupted: "已中断", needs_confirmation: "待确认" };
const outcomes: Record<string, string> = { normal: "未确认降智", degraded: "首组判定降智", aggregate_degraded: "汇总符合降智特征", unavailable: "历史详情不可用", failed: "失败", cancelled: "已取消", interrupted: "已中断", needs_confirmation: "待确认" };
const triggers: Record<string, string> = { scheduled: "定时检测", recheck: "降智复查", error: "错误触发", slow: "慢首字", recovery: "额度恢复验证", credit: "用卡恢复验证" };
const notices: Record<string, string> = { pending: "待推送", queued: "待推送", retry: "待重试", delivered: "已推送", suppressed: "已取消", failed: "投递失败" };
const basis = (summary: DetectionLog["summary"]) => summary.disposition?.basis ?? summary.recovery?.basis;
const evidence = (row: DetectionLog) => { const kind = basis(row.summary) ?? row.summary.classification?.basis; return kind === "sample_family" ? `第 ${(row.summary.disposition?.group_indexes ?? row.summary.classification?.group_indexes ?? []).join("、")} 组跨家族证据` : kind === "aggregate" ? "最终汇总" : "单组预测"; };
const result = (row: DetectionLog) => row.summary.engine === "lm_detector" ? ({ degraded: "判定降智", unavailable: "无法判断", normal: "未确认降智" } as Record<string, string>)[row.summary.outcome] || outcomes[row.summary.outcome] || "待核对" : basis(row.summary) === "aggregate" ? "最终汇总判定降智" : outcomes[row.summary.outcome] || "待核对";
const executionTone: Record<string, string> = { completed: "neutral", failed: "bad", cancelled: "neutral", interrupted: "interrupted", needs_confirmation: "warning" };
const outcomeTone: Record<string, string> = { normal: "good", degraded: "bad", aggregate_degraded: "bad", unavailable: "warning", ...executionTone };
const noticeTone: Record<string, string> = { pending: "warning", queued: "warning", retry: "warning", delivered: "neutral", suppressed: "neutral", failed: "bad" };
function Badge({ text, tone = "warning", title }: { text: string; tone?: string; title?: string }) { return <span className={`detection-badge is-${tone}`} title={title}>{text}</span>; }
const probability = (value: number | null | undefined) => typeof value === "number" ? `${(value * 100).toFixed(1)}%` : "—";
function Receipts({ row }: { row: DetectionLog }) {
  const s = row.summary;
  const verifying = row.triggers.some(t => t === "recovery" || t === "credit") || !!s.verification;
  const pendingTone = (status?: string) => status === "failed" ? "bad" : status === "cancelled" ? "neutral" : "warning";
  return <div className="detection-receipts">
    {s.disposition && <><Badge text={s.disposition.marked === true ? "标记已保存" : "标记待核对"} tone={s.disposition.marked === true ? "bad" : pendingTone(s.disposition.status)} title={s.disposition.reason}/><Badge text={s.disposition.schedule_verified === true ? "停调度已确认" : "停调度待核对"} tone={s.disposition.schedule_verified === true ? "bad" : pendingTone(s.disposition.status)}/></>}
    {s.recovery && <Badge text={s.recovery.mark_cleared === true ? "已取消降智标记" : "取消标记待核对"} tone={s.recovery.mark_cleared === true ? "good" : pendingTone(s.recovery.status)} title={s.recovery.reason}/>}
    {verifying && <Badge text={s.verification?.status === "passed" ? "模型验证通过" : s.verification?.status === "failed" ? "模型验证失败" : "模型验证待核对"} tone={s.verification?.status === "passed" ? "good" : pendingTone(s.verification?.status)} title={s.verification?.reason}/>}
    {s.restoration && <Badge text={s.restoration.status === "restored" ? "已恢复调度" : "恢复调度待核对"} tone={s.restoration.status === "restored" ? "good" : pendingTone(s.restoration.status)} title={s.restoration.reason}/>}
    {!s.disposition && !s.recovery && !verifying && !s.restoration && "—"}
  </div>;
}
function Facts({ row }: { row: DetectionLog }) {
  const { summary, notification } = row;
  return <dl className="detection-meta">
    <dt>请求模型</dt><dd>{row.model_id}</dd><dt>触发</dt><dd>{row.triggers.map(t => triggers[t] || "自动检测").join("、") || "自动检测"}</dd>
    <dt>检测源</dt><dd>{engineName(summary.engine)} · {summary.sample_count ?? 3} 组</dd>
    <dt>执行状态</dt><dd>{statuses[row.status] || "待核对"}</dd><dt>检测结论</dt><dd>{result(row)}</dd>
    {(basis(summary) || summary.classification?.basis) && <><dt>处置依据</dt><dd>{summary.engine === "lm_detector" ? evidence(row) : basis(summary) === "aggregate" ? "最终汇总" : "首组指纹"}</dd></>}
    {summary.engine === "lm_detector" && <><dt>分析状态</dt><dd>{({ partial: "初步结果", scored: "汇总结果", not_model: "序列异常", unscorable: "样本不足" } as Record<string, string>)[summary.decision ?? ""] || "—"}</dd>{summary.reason && <><dt>无法判断原因</dt><dd>{summary.reason}</dd></>}{summary.group_predictions?.map(g => <div className="detection-meta-group" key={g.index}><dt>第 {g.index} 组</dt><dd>{g.prediction || "无排名"}</dd></div>)}<dt>资源版本</dt><dd>{summary.bank_version?.revision || "—"}</dd></>}
    <dt>首组预测</dt><dd>{summary.first_prediction || "暂无记录"}</dd><dt>汇总预测</dt><dd>{summary.prediction_name || summary.prediction || "暂无记录"}</dd>
    <dt>汇总匹配度</dt><dd>{summary.probability == null ? "—" : `${(summary.probability * 100).toFixed(1)}%`}</dd>
    {typeof summary.first_probability === "number" && <><dt>首组匹配度</dt><dd>{(summary.first_probability * 100).toFixed(1)}%</dd></>}
    {summary.disposition && <><dt>降智处置</dt><dd>{summary.disposition.marked ? "标记已保存" : "标记待核对"} · {summary.disposition.schedule_verified ? "停调度已确认" : "停调度待核对"}{summary.disposition.reason && ` · ${summary.disposition.reason}`}</dd></>}
    {summary.recovery && <><dt>复查恢复</dt><dd>{summary.recovery.mark_cleared ? "已取消降智标记" : summary.recovery.reason || "恢复待核对"}</dd></>}
    {row.triggers.some(t => t === "recovery" || t === "credit") && <><dt>恢复验证</dt><dd>{summary.verification?.status === "passed" ? "模型验证通过" : summary.verification?.reason || "验证回执不可用"}</dd></>}
    {summary.restoration && <><dt>调度恢复</dt><dd>{summary.restoration.status === "restored" ? "已恢复调度" : "恢复待核对"} · {fullTime(summary.restoration.completed_at)}</dd></>}
    <dt>通知</dt><dd>{notification ? `${notices[notification.status] || "状态待核对"}${notification.merged ? "（合并用卡通知）" : ""}` : "无通知记录"}</dd>
    {notification?.delivered_at && <><dt>推送时间</dt><dd>{fullTime(notification.delivered_at)}</dd></>}
    <dt>开始</dt><dd>{fullTime(row.started_at)}</dd><dt>结束</dt><dd>{fullTime(row.completed_at)}</dd>
    {row.error && <><dt>原因</dt><dd>{row.error}</dd></>}
  </dl>;
}
function Detail({ row, online, close }: { row: DetectionLog; online: boolean; close: () => void }) {
  const [groups, setGroups] = useState<{ index: number; status: string; duration_ms?: number | null }[]>([]);
  const [error, setError] = useState(""), [loading, setLoading] = useState(online);
  const generation = useRef(0);
  const dialog = useRef<HTMLElement>(null);
  useDialogFocus(dialog);
  const dismiss = () => { ++generation.current; close(); };
  useBackAction(true, dismiss);
  useEffect(() => {
    const epoch = ++generation.current;
    if (!online) { setLoading(false); return; }
    setLoading(true); setError("");
    void api<{ id: string; groups?: typeof groups }>("GET", `/model-tests/${row.job_id}`).then(job => {
      if (epoch !== generation.current) return;
      if (job.id !== row.job_id) throw new Error("任务详情不匹配");
      setGroups(job.groups ?? []);
    }).catch(() => { if (epoch === generation.current) setError("任务详情暂不可用，已保留结果摘要。"); })
      .finally(() => { if (epoch === generation.current) setLoading(false); });
    return () => { ++generation.current; };
  }, [row.job_id, online]);
  return <div className="modal-backdrop" onClick={dismiss}><section ref={dialog} className="model-detection-dialog detection-log-detail" role="dialog" aria-modal="true" aria-label="自动检测日志详情" onClick={e => e.stopPropagation()} onKeyDown={e => { if (e.key === "Escape") { e.stopPropagation(); dismiss(); } }}>
    <header><div><span className="eyebrow">{row.account_name} #{row.account_id}</span><h2>自动检测日志</h2></div><button className="icon-button" aria-label="关闭日志详情" onClick={dismiss}><X size={19}/></button></header>
    <Facts row={row}/>{loading && <p className="muted">正在读取任务详情</p>}{error && <p role="status">{error}</p>}
    {!loading && !error && !groups.length && <p className="muted">历史详情不可用</p>}
    {!!groups.length && <ul>{groups.map(g => <li key={g.index}>第 {g.index} 组：{statuses[g.status] || ({ skipped: "已跳过", queued: "排队中", running: "运行中" } as Record<string, string>)[g.status] || "待核对"}{typeof g.duration_ms === "number" && ` · ${(g.duration_ms / 1000).toFixed(1)} 秒`}</li>)}</ul>}
  </section></div>;
}
export default function DetectionLogs({ active, online, connectionKey }: { active: boolean; online: boolean; connectionKey: string }) {
  const feed = useDetectionLogFeed(active, online, connectionKey);
  const [selected, setSelected] = useState<{ connectionKey: string; row: DetectionLog } | null>(null);
  function open(row: DetectionLog, trigger: HTMLElement) { trigger.focus({ preventScroll: true }); setSelected({ connectionKey, row }); }
  useEffect(() => { setSelected(null); }, [active, connectionKey]);
  return <><div className="section-heading"><span>自动检测结果与恢复 · {feed.items.length} 条记录</span><button className="text-button" disabled={!online || feed.loading} onClick={() => void feed.load()}>刷新记录</button></div>
    {feed.error && <p className="bad-text" role="alert">{feed.error} <button disabled={!online || feed.loading} onClick={() => void feed.load()}>重试</button></p>}
    <div className="detection-log-list" role="region" aria-label="自动检测日志表格" tabIndex={0}><table className="detection-log-table"><thead><tr>{["结果时间", "账号", "模型", "触发", "执行", "结论", "预测", "匹配度", "处置／恢复", "通知"].map(label => <th key={label} scope="col">{label}</th>)}</tr></thead><tbody>{feed.items.map(row => <tr className="detection-log-row" key={row.job_id} tabIndex={0} aria-label={`查看 ${row.account_name || `账号 #${row.account_id}`} 检测日志`} onClick={e => open(row, e.currentTarget)} onKeyDown={e => { if (e.target === e.currentTarget && ["Enter", " "].includes(e.key)) { e.preventDefault(); open(row, e.currentTarget); } }}>
      <td><time>{fullTime(row.created_at)}</time></td>
      <td><button className="detection-account" title={row.account_name} onClick={e => { e.stopPropagation(); open(row, e.currentTarget); }}><span>{row.account_name || `账号 #${row.account_id}`}</span><small>#{row.account_id}</small></button></td>
      <td><span className="detection-ellipsis" title={row.model_id}>{row.model_id}</span></td>
      <td><span className="detection-ellipsis" title={row.triggers.map(t => triggers[t] || "自动检测").join("、")}>{row.triggers.map(t => triggers[t] || "自动检测").join("、") || "自动检测"}</span><small className="muted">{engineName(row.summary.engine)} · {row.summary.sample_count ?? 3} 组</small></td>
      <td><Badge text={statuses[row.status] || "待核对"} tone={executionTone[row.status]}/></td>
      <td><Badge text={result(row)} tone={outcomeTone[row.summary.outcome]}/></td>
      <td><div className="detection-pair"><span>首组 <b title={row.summary.first_prediction}>{row.summary.first_prediction || "—"}</b></span><span>汇总 <b title={row.summary.prediction_name || row.summary.prediction}>{row.summary.prediction_name || row.summary.prediction || "—"}</b></span></div></td>
      <td><div className="detection-pair"><span>首组 <b>{probability(row.summary.first_probability)}</b></span><span>汇总 <b>{probability(row.summary.probability)}</b></span></div></td>
      <td><Receipts row={row}/></td>
      <td>{row.notification ? <div className="detection-notice"><Badge text={notices[row.notification.status] || "待核对"} tone={noticeTone[row.notification.status]} title={row.notification.reason}/>{row.notification.merged && <small>合并用卡通知</small>}</div> : <span className="muted">无通知记录</span>}</td>
    </tr>)}</tbody></table></div>
    {!feed.items.length && <p className="muted">{feed.loading ? "正在读取" : feed.error ? "记录暂不可用" : "暂无自动检测记录"}</p>}
    {feed.cursor !== null && <button className="load-more" disabled={!online || feed.loading} onClick={() => void feed.load("more")}>加载更早记录</button>}
    {active && selected?.connectionKey === connectionKey && <Detail key={`${connectionKey}:${selected.row.job_id}`} row={selected.row} online={online} close={() => setSelected(null)}/>}
  </>;
}
