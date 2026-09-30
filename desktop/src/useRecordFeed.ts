import { useEffect, useRef, useState } from "react";
import { api } from "./bridge";
import type { RecordPage, UsageRecord } from "./records";

type Feed = {
  items: UsageRecord[];
  cursor: string | null;
  latest: number | null;
  newCount: number;
  observed: string;
  totalCost: string | null;
  loading: boolean;
  error: string;
};
const empty = (): Feed => ({
  items: [],
  cursor: null,
  latest: null,
  newCount: 0,
  observed: "",
  totalCost: null,
  loading: false,
  error: "",
});
export function useRecordFeed(
  query: string,
  active: boolean,
  holding: boolean,
) {
  const [feed, setFeed] = useState<Feed>(empty);
  const current = useRef(feed),
    epoch = useRef(0),
    queue = useRef<Promise<unknown>>(Promise.resolve());
  const enabled = useRef(active),
    reading = useRef(holding),
    previousQuery = useRef(query);
  enabled.current = active;
  reading.current = holding;
  function update(patch: Partial<Feed>) {
    current.current = { ...current.current, ...patch };
    setFeed(current.current);
  }
  function serial<T>(fn: () => Promise<T>) {
    const next = queue.current.then(fn, fn);
    queue.current = next.catch(() => {});
    return next;
  }
  function request(mode: "head" | "more" | "poll", generation = epoch.current) {
    return serial(async () => {
      const valid = () => epoch.current === generation && enabled.current;
      if (!valid()) return false;
      if (mode !== "poll") update({ loading: true });
      try {
        if (mode === "poll" && current.current.latest !== null) {
          const check = await api<{ new_count: number }>(
            "GET",
            `/usage-records?${query}&after_id=${current.current.latest}`,
          );
          if (!valid()) return false;
          update({ newCount: check.new_count, error: "" });
          if (
            !check.new_count ||
            reading.current ||
            current.current.items.length > 50
          )
            return true;
        }
        const cursor = mode === "more" ? current.current.cursor : null;
        if (mode === "more" && !cursor) return true;
        const page = await api<RecordPage>(
          "GET",
          `/usage-records?${query}${cursor ? `&cursor=${encodeURIComponent(cursor)}` : "&include_summary=true"}`,
        );
        if (!valid()) return false;
        // A user can start reading while the automatic refresh is in flight.
        if (
          mode === "poll" &&
          (reading.current || current.current.items.length > 50)
        )
          return true;
        update({
          items:
            mode === "more"
              ? [
                  ...new Map(
                    [...current.current.items, ...page.items].map((r) => [
                      r.id,
                      r,
                    ]),
                  ).values(),
                ]
              : page.items,
          cursor: page.next_cursor,
          latest: page.latest_id,
          observed:
            mode === "more" ? current.current.observed : page.observed_at,
          totalCost:
            mode === "more"
              ? current.current.totalCost
              : (page.summary?.actual_cost ?? null),
          newCount: mode === "more" ? current.current.newCount : 0,
          error: "",
        });
        return true;
      } catch (error) {
        if (valid())
          update({
            error: String(error instanceof Error ? error.message : error),
          });
        return false;
      } finally {
        if (valid()) update({ loading: false });
      }
    });
  }
  useEffect(() => {
    const generation = ++epoch.current;
    let timer: ReturnType<typeof setTimeout> | undefined,
      disposed = false,
      failures = 0;
    if (previousQuery.current !== query) {
      previousQuery.current = query;
      update(empty());
    } else update({ loading: false });
    if (active) {
      const cycle = async () => {
        const ok = await request(
          current.current.latest === null ? "head" : "poll",
          generation,
        );
        failures = ok ? 0 : failures + 1;
        if (!disposed)
          timer = setTimeout(
            () => void cycle(),
            Math.min(60000, 10000 * 2 ** Math.min(failures, 3)),
          );
      };
      void cycle();
    }
    return () => {
      disposed = true;
      ++epoch.current;
      clearTimeout(timer);
    };
  }, [query, active]);
  return {
    ...feed,
    refresh: () => request("head"),
    more: () => request("more"),
    detail: (id: number) => {
      const generation = epoch.current;
      return serial(async () => {
        if (generation !== epoch.current || !enabled.current)
          throw new Error("读取已取消");
        const row = await api<UsageRecord>("GET", `/usage-records/${id}`);
        if (generation !== epoch.current || !enabled.current)
          throw new Error("读取已取消");
        return row;
      });
    },
  };
}
