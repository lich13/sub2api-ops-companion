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

const EMPTY_LOGS: DetectionLog[] = [];

export function useDetectionLogFeed(
  active: boolean,
  online: boolean,
  connectionKey: string,
) {
  const [feed, setFeed] = useState<{
    key: string;
    items: DetectionLog[];
    cursor: string | null;
    error: string;
  }>({ key: connectionKey, items: EMPTY_LOGS, cursor: null, error: "" });
  const [loading, setLoading] = useState(false);
  const generation = useRef(0),
    cursorState = useRef<{ key: string; cursor: string | null }>({
      key: connectionKey,
      cursor: null,
    });
  const current = useRef({ active, online, connectionKey });
  current.current = { active, online, connectionKey };
  const load = useCallback(
    (mode: "replace" | "more" = "replace") => {
      if (!current.current.active || !current.current.online)
        return Promise.resolve(false);
      const before =
        mode === "more" && cursorState.current.key === connectionKey
          ? cursorState.current.cursor
          : null;
      if (mode === "more" && before === null) return Promise.resolve(false);
      const epoch = ++generation.current;
      setLoading(true);
      setFeed((prev) =>
        prev.key === connectionKey
          ? prev.error
            ? { ...prev, error: "" }
            : prev
          : { key: connectionKey, items: EMPTY_LOGS, cursor: null, error: "" },
      );
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
          cursorState.current = {
            key: connectionKey,
            cursor: response.next_cursor,
          };
          // Preserve server ordering, including timestamps more precise than JS milliseconds.
          setFeed((prev) => ({
            key: connectionKey,
            items: [
              ...new Map(
                (mode === "more" && prev.key === connectionKey
                  ? [...prev.items, ...response.items]
                  : response.items
                ).map((item) => [item.job_id, item]),
              ).values(),
            ],
            cursor: response.next_cursor,
            error: "",
          }));
          return true;
        } catch (reason) {
          if (valid()) {
            setFeed((prev) => ({
              key: connectionKey,
              items: prev.key === connectionKey ? prev.items : EMPTY_LOGS,
              cursor: prev.key === connectionKey ? prev.cursor : null,
              error: errorMessage(reason),
            }));
          }
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
    if (active && online) void load();
    else setLoading(false);
    return () => {
      generation.current = generation.current + 1;
    };
  }, [active, online, connectionKey, load]);
  const sameKey = feed.key === connectionKey;
  return {
    items: sameKey ? feed.items : EMPTY_LOGS,
    cursor: sameKey ? feed.cursor : null,
    loading,
    error: sameKey ? feed.error : "",
    load,
  };
}
