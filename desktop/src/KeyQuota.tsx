import { useEffect, useState } from "react";
import { RefreshCw, Settings2 } from "lucide-react";
import { fullTime, type KeyQuota, type QuotaPlan } from "./types";
import "./key-quota.css";
const number = (value: number) =>
  value.toLocaleString(undefined, { maximumFractionDigits: 3 });
const compact = (value: number | null) =>
  value == null
    ? "—"
    : value >= 1e9
      ? `${number(value / 1e9)}B`
      : value >= 1e6
        ? `${number(value / 1e6)}M`
        : value >= 1e3
          ? `${number(value / 1e3)}K`
          : number(value);
const time = (value: number) => fullTime(new Date(value * 1000).toISOString());
const amount = (p: QuotaPlan) =>
  p.unlimited
    ? "无限制"
    : p.remaining == null
      ? "未返回剩余额度"
      : `${number(p.remaining)} ${p.unit}`;
function quotaHeadline(quota: KeyQuota | undefined, busy: boolean) {
  const first = quota?.plans[0];
  const keyOnly = quota?.source === "newapi" && first?.name === "Key 额度";
  return first
    ? keyOnly
      ? first.unlimited ? "Key 配额无限制" : `Key 剩余 ${amount(first)}`
      : `${first.unlimited ? "" : "剩余 "}${amount(first)}`
    : quota?.key_status || (quota?.state === "unsupported" ? "不可查询" : quota?.state === "error" ? "查询失败" : busy ? "查询中…" : quota?.state === "ok" ? "余额未知" : "未查询");
}
export function KeyQuotaSummary({ quota, busy }: { quota?: KeyQuota; busy: boolean }) {
  const warning = quota?.stale || !!quota?.error;
  const detail = [
    quota?.source === "sub2api" ? "Sub2API" : quota?.source === "newapi" ? "New API" : "",
    ...(quota?.plans ?? []).map(plan => `${plan.name}：${amount(plan)}`),
    quota?.success_at != null ? `更新 ${time(quota.success_at)}` : "",
    quota?.error,
    quota?.retry_at ? `可重试 ${time(quota.retry_at)}` : "",
  ].filter(Boolean).join(" · ");
  return <div className={`mini-usage key-quota-compact${warning ? " warning" : ""}`} aria-label="上游余额" title={detail}>
    <span>{quotaHeadline(quota, busy)}{warning && " · 异常"}</span>
  </div>;
}
const samePlan = (a: QuotaPlan, b: QuotaPlan) =>
  a.name === b.name &&
  a.unit === b.unit &&
  a.remaining === b.remaining &&
  a.used === b.used &&
  a.total === b.total &&
  a.unlimited === b.unlimited &&
  a.reset_at === b.reset_at;
function Plans({ plans }: { plans: QuotaPlan[] }) {
  if (!plans.length) return null;
  const unit = plans.every((p) => p.unit === plans[0].unit)
    ? plans[0].unit
    : null;
  const value = (n: number | null, p: QuotaPlan) =>
    n == null ? "—" : `${number(n)}${unit ? "" : ` ${p.unit}`}`;
  return (
    <table className="key-quota-plans">
      <thead>
        <tr>
          <th>额度 {unit}</th>
          <th>剩余</th>
          <th>已用 / 上限</th>
        </tr>
      </thead>
      <tbody>
        {plans.map((p, i) => (
          <tr key={i}>
            <th scope="row">
              {p.name}
              {p.reset_at && <time>重置 {fullTime(p.reset_at)}</time>}
            </th>
            <td>{p.unlimited ? "无限制" : value(p.remaining, p)}</td>
            <td>
              {value(p.used, p)} / {p.unlimited ? "∞" : value(p.total, p)}
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}
export default function KeyQuotaView({
  quota,
  busy,
  online,
  refresh,
  settings,
}: {
  quota?: KeyQuota;
  busy: boolean;
  online: boolean;
  refresh: () => void;
  settings: () => void;
}) {
  const [clock, rerender] = useState(0);
  useEffect(() => {
    const remaining = (quota?.retry_at ?? 0) * 1000 - Date.now();
    if (remaining <= 0) return;
    const timer = setTimeout(() => rerender((n) => n + 1), Math.min(remaining, 2_147_483_647));
    return () => clearTimeout(timer);
  }, [quota?.retry_at, clock]);
  const keyOnly =
    quota?.source === "newapi" && quota.plans[0]?.name === "Key 额度";
  const first = quota?.plans[0];
  const headline = quotaHeadline(quota, busy);
  const subscriptions = quota?.subscriptions ?? [];
  const repeated =
    quota?.source === "sub2api" &&
    subscriptions.length === 1 &&
    subscriptions[0].plans.length > 0 &&
    first?.name === (subscriptions[0].name ?? "订阅");
  const plans = (quota?.plans ?? []).filter(
    (p, i) =>
      !repeated ||
      (i !== 0 && !subscriptions[0].plans.some((s) => samePlan(s, p))),
  );
  const expiry = quota?.expires_at_unix
    ? time(quota.expires_at_unix)
    : quota?.expires_at
      ? fullTime(quota.expires_at)
      : null;
  return (
    <section className="key-quota" aria-label="上游余额">
      <div className="key-quota-heading">
        <strong>{headline}</strong>
        <div>
          {quota?.source && (
            <span>{quota.source === "sub2api" ? "Sub2API" : "New API"}</span>
          )}
          <button
            className="usage-action"
            aria-label="刷新余额"
            disabled={
              !online ||
              busy ||
              !!(quota?.retry_at && quota.retry_at > Date.now() / 1000)
            }
            onClick={refresh}
          >
            <RefreshCw size={12} className={busy ? "spin" : ""} />
          </button>
          <button
            className="usage-action"
            aria-label="余额查询设置"
            disabled={!online || busy}
            onClick={settings}
          >
            <Settings2 size={12} />
          </button>
        </div>
      </div>
      {quota?.success_at != null && (
        <time className="key-quota-updated">更新 {time(quota.success_at)}</time>
      )}
      {keyOnly && <span className="usage-unknown">账户余额未开放</span>}
      {quota?.key_status && !!quota.plans.length && (
        <span className="usage-unknown">{quota.key_status}</span>
      )}
      {quota?.error && (
        <span className="key-quota-error" role="status">
          {quota.error}
          {quota.retry_at ? ` · ${time(quota.retry_at)} 后重试` : ""}
        </span>
      )}
      {subscriptions.map((s, i) => (
        <section className="key-quota-subscription" key={i}>
          <header>
            <b>{s.name || "未识别套餐"}</b>
            {s.status && s.status !== "active" && (
              <span>
                {(
                  {
                    expired: "已到期",
                    disabled: "已禁用",
                    canceled: "已取消",
                    cancelled: "已取消",
                  } as Record<string, string>
                )[s.status] || s.status}
              </span>
            )}
            {!s.status && s.active === false && <span>未生效</span>}
            {s.expires_at && <time>到期 {fullTime(s.expires_at)}</time>}
          </header>
          <Plans plans={s.plans} />
        </section>
      ))}
      <Plans plans={plans} />
      {expiry && <time>Key 到期 {expiry}</time>}
      {quota && (quota.today || quota.total_usage) && (
        <details className="key-quota-statistics">
          <summary>上游用量统计</summary>
          <table className="key-quota-plans">
            <thead>
              <tr>
                <th>时间</th>
                <th>请求</th>
                <th>Token</th>
                <th>费用</th>
              </tr>
            </thead>
            <tbody>
              {(
                [
                  ["今日", quota.today],
                  ["累计", quota.total_usage],
                ] as const
              ).map(
                ([label, v]) =>
                  v && (
                    <tr key={label}>
                      <th>{label}</th>
                      <td
                        title={
                          v.requests == null ? undefined : number(v.requests)
                        }
                      >
                        {compact(v.requests)}
                      </td>
                      <td
                        title={v.tokens == null ? undefined : number(v.tokens)}
                      >
                        {compact(v.tokens)}
                      </td>
                      <td>{v.cost == null ? "—" : number(v.cost)}</td>
                    </tr>
                  ),
              )}
            </tbody>
          </table>
        </details>
      )}
    </section>
  );
}
