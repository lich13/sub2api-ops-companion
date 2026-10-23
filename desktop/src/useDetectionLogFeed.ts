import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "./bridge";

export type DetectionReceipt = { status: string; basis?: "first_group" | "aggregate" | null; marked: boolean | null; schedule_verified: boolean | null; mark_cleared: boolean | null; reason: string; completed_at: string | null };
export type DetectionLog = {
  job_id: string; account_id: number; account_name: string; model_id: string;
  status: string; created_at: string; started_at: string | null; completed_at: string | null;
  triggers: string[]; error: string; details_available: boolean;
  summary: { outcome: string; analysis_available: boolean; first_prediction: string; prediction: string;
    probability: number | null; first_probability?: number | null; used_outputs: number | null; first_downgrade: boolean | null; aggregate_downgrade: boolean | null;
    disposition: DetectionReceipt | null; recovery: DetectionReceipt | null; verification?: DetectionReceipt | null; restoration?: DetectionReceipt | null };
  notification: { status: string; merged: boolean; delivered_at: string | null; reason: string } | null;
};

export function useDetectionLogFeed(active: boolean, online: boolean, connectionKey: string) {
  const [items, setItems] = useState<DetectionLog[]>([]), [cursor, setCursor] = useState<string | null>(null);
  const [loading, setLoading] = useState(false), [error, setError] = useState("");
  const feedConnection = useRef(connectionKey);
  const generation = useRef(0), cursorRef = useRef<string | null>(null), supported = useRef<boolean | null>(null);
  const current = useRef({ active, online, connectionKey }); current.current = { active, online, connectionKey };
  const load = useCallback((mode: "replace" | "more" = "replace") => {
    if (!current.current.active || !current.current.online) return Promise.resolve(false);
    const before = mode === "more" ? cursorRef.current : null;
    if (mode === "more" && before === null) return Promise.resolve(false);
    const epoch = ++generation.current;
    setLoading(true); setError("");
    const valid = () => epoch === generation.current && current.current.active && current.current.online && current.current.connectionKey === connectionKey;
    const execute = async () => {
      if (!valid()) return false;
      try {
        if (supported.current !== true) {
          const capabilities = await api<{ features?: { detection_logs?: boolean } }>("GET", "/capabilities");
          if (!valid()) return false;
          supported.current = capabilities.features?.detection_logs === true;
        }
        if (!supported.current) throw new Error("请升级服务端以查看自动检测日志。");
        const response = await api<{ items: DetectionLog[]; next_cursor: string | null }>("GET", `/detection-logs?limit=50${before === null ? "" : `&before=${encodeURIComponent(before)}`}`);
        if (!valid()) return false;
        // Preserve server ordering, including timestamps more precise than JS milliseconds.
        setItems(old => [...new Map((mode === "more" ? [...old, ...response.items] : response.items).map(item => [item.job_id, item])).values()]);
        cursorRef.current = response.next_cursor; setCursor(response.next_cursor); return true;
      } catch (reason) { if (valid()) setError(String(reason).replace(/^Error: /, "")); return false; }
      finally { if (valid()) setLoading(false); }
    };
    return execute();
  }, [connectionKey]);
  useEffect(() => { feedConnection.current = connectionKey; setItems([]); setCursor(null); cursorRef.current = null; supported.current = null; setError(""); }, [connectionKey]);
  useEffect(() => {
    if (active && online) void load(); else setLoading(false);
    return () => { ++generation.current; };
  }, [active, online, connectionKey, load]);
  return { items: feedConnection.current === connectionKey ? items : [], cursor: feedConnection.current === connectionKey ? cursor : null,
    loading, error: feedConnection.current === connectionKey ? error : "", load };
}
