// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { api, command } from "./bridge";
import TestDialog from "./TestDialog";
import { QuotaRefresh } from "./AccountControls";
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
  const account = {
    id: 1,
    name: "Fixture account",
    platform: "openai",
    version: "a".repeat(64),
    operation_versions: { test: "b".repeat(64) },
  } as unknown as Account;
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
});
