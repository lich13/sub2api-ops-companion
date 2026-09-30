// @vitest-environment jsdom
import React, { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { api, watchWindowFocus } from "./bridge";
import UsageRecords from "./UsageRecords";
import {
  defaultRecordColumns,
  type RecordPage,
  type UsageRecord,
} from "./records";
import { useRecordFeed } from "./useRecordFeed";
import { handleBack } from "./mobile";
import type { Account } from "./types";

vi.mock("./bridge", () => ({ api: vi.fn(), watchWindowFocus: vi.fn() }));
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

let container: HTMLDivElement, root: Root, unmounted: boolean;
let visibility: DocumentVisibilityState;
const focusListeners = new Set<(focused: boolean) => void>();
let feed: ReturnType<typeof useRecordFeed>;
const saveColumns = vi.fn<(columns: string[]) => Promise<void>>();
const now = "2026-09-30T04:00:00.000Z";
const query = "limit=50&start_date=2026-09-30&end_date=2026-09-30";
const totalCost = "12.3456789012";
const account = (id: number, name: string, status = "active"): Account => ({
  id,
  name,
  status,
  priority: 0,
  platform: "openai",
  type: "oauth",
  schedulable: true,
  available: true,
  group_ids: [],
  blockers: [],
  managed: false,
  version: "fixture",
  last_success_at: null,
  last_error_at: null,
  last_error_id: null,
  last_error_code: null,
  last_error_status: null,
  error_message: "",
  success_after_error: false,
  usage_windows: [],
});
const record = (
  id: number,
  overrides: Partial<UsageRecord> = {},
): UsageRecord => ({
  id,
  created_at: now,
  user_id: 7,
  user_name: `user-${id}`,
  user_email: null,
  api_key_id: 8,
  api_key_name: "workspace",
  account_id: 9,
  account_name: "test-account",
  group_id: 1,
  group_name: "test-group",
  model: "request-model",
  requested_model: "request-model",
  upstream_model: "forward-model",
  upstream_response_model: "returned-model",
  model_mapping_chain: "request-model → forward-model",
  upstream_model_mismatch: true,
  requested_reasoning_effort: "max",
  reasoning_effort: "high",
  request_type: "stream",
  input_tokens: 1234,
  output_tokens: 0,
  cache_creation_tokens: 0,
  cache_read_tokens: null,
  cache_creation_5m_tokens: 0,
  cache_creation_1h_tokens: 0,
  input_cost: "0",
  output_cost: null,
  cache_creation_cost: "0.0000000000",
  cache_read_cost: null,
  total_cost: "9007199254740993.1234567890",
  actual_cost: "9007199254740993.1234567890",
  account_cost: "0",
  account_stats_cost: null,
  account_rate_multiplier: "0",
  rate_multiplier: "0",
  first_token_ms: 0,
  duration_ms: null,
  user_agent: "fixture-agent",
  ip_address: "2001:db8::1",
  inbound_endpoint: "/v1/responses",
  upstream_endpoint: "/backend-api/codex/responses",
  request_id: `req-${id}`,
  upstream_request_id: `upstream-${id}`,
  billing_type: 0,
  billing_mode: "token",
  service_tier: null,
  image_count: 0,
  image_output_tokens: 0,
  image_output_cost: "0",
  image_input_tokens: 0,
  image_input_cost: "0",
  video_count: 0,
  video_duration_seconds: null,
  video_resolution: null,
  ...overrides,
});
const page = (
  items: UsageRecord[],
  overrides: Partial<RecordPage> = {},
): RecordPage => ({
  items,
  next_cursor: null,
  latest_id: items[0]?.id ?? 0,
  observed_at: now,
  summary: { actual_cost: totalCost },
  ...overrides,
});
function deferred<T>() {
  let resolve!: (value: T) => void, reject!: (reason: unknown) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}
function serve(handler: (path: string) => unknown | Promise<unknown>) {
  vi.mocked(api).mockImplementation(
    async (_method, path) => handler(path) as never,
  );
}
const paths = () => vi.mocked(api).mock.calls.map(([, path]) => path);
const params = (path: string) =>
  new URL(path, "https://fixture.invalid").searchParams;
const tableIds = () =>
  [...container.querySelectorAll<HTMLButtonElement>(".record-user button")].map(
    (b) => b.title,
  );
const summary = () => container.querySelector(".records-summary strong");
function button(label: string) {
  const found = [
    ...container.querySelectorAll<HTMLButtonElement>("button"),
  ].find(
    (b) => b.textContent === label || b.getAttribute("aria-label") === label,
  );
  if (!found) throw new Error(`Missing button: ${label}`);
  return found;
}
async function click(label: string) {
  await act(async () => button(label).click());
}
async function advance(ms: number) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}
async function stop() {
  await act(async () => root.unmount());
  unmounted = true;
}
function Probe({
  active,
  holding,
  search,
}: {
  active: boolean;
  holding: boolean;
  search: string;
}) {
  feed = useRecordFeed(search, active, holding);
  return <output>{feed.items.map((r) => r.id).join(",")}</output>;
}
async function renderFeed({
  active = true,
  holding = false,
  search = query,
} = {}) {
  await act(async () =>
    root.render(
      <React.StrictMode>
        <Probe active={active} holding={holding} search={search} />
      </React.StrictMode>,
    ),
  );
}
type ViewProps = Partial<React.ComponentProps<typeof UsageRecords>>;
async function renderView(props: ViewProps = {}, connectionKey = "fixture") {
  await act(async () =>
    root.render(
      <React.StrictMode>
        <UsageRecords
          key={connectionKey}
          online
          foreground
          desktop
          accounts={[]}
          columns={undefined}
          saveColumns={saveColumns}
          {...props}
        />
      </React.StrictMode>,
    ),
  );
}
async function setVisibility(value: DocumentVisibilityState) {
  visibility = value;
  await act(async () => document.dispatchEvent(new Event("visibilitychange")));
}
function mockFocus(initial: boolean | undefined) {
  vi.mocked(watchWindowFocus).mockImplementation(async (callback) => {
    focusListeners.add(callback);
    if (initial !== undefined) callback(initial);
    return () => {
      focusListeners.delete(callback);
    };
  });
}
async function setFocus(value: boolean) {
  await act(async () => {
    for (const callback of focusListeners) callback(value);
  });
}
async function scrollTo(top: number) {
  await act(async () => {
    const node = container.querySelector<HTMLDivElement>(".records-scroll")!;
    node.scrollTop = top;
    node.dispatchEvent(new Event("scroll", { bubbles: true }));
  });
}
async function setInput(label: string, value: string) {
  await act(async () => {
    const node = container.querySelector<HTMLInputElement>(
      `[aria-label="${label}"]`,
    )!;
    Object.getOwnPropertyDescriptor(
      HTMLInputElement.prototype,
      "value",
    )!.set!.call(node, value);
    node.dispatchEvent(new Event("input", { bubbles: true }));
  });
}
async function applyFilters() {
  await act(async () =>
    container
      .querySelector<HTMLButtonElement>(
        '.records-toolbar button[type="submit"]',
      )!
      .click(),
  );
}
async function escape(selector: string) {
  await act(async () =>
    container
      .querySelector(selector)!
      .dispatchEvent(
        new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
      ),
  );
}
function detailValue(label: string) {
  return [...container.querySelectorAll("[role=dialog] dt")].find(
    (node) => node.textContent === label,
  )?.nextElementSibling?.textContent;
}

beforeEach(() => {
  vi.resetAllMocks();
  vi.useFakeTimers();
  vi.setSystemTime(new Date(now));
  visibility = "visible";
  vi.spyOn(document, "visibilityState", "get").mockImplementation(
    () => visibility,
  );
  focusListeners.clear();
  mockFocus(true);
  saveColumns.mockResolvedValue();
  unmounted = false;
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  serve((path) =>
    params(path).has("after_id")
      ? { new_count: 0 }
      : page([record(100), record(99)]),
  );
});
afterEach(async () => {
  if (!unmounted) await stop();
  container.remove();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("record feed lifecycle", () => {
  it("loads once in StrictMode and polls every ten seconds after completion", async () => {
    await renderFeed();
    expect(api).toHaveBeenCalledTimes(1);
    expect(feed.items.map((r) => r.id)).toEqual([100, 99]);
    expect(params(paths()[0]).get("include_summary")).toBe("true");
    expect(feed.totalCost).toBe(totalCost);
    await advance(9999);
    expect(api).toHaveBeenCalledTimes(1);
    await advance(1);
    expect(api).toHaveBeenCalledTimes(2);
    expect(params(paths()[1]).get("after_id")).toBe("100");
    expect(params(paths()[1]).has("include_summary")).toBe(false);
    expect(feed.totalCost).toBe(totalCost);
    expect(feed.observed).toBe(now);
    await advance(10000);
    expect(api).toHaveBeenCalledTimes(3);
    expect(
      vi.mocked(api).mock.calls.every(([method]) => method === "GET"),
    ).toBe(true);
  });

  it("serializes slow initial, manual, and detail reads without accumulating timer requests", async () => {
    const initial = deferred<RecordPage>(),
      refresh = deferred<RecordPage>(),
      detail = deferred<UsageRecord>();
    let inFlight = 0,
      maximum = 0,
      index = 0;
    const replies = [initial.promise, refresh.promise, detail.promise];
    serve(async () => {
      ++inFlight;
      maximum = Math.max(maximum, inFlight);
      try {
        return await replies[index++];
      } finally {
        --inFlight;
      }
    });
    await renderFeed();
    let manual!: Promise<boolean>, selected!: Promise<UsageRecord>;
    await act(async () => {
      manual = feed.refresh();
      selected = feed.detail(100);
    });
    await advance(60000);
    expect(api).toHaveBeenCalledTimes(1);
    await act(async () => initial.resolve(page([record(100)])));
    expect(api).toHaveBeenCalledTimes(2);
    await act(async () => refresh.resolve(page([record(101)])));
    expect(api).toHaveBeenCalledTimes(3);
    expect(paths()[2]).toBe("/usage-records/100");
    await act(async () => detail.resolve(record(100)));
    expect(await manual).toBe(true);
    expect((await selected).id).toBe(100);
    expect(maximum).toBe(1);
    expect(feed.items.map((r) => r.id)).toEqual([101]);
  });

  it("discards the old query result while the replacement waits for the same serial queue", async () => {
    const old = deferred<RecordPage>(),
      fresh = deferred<RecordPage>();
    serve((path) =>
      params(path).get("model") === "old-model" ? old.promise : fresh.promise,
    );
    await renderFeed({ search: `${query}&model=old-model` });
    await renderFeed({ search: `${query}&model=new-model` });
    expect(api).toHaveBeenCalledTimes(1);
    await act(async () => old.resolve(page([record(100)])));
    expect(api).toHaveBeenCalledTimes(2);
    expect(feed.items).toEqual([]);
    expect(feed.observed).toBe("");
    expect(feed.totalCost).toBeNull();
    await act(async () => fresh.resolve(page([record(200)])));
    expect(feed.items.map((r) => r.id)).toEqual([200]);
    expect(params(paths()[1]).get("model")).toBe("new-model");
  });

  it("invalidates in-flight results and queued detail work on unmount", async () => {
    await renderFeed();
    const pending = deferred<RecordPage>();
    serve(() => pending.promise);
    let refresh!: Promise<boolean>, detail!: Promise<UsageRecord | string>;
    await act(async () => {
      refresh = feed.refresh();
      detail = feed.detail(100).catch((error: Error) => error.message);
    });
    await stop();
    await act(async () => pending.resolve(page([record(200)])));
    expect(await refresh).toBe(false);
    expect(await detail).toBe("读取已取消");
    expect(api).toHaveBeenCalledTimes(2);
    await advance(60000);
    expect(api).toHaveBeenCalledTimes(2);
  });

  it("deduplicates pagination and preserves the expanded historical window while counting arrivals", async () => {
    const first = Array.from({ length: 50 }, (_, i) => record(100 - i));
    const second = [
      record(51, { user_name: "updated-duplicate" }),
      ...Array.from({ length: 50 }, (_, i) => record(50 - i)),
    ];
    const cursor = "cursor: +/?=";
    serve((path) => {
      const search = params(path);
      if (search.has("after_id")) return { new_count: 3 };
      return search.has("cursor")
        ? page(second, {
            latest_id: 100,
            observed_at: "2026-09-30T05:00:00.000Z",
            summary: { actual_cost: "999" },
          })
        : page(first, { next_cursor: cursor });
    });
    await renderFeed();
    await act(async () => {
      await feed.more();
    });
    expect(params(paths()[1]).get("cursor")).toBe(cursor);
    expect(params(paths()[1]).has("include_summary")).toBe(false);
    expect(feed.observed).toBe(now);
    expect(feed.totalCost).toBe(totalCost);
    expect(feed.items).toHaveLength(100);
    expect(feed.items.map((r) => r.id)).toEqual(
      Array.from({ length: 100 }, (_, i) => 100 - i),
    );
    expect(feed.items.find((r) => r.id === 51)?.user_name).toBe(
      "updated-duplicate",
    );
    expect(feed.cursor).toBeNull();
    await act(async () => {
      await feed.more();
    });
    expect(api).toHaveBeenCalledTimes(2);
    await advance(10000);
    expect(api).toHaveBeenCalledTimes(3);
    expect(params(paths()[2]).get("after_id")).toBe("100");
    expect(feed.newCount).toBe(3);
    expect(feed.items).toHaveLength(100);
    expect(feed.totalCost).toBe(totalCost);
    expect(feed.observed).toBe(now);
    expect(params(paths()[2]).has("include_summary")).toBe(false);
  });

  it("refreshes the head for new arrivals when no history or detail is held", async () => {
    let heads = 0;
    serve((path) =>
      params(path).has("after_id")
        ? { new_count: 2 }
        : page([record(++heads === 1 ? 100 : 102)], {
            summary: { actual_cost: heads === 1 ? totalCost : "18.9999999999" },
            observed_at: heads === 1 ? now : "2026-09-30T04:00:10.000Z",
          }),
    );
    await renderFeed();
    await advance(10000);
    expect(api).toHaveBeenCalledTimes(3);
    expect(feed.items.map((r) => r.id)).toEqual([102]);
    expect(feed.newCount).toBe(0);
    expect(feed.latest).toBe(102);
    expect(feed.totalCost).toBe("18.9999999999");
    expect(feed.observed).toBe("2026-09-30T04:00:10.000Z");
    expect(params(paths()[2]).get("include_summary")).toBe("true");
  });

  it("preserves cached rows after errors and backs off before resuming normal polling", async () => {
    let checks = 0;
    serve((path) => {
      if (!params(path).has("after_id")) return page([record(100)]);
      if (++checks === 1) throw new Error("temporary fixture failure");
      return { new_count: 0 };
    });
    await renderFeed();
    await advance(10000);
    expect(feed.error).toBe("temporary fixture failure");
    expect(feed.items.map((r) => r.id)).toEqual([100]);
    expect(feed.totalCost).toBe(totalCost);
    expect(feed.observed).toBe(now);
    await advance(19999);
    expect(api).toHaveBeenCalledTimes(2);
    await advance(1);
    expect(api).toHaveBeenCalledTimes(3);
    expect(feed.error).toBe("");
    await advance(10000);
    expect(api).toHaveBeenCalledTimes(4);
  });

  it("keeps list, total, and observation together while a refresh is pending or fails", async () => {
    await renderFeed();
    const pending = deferred<RecordPage>();
    serve(() => pending.promise);
    let refresh!: Promise<boolean>;
    await act(async () => {
      refresh = feed.refresh();
    });
    expect(feed.items.map((row) => row.id)).toEqual([100, 99]);
    expect(feed.totalCost).toBe(totalCost);
    expect(feed.observed).toBe(now);
    await act(async () => pending.reject(new Error("summary unavailable")));
    expect(await refresh).toBe(false);
    expect(feed.error).toBe("summary unavailable");
    expect(feed.items.map((row) => row.id)).toEqual([100, 99]);
    expect(feed.totalCost).toBe(totalCost);
    expect(feed.observed).toBe(now);

    serve(() =>
      page([record(101)], {
        summary: { actual_cost: "9007199254740993.1234567890" },
        observed_at: "2026-09-30T05:00:00.000Z",
      }),
    );
    await act(async () => {
      await feed.refresh();
    });
    expect(feed.items.map((row) => row.id)).toEqual([101]);
    expect(feed.totalCost).toBe("9007199254740993.1234567890");
    expect(feed.observed).toBe("2026-09-30T05:00:00.000Z");
    expect(feed.error).toBe("");
    expect(
      paths().every((path) => params(path).get("include_summary") === "true"),
    ).toBe(true);
  });
});

describe("record view", () => {
  it.each([false, true])(
    "only displays the six-decimal total without a precise tooltip or copy control, mobile=%s",
    async (mobile) => {
      const exact = "9007199254740993.1234567890";
      serve(() => page([], { summary: { actual_cost: exact } }));
      await renderView({ mobile, desktop: !mobile });
      expect(summary()?.textContent).toBe("$9007199254740993.123457");
      const area = container.querySelector(".records-summary")!;
      expect(area.innerHTML).not.toContain(exact);
      expect(area.querySelector("[title], button, details")).toBeNull();
      expect(container.querySelector(".records-total-detail")).toBeNull();
    },
  );

  it("defaults to today in Beijing and only changes date queries after Apply", async () => {
    await renderView();
    expect(params(paths()[0]).get("start_date")).toBe("2026-09-30");
    expect(params(paths()[0]).get("end_date")).toBe("2026-09-30");
    expect(params(paths()[0]).get("include_summary")).toBe("true");
    expect(params(paths()[0]).has("from_at")).toBe(false);
    expect(params(paths()[0]).has("to_at")).toBe(false);
    expect(button("时间范围").textContent).toBe("今天");
    expect(summary()?.textContent).toBe("$12.345679");

    await click("时间范围");
    await click("近24小时");
    expect(api).toHaveBeenCalledTimes(1);
    await click("应用");
    expect(api).toHaveBeenCalledTimes(2);
    expect(params(paths()[1]).get("start_date")).toBe("2026-09-29");
    expect(params(paths()[1]).get("end_date")).toBe("2026-09-30");
    expect(button("时间范围").textContent).toBe("近24小时");

    await click("时间范围");
    await setInput("开始日期", "2026-09-12");
    await setInput("结束日期", "2026-09-15");
    await click("应用");
    expect(params(paths()[2]).get("start_date")).toBe("2026-09-12");
    expect(params(paths()[2]).get("end_date")).toBe("2026-09-15");
    expect(button("时间范围").textContent).toBe("2026-09-12 — 2026-09-15");
  });

  it("shows an unknown total on initial failure and retries the complete list with a genuine zero total", async () => {
    serve(() => {
      throw new Error("records and summary unavailable");
    });
    await renderView();
    expect(summary()?.textContent).toBe("—");
    expect(container.querySelector(".records-total-detail")).toBeNull();
    expect(container.querySelector('[role="alert"]')?.textContent).toContain(
      "records and summary unavailable",
    );
    serve(() => page([], { summary: { actual_cost: "0.0000000000" } }));
    await click("重试");
    expect(params(paths().at(-1)!).get("include_summary")).toBe("true");
    expect(summary()?.textContent).toBe("$0.000000");
    expect(summary()?.getAttribute("title")).toBeNull();
    expect(container.querySelector('[role="alert"]')).toBeNull();
  });

  it("does not show the old total under a newly applied date range when its read fails", async () => {
    await renderView();
    const pending = deferred<RecordPage>();
    serve(() => pending.promise);
    await click("时间范围");
    await click("昨天");
    await click("应用");
    expect(button("时间范围").textContent).toBe("昨天");
    expect(params(paths().at(-1)!).get("start_date")).toBe("2026-09-29");
    expect(params(paths().at(-1)!).get("end_date")).toBe("2026-09-29");
    expect(tableIds()).toEqual([]);
    expect(summary()?.textContent).toBe("—");
    await act(async () => pending.reject(new Error("yesterday unavailable")));
    expect(tableIds()).toEqual([]);
    expect(summary()?.textContent).toBe("—");
    expect(container.querySelector('[role="alert"]')?.textContent).toContain(
      "yesterday unavailable",
    );
    serve(() =>
      page([record(90)], { summary: { actual_cost: "9.1234567890" } }),
    );
    await click("重试");
    expect(params(paths().at(-1)!).get("start_date")).toBe("2026-09-29");
    expect(tableIds()).toEqual(["查看记录 #90"]);
    expect(summary()?.textContent).toBe("$9.123457");
  });

  it("replaces today and its total at Beijing midnight while reading the live head", async () => {
    vi.setSystemTime(new Date("2026-09-30T15:59:59Z"));
    serve((path) =>
      params(path).get("start_date") === "2026-10-01"
        ? page([record(200)], { summary: { actual_cost: "0.125" } })
        : page([record(100)]),
    );
    await renderView();
    await advance(1000);
    expect(api).toHaveBeenCalledTimes(2);
    expect(params(paths()[1]).get("start_date")).toBe("2026-10-01");
    expect(params(paths()[1]).get("end_date")).toBe("2026-10-01");
    expect(tableIds()).toEqual(["查看记录 #200"]);
    expect(summary()?.textContent).toBe("$0.125000");
    expect(container.querySelector(".new-records")).toBeNull();
  });

  it.each(["scroll", "detail", "history", "filters"])(
    "defers a midnight date change while holding %s until an explicit refresh",
    async (holding) => {
      vi.setSystemTime(new Date("2026-09-30T15:59:59Z"));
      const props =
        holding === "filters" ? { mobile: true, desktop: false } : {};
      serve((path) => {
        if (path === "/usage-records/100") return record(100);
        const search = params(path);
        if (search.get("start_date") === "2026-10-01")
          return page([record(200)], { summary: { actual_cost: "0.125" } });
        if (search.has("after_id")) return { new_count: 3 };
        if (search.has("cursor"))
          return page(
            Array.from({ length: 50 }, (_, i) => record(50 - i)),
            { summary: { actual_cost: "999" } },
          );
        return holding === "history"
          ? page(
              Array.from({ length: 50 }, (_, i) => record(100 - i)),
              { next_cursor: "history-page" },
            )
          : page([record(100)]);
      });
      await renderView(props);
      if (holding === "scroll") await scrollTo(240);
      if (holding === "detail")
        await act(async () =>
          container
            .querySelector<HTMLButtonElement>('[title="查看记录 #100"]')!
            .click(),
        );
      if (holding === "history") await click("加载更多");
      if (holding === "filters") await click("筛选记录");
      const before = paths().length;
      await advance(1000);
      expect(api).toHaveBeenCalledTimes(before);
      expect(button("日期已更新 · 刷新")).toBeDefined();
      expect(summary()?.textContent).toBe("$12.345679");
      expect(container.textContent).toContain("user-100");
      if (holding === "scroll") {
        expect(container.querySelector(".records-scroll")?.scrollTop).toBe(240);
        await scrollTo(0);
      }
      if (holding === "detail") await click("关闭详情");
      if (holding === "filters") await click("关闭记录筛选");
      await advance(1000);
      expect(api).toHaveBeenCalledTimes(before);
      expect(button("日期已更新 · 刷新")).toBeDefined();

      await click("日期已更新 · 刷新");
      expect(api).toHaveBeenCalledTimes(before + 1);
      expect(params(paths().at(-1)!).get("start_date")).toBe("2026-10-01");
      expect(params(paths().at(-1)!).get("include_summary")).toBe("true");
      expect(summary()?.textContent).toBe("$0.125000");
      expect(container.textContent).toContain("user-200");
      expect(container.textContent).not.toContain("user-100");
      expect(container.querySelector(".records-scroll")?.scrollTop).toBe(0);
      expect(container.querySelector(".new-records")).toBeNull();
    },
  );

  it("does not query across midnight in the background and resumes using the new Beijing day", async () => {
    vi.setSystemTime(new Date("2026-09-30T15:59:59Z"));
    serve((path) =>
      params(path).get("start_date") === "2026-10-01"
        ? page([record(200)], { summary: { actual_cost: "0.125" } })
        : page([record(100)]),
    );
    await renderView({ mobile: true, desktop: false });
    await renderView({ mobile: true, desktop: false, foreground: false });
    await advance(30000);
    expect(api).toHaveBeenCalledTimes(1);
    expect(summary()?.textContent).toBe("$12.345679");
    await renderView({ mobile: true, desktop: false });
    expect(params(paths().at(-1)!).get("start_date")).toBe("2026-10-01");
    expect(summary()?.textContent).toBe("$0.125000");
    expect(container.textContent).toContain("user-200");
  });

  it("keeps an applied custom date range unchanged when Beijing midnight passes", async () => {
    vi.setSystemTime(new Date("2026-09-30T15:59:59Z"));
    await renderView();
    await click("时间范围");
    await setInput("开始日期", "2026-09-12");
    await setInput("结束日期", "2026-09-15");
    await click("应用");
    await advance(1000);
    expect(api).toHaveBeenCalledTimes(2);
    expect(params(paths().at(-1)!).get("start_date")).toBe("2026-09-12");
    expect(params(paths().at(-1)!).get("end_date")).toBe("2026-09-15");
    expect(container.querySelector(".new-records")).toBeNull();
  });

  it("clears the old connection's list and total and ignores its late refresh response", async () => {
    await renderView({}, "connection-a");
    const old = deferred<RecordPage>(),
      fresh = deferred<RecordPage>();
    serve(() => old.promise);
    await click("刷新记录");
    serve(() => fresh.promise);
    await renderView({}, "connection-b");
    expect(tableIds()).toEqual([]);
    expect(summary()?.textContent).toBe("—");
    await act(async () =>
      fresh.resolve(page([record(200)], { summary: { actual_cost: "20.2" } })),
    );
    await act(async () =>
      old.resolve(page([record(999)], { summary: { actual_cost: "999" } })),
    );
    expect(tableIds()).toEqual(["查看记录 #200"]);
    expect(summary()?.textContent).toBe("$20.200000");
    expect(focusListeners.size).toBe(1);
    expect(api).toHaveBeenCalledTimes(3);
  });

  it("renders every selected record field in mobile cards and retains request type only in details", async () => {
    const row = record(100, {
      request_type: "ws_v2",
      output_tokens: 120,
      cache_read_tokens: 127100,
      cache_creation_tokens: 1250000,
      first_token_ms: 10000,
      duration_ms: 70000,
    });
    serve((path) => (path === "/usage-records/100" ? row : page([row])));
    await renderView({
      mobile: true,
      desktop: false,
      columns: [
        "api_key",
        "account",
        "model",
        "reasoning",
        "tokens",
        "cost",
        "latency",
        "user_agent",
        "ip",
        "endpoint",
        "group",
        "billing",
        "request_id",
        "upstream_id",
        "type",
      ],
    });
    expect(container.querySelector(".records-table")).toBeNull();
    const card = container.querySelector(".mobile-record")!;
    expect(
      [...card.querySelectorAll(".mobile-record-label")].map(
        (node) => node.textContent,
      ),
    ).toEqual([
      "API 密钥",
      "账户",
      "模型",
      "推理强度",
      "Token",
      "延迟",
      "User-Agent",
      "IP",
      "端点",
      "分组",
      "计费模式",
      "请求 ID",
      "上游 ID",
    ]);
    expect(card.querySelector(".mobile-record-user")?.textContent).toContain(
      "user-100 #7",
    );
    expect(card.querySelector("time")?.textContent).toMatch(/\d/);
    for (const text of [
      "workspace",
      "#8",
      "test-account",
      "request-model",
      "forward-model",
      "returned-model",
      "max",
      "high",
      "fixture-agent",
      "2001:db8::1",
      "/v1/responses",
      "/backend-api/codex/responses",
      "test-group",
      "req-100",
      "upstream-100",
    ])
      expect(card.textContent).toContain(text);
    expect(card.querySelector('[title="输入 Token"]')?.textContent).toBe(
      "1,234",
    );
    expect(card.querySelector('[title="输出 Token"]')?.textContent).toBe("120");
    expect(card.querySelector(".record-cache-read")?.textContent).toBe(
      "127.1K",
    );
    expect(
      card.querySelector(".record-cache-read")?.getAttribute("title"),
    ).toBe("缓存读取：127,100");
    expect(card.querySelector(".record-cache-write")?.textContent).toBe("1.3M");
    expect(card.querySelector('[title="用户实扣"]')?.textContent).toBe(
      "$9007199254740993.123457",
    );
    expect(card.querySelector('[title="账户费用"]')?.textContent).toBe(
      "A $0.000000",
    );
    expect(card.querySelector(".record-tps")?.textContent).toBe("2.00 tok/s");
    expect(card.querySelector(".mobile-record-latency")?.textContent).toContain(
      "10.00s",
    );
    expect(card.querySelector(".mobile-record-latency")?.textContent).toContain(
      "70.00s",
    );
    expect(card.querySelector(".mobile-record-billing")?.textContent).toBe(
      "计费模式Token",
    );
    expect(card.querySelector(".mobile-record-type")).toBeNull();
    expect(card.textContent).not.toContain("WebSocket");
    expect(saveColumns).not.toHaveBeenCalled();
    await click("查看记录 #100");
    expect(detailValue("类型")).toBe("WebSocket");
    expect(detailValue("用户实扣")).toBe("$9007199254740993.1234567890");
    const fallback = vi.fn();
    await act(async () => handleBack(fallback));
    expect(container.querySelector('[role="dialog"]')).toBeNull();
    expect(fallback).not.toHaveBeenCalled();
  });

  it("applies saved field preferences to mobile cards and preserves mandatory user and time", async () => {
    const props = {
      mobile: true,
      desktop: false,
      columns: ["model", "cost", "user_agent", "type"],
    };
    await renderView(props);
    const labels = () =>
      [
        ...container.querySelectorAll(
          ".mobile-record:first-child .mobile-record-label",
        ),
      ].map((node) => node.textContent);
    expect(labels()).toEqual(["模型", "User-Agent"]);
    expect(
      container.querySelector(".mobile-record .record-money"),
    ).not.toBeNull();
    const menu = container.querySelector<HTMLDetailsElement>(
      ".records-column-menu",
    )!;
    await act(async () => menu.querySelector("summary")!.click());
    await advance(0);
    const costToggle = [...menu.querySelectorAll("label")]
      .find((label) => label.querySelector("span")?.textContent === "费用")!
      .querySelector<HTMLInputElement>("input")!;
    await act(async () => costToggle.click());
    expect(saveColumns).toHaveBeenCalledExactlyOnceWith([
      "model",
      "user_agent",
    ]);
    await renderView({ ...props, columns: ["model", "user_agent"] });
    expect(container.querySelector(".mobile-record .record-money")).toBeNull();
    expect(labels()).toEqual(["模型", "User-Agent"]);
    expect(
      container.querySelector(".mobile-record-user")?.textContent,
    ).toContain("user-100");
    expect(container.querySelector(".mobile-record time")).not.toBeNull();
    const fallback = vi.fn();
    await act(async () => handleBack(fallback));
    await advance(0);
    expect(menu.open).toBe(false);
    expect(fallback).not.toHaveBeenCalled();
    await act(async () => handleBack(fallback));
    expect(fallback).toHaveBeenCalledTimes(1);
  });

  it.each(["close", "Escape", "Back", "backdrop"])(
    "discards mobile filter drafts when closed with %s and applies them only on submit",
    async (action) => {
      await renderView({ mobile: true, desktop: false });
      await click("筛选记录");
      await setInput("模型筛选", "discarded-model");
      const fallback = vi.fn();
      if (action === "close") await click("关闭记录筛选");
      else if (action === "Escape")
        await escape('.records-toolbar[role="dialog"]');
      else if (action === "Back") await act(async () => handleBack(fallback));
      else
        await act(async () =>
          container
            .querySelector<HTMLElement>(".records-filter-backdrop")!
            .click(),
        );
      expect(
        container.querySelector('.records-toolbar[role="dialog"]'),
      ).toBeNull();
      expect(api).toHaveBeenCalledTimes(1);
      expect(fallback).not.toHaveBeenCalled();
      await click("筛选记录");
      expect(
        container.querySelector<HTMLInputElement>('[aria-label="模型筛选"]')
          ?.value,
      ).toBe("");
      await setInput("模型筛选", "committed-model");
      await applyFilters();
      expect(params(paths().at(-1)!).get("model")).toBe("committed-model");
      expect(params(paths().at(-1)!).get("include_summary")).toBe("true");
      expect(
        container.querySelector('.records-toolbar[role="dialog"]'),
      ).toBeNull();
      expect(api).toHaveBeenCalledTimes(2);
    },
  );

  it.each([
    { state: "unconfirmed", initial: undefined },
    { state: "unfocused", initial: false },
  ])(
    "waits for confirmed window focus when initially $state",
    async ({ initial }) => {
      mockFocus(initial);
      await renderView();
      await advance(30000);
      expect(document.visibilityState).toBe("visible");
      expect(api).not.toHaveBeenCalled();
      expect(button("刷新记录").disabled).toBe(true);
      expect(button("用户筛选").disabled).toBe(true);
      expect(focusListeners.size).toBe(1);

      await setFocus(true);
      expect(api).toHaveBeenCalledTimes(1);
      expect(tableIds()).toEqual(["查看记录 #100", "查看记录 #99"]);
      expect(button("刷新记录").disabled).toBe(false);
    },
  );

  it("pauses for native focus loss and checks immediately on return while preserving history and detail", async () => {
    serve((path) => {
      if (path === "/usage-records/99") return record(99);
      return params(path).has("after_id")
        ? { new_count: 2 }
        : page([record(100), record(99)]);
    });
    await renderView();
    await scrollTo(240);
    await act(async () =>
      container
        .querySelector<HTMLButtonElement>('[title="查看记录 #99"]')!
        .click(),
    );
    expect(api).toHaveBeenCalledTimes(2);

    await setFocus(false);
    expect(document.visibilityState).toBe("visible");
    expect(button("刷新记录").disabled).toBe(true);
    await advance(30000);
    expect(api).toHaveBeenCalledTimes(2);

    await setFocus(true);
    expect(api).toHaveBeenCalledTimes(3);
    expect(params(paths()[2]).get("after_id")).toBe("100");
    expect(tableIds()).toEqual(["查看记录 #100", "查看记录 #99"]);
    expect(container.querySelector(".records-scroll")?.scrollTop).toBe(240);
    expect(container.querySelector("[role=dialog]")?.textContent).toContain(
      "#99",
    );
    expect(button("2 条新记录")).toBeDefined();
    await advance(10000);
    expect(api).toHaveBeenCalledTimes(4);
    expect(params(paths()[3]).get("after_id")).toBe("100");
  });

  it.each(["unfocused", "background"])(
    "discards an in-flight refresh when %s without replacing cached rows",
    async (state) => {
      await renderView();
      const stale = deferred<RecordPage>();
      serve((path) =>
        params(path).has("after_id") ? { new_count: 0 } : stale.promise,
      );
      await click("刷新记录");
      expect(api).toHaveBeenCalledTimes(2);
      if (state === "unfocused") await setFocus(false);
      else {
        await renderView({ foreground: false });
        await setFocus(true);
      }
      expect(document.visibilityState).toBe("visible");
      expect(button("刷新记录").disabled).toBe(true);
      await act(async () =>
        stale.resolve(page([record(200)], { summary: { actual_cost: "999" } })),
      );
      expect(tableIds()).toEqual(["查看记录 #100", "查看记录 #99"]);
      expect(summary()?.textContent).toBe("$12.345679");
      await advance(30000);
      expect(api).toHaveBeenCalledTimes(2);

      if (state === "unfocused") await setFocus(true);
      else await renderView();
      expect(api).toHaveBeenCalledTimes(3);
      expect(params(paths()[2]).get("after_id")).toBe("100");
      expect(tableIds()).toEqual(["查看记录 #100", "查看记录 #99"]);
      expect(summary()?.textContent).toBe("$12.345679");
      await advance(10000);
      expect(api).toHaveBeenCalledTimes(4);
    },
  );

  it("disposes delayed focus subscriptions from StrictMode and unmount and ignores their callbacks", async () => {
    const first = deferred<() => void>(),
      second = deferred<() => void>();
    const firstStop = vi.fn(),
      secondStop = vi.fn();
    const callbacks: ((focused: boolean) => void)[] = [];
    vi.mocked(watchWindowFocus).mockImplementation((callback) => {
      callbacks.push(callback);
      return callbacks.length === 1 ? first.promise : second.promise;
    });
    await renderView();
    expect(watchWindowFocus).toHaveBeenCalledTimes(2);
    await act(async () => {
      callbacks[0](true);
      first.resolve(firstStop);
    });
    expect(firstStop).toHaveBeenCalledTimes(1);
    expect(api).not.toHaveBeenCalled();

    await act(async () => callbacks[1](true));
    expect(api).toHaveBeenCalledTimes(1);
    await stop();
    await act(async () => {
      callbacks[0](false);
      callbacks[1](false);
      callbacks[1](true);
      second.resolve(secondStop);
    });
    await advance(30000);
    expect(secondStop).toHaveBeenCalledTimes(1);
    expect(firstStop).toHaveBeenCalledTimes(1);
    expect(api).toHaveBeenCalledTimes(1);
    expect(container.textContent).toBe("");
  });

  it("reports a failed initial focus read and leaves record reads paused", async () => {
    vi.mocked(watchWindowFocus).mockRejectedValue(
      new Error("focus unavailable"),
    );
    await renderView();
    expect(container.querySelector("[role=alert]")?.textContent).toContain(
      "窗口状态读取失败",
    );
    await advance(30000);
    expect(api).not.toHaveBeenCalled();
    expect(button("刷新记录").disabled).toBe(true);
  });

  it("uses foreground state without a window focus subscription on non-desktop clients", async () => {
    await renderView({ desktop: false, foreground: false });
    await advance(30000);
    expect(api).not.toHaveBeenCalled();
    expect(watchWindowFocus).not.toHaveBeenCalled();

    await renderView({ desktop: false });
    expect(api).toHaveBeenCalledTimes(1);
    await renderView({ desktop: false, foreground: false });
    await advance(30000);
    expect(api).toHaveBeenCalledTimes(1);
    expect(tableIds()).toEqual(["查看记录 #100", "查看记录 #99"]);
    await renderView({ desktop: false });
    expect(api).toHaveBeenCalledTimes(2);
    expect(params(paths()[1]).get("after_id")).toBe("100");
    expect(watchWindowFocus).not.toHaveBeenCalled();
  });

  it.each([false, true])(
    "only offers snapshot accounts and applies local name or ID search, mobile=%s",
    async (mobile) => {
      const accounts = [
        account(22, "Current East"),
        account(33, "Current West", "disabled"),
      ];
      serve((path) =>
        page([
          params(path).get("account_id") === "22"
            ? record(200, { account_id: 22, account_name: "Current East" })
            : record(100),
        ]),
      );
      await renderView({ accounts, mobile, desktop: !mobile });
      expect(container.textContent).toContain("test-account");
      if (mobile) await click("筛选记录");
      await click("账户筛选");
      const list = () => container.querySelector('[role="listbox"]')!;
      expect(list().textContent).toContain("Current East#22");
      expect(list().textContent).toContain("Current West#33停用");
      expect(list().textContent).not.toContain("test-account");
      await setInput("搜索账户", " west ");
      expect(list().textContent).toContain("Current West#33");
      expect(list().textContent).not.toContain("Current East");
      await setInput("搜索账户", "#9");
      expect(list().querySelectorAll('[role="option"]')).toHaveLength(1);
      await setInput("搜索账户", "#22");
      expect(list().querySelectorAll('[role="option"]')).toHaveLength(2);
      await click("Current East#22");
      expect(api).toHaveBeenCalledTimes(1);
      await applyFilters();
      expect(api).toHaveBeenCalledTimes(2);
      expect(params(paths().at(-1)!).get("account_id")).toBe("22");
      expect(paths().every((path) => path.startsWith("/usage-records?"))).toBe(
        true,
      );
      expect(container.textContent).not.toContain("test-account");
      expect(container.textContent).toContain("Current East");
    },
  );

  it.each(["draft", "applied"])(
    "clears a removed account from the %s selection when the online snapshot changes",
    async (stage) => {
      const accounts = [account(22, "Current East")];
      await renderView({ accounts });
      await click("账户筛选");
      await click("Current East#22");
      if (stage === "applied") {
        await applyFilters();
        expect(params(paths().at(-1)!).get("account_id")).toBe("22");
      }
      await renderView({ accounts: [account(33, "Current West")] });
      expect(button("账户筛选").textContent).toBe("全部账户");
      await applyFilters();
      await click("刷新记录");
      expect(params(paths().at(-1)!).has("account_id")).toBe(false);
      await click("账户筛选");
      expect(
        container.querySelector('[role="listbox"]')?.textContent,
      ).toContain("Current West#33");
      expect(
        container.querySelector('[role="listbox"]')?.textContent,
      ).not.toContain("Current East");
    },
  );

  it("preserves an applied account while offline and clears it when a fresh online snapshot removes it", async () => {
    await renderView({ accounts: [account(22, "Current East")] });
    await click("账户筛选");
    await click("Current East#22");
    await applyFilters();
    const before = paths().length;
    await renderView({ online: false, accounts: [] });
    await advance(30000);
    expect(api).toHaveBeenCalledTimes(before);
    await renderView({ accounts: [account(22, "Current East")] });
    expect(button("账户筛选").textContent).toBe("Current East #22");
    expect(params(paths().at(-1)!).get("account_id")).toBe("22");
    await renderView({ accounts: [] });
    expect(button("账户筛选").textContent).toBe("全部账户");
    expect(params(paths().at(-1)!).has("account_id")).toBe(false);
  });

  it("uses the complete directory with no recent records and clears the key whenever the user changes", async () => {
    const users = [
      {
        id: 71,
        name: "user-one",
        email: "one@example.invalid",
        deleted: false,
      },
      {
        id: 72,
        name: "user-two",
        email: "two@example.invalid",
        deleted: false,
      },
    ];
    serve((path) => {
      if (!path.startsWith("/usage-record-options?")) return page([]);
      if (params(path).get("kind") === "users")
        return { items: users, next_cursor: null };
      const owner = Number(params(path).get("user_id"));
      return {
        items: [
          {
            id: owner + 100,
            name: "unused-key",
            user_id: owner,
            user_name: `owner-${owner}`,
            deleted: false,
          },
        ],
        next_cursor: null,
      };
    });
    const choose = async (text: string) => {
      const option = [
        ...container.querySelectorAll<HTMLButtonElement>('[role="option"]'),
      ].find((node) => node.textContent?.startsWith(text));
      if (!option) throw new Error(`Missing option: ${text}`);
      await act(async () => option.click());
    };
    await renderView();
    expect(tableIds()).toEqual([]);
    await click("用户筛选");
    await advance(0);
    await choose("user-one#71");
    await click("API 密钥筛选");
    await advance(0);
    expect(params(paths().at(-1)!).get("user_id")).toBe("71");
    expect(container.querySelector('[role="listbox"]')?.textContent).toContain(
      "owner-71 #71",
    );
    await choose("unused-key#171");
    await click("筛选");
    expect(params(paths().at(-1)!).get("user_id")).toBe("71");
    expect(params(paths().at(-1)!).get("api_key_id")).toBe("171");

    await click("用户筛选");
    await advance(0);
    await choose("user-two#72");
    expect(button("API 密钥筛选").textContent).toBe("全部API 密钥");
    await click("筛选");
    expect(params(paths().at(-1)!).get("user_id")).toBe("72");
    expect(params(paths().at(-1)!).has("api_key_id")).toBe(false);
    await click("API 密钥筛选");
    await advance(0);
    expect(params(paths().at(-1)!).get("user_id")).toBe("72");
    await choose("unused-key#172");
    await click("用户筛选");
    await advance(0);
    await choose("全部用户");
    expect(button("API 密钥筛选").textContent).toBe("全部API 密钥");
    await click("筛选");
    expect(params(paths().at(-1)!).has("user_id")).toBe(false);
    expect(params(paths().at(-1)!).has("api_key_id")).toBe(false);
    expect(
      paths()
        .filter((path) => path.startsWith("/usage-record-options?"))
        .every((path) =>
          ["from_at", "to_at", "start_date", "end_date"].every(
            (key) => !params(path).has(key),
          ),
        ),
    ).toBe(true);
  });

  it.each(["hidden", "offline", "background", "unfocused"])(
    "stops directory interaction when the record view becomes %s",
    async (state) => {
      const stale = deferred<unknown>();
      serve((path) =>
        path.startsWith("/usage-record-options?") ? stale.promise : page([]),
      );
      await renderView();
      await click("用户筛选");
      await advance(0);
      if (state === "hidden") await setVisibility("hidden");
      else if (state === "unfocused") await setFocus(false);
      else
        await renderView(
          state === "offline" ? { online: false } : { foreground: false },
        );
      if (state === "background") await setFocus(true);
      expect(button("用户筛选").disabled).toBe(true);
      expect(button("API 密钥筛选").disabled).toBe(true);
      expect(container.querySelector('[role="listbox"]')).toBeNull();
      await act(async () =>
        stale.resolve({
          items: [{ id: 71, name: "stale-user", deleted: false }],
          next_cursor: null,
        }),
      );
      await advance(30000);
      expect(api).toHaveBeenCalledTimes(2);
      expect(
        paths().filter((path) => path.startsWith("/usage-record-options?")),
      ).toHaveLength(1);
      expect(container.textContent).not.toContain("stale-user");
      if (state === "hidden") await setVisibility("visible");
      else if (state === "unfocused") await setFocus(true);
      else await renderView();
      expect(button("用户筛选").disabled).toBe(false);
      expect(container.querySelector('[role="listbox"]')).toBeNull();
    },
  );

  it("shows compact caches and latency bands while retaining exact cache counts and TPS in details", async () => {
    const row = record(100, {
      cache_read_tokens: 127100,
      cache_creation_tokens: 1250000,
      output_tokens: 120,
      first_token_ms: 10000,
      duration_ms: 70000,
    });
    serve((path) => (path === "/usage-records/100" ? row : page([row])));
    await renderView();
    const body = container.querySelector("tbody")!;
    expect(body.querySelector(".record-cache-read")?.textContent).toBe(
      "127.1K",
    );
    expect(
      body.querySelector(".record-cache-read")?.getAttribute("title"),
    ).toBe("缓存读取：127,100");
    expect(body.querySelector(".record-cache-write")?.textContent).toBe("1.3M");
    expect(
      body.querySelector(".record-cache-write")?.getAttribute("title"),
    ).toBe("缓存写入：1,250,000");
    expect(
      [...body.querySelectorAll(".latency-warn")].map(
        (node) => node.textContent,
      ),
    ).toEqual(["10.00s", "70.00s"]);
    expect(body.querySelector(".record-tps")?.textContent).toBe("2.00 tok/s");
    await act(async () =>
      container
        .querySelector<HTMLButtonElement>('[title="查看记录 #100"]')!
        .click(),
    );
    expect(detailValue("缓存读取")).toBe("127,100");
    expect(detailValue("缓存写入")).toBe("1,250,000");
    expect(detailValue("TPS")).toBe("2.00 tok/s");
  });

  it("stops for hidden, offline, and background states and resumes immediately without losing rows", async () => {
    await renderView();
    await setVisibility("hidden");
    await advance(30000);
    expect(api).toHaveBeenCalledTimes(1);
    await setVisibility("visible");
    expect(api).toHaveBeenCalledTimes(2);
    await renderView({ online: false });
    expect(container.textContent).toContain("连接已断开，显示已读取记录");
    expect(button("刷新记录").disabled).toBe(true);
    expect(tableIds()).toEqual(["查看记录 #100", "查看记录 #99"]);
    await advance(30000);
    expect(api).toHaveBeenCalledTimes(2);
    await renderView();
    expect(api).toHaveBeenCalledTimes(3);
    await renderView({ foreground: false });
    await advance(30000);
    expect(api).toHaveBeenCalledTimes(3);
    await renderView();
    expect(api).toHaveBeenCalledTimes(4);
  });

  it("does not start a request while initially hidden and cancels a hidden in-flight result", async () => {
    visibility = "hidden";
    await renderView();
    expect(api).not.toHaveBeenCalled();
    const stale = deferred<RecordPage>();
    serve(() => stale.promise);
    await setVisibility("visible");
    expect(api).toHaveBeenCalledTimes(1);
    await setVisibility("hidden");
    await act(async () => stale.resolve(page([record(200)])));
    expect(tableIds()).toEqual([]);
    serve(() => page([record(201)]));
    await setVisibility("visible");
    expect(tableIds()).toEqual(["查看记录 #201"]);
  });

  it("retains scroll and detail while announcing new records, then explicitly refreshes to the top", async () => {
    let heads = 0,
      count = 2;
    serve((path) => {
      if (path === "/usage-records/99") return record(99);
      if (params(path).has("after_id")) return { new_count: count };
      return page(
        ++heads === 1 ? [record(100), record(99)] : [record(103), record(102)],
      );
    });
    await renderView();
    await scrollTo(240);
    await advance(10000);
    expect(container.querySelector(".records-scroll")?.scrollTop).toBe(240);
    expect(tableIds()).toEqual(["查看记录 #100", "查看记录 #99"]);
    expect(button("2 条新记录")).toBeDefined();
    expect(heads).toBe(1);
    await scrollTo(0);
    await act(async () =>
      container
        .querySelector<HTMLButtonElement>('[title="查看记录 #99"]')!
        .click(),
    );
    count = 3;
    await advance(10000);
    expect(container.querySelector("[role=dialog]")?.textContent).toContain(
      "#99",
    );
    expect(button("3 条新记录")).toBeDefined();
    expect(heads).toBe(1);
    await click("关闭详情");
    await scrollTo(240);
    await click("3 条新记录");
    expect(container.querySelector(".records-scroll")?.scrollTop).toBe(0);
    expect(container.querySelector(".new-records")).toBeNull();
    expect(tableIds()).toEqual(["查看记录 #103", "查看记录 #102"]);
  });

  it("closes an in-flight detail when applying a query and never resurrects the old result", async () => {
    const stale = deferred<UsageRecord>();
    serve((path) => {
      if (path === "/usage-records/100") return stale.promise;
      return page([
        record(params(path).get("model") === "next-model" ? 200 : 100),
      ]);
    });
    await renderView();
    await act(async () =>
      container
        .querySelector<HTMLButtonElement>('[title="查看记录 #100"]')!
        .click(),
    );
    expect(container.querySelector("[role=dialog]")?.textContent).toContain(
      "正在读取记录",
    );
    await setInput("模型筛选", "next-model");
    await click("筛选");
    expect(container.querySelector("[role=dialog]")).toBeNull();
    await act(async () => stale.resolve(record(100)));
    expect(container.querySelector("[role=dialog]")).toBeNull();
    expect(tableIds()).toEqual(["查看记录 #200"]);
    expect(params(paths().at(-1)!).get("model")).toBe("next-model");
  });

  it.each(["scroll", "detail"])(
    "preserves the current window when %s starts during an automatic head request",
    async (action) => {
      const replacement = deferred<RecordPage>();
      let heads = 0;
      serve((path) => {
        if (path === "/usage-records/100") return record(100);
        if (params(path).has("after_id")) return { new_count: 2 };
        return ++heads === 1
          ? page([record(100), record(99)])
          : replacement.promise;
      });
      await renderView();
      await advance(10000);
      expect(heads).toBe(2);
      if (action === "scroll") await scrollTo(240);
      else
        await act(async () =>
          container
            .querySelector<HTMLButtonElement>('[title="查看记录 #100"]')!
            .click(),
        );
      await act(async () =>
        replacement.resolve(
          page([record(102), record(101)], { summary: { actual_cost: "999" } }),
        ),
      );
      expect(tableIds()).toEqual(["查看记录 #100", "查看记录 #99"]);
      expect(summary()?.textContent).toBe("$12.345679");
      expect(button("2 条新记录")).toBeDefined();
      if (action === "scroll")
        expect(container.querySelector(".records-scroll")?.scrollTop).toBe(240);
      else
        expect(container.querySelector("[role=dialog]")?.textContent).toContain(
          "#100",
        );
    },
  );

  it("offers the default columns and persists visibility through the supplied preferences callback", async () => {
    await renderView();
    const headers = () =>
      [...container.querySelectorAll("th")].map((th) => th.textContent);
    expect(headers()).toEqual([
      "用户",
      "API 密钥",
      "账户",
      "模型",
      "推理强度",
      "Token",
      "费用",
      "延迟",
      "User-Agent",
      "IP",
      "时间",
    ]);
    const toggle = (name: string) =>
      [
        ...container.querySelectorAll<HTMLLabelElement>(
          ".records-column-options label",
        ),
      ]
        .find((label) => label.querySelector("span")?.textContent === name)!
        .querySelector<HTMLInputElement>("input")!;
    await act(async () => toggle("端点").click());
    const withEndpoint = [...defaultRecordColumns, "endpoint"];
    expect(saveColumns).toHaveBeenLastCalledWith(withEndpoint);
    await renderView({ columns: withEndpoint });
    expect(headers()).toContain("端点");
    await act(async () => toggle("User-Agent").click());
    const saved = withEndpoint.filter((key) => key !== "user_agent");
    expect(saveColumns).toHaveBeenLastCalledWith(saved);
    await renderView({ columns: saved });
    expect(headers()).not.toContain("User-Agent");
    expect(toggle("User-Agent").checked).toBe(false);
    expect(toggle("端点").checked).toBe(true);
    expect(saveColumns).toHaveBeenCalledTimes(2);
  });

  it("migrates saved type-column preferences without losing the user's remaining columns", async () => {
    await renderView({ columns: ["type", "model", "cost", "user_agent"] });
    expect(
      [...container.querySelectorAll("th")].map((node) => node.textContent),
    ).toEqual(["用户", "模型", "费用", "User-Agent", "时间"]);
    expect(container.querySelector('[aria-label="请求类型"]')).toBeNull();
    const options = [
      ...container.querySelectorAll<HTMLLabelElement>(
        ".records-column-options label",
      ),
    ];
    expect(
      options.some(
        (label) => label.querySelector("span")?.textContent === "类型",
      ),
    ).toBe(false);
    expect(
      options.some((label) =>
        ["用户", "时间"].includes(
          label.querySelector("span")?.textContent || "",
        ),
      ),
    ).toBe(false);
    await act(async () =>
      options
        .find((label) => label.querySelector("span")?.textContent === "端点")!
        .querySelector<HTMLInputElement>("input")!
        .click(),
    );
    expect(saveColumns).toHaveBeenLastCalledWith([
      "model",
      "cost",
      "user_agent",
      "endpoint",
    ]);
  });

  it.each([{ columns: [] }, { columns: ["type"] }])(
    "preserves a saved selection with no remaining optional columns: $columns",
    async ({ columns }) => {
      await renderView({ columns });
      expect(
        [...container.querySelectorAll("th")].map((node) => node.textContent),
      ).toEqual(["用户", "时间"]);
      expect(saveColumns).not.toHaveBeenCalled();
    },
  );

  it("keeps the legacy request type in details after removing the table column and type filter", async () => {
    const row = record(100, { request_type: "ws_v2" });
    serve((path) => (path === "/usage-records/100" ? row : page([row])));
    await renderView();
    expect(container.querySelector(".record-col-type")).toBeNull();
    expect(container.querySelector('[aria-label="请求类型"]')).toBeNull();
    await act(async () =>
      container
        .querySelector<HTMLButtonElement>('[title="查看记录 #100"]')!
        .click(),
    );
    expect(detailValue("类型")).toBe("WebSocket");
  });

  it("blocks overlapping column writes until the pending preference save settles", async () => {
    const pending = deferred<void>();
    saveColumns.mockReturnValue(pending.promise);
    await renderView();
    const toggles = [
      ...container.querySelectorAll<HTMLInputElement>(
        ".records-column-options input",
      ),
    ];
    await act(async () => toggles[0].click());
    expect(toggles.every((toggle) => toggle.disabled)).toBe(true);
    await act(async () => toggles[1].click());
    expect(saveColumns).toHaveBeenCalledTimes(1);
    await act(async () => pending.resolve());
    expect(toggles.every((toggle) => !toggle.disabled)).toBe(true);
  });

  it("keeps column choices open for internal actions and closes them on outside pointer or Escape", async () => {
    await renderView();
    const menu = container.querySelector<HTMLDetailsElement>(
      ".records-column-menu",
    )!;
    const summary = menu.querySelector("summary")!;
    await act(async () => summary.click());
    expect(menu.open).toBe(true);
    await act(async () =>
      menu
        .querySelector("input")!
        .dispatchEvent(new Event("pointerdown", { bubbles: true })),
    );
    expect(menu.open).toBe(true);
    await act(async () =>
      container
        .querySelector("form")!
        .dispatchEvent(new Event("pointerdown", { bubbles: true })),
    );
    expect(menu.open).toBe(false);
    await act(async () => summary.click());
    expect(menu.open).toBe(true);
    await act(async () =>
      summary.dispatchEvent(
        new KeyboardEvent("keydown", { key: "Escape", bubbles: true }),
      ),
    );
    expect(menu.open).toBe(false);
  });

  it("reports a rejected column preference write while retaining the supplied selection", async () => {
    saveColumns.mockRejectedValue(new Error("保存列失败"));
    await renderView();
    await act(async () =>
      container
        .querySelector<HTMLInputElement>(".records-column-options input")!
        .click(),
    );
    expect(container.querySelector("[role=alert]")?.textContent).toContain(
      "保存列失败",
    );
    expect(container.querySelector(".record-col-api_key")).not.toBeNull();
    expect(
      container.querySelector<HTMLInputElement>(
        ".records-column-options input",
      )!.checked,
    ).toBe(true);
  });

  it("shows request/forward/response and reasoning differences, retaining exact detail money and zero values", async () => {
    const row = record(100);
    serve((path) => (path === "/usage-records/100" ? row : page([row])));
    await renderView();
    const cells = container.querySelector("tbody")!;
    expect(cells.querySelector(".record-col-model")?.textContent).toContain(
      "request-model",
    );
    expect(cells.querySelector(".record-col-model")?.textContent).toContain(
      "forward-model",
    );
    expect(cells.querySelector(".record-col-model")?.textContent).toContain(
      "returned-model",
    );
    expect(cells.querySelector(".route-mismatch")?.textContent).toBe(
      "返回差异",
    );
    expect(cells.querySelector(".record-col-reasoning")?.textContent).toBe(
      "max↳ high",
    );
    expect(cells.querySelector('[title="用户实扣"]')?.textContent).toBe(
      "$9007199254740993.123457",
    );
    expect(cells.querySelector('[title="账户费用"]')?.textContent).toBe(
      "A $0.000000",
    );
    await act(async () =>
      container
        .querySelector<HTMLButtonElement>('[title="查看记录 #100"]')!
        .click(),
    );
    expect(detailValue("请求")).toBe("request-model");
    expect(detailValue("转发")).toBe("forward-model");
    expect(detailValue("返回")).toBe("returned-model");
    expect(detailValue("请求强度")).toBe("max");
    expect(detailValue("实际强度")).toBe("high");
    expect(detailValue("用户实扣")).toBe("$9007199254740993.1234567890");
    expect(detailValue("账户费用")).toBe("$0");
    expect(detailValue("输入费用")).toBe("$0");
    expect(detailValue("输出费用")).toBe("—");
    expect(detailValue("输出")).toBe("0");
    expect(detailValue("缓存读取")).toBe("—");
    expect(detailValue("首字延迟")).toBe("0.00s");
    expect(detailValue("总耗时")).toBe("—");
    expect(detailValue("用户倍率")).toBe("0");
    expect(detailValue("账户倍率")).toBe("0");
    expect(row.actual_cost).toBe("9007199254740993.1234567890");
  });

  it.each([
    { requested: null, actual: null, display: "—" },
    { requested: "max", actual: null, display: "max" },
    { requested: null, actual: "high", display: "high" },
  ])(
    "does not fabricate missing response or reasoning fields: $requested / $actual",
    async ({ requested, actual, display }) => {
      const row = record(100, {
        requested_model: null,
        model: "legacy-model",
        upstream_model: null,
        upstream_response_model: null,
        model_mapping_chain: null,
        upstream_model_mismatch: false,
        requested_reasoning_effort: requested,
        reasoning_effort: actual,
        actual_cost: null,
        account_cost: null,
      });
      serve((path) => (path === "/usage-records/100" ? row : page([row])));
      await renderView();
      expect(
        container.querySelector("tbody .record-col-model")?.textContent,
      ).toBe("legacy-model");
      expect(
        container.querySelector("tbody .record-col-reasoning")?.textContent,
      ).toBe(display);
      expect(container.querySelector("tbody .route-mismatch")).toBeNull();
      await act(async () =>
        container
          .querySelector<HTMLButtonElement>('[title="查看记录 #100"]')!
          .click(),
      );
      expect(detailValue("请求")).toBe("legacy-model");
      expect(detailValue("转发")).toBe("legacy-model");
      expect(detailValue("返回")).toBe("—");
      expect(detailValue("请求强度")).toBe(requested || "—");
      expect(detailValue("实际强度")).toBe(actual || "—");
      expect(detailValue("用户实扣")).toBe("—");
    },
  );
});
