import React, { useEffect, useMemo, useRef, useState } from "react";
import {
  ArrowDown,
  ArrowUp,
  Check,
  ChevronDown,
  Copy,
  CornerDownRight,
  Archive,
  SquarePen,
  ListFilter,
  RefreshCw,
  X,
} from "lucide-react";
import { fullTime, type Account, type Preferences } from "./types";
import { watchWindowFocus } from "./bridge";
import {
  cacheTokenCount,
  latency,
  latencyTone,
  modelRoute,
  money,
  normalizeRecordColumns,
  recordColumns,
  requestTypes,
  tokenCount,
  tokensPerSecond,
  type RecordOption,
  type RecordColumn,
  type UsageRecord,
} from "./records";
import { useRecordFeed } from "./useRecordFeed";
import RecordFilter from "./RecordFilter";
import { useBackAction } from "./mobile";
import "./records.css";

function Route({ row }: { row: UsageRecord }) {
  const route = modelRoute(row);
  return (
    <div className="record-route">
      {route.map((step, i) => (
        <div
          key={`${i}:${step.model}`}
          title={`${step.labels.join(" · ")}：${step.model}`}
        >
          {i > 0 && (
            <>
              <CornerDownRight size={12} />
              <small>
                {step.labels.includes("返回")
                  ? "返回"
                  : step.labels.includes("转发")
                    ? "转发"
                    : "映射"}
              </small>
            </>
          )}
          <span>{step.model}</span>
        </div>
      ))}
      {!route.length && "—"}
      {row.upstream_model_mismatch && (
        <span className="route-mismatch">返回差异</span>
      )}
    </div>
  );
}
function Reasoning({ row }: { row: UsageRecord }) {
  const requested = row.requested_reasoning_effort,
    actual = row.reasoning_effort;
  return (
    <div className="record-stack">
      <span>{requested || actual || "—"}</span>
      {requested && actual && requested !== actual && (
        <small title="实际推理强度">↳ {actual}</small>
      )}
    </div>
  );
}
function Latency({ row }: { row: UsageRecord }) {
  const first = latencyTone(row.first_token_ms, "first");
  const total = latencyTone(row.duration_ms, "total");
  const speed = tokensPerSecond(row);
  return (
    <div
      className="record-latency"
      style={
        {
          "--latency-first": `var(--record-${first})`,
          "--latency-total": `var(--record-${total})`,
        } as React.CSSProperties
      }
    >
      <i aria-hidden="true" />
      <div>
        <small>首字</small>
        <span className={`latency-${first}`}>
          {latency(row.first_token_ms)}
        </span>
        <small>总耗时</small>
        <span className={`latency-${total}`}>{latency(row.duration_ms)}</span>
        <small>TPS</small>
        <span className="record-tps">
          {speed == null ? (
            "—"
          ) : (
            <>
              {speed.toFixed(2)}
              <small> tok/s</small>
            </>
          )}
        </span>
      </div>
    </div>
  );
}
function Cell({ column, row }: { column: RecordColumn; row: UsageRecord }) {
  switch (column) {
    case "api_key":
      return (
        <div
          className="record-stack record-key"
          title={`${row.api_key_name || "API 密钥"} #${row.api_key_id}`}
        >
          <span className="record-ellipsis">
            {row.api_key_name || "API 密钥"}
          </span>
          <small>#{row.api_key_id}</small>
        </div>
      );
    case "account":
      return (
        <span
          className="record-ellipsis"
          title={`${row.account_name || "账户"} #${row.account_id}`}
        >
          {row.account_name || `#${row.account_id}`}
        </span>
      );
    case "model":
      return <Route row={row} />;
    case "reasoning":
      return <Reasoning row={row} />;
    case "tokens":
      return (
        <div className="record-tokens">
          <span title="输入 Token">
            <ArrowDown size={12} />
            {tokenCount(row.input_tokens)}
          </span>
          <span title="输出 Token">
            <ArrowUp size={12} />
            {tokenCount(row.output_tokens)}
          </span>
          <span
            className="record-cache-read"
            title={`缓存读取：${tokenCount(row.cache_read_tokens)}`}
          >
            <Archive size={13} />
            {cacheTokenCount(row.cache_read_tokens)}
          </span>
          <span
            className="record-cache-write"
            title={`缓存写入：${tokenCount(row.cache_creation_tokens)}`}
          >
            <SquarePen size={13} />
            {cacheTokenCount(row.cache_creation_tokens)}
          </span>
        </div>
      );
    case "cost":
      return (
        <div className="record-stack record-money">
          <span title="用户实扣">{money(row.actual_cost)}</span>
          <small title="账户费用">A {money(row.account_cost)}</small>
        </div>
      );
    case "latency":
      return <Latency row={row} />;
    case "user_agent":
      return (
        <span className="record-ellipsis" title={row.user_agent || undefined}>
          {row.user_agent || "—"}
        </span>
      );
    case "ip":
      return (
        <span className="record-ellipsis" title={row.ip_address || undefined}>
          {row.ip_address || "—"}
        </span>
      );
    case "endpoint":
      return (
        <div className="record-stack">
          <span>{row.inbound_endpoint || "—"}</span>
          <small>{row.upstream_endpoint || "—"}</small>
        </div>
      );
    case "group":
      return row.group_name || (row.group_id ? `#${row.group_id}` : "—");
    case "billing":
      return (
        { token: "Token", image: "图片", per_request: "按次" }[
          row.billing_mode || ""
        ] ||
        row.billing_mode ||
        "—"
      );
    case "request_id":
      return (
        <span className="record-ellipsis" title={row.request_id || undefined}>
          {row.request_id || "—"}
        </span>
      );
    case "upstream_id":
      return (
        <span
          className="record-ellipsis"
          title={row.upstream_request_id || undefined}
        >
          {row.upstream_request_id || "—"}
        </span>
      );
  }
}
const defaultFilters = {
  period: "24",
  from: "",
  to: "",
  account: "",
  user: null as RecordOption | null,
  key: null as RecordOption | null,
  model: "",
  mismatch: false,
};
function CopyValue({ value }: { value: string | null | undefined }) {
  const [status, setStatus] = useState("");
  return (
    <span className="record-copy">
      <span>{value || "—"}</span>
      {value && (
        <button
          className="icon-button"
          title={status || "复制"}
          aria-label={status || "复制"}
          onClick={() => {
            void (
              navigator.clipboard
                ? navigator.clipboard.writeText(value)
                : Promise.reject()
            ).then(
              () => setStatus("已复制"),
              () => setStatus("复制失败，请选中文本"),
            );
          }}
        >
          {status === "已复制" ? <Check size={13} /> : <Copy size={13} />}
        </button>
      )}
      {status.startsWith("复制失败") && <small role="status">{status}</small>}
    </span>
  );
}
function RecordDetail({
  row,
  error,
  close,
}: {
  row: UsageRecord | null;
  error: string;
  close: () => void;
}) {
  const panel = useRef<HTMLElement>(null);
  useBackAction(true, close);
  useEffect(() => {
    const before = document.activeElement as HTMLElement | null;
    panel.current?.querySelector<HTMLButtonElement>("button")?.focus();
    return () => before?.focus();
  }, []);
  const pair = (label: string, value: React.ReactNode) => (
    <React.Fragment key={label}>
      <dt>{label}</dt>
      <dd>{value ?? "—"}</dd>
    </React.Fragment>
  );
  const rawMoney = (value: string | null) =>
    value === null ? "—" : `$${value}`;
  return (
    <div
      className="drawer-backdrop"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) close();
      }}
    >
      <section
        ref={panel}
        className="drawer record-detail"
        role="dialog"
        aria-modal="true"
        aria-label="调用记录详情"
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            e.stopPropagation();
            close();
          }
          if (e.key === "Tab") {
            const nodes = [
              ...(panel.current?.querySelectorAll<HTMLElement>(
                'button:not(:disabled),[tabindex="0"]',
              ) || []),
            ];
            if (e.shiftKey && document.activeElement === nodes[0]) {
              e.preventDefault();
              nodes.at(-1)?.focus();
            } else if (!e.shiftKey && document.activeElement === nodes.at(-1)) {
              e.preventDefault();
              nodes[0]?.focus();
            }
          }
        }}
      >
        <header>
          <div>
            <h2>调用记录</h2>
            {row && (
              <span className="record-detail-time">
                #{row.id} · {fullTime(row.created_at)}
              </span>
            )}
          </div>
          <button className="icon-button" aria-label="关闭详情" onClick={close}>
            <X size={18} />
          </button>
        </header>
        {error ? (
          <p role="alert" className="records-error">
            {error}
          </p>
        ) : !row ? (
          <p role="status">正在读取记录…</p>
        ) : (
          <>
            <section className="record-detail-section">
              <h3>模型路由</h3>
              <Route row={row} />
              <dl>
                {pair(
                  "请求",
                  <CopyValue value={row.requested_model || row.model} />,
                )}
                {row.model_mapping_chain &&
                  pair("映射", <CopyValue value={row.model_mapping_chain} />)}
                {pair(
                  "转发",
                  <CopyValue
                    value={
                      row.upstream_model || row.requested_model || row.model
                    }
                  />,
                )}
                {pair(
                  "返回",
                  <CopyValue value={row.upstream_response_model} />,
                )}
                {pair("请求强度", row.requested_reasoning_effort || "—")}
                {pair("实际强度", row.reasoning_effort || "—")}
              </dl>
            </section>
            <section className="record-detail-section">
              <h3>请求</h3>
              <dl>
                {pair(
                  "用户",
                  `${row.user_name || row.user_email || "用户"} #${row.user_id}`,
                )}
                {pair(
                  "API 密钥",
                  `${row.api_key_name || "密钥"} #${row.api_key_id}`,
                )}
                {pair(
                  "账户",
                  `${row.account_name || "账户"} #${row.account_id}`,
                )}
                {pair(
                  "分组",
                  row.group_name || (row.group_id ? `#${row.group_id}` : "—"),
                )}
                {pair("类型", requestTypes[row.request_type])}
                {pair("服务等级", row.service_tier || "—")}
                {pair("入站端点", <CopyValue value={row.inbound_endpoint} />)}
                {pair("上游端点", <CopyValue value={row.upstream_endpoint} />)}
                {pair("请求 ID", <CopyValue value={row.request_id} />)}
                {pair("上游 ID", <CopyValue value={row.upstream_request_id} />)}
                {pair("User-Agent", <CopyValue value={row.user_agent} />)}
                {pair("IP", <CopyValue value={row.ip_address} />)}
                {pair("首字延迟", latency(row.first_token_ms))}
                {pair("总耗时", latency(row.duration_ms))}
                {pair(
                  "TPS",
                  tokensPerSecond(row) == null
                    ? "—"
                    : `${tokensPerSecond(row)!.toFixed(2)} tok/s`,
                )}
              </dl>
            </section>
            <section className="record-detail-section">
              <h3>Token 与费用</h3>
              <dl>
                {pair("输入", tokenCount(row.input_tokens))}
                {pair("输出", tokenCount(row.output_tokens))}
                {pair("缓存读取", tokenCount(row.cache_read_tokens))}
                {pair("缓存写入", tokenCount(row.cache_creation_tokens))}
                {!!row.cache_creation_5m_tokens &&
                  pair("5m 缓存", tokenCount(row.cache_creation_5m_tokens))}
                {!!row.cache_creation_1h_tokens &&
                  pair("1h 缓存", tokenCount(row.cache_creation_1h_tokens))}
                {pair("输入费用", rawMoney(row.input_cost))}
                {pair("输出费用", rawMoney(row.output_cost))}
                {pair("缓存读取费用", rawMoney(row.cache_read_cost))}
                {pair("缓存写入费用", rawMoney(row.cache_creation_cost))}
                {pair("标准费用", rawMoney(row.total_cost))}
                {pair("用户实扣", rawMoney(row.actual_cost))}
                {pair("账户费用", rawMoney(row.account_cost))}
                {pair("用户倍率", row.rate_multiplier ?? "—")}
                {pair("账户倍率", row.account_rate_multiplier ?? "1")}
                {pair("计费模式", <Cell column="billing" row={row} />)}
                {!!row.image_count && pair("图片", tokenCount(row.image_count))}
                {!!row.image_input_tokens &&
                  pair("图片输入 Token", tokenCount(row.image_input_tokens))}
                {!!row.image_output_tokens &&
                  pair("图片输出 Token", tokenCount(row.image_output_tokens))}
                {!!Number(row.image_input_cost) &&
                  pair("图片输入费用", rawMoney(row.image_input_cost))}
                {!!Number(row.image_output_cost) &&
                  pair("图片输出费用", rawMoney(row.image_output_cost))}
                {!!row.video_count &&
                  pair(
                    "视频",
                    `${row.video_count} · ${row.video_resolution || "—"} · ${row.video_duration_seconds ?? "—"}s`,
                  )}
              </dl>
            </section>
          </>
        )}
      </section>
    </div>
  );
}

export default function UsageRecords({
  online,
  foreground,
  desktop = true,
  accounts,
  columns,
  saveColumns,
}: {
  online: boolean;
  foreground: boolean;
  desktop?: boolean;
  accounts: Account[];
  columns: Preferences["record_columns"];
  saveColumns: (columns: string[]) => Promise<void>;
}) {
  const [draft, setDraft] = useState(defaultFilters),
    [filters, setFilters] = useState(defaultFilters);
  const [anchor, setAnchor] = useState(Date.now),
    [visible, setVisible] = useState(document.visibilityState !== "hidden");
  const [focused, setFocused] = useState(!desktop);
  const [scrolled, setScrolled] = useState(false),
    [selected, setSelected] = useState<number | null>(null);
  const [detail, setDetail] = useState<UsageRecord | null>(null),
    [detailError, setDetailError] = useState("");
  const [filterError, setFilterError] = useState(""),
    [columnError, setColumnError] = useState("");
  const [savingColumns, setSavingColumns] = useState(false);
  const scroll = useRef<HTMLDivElement>(null),
    menu = useRef<HTMLDetailsElement>(null),
    detailEpoch = useRef(0);
  const [knownAccounts, setKnownAccounts] = useState<Map<number, string>>(
    new Map(),
  );
  useEffect(() => {
    setFocused(!desktop);
    if (!desktop) return;
    let live = true;
    let unlisten: (() => void) | undefined;
    void watchWindowFocus((value) => {
      if (live) setFocused(value);
    }).then((stop) => {
      if (live) unlisten = stop;
      else stop();
    }).catch(() => {
      if (live) setFilterError("窗口状态读取失败，请重新打开记录页");
    });
    return () => {
      live = false;
      unlisten?.();
    };
  }, [desktop]);
  useEffect(() => {
    const visibility = () => setVisible(document.visibilityState !== "hidden");
    document.addEventListener("visibilitychange", visibility);
    const outsideMenu = (event: PointerEvent) => {
      if (menu.current?.open && !menu.current.contains(event.target as Node))
        menu.current.open = false;
    };
    document.addEventListener("pointerdown", outsideMenu);
    return () => {
      document.removeEventListener("visibilitychange", visibility);
      document.removeEventListener("pointerdown", outsideMenu);
    };
  }, []);
  const query = useMemo(() => {
    const p = new URLSearchParams({
      limit: "50",
      from_at:
        filters.period === "custom"
          ? new Date(`${filters.from}+08:00`).toISOString()
          : new Date(anchor - Number(filters.period) * 3600000).toISOString(),
    });
    if (filters.period === "custom" && filters.to)
      p.set("to_at", new Date(`${filters.to}+08:00`).toISOString());
    if (filters.account) p.set("account_id", filters.account);
    if (filters.user) p.set("user_id", String(filters.user.id));
    if (filters.key) p.set("api_key_id", String(filters.key.id));
    if (filters.model.trim()) p.set("model", filters.model.trim());
    if (filters.mismatch) p.set("mismatch_only", "true");
    return p.toString();
  }, [filters, anchor]);
  const active = online && foreground && visible && focused;
  const feed = useRecordFeed(query, active, scrolled || selected !== null);
  const chosen = normalizeRecordColumns(columns);
  const shown = recordColumns.filter(([key]) => chosen.includes(key));
  useEffect(() => {
    setKnownAccounts(
      (old) =>
        new Map([
          ...old,
          ...feed.items.map(
            (r) =>
              [r.account_id, r.account_name || `#${r.account_id}`] as const,
          ),
        ]),
    );
  }, [feed.items]);
  useEffect(() => {
    if (scroll.current) scroll.current.scrollTop = 0;
    setScrolled(false);
    ++detailEpoch.current;
    setSelected(null);
  }, [query]);
  useEffect(
    () => () => {
      ++detailEpoch.current;
    },
    [],
  );
  const closeDetail = () => {
    ++detailEpoch.current;
    setSelected(null);
    setDetail(null);
    setDetailError("");
  };
  async function openDetail(id: number) {
    const generation = ++detailEpoch.current;
    setSelected(id);
    setDetail(null);
    setDetailError("");
    try {
      const row = await feed.detail(id);
      if (detailEpoch.current === generation) setDetail(row);
    } catch (e) {
      if (detailEpoch.current === generation)
        setDetailError(String(e instanceof Error ? e.message : e));
    }
  }
  async function refresh() {
    if (await feed.refresh()) {
      if (scroll.current) scroll.current.scrollTop = 0;
      setScrolled(false);
    }
  }
  return (
    <section className="records-view" aria-label="调用记录">
      <form
        className="records-toolbar"
        onSubmit={(e) => {
          e.preventDefault();
          if (
            draft.period === "custom" &&
            (!draft.from ||
              !draft.to ||
              new Date(`${draft.from}+08:00`).getTime() >=
                new Date(`${draft.to}+08:00`).getTime())
          ) {
            setFilterError("请选择有效的北京时间范围");
            return;
          }
          setFilterError("");
          setAnchor(Date.now());
          setFilters({ ...draft });
        }}
      >
        <select
          aria-label="时间范围"
          value={draft.period}
          onChange={(e) => setDraft({ ...draft, period: e.target.value })}
        >
          <option value="24">最近 24 小时</option>
          <option value="168">最近 7 天</option>
          <option value="custom">自定时间</option>
        </select>
        <RecordFilter
          kind="users"
          active={active}
          value={draft.user}
          onChange={(user) => setDraft({ ...draft, user, key: null })}
        />
        <select
          aria-label="账户筛选"
          value={draft.account}
          onChange={(e) => setDraft({ ...draft, account: e.target.value })}
        >
          <option value="">全部账户</option>
          {[
            ...new Map([
              ...knownAccounts,
              ...accounts.map((a) => [a.id, a.name] as const),
            ]),
          ].map(([id, name]) => (
            <option key={id} value={id}>
              {name} #{id}
            </option>
          ))}
        </select>
        <RecordFilter
          kind="api_keys"
          active={active}
          value={draft.key}
          userId={draft.user?.id}
          onChange={(key) => setDraft({ ...draft, key })}
        />
        <input
          className="records-model-filter"
          aria-label="模型筛选"
          placeholder="搜索模型"
          maxLength={200}
          value={draft.model}
          onChange={(e) => setDraft({ ...draft, model: e.target.value })}
        />
        <label className="records-mismatch-filter">
          <input
            type="checkbox"
            checked={draft.mismatch}
            onChange={(e) => setDraft({ ...draft, mismatch: e.target.checked })}
          />
          返回差异
        </label>
        <button type="submit" disabled={!active || feed.loading}>
          筛选
        </button>
        {draft.period === "custom" && (
          <div className="records-date-range">
            <input
              aria-label="开始时间（北京时间）"
              type="datetime-local"
              step="1"
              value={draft.from}
              onChange={(e) => setDraft({ ...draft, from: e.target.value })}
            />
            <span>至</span>
            <input
              aria-label="结束时间（北京时间）"
              type="datetime-local"
              step="1"
              value={draft.to}
              onChange={(e) => setDraft({ ...draft, to: e.target.value })}
            />
          </div>
        )}
      </form>
      <div className="records-actions">
        <span className="records-status">
          {feed.items.length ? `${feed.items.length} 条` : ""}
          {feed.observed && <time>更新于 {fullTime(feed.observed)}</time>}
        </span>
        {feed.newCount > 0 && (
          <button
            className="new-records"
            disabled={!active || feed.loading}
            onClick={() => void refresh()}
          >
            {feed.newCount} 条新记录
          </button>
        )}
        <button
          className="icon-button"
          title="刷新记录"
          aria-label="刷新记录"
          disabled={!active || feed.loading}
          onClick={() => void refresh()}
        >
          <RefreshCw size={15} className={feed.loading ? "spinning" : ""} />
        </button>
        <details
          className="records-column-menu"
          ref={menu}
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              e.currentTarget.open = false;
              e.stopPropagation();
            }
          }}
        >
          <summary>
            <ListFilter size={14} />列<ChevronDown size={12} />
          </summary>
          <div className="records-column-options">
            {recordColumns.map(([key, label]) => (
              <label key={key}>
                <span>{label}</span>
                <input
                  type="checkbox"
                  checked={chosen.includes(key)}
                  disabled={savingColumns}
                  onChange={(e) => {
                    setColumnError("");
                    setSavingColumns(true);
                    void saveColumns(
                      e.target.checked
                        ? [...chosen, key]
                        : chosen.filter((v) => v !== key),
                    )
                      .catch((e) => setColumnError(String(e)))
                      .finally(() => setSavingColumns(false));
                  }}
                />
              </label>
            ))}
          </div>
        </details>
      </div>
      {(feed.error || filterError || columnError) && (
        <div className="records-error" role="alert">
          {filterError || columnError || feed.error}
        </div>
      )}
      {!online && (
        <div className="records-offline" role="status">
          连接已断开，显示已读取记录
        </div>
      )}
      <div
        className="records-scroll"
        ref={scroll}
        onScroll={(e) => setScrolled(e.currentTarget.scrollTop > 8)}
      >
        <table className="records-table">
          <thead>
            <tr>
              <th className="record-user">用户</th>
              {shown.map(([key, label]) => (
                <th key={key} className={`record-col-${key}`}>
                  {label}
                </th>
              ))}
              <th className="record-time">时间</th>
            </tr>
          </thead>
          <tbody>
            {feed.items.map((row) => (
              <tr
                key={row.id}
                onClick={() => {
                  if (active) void openDetail(row.id);
                }}
              >
                <td className="record-user">
                  <button
                    disabled={!active}
                    title={`查看记录 #${row.id}`}
                    onClick={(e) => {
                      e.stopPropagation();
                      void openDetail(row.id);
                    }}
                  >
                    <span>
                      {row.user_name || row.user_email || `#${row.user_id}`}
                    </span>
                    <small>#{row.user_id}</small>
                  </button>
                </td>
                {shown.map(([key]) => (
                  <td key={key} className={`record-col-${key}`}>
                    <Cell column={key} row={row} />
                  </td>
                ))}
                <td className="record-time">
                  <time>{fullTime(row.created_at)}</time>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
        {!feed.items.length && (
          <div className="records-empty" role="status">
            {feed.loading
              ? "正在读取记录…"
              : feed.error
                ? "记录暂不可用"
                : "暂无记录"}
          </div>
        )}
        {feed.cursor && (
          <div className="records-pagination">
            <button
              disabled={!active || feed.loading}
              onClick={() => void feed.more()}
            >
              {feed.loading ? "正在读取…" : "加载更多"}
            </button>
          </div>
        )}
      </div>
      {selected !== null && (
        <RecordDetail row={detail} error={detailError} close={closeDetail} />
      )}
    </section>
  );
}
