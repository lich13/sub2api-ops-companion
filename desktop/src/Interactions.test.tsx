// @vitest-environment jsdom
import React, { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { api, command } from "./bridge";
import TestDialog from "./TestDialog";
import { QuotaRefresh } from "./AccountControls";
import { usageRefreshEvent } from "./UsageCell";
import { setOperations } from "./accountOperations";
import type { Account, TestEvent } from "./types";

vi.mock("./bridge", () => ({ api: vi.fn(), command: vi.fn() }));
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
let container: HTMLDivElement, root: Root, mounted: boolean;
beforeEach(() => {
  vi.resetAllMocks();
  setOperations([]);
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  mounted = true;
});
afterEach(async () => {
  if (mounted) await act(async () => root.unmount());
  container.remove();
  vi.useRealTimers();
});
const button = (text: string) =>
  [...container.querySelectorAll("button")].find((b) =>
    b.textContent?.includes(text),
  )!;

describe("test interaction", () => {
  const account: Account = {
    id: 1,
    name: "Fixture account",
    priority: 1,
    platform: "openai",
    type: "oauth",
    status: "active",
    schedulable: true,
    available: true,
    group_ids: [],
    blockers: [],
    version: "a".repeat(64),
    operation_versions: { test: "b".repeat(64) },
    last_success_at: null,
    last_error_at: null,
    last_error_id: null,
    last_error_code: null,
    last_error_status: null,
    error_message: "",
    success_after_error: false,
    usage_windows: [],
  };
  const id = "c".repeat(32);
  const job = (status: string, events: TestEvent[] = []) => ({
    id,
    account_id: 1,
    account_name: account.name,
    action: "test",
    status,
    requested: {},
    events,
    next_event: events.length,
  });
  const posts = () =>
    vi
      .mocked(api)
      .mock.calls.filter(
        ([method, path]) =>
          method === "POST" && path === "/accounts/1/operations",
      );
  const cancels = () =>
    vi.mocked(api).mock.calls.filter(([, path]) => path.endsWith("/cancel"));
  function serve(submit: () => unknown, poll?: () => unknown) {
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (path === "/account-operations") return { items: [] } as never;
      if (path.endsWith("/models?purpose=model_test"))
        return [
          { id: "gpt-6-sol", display_name: "GPT-6 Sol", type: "text" },
        ] as never;
      if (method === "POST" && path === "/accounts/1/operations")
        return submit() as never;
      if (method === "POST" && path === `/account-operations/${id}/cancel`)
        return job("cancelled") as never;
      if (
        method === "GET" &&
        poll &&
        path.startsWith(`/account-operations/${id}?after_event=`)
      )
        return poll() as never;
      throw new Error(`Unexpected fixture request: ${method} ${path}`);
    });
  }
  const open = () =>
    act(async () =>
      root.render(<TestDialog account={account} online close={vi.fn()} />),
    );

  it("starts from the dialog without confirmation and preserves streamed whitespace", async () => {
    vi.useFakeTimers();
    const parts = [
      "Hello",
      " ",
      "world!\n",
      "\t",
      "  code\n\n",
      "中文 👋",
      " ",
    ];
    const polled: string[] = [];
    serve(
      () =>
        job(
          "running",
          parts.map((text) => ({ type: "content", text })),
        ),
      () => {
        polled.push(vi.mocked(api).mock.lastCall![1]);
        return { ...job("cancelled"), next_event: parts.length };
      },
    );
    await open();
    expect(container.querySelector('input[type="checkbox"]')).toBeNull();
    expect(posts()).toHaveLength(0);
    const start = button("开始测试");
    expect(start.disabled).toBe(false);
    await act(async () => {
      start.click();
      start.click();
    });
    expect(posts()).toHaveLength(1);
    expect(posts()[0][2]).toMatchObject({
      action: "test",
      expected_version: "b".repeat(64),
      payload: { mode: "default", confirmed: true },
    });
    expect(container.querySelector("pre")?.textContent).toBe(parts.join(""));
    await act(async () => {
      button("取消").click();
    });
    expect(api).toHaveBeenCalledWith(
      "POST",
      `/account-operations/${id}/cancel`,
      {},
    );
    await act(async () => {
      await vi.advanceTimersByTimeAsync(500);
    });
    expect(polled).toEqual([
      `/account-operations/${id}?after_event=${parts.length}`,
    ]);
    expect(container.querySelector(".test-status")?.textContent).toBe(
      "已取消，未重放请求",
    );
    expect(container.querySelector('[role="alert"]')).toBeNull();
    expect(posts()).toHaveLength(1);
  });

  it("reports the cancellation when the task id has not arrived yet", async () => {
    let accept!: (value: unknown) => void;
    serve(
      () =>
        new Promise((resolve) => {
          accept = resolve;
        }),
    );
    await open();
    await act(async () => {
      button("开始测试").click();
    });
    await act(async () => {
      button("取消").click();
    });
    await act(async () => {
      accept(job("completed", [{ type: "test_complete", success: true }]));
    });
    expect(container.querySelector(".test-status")?.textContent).toBe(
      "已取消，未重放请求",
    );
    expect(container.querySelector('[role="alert"]')).toBeNull();
    expect(cancels()).toHaveLength(0);
    expect(posts()).toHaveLength(1);
  });

  it("does not report success when the task completes without a terminal test event", async () => {
    serve(() => job("completed"));
    await open();
    await act(async () => {
      button("开始测试").click();
    });
    expect(posts()).toHaveLength(1);
    expect(container.textContent).not.toContain("测试通过");
    expect(container.querySelector('[role="alert"]')?.textContent).toBe(
      "连接结束，未收到完成结果",
    );
  });

  it("ignores late test events after the dialog unmounts and leaves the queued task running", async () => {
    vi.useFakeTimers();
    let polls = 0;
    serve(
      () => job("running"),
      () => {
        ++polls;
        return job("completed", [
          { type: "content", text: "late event" },
          { type: "test_complete", success: true },
        ]);
      },
    );
    await open();
    await act(async () => {
      button("开始测试").click();
    });
    expect(posts()).toHaveLength(1);

    await act(async () => root.unmount());
    mounted = false;
    await act(async () => {
      await vi.advanceTimersByTimeAsync(500);
    });

    expect(polls).toBe(1);
    expect(container.textContent).toBe("");
    expect(cancels()).toHaveLength(0);
  });
});

describe("quota batch polling", () => {
  it("pauses a running mobile batch in background and reads actual status on return", async () => {
    vi.useFakeTimers();
    vi.mocked(api).mockResolvedValue({
      id: "batch",
      status: "running",
      items: [],
      total: 2,
      completed: 0,
    });
    const report = vi.fn();
    await act(async () =>
      root.render(<QuotaRefresh online active report={report} />),
    );
    await act(async () =>
      root.render(<QuotaRefresh online active={false} report={report} />),
    );
    const paused = vi.mocked(api).mock.calls.length;
    await act(async () => {
      await vi.advanceTimersByTimeAsync(5000);
    });
    expect(api).toHaveBeenCalledTimes(paused);
    vi.mocked(api).mockResolvedValue({
      id: "batch",
      status: "completed",
      items: [],
      total: 2,
      completed: 2,
    });
    await act(async () =>
      root.render(<QuotaRefresh online active report={report} />),
    );
    expect(vi.mocked(api).mock.calls.length).toBeGreaterThan(paused);
    expect(
      vi.mocked(api).mock.calls.every(([method]) => method === "GET"),
    ).toBe(true);
    expect(command).toHaveBeenCalledWith("refresh");
  });
  it("reads once when idle, polls only a running batch, and stops on completion", async () => {
    vi.useFakeTimers();
    const idle = { status: "idle", items: [], total: 0, completed: 0 };
    const active = {
      id: "batch",
      status: "running",
      items: [],
      total: 2,
      completed: 0,
    };
    vi.mocked(api).mockResolvedValue(idle);
    await act(async () =>
      root.render(<QuotaRefresh online report={vi.fn()} />),
    );
    expect(api).toHaveBeenCalledTimes(1);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(5000);
    });
    expect(api).toHaveBeenCalledTimes(1);
    vi.mocked(api).mockResolvedValue(active);
    await act(async () => {
      button("刷新全部").click();
      button("刷新全部").click();
    });
    expect(
      vi.mocked(api).mock.calls.filter(([method]) => method === "POST"),
    ).toHaveLength(1);
    vi.mocked(api).mockResolvedValue({
      ...active,
      status: "completed",
      completed: 2,
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(1000);
    });
    const calls = vi.mocked(api).mock.calls.length;
    await act(async () => {
      await vi.advanceTimersByTimeAsync(5000);
    });
    expect(api).toHaveBeenCalledTimes(calls);
    expect(command).toHaveBeenCalledWith("refresh");
  });
  it("does not emit false refresh completion when switching connections during a running batch", async () => {
    vi.useFakeTimers();
    vi.mocked(api).mockResolvedValueOnce({
      id: "batch-a",
      status: "running",
      items: [],
      total: 4,
      completed: 1,
      started_at: "2026-10-01T00:00:00Z",
    });
    const report = vi.fn();
    await act(async () =>
      root.render(
        <QuotaRefresh online active connectionKey="conn-a" report={report} />,
      ),
    );
    expect(command).not.toHaveBeenCalled();

    vi.mocked(api).mockResolvedValueOnce({
      id: "batch-b",
      status: "completed",
      items: [],
      total: 2,
      completed: 2,
      started_at: "2026-10-01T00:00:00Z",
    });
    await act(async () =>
      root.render(
        <QuotaRefresh online active connectionKey="conn-b" report={report} />,
      ),
    );
    expect(command).not.toHaveBeenCalled();
  });
  it("preserves initial running state and continues 1s polling under React.StrictMode", async () => {
    vi.useFakeTimers();
    const report = vi.fn();
    let step = 0;
    vi.mocked(api).mockImplementation(async () => {
      step += 1;
      if (step <= 2) {
        return {
          id: "strict-batch",
          status: "running",
          items: [],
          total: 3,
          completed: 1,
          started_at: "2026-10-01T00:00:00Z",
        };
      }
      if (step === 3) {
        return {
          id: "strict-batch",
          status: "running",
          items: [],
          total: 3,
          completed: 2,
          started_at: "2026-10-01T00:00:00Z",
        };
      }
      return {
        id: "strict-batch",
        status: "completed",
        items: [],
        total: 3,
        completed: 3,
        started_at: "2026-10-01T00:00:00Z",
        completed_at: "2026-10-01T00:00:02Z",
      };
    });
    await act(async () =>
      root.render(
        <React.StrictMode>
          <QuotaRefresh
            online
            active
            connectionKey="conn-strict"
            report={report}
          />
        </React.StrictMode>,
      ),
    );
    expect(container.textContent).toContain("1/3 查询中");
    expect(button("刷新全部").disabled).toBe(true);
    expect(command).not.toHaveBeenCalled();

    await act(async () => {
      await vi.advanceTimersByTimeAsync(1000);
    });
    expect(container.textContent).toContain("2/3 查询中");
    expect(command).not.toHaveBeenCalled();

    await act(async () => {
      await vi.advanceTimersByTimeAsync(1000);
    });
    expect(container.textContent).toContain("3/3 已完成");
    expect(button("刷新全部").disabled).toBe(false);
    expect(command).toHaveBeenCalledTimes(1);
    expect(command).toHaveBeenCalledWith("refresh");

    const settledCalls = vi.mocked(api).mock.calls.length;
    await act(async () => {
      await vi.advanceTimersByTimeAsync(5000);
    });
    expect(vi.mocked(api)).toHaveBeenCalledTimes(settledCalls);
  });
  it("notifies completion on immediate terminal POST, ignores pre-POST GET, and isolates A->B->A in-flight POSTs", async () => {
    vi.useFakeTimers();
    const report = vi.fn();
    let resolveInitialGetA!: (value: unknown) => void;
    let resolvePostA!: (value: unknown) => void;
    let getCount = 0;
    vi.mocked(api).mockImplementation((method) => {
      if (method === "GET") {
        getCount += 1;
        if (getCount === 1) {
          return new Promise((resolve) => {
            resolveInitialGetA = resolve;
          });
        }
        return Promise.resolve({
          id: "batch-fresh",
          status: "completed",
          items: [],
          total: 4,
          completed: 4,
          started_at: "2026-10-01T00:00:00Z",
          completed_at: "2026-10-01T00:00:01Z",
        });
      }
      return new Promise((resolve) => {
        resolvePostA = resolve;
      });
    });

    const events: CustomEvent[] = [];
    const onRefreshEvent = (event: Event) => events.push(event as CustomEvent);
    document.addEventListener(usageRefreshEvent, onRefreshEvent);
    try {
      await act(async () =>
        root.render(
          <QuotaRefresh online active connectionKey="conn-a" report={report} />,
        ),
      );
      expect(button("刷新全部").disabled).toBe(false);

      // Click manual refresh while initial GET on conn-a is still in flight; POST resolves immediately as completed.
      await act(async () => {
        button("刷新全部").click();
      });
      expect(button("刷新全部").disabled).toBe(true);
      await act(async () => {
        resolvePostA({
          id: "post-immediate",
          status: "completed",
          items: [],
          total: 2,
          completed: 2,
          started_at: "2026-10-01T00:00:00Z",
          completed_at: "2026-10-01T00:00:00Z",
        });
      });
      expect(container.textContent).toContain("2/2 已完成");
      expect(command).toHaveBeenCalledTimes(1);
      expect(command).toHaveBeenCalledWith("refresh");
      expect(events).toHaveLength(1);
      expect(events[0]?.detail.connectionKey).toBe("conn-a");

      // Late pre-POST GET returning idle must not overwrite the completed POST result.
      await act(async () => {
        resolveInitialGetA({
          status: "idle",
          items: [],
          total: 0,
          completed: 0,
        });
      });
      expect(container.textContent).toContain("2/2 已完成");

      // Start another slow POST on conn-a, then switch conn-a -> conn-b -> conn-a before it settles.
      await act(async () => {
        button("刷新全部").click();
      });
      expect(button("刷新全部").disabled).toBe(true);

      await act(async () =>
        root.render(
          <QuotaRefresh online active connectionKey="conn-b" report={report} />,
        ),
      );
      // conn-b must not inherit conn-a's starting lock.
      expect(button("刷新全部").disabled).toBe(false);

      await act(async () =>
        root.render(
          <QuotaRefresh online active connectionKey="conn-a" report={report} />,
        ),
      );
      expect(container.textContent).toContain("4/4 已完成");

      // Stale POST from the earlier conn-a visit resolves as running and must be ignored.
      await act(async () => {
        resolvePostA({
          id: "stale-post-a",
          status: "running",
          items: [],
          total: 4,
          completed: 0,
          started_at: "2026-10-01T00:00:00Z",
        });
      });
      expect(container.textContent).toContain("4/4 已完成");
      expect(button("刷新全部").disabled).toBe(false);
    } finally {
      document.removeEventListener(usageRefreshEvent, onRefreshEvent);
    }
  });
});
