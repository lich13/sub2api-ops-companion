import { useEffect, useState } from "react";
import { RefreshCw, X } from "lucide-react";
import { api } from "./bridge";
import {
  fullTime,
  type Account,
  type Quality,
  type QualityDetail,
  type QualityMetric,
} from "./types";

const num = (value: number | null | undefined, suffix = "") =>
  value == null ? "—" : `${value.toFixed(1)}${suffix}`;
const percent = (value: number | null | undefined) =>
  value == null ? "—" : `${(value * 100).toFixed(1)}%`;
const mode = (value: string) =>
  value === "70/30"
    ? "近 24h 70% / 此前 6d 30%"
    : value === "24h"
      ? "近 24h"
      : "近 7d";

export function QualityBadge({
  value,
  onClick,
}: {
  value?: Quality;
  onClick: () => void;
}) {
  return (
    <button
      className={`quality-badge ${value?.grade ?? "yellow"}`}
      onClick={onClick}
      title={
        value?.data_status === "delayed"
          ? `计算延迟 · ${fullTime(value.computed_at)}`
          : `计算时间 ${fullTime(value?.computed_at)}`
      }
    >
      <b>{value?.score ?? "—"}</b>
      <span>{value?.reasons.join(" · ") ?? "待积累"}</span>
      {value?.data_status === "delayed" && (
        <span aria-label="计算延迟">⌛</span>
      )}
    </button>
  );
}

function Metric({
  label,
  value,
  unit,
  tail,
}: {
  label: string;
  value: QualityMetric;
  unit: string;
  tail: string;
}) {
  return (
    <section className="quality-metric">
      <h3>
        {label}
        <strong>
          {num(value.score)}
          <small> / 100</small>
        </strong>
      </h3>
      <dl>
        <div>
          <dt>P50</dt>
          <dd>{num(value.p50, unit)}</dd>
        </div>
        <div>
          <dt>{tail}</dt>
          <dd>{num(value.tail, unit)}</dd>
        </div>
        <div>
          <dt>有效样本</dt>
          <dd>{value.samples}</dd>
        </div>
        <div>
          <dt>可比覆盖</dt>
          <dd>{percent(value.coverage)}</dd>
        </div>
      </dl>
      <p className="quality-period">{mode(value.mode)}</p>
      <details>
        <summary>同类基准</summary>
        {(value.mode === "70/30"
          ? ([
              ["近 24h", value.recent],
              ["此前 6d", value.history],
            ] as const)
          : ([
              [
                mode(value.mode),
                value.mode === "24h" ? value.recent : value.all,
              ],
            ] as const)
        ).map(([period, window]) => (
          <div key={period}>
            <h4>{period}</h4>
            {window.cohorts.length === 0 ? (
              <p>暂无可比基准</p>
            ) : (
              window.cohorts.map((c, i) => (
                <div className="quality-cohort" key={i}>
                  <strong>{c.model}</strong>
                  <span>
                    {c.reasoning_effort} · {c.service_tier} · {c.transport} ·{" "}
                    {c.input_bucket}
                  </span>
                  <span>
                    P50 {num(c.p50, unit)} / 基准 {num(c.baseline_p50, unit)}
                  </span>
                  <span>
                    {tail} {num(c.tail, unit)} / 基准{" "}
                    {num(c.baseline_tail, unit)}
                  </span>
                  <span>
                    {c.samples} 条 · 基准 {c.baseline_accounts} 个账号 /{" "}
                    {c.baseline_samples} 条
                  </span>
                </div>
              ))
            )}
          </div>
        ))}
      </details>
    </section>
  );
}

export default function QualityDialog({
  account,
  onClose,
}: {
  account: Account;
  onClose: () => void;
}) {
  const [value, setValue] = useState<QualityDetail | null>(null),
    [error, setError] = useState(""),
    [busy, setBusy] = useState(false);
  const [refresh, setRefresh] = useState(0);
  useEffect(() => {
    let cancelled = false;
    setBusy(true);
    api<QualityDetail>("GET", `/accounts/${account.id}/quality`)
      .then((result) => {
        if (!cancelled) {
          setValue(result);
          setError("");
        }
      })
      .catch((e) => {
        if (!cancelled) setError(String(e));
      })
      .finally(() => {
        if (!cancelled) setBusy(false);
      });
    return () => {
      cancelled = true;
    };
  }, [account.id, refresh]);
  useEffect(() => {
    const key = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  }, [onClose]);
  const quality = value ?? account.quality;
  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div
        className="quality-dialog"
        role="dialog"
        aria-modal="true"
        aria-label="账号质量明细"
        onClick={(e) => e.stopPropagation()}
      >
        <header>
          <div>
            <h2>账号质量</h2>
            <strong title={account.name}>{account.name}</strong>
          </div>
          <div className="quality-tools">
            <button
              aria-label="刷新评分"
              disabled={busy}
              onClick={() => setRefresh((n) => n + 1)}
            >
              <RefreshCw size={16} className={busy ? "spin" : ""} />
            </button>
            <button aria-label="关闭评分" onClick={onClose}>
              <X size={18} />
            </button>
          </div>
        </header>
        {error && (
          <p role="alert" className="danger-text">
            {error}
          </p>
        )}
        <div className={`quality-headline ${quality?.grade ?? "yellow"}`}>
          <b>{quality?.score ?? "—"}</b>
          <span>{quality?.reasons.join(" · ") ?? "计算中"}</span>
        </div>
        {quality && quality.data_status !== "fresh" && (
          <p>
            {quality?.data_status === "stale" ? "数据过期" : "计算延迟"} ·{" "}
            {fullTime(quality?.computed_at)}
          </p>
        )}
        {value?.reliability && (
          <>
            <div className="quality-metrics">
              <section className="quality-metric">
                <h3>
                  错误表现{" "}
                  <strong>
                    {num(value.reliability.score)}
                    <small> / 100</small>
                  </strong>
                </h3>
                <dl>
                  <div>
                    <dt>7d 可观测失败率</dt>
                    <dd>{percent(value.reliability.rate)}</dd>
                  </div>
                  <div>
                    <dt>计分失败率</dt>
                    <dd>{percent(value.reliability.effective_rate)}</dd>
                  </div>
                  <div>
                    <dt>成功调用</dt>
                    <dd>{value.reliability.successes}</dd>
                  </div>
                  <div>
                    <dt>失败账号记录</dt>
                    <dd>{value.reliability.failures}</dd>
                  </div>
                  <div>
                    <dt>总有效记录</dt>
                    <dd>{value.reliability.total}</dd>
                  </div>
                </dl>
                <p className="quality-period">{mode(value.reliability.mode)}</p>
              </section>
              {value.ttft && (
                <Metric
                  label="首字表现"
                  value={value.ttft}
                  unit="s"
                  tail="P90"
                />
              )}
              {value.throughput && (
                <Metric
                  label="输出速率"
                  value={value.throughput}
                  unit=" t/s"
                  tail="P10"
                />
              )}
            </div>
            <details className="quality-rules">
              <summary>评分口径</summary>
              <p>
                错误 50% · 首字 25% · 速率 25%；绿色 ≥85，黄色 60–84，红色
                &lt;60。
              </p>
              <p>
                失败率为失败账号记录 ÷（成功调用数 +
                失败账号记录），不是最终业务请求失败率。重试成功保留原账号失败，明确额度耗尽不扣分。
              </p>
              <p>
                同平台、模型、推理档位、服务档位及传输方式比较。样本不足时仅放宽输入规模；OAuth
                与 Key 使用相同标准。
              </p>
              <p>
                数字分要求 20 条有效记录、每项性能 10 条样本、60%
                可比覆盖；严重故障仍可标红。近 24h 样本达标时占 70%，此前 6d 占
                30%。
              </p>
              {value.cap != null && value.cap < 100 && (
                <p>
                  本次分数上限 {value.cap} · 近期连续失败{" "}
                  {value.consecutive_failures ?? 0} 次
                </p>
              )}
            </details>
          </>
        )}
        <footer>
          <span>计算 {fullTime(quality?.computed_at)}</span>
          {value?.period && (
            <span>
              {fullTime(value.period.start)} — {fullTime(value.period.end)}
            </span>
          )}
        </footer>
      </div>
    </div>
  );
}
