import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "./bridge";
import { errorMessage } from "./bridgeError";
import type {
  DetectionLog as DetectionLogRecord,
  DetectionLogNotification,
  DetectionLogPageReply,
  DetectionLogSummary,
  DetectionReceipt as DetectionReceiptRecord,
} from "./protocol";
import type { WireNull } from "./types";

type DetectionReceipt = WireNull<
  DetectionReceiptRecord,
  "marked" | "schedule_verified" | "mark_cleared" | "completed_at",
  "basis"
>;
export type DetectionLog = Omit<
  WireNull<DetectionLogRecord, "started_at" | "completed_at">,
  "summary" | "notification"
> & {
  summary: Omit<
    WireNull<
      DetectionLogSummary,
      | "probability"
      | "used_outputs"
      | "first_downgrade"
      | "aggregate_downgrade",
      "first_probability"
    >,
    "bank_version" | "disposition" | "recovery" | "verification" | "restoration"
  > & {
    bank_version?: { revision?: string };
    disposition: DetectionReceipt | null;
    recovery: DetectionReceipt | null;
    verification?: DetectionReceipt | null;
    restoration?: DetectionReceipt | null;
  };
  notification: WireNull<DetectionLogNotification, "delivered_at"> | null;
};
type DetectionLogPage = Omit<
  WireNull<DetectionLogPageReply, "next_cursor">,
  "items"
> & { items: DetectionLog[] };

export function useDetectionLogFeed(
  active: boolean,
  online: boolean,
  connectionKey: string,
) {
  const [items, setItems] = useState<DetectionLog[]>([]),
    [cursor, setCursor] = useState<string | null>(null);
  const [loading, setLoading] = useState(false),
    [error, setError] = useState("");
  const feedConnection = useRef(connectionKey);
  const generation = useRef(0),
    cursorRef = useRef<string | null>(null);
  const current = useRef({ active, online, connectionKey });
  current.current = { active, online, connectionKey };
  const load = useCallback(
    (mode: "replace" | "more" = "replace") => {
      if (!current.current.active || !current.current.online)
        return Promise.resolve(false);
      const before = mode === "more" ? cursorRef.current : null;
      if (mode === "more" && before === null) return Promise.resolve(false);
      const epoch = ++generation.current;
      setLoading(true);
      setError("");
      const valid = () =>
        epoch === generation.current &&
        current.current.active &&
        current.current.online &&
        current.current.connectionKey === connectionKey;
      const execute = async () => {
        if (!valid()) return false;
        try {
          const response = await api<DetectionLogPage>(
            "GET",
            `/detection-logs?limit=50${before === null ? "" : `&before=${encodeURIComponent(before)}`}`,
          );
          if (!valid()) return false;
          // Preserve server ordering, including timestamps more precise than JS milliseconds.
          setItems((old) => [
            ...new Map(
              (mode === "more"
                ? [...old, ...response.items]
                : response.items
              ).map((item) => [item.job_id, item]),
            ).values(),
          ]);
          cursorRef.current = response.next_cursor;
          setCursor(response.next_cursor);
          return true;
        } catch (reason) {
          if (valid()) setError(errorMessage(reason));
          return false;
        } finally {
          if (valid()) setLoading(false);
        }
      };
      return execute();
    },
    [connectionKey],
  );
  useEffect(() => {
    feedConnection.current = connectionKey;
    setItems([]);
    setCursor(null);
    cursorRef.current = null;
    setError("");
  }, [connectionKey]);
  useEffect(() => {
    if (active && online) void load();
    else setLoading(false);
    return () => {
      generation.current = generation.current + 1;
    };
  }, [active, online, connectionKey, load]);
  return {
    items: feedConnection.current === connectionKey ? items : [],
    cursor: feedConnection.current === connectionKey ? cursor : null,
    loading,
    error: feedConnection.current === connectionKey ? error : "",
    load,
  };
}
