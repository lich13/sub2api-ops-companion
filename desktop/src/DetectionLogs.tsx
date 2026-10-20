import { useEffect, useRef, useState } from "react";
import { X } from "lucide-react";
import { api } from "./bridge";
import { fullTime } from "./types";
import { useBackAction } from "./mobile";
import { useDetectionLogFeed, type DetectionLog } from "./useDetectionLogFeed";
import "./detection-logs.css";

const statuses: Record<string, string> = { completed: "已完成", failed: "失败", cancelled: "已取消", interrupted: "已中断", needs_confirmation: "待确认" };
const outcomes: Record<string, string> = { normal: "未确认降智", degraded: "首组判定降智", aggregate_degraded: "汇总符合降智特征", unavailable: "历史详情不可用" };
const triggers: Record<string, string> = { scheduled: "定时检测", recheck: "降智复查", error: "错误触发", slow: "慢首字", recovery: "额度恢复验证", credit: "用卡恢复验证" };
const notices: Record<string, string> = { pending: "待推送", queued: "待推送", retry: "待重试", delivered: "已推送", suppressed: "已取消", failed: "投递失败" };
const result = (row: DetectionLog) => outcomes[row.summary.outcome] || statuses[row.status] || "结果待核对";
function Facts({ row }: { row: DetectionLog }) {
  const { summary, notification } = row;
  return <dl className="detection-meta">
    <dt>请求模型</dt><dd>{row.model_id}</dd><dt>触发</dt><dd>{row.triggers.map(t => triggers[t] || "自动检测").join("、") || "自动检测"}</dd>
    <dt>执行状态</dt><dd>{statuses[row.status] || "待核对"}</dd><dt>检测结论</dt><dd>{result(row)}</dd>
    <dt>首组预测</dt><dd>{summary.first_prediction || "暂无记录"}</dd><dt>汇总预测</dt><dd>{summary.prediction || "暂无记录"}</dd>
    <dt>汇总匹配度</dt><dd>{summary.probability === null ? "暂无记录" : `${(summary.probability * 100).toFixed(1)}%`}</dd>
    {typeof summary.first_probability === "number" && <><dt>首组匹配度</dt><dd>{(summary.first_probability * 100).toFixed(1)}%</dd></>}
    {summary.disposition && <><dt>降智处置</dt><dd>{summary.disposition.marked ? "标记已保存" : "标记待核对"} · {summary.disposition.schedule_verified ? "停调度已确认" : "停调度待核对"}{summary.disposition.reason && ` · ${summary.disposition.reason}`}</dd></>}
    {summary.recovery && <><dt>复查恢复</dt><dd>{summary.recovery.mark_cleared ? "已取消降智标记 · 调度保持不变" : summary.recovery.reason || "恢复待核对"}</dd></>}
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
  return <div className="modal-backdrop" onClick={dismiss}><section className="model-detection-dialog detection-log-detail" role="dialog" aria-modal="true" aria-label="自动检测日志详情" onClick={e => e.stopPropagation()}>
    <header><div><span className="eyebrow">{row.account_name} #{row.account_id}</span><h2>自动检测日志</h2></div><button className="icon-button" aria-label="关闭日志详情" onClick={dismiss}><X size={19}/></button></header>
    <Facts row={row}/>{loading && <p className="muted">正在读取任务详情</p>}{error && <p role="status">{error}</p>}
    {!loading && !error && !groups.length && <p className="muted">历史详情不可用</p>}
    {!!groups.length && <ul>{groups.map(g => <li key={g.index}>第 {g.index} 组：{statuses[g.status] || ({ skipped: "已跳过", queued: "排队中", running: "运行中" } as Record<string, string>)[g.status] || "待核对"}{typeof g.duration_ms === "number" && ` · ${(g.duration_ms / 1000).toFixed(1)} 秒`}</li>)}</ul>}
  </section></div>;
}
export default function DetectionLogs({ active, online, connectionKey }: { active: boolean; online: boolean; connectionKey: string }) {
  const feed = useDetectionLogFeed(active, online, connectionKey);
  const [selected, setSelected] = useState<{ connectionKey: string; row: DetectionLog } | null>(null);
  useEffect(() => { setSelected(null); }, [active, connectionKey]);
  return <><div className="section-heading"><span>自动检测结果与恢复 · {feed.items.length} 条记录</span><button className="text-button" disabled={!online || feed.loading} onClick={() => void feed.load()}>刷新记录</button></div>
    {feed.error && <p className="bad-text" role="alert">{feed.error} <button disabled={!online || feed.loading} onClick={() => void feed.load()}>重试</button></p>}
    <div className="detection-log-list">{feed.items.map(row => <button className="detection-log-row" key={row.job_id} onClick={() => setSelected({ connectionKey, row })}>
      <span><strong>{row.account_name || `账号 #${row.account_id}`}</strong><small>{fullTime(row.created_at)}</small></span>
      <span>{result(row)} · {row.model_id}</span><small>{row.triggers.map(t => triggers[t] || "自动检测").join("、")}{row.summary.verification?.status === "passed" ? " · 验证通过" : ""}{row.summary.recovery?.mark_cleared ? " · 已取消降智标记" : ""}{row.summary.restoration?.status === "restored" ? " · 已恢复调度" : ""}{row.notification ? ` · ${notices[row.notification.status] || "通知待核对"}${row.notification.merged ? "（合并用卡通知）" : ""}` : ""}</small>
    </button>)}</div>
    {!feed.items.length && <p className="muted">{feed.loading ? "正在读取" : feed.error ? "记录暂不可用" : "暂无自动检测记录"}</p>}
    {feed.cursor !== null && <button className="load-more" disabled={!online || feed.loading} onClick={() => void feed.load("more")}>加载更早记录</button>}
    {active && selected?.connectionKey === connectionKey && <Detail key={`${connectionKey}:${selected.row.job_id}`} row={selected.row} online={online} close={() => setSelected(null)}/>}
  </>;
}
