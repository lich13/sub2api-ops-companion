import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "./bridge";
import { errorMessage } from "./bridgeError";
import type { OpsError } from "./types";

type ErrorsPage = { items: OpsError[]; next_cursor: number | null };

export type ErrorCategory = "degradation" | "other";

const EMPTY_ERRORS: OpsError[] = [];

/** Each category owns its cursor and generation; snapshots never seed history. */
export function useErrorFeed(
  category: ErrorCategory,
  active: boolean,
  online: boolean,
  connectionKey: string,
) {
  const [feed, setFeed] = useState<{
    key: string;
    items: OpsError[];
    cursor: number | null;
    error: string;
  }>({ key: connectionKey, items: EMPTY_ERRORS, cursor: null, error: "" });
  const [loading, setLoading] = useState(false);
  const cursorState = useRef<{ key: string; cursor: number | null }>({
    key: connectionKey,
    cursor: null,
  });
  const generation = useRef(0);
  const controller = useRef<AbortController | null>(null);
  const queue = useRef<Promise<unknown>>(Promise.resolve());
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
      controller.current?.abort();
      const request = new AbortController();
      controller.current = request;
      setLoading(true);
      setFeed((prev) =>
        prev.key === connectionKey
          ? prev.error
            ? { ...prev, error: "" }
            : prev
          : {
              key: connectionKey,
              items: EMPTY_ERRORS,
              cursor: null,
              error: "",
            },
      );
      const valid = () =>
        !request.signal.aborted &&
        epoch === generation.current &&
        current.current.active &&
        current.current.online &&
        current.current.connectionKey === connectionKey;
      const execute = async () => {
        if (!valid()) return false;
        try {
          const response = await api<ErrorsPage>(
            "GET",
            `/errors?category=${category}${before === null ? "" : `&before_id=${before}`}`,
          );
          if (!valid()) return false;
          cursorState.current = {
            key: connectionKey,
            cursor: response.next_cursor,
          };
          setFeed((prev) => ({
            key: connectionKey,
            items: [
              ...new Map(
                (mode === "more" && prev.key === connectionKey
                  ? [...prev.items, ...response.items]
                  : response.items
                ).map((item) => [item.id, item]),
              ).values(),
            ].sort((a, b) => b.id - a.id),
            cursor: response.next_cursor,
            error: "",
          }));
          return true;
        } catch (reason) {
          if (valid()) {
            setFeed((prev) => ({
              key: connectionKey,
              items: prev.key === connectionKey ? prev.items : EMPTY_ERRORS,
              cursor: prev.key === connectionKey ? prev.cursor : null,
              error: errorMessage(reason),
            }));
          }
          return false;
        } finally {
          if (valid()) {
            setLoading(false);
            controller.current = null;
          }
        }
      };
      const next = queue.current.then(execute, execute);
      queue.current = next.catch(() => {});
      return next;
    },
    [category, connectionKey],
  );
  useEffect(() => {
    if (active && online) void load();
    else setLoading(false);
    return () => {
      generation.current = generation.current + 1;
      controller.current?.abort();
      controller.current = null;
    };
  }, [active, online, connectionKey, load]);
  const sameKey = feed.key === connectionKey;
  return {
    items: sameKey ? feed.items : EMPTY_ERRORS,
    cursor: sameKey ? feed.cursor : null,
    loading,
    error: sameKey ? feed.error : "",
    load,
  };
}
