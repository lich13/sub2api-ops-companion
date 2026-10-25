import { useState } from "react";
import type { ModelTestResult } from "./modelTestTypes";

const confidence = (value?: number | null) => value == null ? "—" : `${(value * 100).toFixed(1)}%`;
export function ModelFingerprintResult({ job }: { job: ModelTestResult }) {
  const [expanded, setExpanded] = useState(false);
  const report = job.report, candidates = report?.results ?? [];
  const label = report?.decision === "not_model" ? "序列异常，无法分析" : report?.decision === "unscorable" ? "有效样本不足，无法分析" : report?.prediction_name || "等待分析";
  return <div className="lm-fingerprint-result">
    <div className="lm-conclusion"><strong>{label}</strong><span className="muted">{report?.decision === "partial" ? "初步结果" : candidates.length ? "汇总结果" : ""}{report?.merged_class ? " · 合并类别，无法区分" : ""}</span></div>
    <dl><div><dt>有效采样</dt><dd>{report?.used_outputs ?? job.valid_groups}/{job.planned_groups ?? 1} 组</dd></div><div><dt>置信度</dt><dd>{confidence(report?.probability)}</dd></div></dl>
    {job.classification?.outcome === "unavailable" && <p className="muted">{job.classification.reason}</p>}
    {!!candidates.length && <><div className="lm-candidates" role="table" aria-label="模型候选排名">
      <div role="row" className="lm-candidate-heading"><span role="columnheader">候选模型</span><span role="columnheader">排名分数</span><span role="columnheader">置信度</span></div>
      {(expanded ? candidates : candidates.slice(0, 5)).map((candidate, index) => <div role="row" key={candidate.model}><span role="cell"><small>{index + 1}</small>{candidate.display_name || candidate.model}</span><span role="cell">{candidate.score.toFixed(4)}</span><span role="cell">{confidence(candidate.probability)}</span></div>)}
    </div>{candidates.length > 5 && <button className="text-button lm-expand" aria-expanded={expanded} onClick={() => setExpanded(!expanded)}>{expanded ? "收起候选" : `展开全部 ${candidates.length} 个候选`}</button>}</>}
  </div>;
}

export function TokenizerResult({ job }: { job: ModelTestResult }) {
  if (!job.tokenizer_probe) return null;
  const value = job.tokenizer, report = value?.report;
  const status = ({ running: "探测中", completed: "已完成", cancelled: "已停止", interrupted: "已中断", failed: "探测失败" } as Record<string, string>)[value?.status ?? ""] || (job.status === "running" || job.status === "queued" ? "等待数字采样完成" : "未执行");
  const claim = value?.baseline_drift ? "基线漂移，无法确认一致性" : ({ consistent: "与请求模型一致", inconsistent: "与请求模型不一致", uncertain: "无法确认一致性" } as Record<string, string>)[report?.claim?.status ?? ""] || "请求模型暂无分词器参照";
  return <section className="lm-tokenizer"><h3>分词器探测 <span>{status}</span></h3>
    <dl><div><dt>请求数</dt><dd>{value?.requests ?? 0} / 22</dd></div>{report && <><div><dt>候选</dt><dd>{report.kind === "unknown" ? "未知分词器" : report.name}{report.kind === "related" ? "（相近）" : ""}</dd></div><div><dt>一致性</dt><dd>{claim}</dd></div></>}</dl>
    {value?.baseline_drift && <p className="bad-text">两次基线的输入 Token 用量不一致，分词器结果仅供参考。</p>}
    {value?.error && <p className="bad-text">{value.error}</p>}
    {!!report?.candidates.length && <details><summary>分词器候选</summary><ul>{report.candidates.map(c => <li key={c.id}><span>{c.name}</span><span>{confidence(c.probability)}</span></li>)}</ul></details>}
  </section>;
}
