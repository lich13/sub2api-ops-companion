import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "./bridge";
import type { OpsError } from "./types";

export type ErrorCategory = "degradation" | "other";

/** Each category owns its cursor and generation; snapshots never seed history. */
export function useErrorFeed(category: ErrorCategory, active: boolean, online: boolean, connectionKey: string) {
  const [items, setItems] = useState<OpsError[]>([]);
  const [cursor, setCursor] = useState<number | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const cursorRef = useRef<number | null>(null);
  const generation = useRef(0);
  const controller = useRef<AbortController | null>(null);
  const queue = useRef<Promise<unknown>>(Promise.resolve());
  const current = useRef({ active, online, connectionKey });
  current.current = { active, online, connectionKey };
  const load = useCallback((mode: "replace" | "more" = "replace") => {
    if (!current.current.active || !current.current.online) return Promise.resolve(false);
    const before = mode === "more" ? cursorRef.current : null;
    if (mode === "more" && before === null) return Promise.resolve(false);
    const epoch = ++generation.current;
    controller.current?.abort();
    const request = new AbortController();
    controller.current = request;
    setLoading(true);
    setError("");
    const valid = () => !request.signal.aborted && epoch === generation.current
      && current.current.active && current.current.online && current.current.connectionKey === connectionKey;
    const execute = async () => {
      if (!valid()) return false;
      try {
        const response = await api<{ items: OpsError[]; next_cursor: number | null }>(
          "GET", `/errors?category=${category}${before === null ? "" : `&before_id=${before}`}`,
        );
        if (!valid()) return false;
        setItems(old => [...new Map((mode === "more" ? [...old, ...response.items] : response.items)
          .map(item => [item.id, item])).values()].sort((a, b) => b.id - a.id));
        cursorRef.current = response.next_cursor;
        setCursor(response.next_cursor);
        return true;
      } catch (reason) {
        if (valid()) setError(String(reason).replace(/^Error: /, ""));
        return false;
      } finally {
        if (valid()) { setLoading(false); controller.current = null; }
      }
    };
    const next = queue.current.then(execute, execute);
    queue.current = next.catch(() => {});
    return next;
  }, [category, connectionKey]);
  useEffect(() => {
    setItems([]); setCursor(null); cursorRef.current = null; setError("");
  }, [connectionKey]);
  useEffect(() => {
    if (active && online) void load();
    else setLoading(false);
    return () => { ++generation.current; controller.current?.abort(); controller.current = null; };
  }, [active, online, connectionKey, load]);
  return { items, cursor, loading, error, load };
}
