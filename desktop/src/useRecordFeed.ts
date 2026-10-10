import { useCallback, useEffect, useRef, useState } from "react";
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
  pageError: string;
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
  pageError: "",
});
export function useRecordFeed(
  query: string,
  active: boolean,
  holding: boolean,
  paginationEnabled = true,
) {
  const [feed, setFeed] = useState<Feed>(empty);
  const current = useRef(feed),
    epoch = useRef(0),
    queue = useRef<Promise<unknown>>(Promise.resolve());
  const enabled = useRef(active),
    reading = useRef(holding),
    previousQuery = useRef(query),
    liveQuery = useRef(query),
    canPage = useRef(paginationEnabled),
    pageRevision = useRef(0),
    consumedPages = useRef(new Set<string>()),
    pendingPages = useRef(new Set<string>());
  enabled.current = active;
  reading.current = holding;
  liveQuery.current = query;
  canPage.current = paginationEnabled;
  const update = useCallback((patch: Partial<Feed>) => {
    current.current = { ...current.current, ...patch };
    setFeed(current.current);
  }, []);
  const serial = useCallback(<T,>(fn: () => Promise<T>) => {
    const next = queue.current.then(fn, fn);
    queue.current = next.catch(() => {});
    return next;
  }, []);
  const request = useCallback((mode: "head" | "more" | "poll", generation = epoch.current, retry = false) => {
    const cursor = mode === "more" ? current.current.cursor : null;
    const revision = pageRevision.current;
    const identity = `${generation}:${revision}:${cursor}`;
    if (mode === "more") {
      if (!enabled.current || !canPage.current || !cursor || current.current.loading ||
          (current.current.pageError && !retry) || pendingPages.current.has(identity)) return Promise.resolve(false);
      pendingPages.current.add(identity);
      update({ loading: true, pageError: "" });
    }
    return serial(async () => {
      const valid = () => epoch.current === generation && enabled.current && liveQuery.current === query;
      if (!valid()) return false;
      if (mode === "more" && (!canPage.current || revision !== pageRevision.current || cursor !== current.current.cursor)) {
        update({ loading: false });
        return false;
      }
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
            current.current.pageError ||
            current.current.items.length > 50
          )
            return true;
        }
        const page = await api<RecordPage>(
          "GET",
          `/usage-records?${query}${cursor ? `&cursor=${encodeURIComponent(cursor)}` : "&include_summary=true"}`,
        );
        if (!valid()) return false;
        // A user can start reading while the automatic refresh is in flight.
        if (
          mode === "poll" &&
          (reading.current || current.current.pageError || current.current.items.length > 50)
        )
          return true;
        const items = mode === "more"
          ? [...new Map([...current.current.items, ...page.items].map(row => [row.id, row])).values()]
          : page.items;
        const stalled = mode === "more" && !!page.next_cursor &&
          (page.next_cursor === cursor || consumedPages.current.has(`${revision}:${page.next_cursor}`) ||
            items.length === current.current.items.length);
        if (mode !== "more") {
          ++pageRevision.current;
          consumedPages.current.clear();
        } else if (!stalled) consumedPages.current.add(`${revision}:${cursor}`);
        update({
          items,
          cursor: stalled ? cursor : page.next_cursor,
          latest: mode === "more" ? current.current.latest : page.latest_id,
          pageError: stalled ? "分页未返回后续记录，请重试" : "",
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
          update(mode === "more"
            ? { pageError: String(error instanceof Error ? error.message : error) }
            : { error: String(error instanceof Error ? error.message : error) });
        return false;
      } finally {
        if (valid()) update({ loading: false });
      }
    }).finally(() => { pendingPages.current.delete(identity); });
  }, [query, serial, update]);
  const more = useCallback(() => request("more"), [request]);
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
  }, [query, active, request, update]);
  return {
    ...feed,
    refresh: () => request("head"),
    more,
    retryMore: () => request("more", epoch.current, true),
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
