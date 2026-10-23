import { useEffect, useId, useLayoutEffect, useRef, useState, type PointerEvent as ReactPointerEvent } from "react";
import { createPortal } from "react-dom";
import { Info } from "lucide-react";
import { money, type UsageRecord } from "./records";

const number = (value: string | number | null | undefined) =>
  value == null || value === "" || !Number.isFinite(Number(value)) ? null : Number(value);
const amount = (value: string | number | null | undefined) => {
  return value == null ? "—" : money(String(value), 8);
};
const multiplier = (value: string | null) => {
  const n = number(value);
  return n == null ? "—" : `${n.toFixed(2)}x`;
};

export function costDetails(row: UsageRecord): [string, string][] {
  const rows: [string, string][] = [
    ["输入费用", amount(row.input_cost)], ["输出费用", amount(row.output_cost)],
  ];
  const price = (label: string, cost: string | null, tokens: number | null) => {
    const n = number(cost);
    if (tokens != null && tokens > 0) rows.push([label, n == null ? "—" : `$${(n / tokens * 1e6).toFixed(4)} / 1M Token`]);
  };
  const imageBilling = (row.image_count ?? 0) > 0 && row.billing_mode !== "token" && row.billing_mode !== "video";
  if (!imageBilling && (!row.billing_mode || row.billing_mode === "token")) {
    price("输入单价", row.input_cost, row.input_tokens == null ? null : Math.max(0, row.input_tokens - (row.image_input_tokens ?? 0)));
    price("输出单价", row.output_cost, row.output_tokens == null ? null : Math.max(0, row.output_tokens - (row.image_output_tokens ?? 0)));
    if ((row.image_input_tokens ?? 0) > 0) {
      rows.push(["图片输入费用", amount(row.image_input_cost)]);
      price("图片输入单价", row.image_input_cost, row.image_input_tokens);
    }
    if ((row.image_output_tokens ?? 0) > 0) {
      rows.push(["图片输出费用", amount(row.image_output_cost)]);
      price("图片输出单价", row.image_output_cost, row.image_output_tokens);
    }
  } else if (imageBilling || row.billing_mode === "image") {
    rows.push(["图片数量", row.image_count == null ? "—" : String(row.image_count)]);
    rows.push(["图片单价", (row.image_count ?? 0) > 0 && number(row.total_cost) != null ? amount(Number(row.total_cost) / row.image_count!) : "—"]);
  } else if (row.billing_mode === "per_request") rows.push(["每次费用", amount(row.total_cost)]);
  if (row.cache_read_cost != null) rows.push(["缓存读取费用", amount(row.cache_read_cost)]);
  if (row.cache_creation_cost != null) rows.push(["缓存写入费用", amount(row.cache_creation_cost)]);
  const tier = row.service_tier?.trim().toLowerCase() || "default";
  rows.push(
    ["服务档位", ({ default: "Standard", standard: "Standard", fast: "Priority", priority: "Priority", ultrafast: "Ultrafast", flex: "Flex", scale: "Scale" } as Record<string, string>)[tier] ?? tier],
    ["倍率", multiplier(row.rate_multiplier)], ["原始", amount(row.total_cost)],
    ["用户扣费", amount(row.actual_cost)], ["账号倍率", multiplier(row.account_rate_multiplier)],
    ["账号计费", amount(row.account_cost)],
  );
  return rows;
}

export default function CostDetails({ row }: { row: UsageRecord }) {
  const [open, setOpen] = useState(false);
  const [position, setPosition] = useState({ left: 0, top: 0 });
  const trigger = useRef<HTMLButtonElement>(null), panel = useRef<HTMLDivElement>(null);
  const pointer = useRef(false);
  const id = useId();
  const openFromMouse = (event: ReactPointerEvent<HTMLButtonElement>) => {
    if (event.pointerType !== "mouse") return;
    if (window.matchMedia && !window.matchMedia("(any-hover: hover)").matches) return;
    if (event.isTrusted) {
      const bounds = event.currentTarget.getBoundingClientRect();
      if (event.clientX < bounds.left || event.clientX > bounds.right || event.clientY < bounds.top || event.clientY > bounds.bottom) return;
    }
    setOpen(true);
  };
  useLayoutEffect(() => {
    if (!open || !trigger.current || !panel.current) return;
    const anchor = trigger.current.getBoundingClientRect(), bounds = panel.current.getBoundingClientRect();
    const left = anchor.right + 10 + bounds.width <= window.innerWidth - 8 ? anchor.right + 10 : anchor.left - bounds.width - 10;
    setPosition({ left: Math.max(8, Math.min(left, window.innerWidth - bounds.width - 8)), top: Math.max(8, Math.min(anchor.top, window.innerHeight - bounds.height - 8)) });
  }, [open, row]);
  useEffect(() => {
    if (!open) return;
    const close = () => setOpen(false);
    const outside = (e: Event) => { if (!trigger.current?.contains(e.target as Node) && !panel.current?.contains(e.target as Node)) close(); };
    const scroll = (e: Event) => { if (!panel.current?.contains(e.target as Node)) close(); };
    const key = (e: KeyboardEvent) => { if (e.key === "Escape") { e.stopPropagation(); close(); } };
    document.addEventListener("pointerdown", outside, true);
    document.addEventListener("click", outside, true);
    document.addEventListener("focusin", outside, true);
    document.addEventListener("keydown", key, true);
    window.addEventListener("resize", close);
    window.addEventListener("scroll", scroll, true);
    return () => {
      document.removeEventListener("pointerdown", outside, true);
      document.removeEventListener("click", outside, true);
      document.removeEventListener("focusin", outside, true);
      document.removeEventListener("keydown", key, true);
      window.removeEventListener("resize", close);
      window.removeEventListener("scroll", scroll, true);
    };
  }, [open]);
  return <>
    <button type="button" ref={trigger} className="record-stack record-money record-cost-trigger" aria-label="查看费用明细" aria-describedby={open ? id : undefined} aria-expanded={open}
      onPointerEnter={openFromMouse} onPointerLeave={e => { if (e.pointerType === "mouse") setOpen(false); }}
      onPointerDown={() => { pointer.current = true; }}
      onFocus={() => { if (!pointer.current) setOpen(true); }} onBlur={e => { pointer.current = false; if (!panel.current?.contains(e.relatedTarget as Node)) setOpen(false); }}
      onClick={e => { e.stopPropagation(); pointer.current = false; setOpen(value => !value); }}
      onKeyDown={e => e.stopPropagation()}>
      <span><span title="用户实扣">{money(row.actual_cost)}</span> <Info size={12}/></span><small title="账户费用">A {money(row.account_cost)}</small>
    </button>
    {open && createPortal(<div ref={panel} id={id} role="tooltip" tabIndex={-1} className="record-cost-tooltip" style={position} onBlur={e => { if (!trigger.current?.contains(e.relatedTarget as Node)) setOpen(false); }}>
      <strong>费用明细</strong><dl>{costDetails(row).map(([label, value]) => <div key={label} className={label === "服务档位" || label === "账号倍率" ? "cost-section" : undefined}><dt>{label}</dt><dd>{value}</dd></div>)}</dl>
    </div>, document.body)}
  </>;
}
