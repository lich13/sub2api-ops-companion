// @vitest-environment jsdom
import React, { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { api } from "./bridge";
import RecordFilter from "./RecordFilter";
import type { RecordOption, RecordOptionPage } from "./records";

vi.mock("./bridge", () => ({ api: vi.fn() }));
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

let container: HTMLDivElement, root: Root, unmounted: boolean;
const onChange = vi.fn<(value: RecordOption | null) => void>();
const user = (
  id: number,
  fields: Partial<RecordOption> = {},
): RecordOption => ({
  id,
  name: `user-${id}`,
  email: `user-${id}@example.invalid`,
  status: "active",
  deleted: false,
  ...fields,
});
const page = (
  items: RecordOption[],
  next_cursor: string | null = null,
): RecordOptionPage => ({ items, next_cursor });
const paths = () => vi.mocked(api).mock.calls.map(([, path]) => path);
const params = (path: string) =>
  new URL(path, "https://fixture.invalid").searchParams;
const options = () => [
  ...container.querySelectorAll<HTMLButtonElement>('[role="option"]'),
];
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
type Props = Partial<React.ComponentProps<typeof RecordFilter>>;
async function renderFilter(props: Props = {}, connection = "connection-a") {
  await act(async () =>
    root.render(
      <React.StrictMode>
        <RecordFilter
          key={connection}
          kind="users"
          active
          value={null}
          onChange={onChange}
          {...props}
        />
      </React.StrictMode>,
    ),
  );
}
async function click(label: string) {
  const button = [
    ...container.querySelectorAll<HTMLButtonElement>("button"),
  ].find(
    (node) =>
      node.getAttribute("aria-label") === label || node.textContent === label,
  );
  if (!button) throw new Error(`Missing button: ${label}`);
  await act(async () => button.click());
}
async function advance(ms: number) {
  await act(async () => vi.advanceTimersByTimeAsync(ms));
}
async function open(label = "用户筛选") {
  await click(label);
  await advance(0);
}
async function search(value: string) {
  await act(async () => {
    const input =
      container.querySelector<HTMLInputElement>('[role="combobox"]')!;
    Object.getOwnPropertyDescriptor(
      HTMLInputElement.prototype,
      "value",
    )!.set!.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}
async function press(key: string) {
  await act(async () =>
    container
      .querySelector('[role="combobox"]')!
      .dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true })),
  );
}
async function bottom() {
  await act(async () => {
    const list = container.querySelector<HTMLDivElement>('[role="listbox"]')!;
    Object.defineProperties(list, {
      scrollHeight: { configurable: true, value: 1000 },
      clientHeight: { configurable: true, value: 200 },
    });
    list.scrollTop = 790;
    list.dispatchEvent(new Event("scroll", { bubbles: true }));
    list.dispatchEvent(new Event("scroll", { bubbles: true }));
  });
}

beforeEach(() => {
  vi.resetAllMocks();
  vi.useFakeTimers();
  unmounted = false;
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  serve(() => page([user(1), user(2)]));
});
afterEach(async () => {
  if (!unmounted) await act(async () => root.unmount());
  container.remove();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("record filter directory", () => {
  it("reads only on open, once in StrictMode, without ten-second polling", async () => {
    await renderFilter();
    await advance(60000);
    expect(api).not.toHaveBeenCalled();
    await open();
    expect(paths()).toEqual(["/usage-record-options?kind=users&limit=50"]);
    expect(options()).toHaveLength(3);
    expect(document.activeElement).toBe(
      container.querySelector('[role="combobox"]'),
    );
    await advance(60000);
    expect(api).toHaveBeenCalledTimes(1);
    expect(vi.mocked(api).mock.calls[0][0]).toBe("GET");
  });

  it("loads more than one hundred directory entries and deduplicates page overlap", async () => {
    serve((path) => {
      const cursor = params(path).get("cursor");
      if (cursor === "page-3")
        return page(Array.from({ length: 26 }, (_, i) => user(i + 100)));
      if (cursor === "page-2")
        return page(
          Array.from({ length: 50 }, (_, i) => user(i + 50)),
          "page-3",
        );
      return page(
        Array.from({ length: 50 }, (_, i) => user(i + 1)),
        "page-2",
      );
    });
    await renderFilter();
    await open();
    expect(options()).toHaveLength(51);
    await bottom();
    expect(options()).toHaveLength(100);
    await bottom();
    expect(options()).toHaveLength(126);
    expect(
      options().filter((node) => node.textContent?.includes("user-50#50")),
    ).toHaveLength(1);
    expect(options().at(-1)?.textContent).toContain("user-125#125");
    expect(paths().map((path) => params(path).get("cursor"))).toEqual([
      null,
      "page-2",
      "page-3",
    ]);
    expect(
      paths().every(
        (path) =>
          path.startsWith("/usage-record-options?") &&
          params(path).get("limit") === "50",
      ),
    ).toBe(true);
    await bottom();
    expect(api).toHaveBeenCalledTimes(3);
    expect(container.querySelector(".record-filter-more")).toBeNull();
  });

  it("debounces the latest trimmed search for 250ms and starts its own paging window", async () => {
    serve((path) =>
      params(path).get("q")
        ? page([user(90, { name: "李 & 李" })])
        : page([user(1)], "old-cursor"),
    );
    await renderFilter();
    await open();
    await search("李");
    await advance(200);
    await search("  李 & 李  ");
    await advance(249);
    expect(api).toHaveBeenCalledTimes(1);
    expect(options()).toHaveLength(1);
    await advance(1);
    expect(api).toHaveBeenCalledTimes(2);
    expect(params(paths()[1]).get("q")).toBe("李 & 李");
    expect(params(paths()[1]).has("cursor")).toBe(false);
    expect(options().at(-1)?.textContent).toContain("李 & 李#90");
  });

  it("retries failed initial and paginated requests without losing loaded options", async () => {
    let initial = 0,
      more = 0;
    serve((path) => {
      if (!params(path).has("cursor")) {
        if (++initial === 1) throw new Error("directory unavailable");
        return page([user(1)], "next-page");
      }
      if (++more === 1) throw new Error("page unavailable");
      return page([user(2)]);
    });
    await renderFilter();
    await open();
    expect(container.querySelector('[role="alert"]')?.textContent).toContain(
      "directory unavailable",
    );
    await click("重试");
    expect(options()).toHaveLength(2);
    expect(container.querySelector('[role="alert"]')).toBeNull();
    await bottom();
    expect(container.querySelector('[role="alert"]')?.textContent).toContain(
      "page unavailable",
    );
    expect(options()[1].textContent).toContain("user-1");
    await bottom();
    expect(api).toHaveBeenCalledTimes(3);
    await click("重试");
    expect(options()).toHaveLength(3);
    expect(paths().map((path) => params(path).get("cursor"))).toEqual([
      null,
      null,
      "next-page",
      "next-page",
    ]);
  });

  it("serializes slow requests and discards old search pages and queued superseded searches", async () => {
    const initial = deferred<RecordOptionPage>();
    let inFlight = 0,
      maximum = 0;
    serve(async (path) => {
      ++inFlight;
      maximum = Math.max(maximum, inFlight);
      try {
        return params(path).get("q")
          ? page([user(3, { name: "latest" })])
          : await initial.promise;
      } finally {
        --inFlight;
      }
    });
    await renderFilter();
    await open();
    await search("superseded");
    await advance(250);
    await search("latest");
    await advance(250);
    expect(api).toHaveBeenCalledTimes(1);
    await act(async () =>
      initial.resolve(page([user(1, { name: "stale" })], "stale-cursor")),
    );
    expect(maximum).toBe(1);
    expect(paths().map((path) => params(path).get("q"))).toEqual([
      null,
      "latest",
    ]);
    expect(container.textContent).not.toContain("stale");
    expect(options()).toHaveLength(2);
    expect(options()[1].textContent).toContain("latest#3");
    expect(container.querySelector(".record-filter-more")).toBeNull();
  });

  it("discards an old error when the API-key owner changes and scopes the next request", async () => {
    const stale = deferred<RecordOptionPage>();
    serve((path) =>
      params(path).get("user_id") === "7"
        ? stale.promise
        : page([
            user(22, {
              name: "key-22",
              email: null,
              user_id: 8,
              user_name: "owner-eight",
            }),
          ]),
    );
    await renderFilter({ kind: "api_keys", userId: 7 });
    await open("API 密钥筛选");
    await renderFilter({ kind: "api_keys", userId: 8 });
    await advance(0);
    expect(api).toHaveBeenCalledTimes(1);
    await act(async () => stale.reject(new Error("old owner failed")));
    expect(paths().map((path) => params(path).get("user_id"))).toEqual([
      "7",
      "8",
    ]);
    expect(
      paths().every((path) => params(path).get("kind") === "api_keys"),
    ).toBe(true);
    expect(container.querySelector('[role="alert"]')).toBeNull();
    expect(options()[1].textContent).toContain("owner-eight #8");
  });

  it("replaces stale user results when the directory kind changes", async () => {
    const stale = deferred<RecordOptionPage>();
    serve((path) =>
      params(path).get("kind") === "users"
        ? stale.promise
        : page([user(80, { name: "api-key", user_id: 7 })]),
    );
    await renderFilter({ userId: 7 });
    await open();
    expect(params(paths()[0]).has("user_id")).toBe(false);
    await renderFilter({ kind: "api_keys", userId: 7 });
    await advance(0);
    await act(async () => stale.resolve(page([user(1)])));
    expect(
      container.querySelector('[aria-label="搜索API 密钥"]'),
    ).not.toBeNull();
    expect(options()[1].textContent).toContain("api-key#80");
    expect(options()).toHaveLength(2);
    expect(params(paths()[1]).get("user_id")).toBe("7");
  });

  it("closes and drops in-flight results while inactive, then reads fresh on reopening", async () => {
    const stale = deferred<RecordOptionPage>();
    let count = 0;
    serve(() => (++count === 1 ? stale.promise : page([user(9)])));
    await renderFilter();
    await open();
    await renderFilter({ active: false });
    expect(container.querySelector('[role="listbox"]')).toBeNull();
    expect(container.querySelector<HTMLButtonElement>("button")!.disabled).toBe(
      true,
    );
    await act(async () => stale.resolve(page([user(1)])));
    await advance(60000);
    expect(api).toHaveBeenCalledTimes(1);
    await renderFilter();
    expect(container.querySelector('[role="listbox"]')).toBeNull();
    await open();
    expect(api).toHaveBeenCalledTimes(2);
    expect(options()[1].textContent).toContain("user-9#9");
  });

  it("cancels a pending search debounce on backgrounding", async () => {
    await renderFilter();
    await open();
    await search("cancelled");
    await advance(249);
    await renderFilter({ active: false });
    await advance(10000);
    expect(api).toHaveBeenCalledTimes(1);
    expect(container.querySelector('[role="listbox"]')).toBeNull();
    await click("用户筛选");
    expect(container.querySelector('[role="listbox"]')).toBeNull();
  });

  it("discards old connection responses after remounting with a new connection key", async () => {
    const stale = deferred<RecordOptionPage>();
    let count = 0;
    serve(() => (++count === 1 ? stale.promise : page([user(200)])));
    await renderFilter({}, "connection-a");
    await open();
    await renderFilter({}, "connection-b");
    await open();
    expect(options()[1].textContent).toContain("user-200#200");
    await act(async () => stale.resolve(page([user(100)])));
    expect(options()).toHaveLength(2);
    expect(options()[1].textContent).toContain("user-200#200");
    expect(onChange).not.toHaveBeenCalled();
  });

  it("ignores a pending result after unmount", async () => {
    const stale = deferred<RecordOptionPage>();
    serve(() => stale.promise);
    await renderFilter();
    await open();
    await act(async () => root.unmount());
    unmounted = true;
    await act(async () => stale.resolve(page([user(100)])));
    await advance(60000);
    expect(container.textContent).toBe("");
    expect(api).toHaveBeenCalledTimes(1);
    expect(onChange).not.toHaveBeenCalled();
  });

  it.each(["12345", " #12345 "])(
    "accepts historical ID %s absent from the current directory",
    async (query) => {
      serve(() => page([]));
      await renderFilter({ kind: "api_keys", userId: 7 });
      await open("API 密钥筛选");
      await search(query);
      await advance(250);
      expect(options()[1].textContent).toContain("按 ID #12345 筛选");
      await press("Enter");
      expect(onChange).toHaveBeenLastCalledWith({
        id: 12345,
        name: null,
        deleted: false,
        user_id: 7,
      });
      expect(container.querySelector('[role="listbox"]')).toBeNull();
    },
  );

  it.each(["0", "-1", "1.5", "1e3", "9007199254740992", "##12", "abc"])(
    "rejects invalid historical ID %s",
    async (query) => {
      serve(() => page([]));
      await renderFilter();
      await open();
      await search(query);
      await advance(250);
      expect(options()).toHaveLength(1);
      await press("Enter");
      expect(onChange).not.toHaveBeenCalled();
    },
  );

  it("uses the actual option rather than duplicating a found numeric ID", async () => {
    const option = user(123, { deleted: true, status: "disabled" });
    serve(() => page([option]));
    await renderFilter();
    await open();
    await search("#123");
    await advance(250);
    expect(options()).toHaveLength(2);
    expect(container.textContent).not.toContain("按 ID");
    expect(options()[1].textContent).toContain("已删除");
    await press("Enter");
    expect(onChange).toHaveBeenLastCalledWith(option);
  });

  it("shows owner and disabled status, preserves the selected value, and clears to all", async () => {
    const option = user(12, {
      name: "project-key",
      email: null,
      user_id: 7,
      user_email: "owner@example.invalid",
      status: "disabled",
    });
    serve(() => page([option]));
    await renderFilter({ kind: "api_keys", value: option });
    expect(container.querySelector("button")?.textContent).toBe(
      "project-key #12",
    );
    await open("API 密钥筛选");
    expect(options()[1].getAttribute("aria-selected")).toBe("true");
    expect(options()[1].textContent).toContain("owner@example.invalid #7");
    expect(options()[1].textContent).toContain("停用");
    await click("全部API 密钥");
    expect(onChange).toHaveBeenLastCalledWith(null);
  });

  it("closes a paginated directory on Tab and Shift+Tab without selecting or preventing normal tab navigation", async () => {
    serve(() => page([user(1), user(2)], "next-page"));
    await renderFilter({ value: user(2) });
    for (const shiftKey of [false, true]) {
      await open();
      expect(container.querySelector(".record-filter-more")).not.toBeNull();
      await press("ArrowDown");
      await press("ArrowDown");
      const input = container.querySelector('[role="combobox"]')!;
      expect(
        document.getElementById(input.getAttribute("aria-activedescendant")!)
          ?.textContent,
      ).toContain("user-1#1");
      const event = new KeyboardEvent("keydown", {
        key: "Tab",
        shiftKey,
        bubbles: true,
        cancelable: true,
      });
      await act(async () => input.dispatchEvent(event));
      expect(event.defaultPrevented).toBe(false);
      expect(container.querySelector('[role="listbox"]')).toBeNull();
      expect(onChange).not.toHaveBeenCalled();
      const trigger = container.querySelector('[aria-label="用户筛选"]');
      expect(trigger?.textContent).toBe("user-2 #2");
      expect(document.activeElement).toBe(trigger);
    }
  });

  it("supports keyboard selection and Escape without changing the selection", async () => {
    await renderFilter();
    await open();
    await press("ArrowDown");
    await press("ArrowDown");
    const input = container.querySelector('[role="combobox"]')!;
    expect(
      document.getElementById(input.getAttribute("aria-activedescendant")!)
        ?.textContent,
    ).toContain("user-1#1");
    await press("Enter");
    expect(onChange).toHaveBeenLastCalledWith(user(1));
    await open();
    await press("Escape");
    expect(container.querySelector('[role="listbox"]')).toBeNull();
    expect(onChange).toHaveBeenCalledTimes(1);
    expect(document.activeElement).toBe(container.querySelector("button"));
  });
});
