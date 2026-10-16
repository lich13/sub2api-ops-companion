// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { api, command, runTest } from "./bridge";
import TestDialog from "./TestDialog";
import { QuotaRefresh } from "./AccountControls";
import type { Account, TestEvent } from "./types";

vi.mock("./bridge", () => ({ api: vi.fn(), command: vi.fn(), runTest: vi.fn() }));
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
let container: HTMLDivElement, root: Root, mounted: boolean;
beforeEach(() => { vi.resetAllMocks(); container = document.createElement("div"); document.body.append(container); root = createRoot(container); mounted = true; });
afterEach(async () => { if (mounted) await act(async () => root.unmount()); container.remove(); vi.useRealTimers(); });
const button = (text: string) => [...container.querySelectorAll("button")].find((b) => b.textContent?.includes(text))!;

describe("test interaction", () => {
  it("starts from the dialog without confirmation and preserves streamed whitespace", async () => {
    vi.mocked(api).mockResolvedValue([{ id: "gpt-6-sol", display_name: "GPT-6 Sol", type: "text" }]);
    let finish: () => void = () => {};
    const pending = new Promise<void>((resolve) => { finish = resolve; });
    const parts = ["Hello", " ", "world!\n", "\t", "  code\n\n", "中文 👋", " "];
    vi.mocked(runTest).mockImplementation(async (_id, _payload, emit) => {
      for (const text of parts) emit({ type: "content", text });
      await pending; emit({ type: "test_complete", success: true });
    });
    await act(async () => root.render(<TestDialog account={{id:1,name:"账号",platform:"openai",version:"a".repeat(64)} as Account} online close={vi.fn()}/>));
    expect(container.querySelector('input[type="checkbox"]')).toBeNull();
    expect(runTest).not.toHaveBeenCalled();
    const start = button("开始测试");
    expect(start.disabled).toBe(false);
    await act(async () => { start.click(); start.click(); });
    expect(runTest).toHaveBeenCalledTimes(1);
    expect(vi.mocked(runTest).mock.calls[0][1]).not.toHaveProperty("confirmed");
    expect(container.querySelector("pre")?.textContent).toBe(parts.join(""));
    await act(async () => { button("取消").click(); });
    expect(command).toHaveBeenCalledWith("cancel_test");
    await act(async () => { finish(); await pending; });
    expect(runTest).toHaveBeenCalledTimes(1);
  });

  it("does not report success when the stream ends without a successful terminal event", async () => {
    vi.mocked(api).mockResolvedValue([{ id: "gpt-6-sol", display_name: "GPT-6 Sol", type: "text" }]);
    vi.mocked(runTest).mockImplementation(async () => {});
    await act(async () => root.render(<TestDialog account={{id:1,name:"Fixture account",platform:"openai",version:"fixture-version"} as Account} online close={vi.fn()}/>));

    await act(async () => {
      button("开始测试").click();
      await Promise.resolve();
      await Promise.resolve();
    });

    expect(runTest).toHaveBeenCalledOnce();
    expect(container.textContent).not.toContain("测试通过");
    expect(container.querySelector('[role="alert"]')?.textContent).toBe(
      "连接结束，未收到完成结果",
    );
  });

  it("ignores late test events after the dialog unmounts", async () => {
    vi.mocked(api).mockResolvedValue([{ id: "gpt-6-sol", display_name: "GPT-6 Sol", type: "text" }]);
    let emit: (event: TestEvent) => void = () => {};
    let finish!: () => void;
    const pending = new Promise<void>((resolve) => { finish = resolve; });
    vi.mocked(runTest).mockImplementation(async (_id, _payload, receive) => {
      emit = receive;
      await pending;
    });
    await act(async () => root.render(<TestDialog account={{id:1,name:"Fixture account",platform:"openai",version:"fixture-version"} as Account} online close={vi.fn()}/>));

    await act(async () => {
      button("开始测试").click();
      await Promise.resolve();
    });
    expect(runTest).toHaveBeenCalledOnce();

    await act(async () => root.unmount());
    mounted = false;
    expect(command).toHaveBeenCalledWith("cancel_test");

    await act(async () => {
      emit({ type: "content", text: "late event" });
      emit({ type: "test_complete", success: true });
      finish();
      await pending;
      await Promise.resolve();
    });

    expect(container.textContent).toBe("");
  });
});

describe("quota batch polling", () => {
  it("pauses a running mobile batch in background and reads actual status on return", async () => {
    vi.useFakeTimers();
    vi.mocked(api).mockResolvedValue({id:"batch",status:"running",items:[],total:2,completed:0});
    const report = vi.fn();
    await act(async () => root.render(<QuotaRefresh online active report={report}/>));
    await act(async () => root.render(<QuotaRefresh online active={false} report={report}/>));
    const paused = vi.mocked(api).mock.calls.length;
    await act(async () => { await vi.advanceTimersByTimeAsync(5000); });
    expect(api).toHaveBeenCalledTimes(paused);
    vi.mocked(api).mockResolvedValue({id:"batch",status:"completed",items:[],total:2,completed:2});
    await act(async () => root.render(<QuotaRefresh online active report={report}/>));
    expect(vi.mocked(api).mock.calls.length).toBeGreaterThan(paused);
    expect(vi.mocked(api).mock.calls.every(([method]) => method === "GET")).toBe(true);
    expect(command).toHaveBeenCalledWith("refresh");
  });
  it("reads once when idle, polls only a running batch, and stops on completion", async () => {
    vi.useFakeTimers();
    const idle = {status:"idle",items:[],total:0,completed:0};
    const active = {id:"batch",status:"running",items:[],total:2,completed:0};
    vi.mocked(api).mockResolvedValue(idle);
    await act(async () => root.render(<QuotaRefresh online report={vi.fn()}/>));
    expect(api).toHaveBeenCalledTimes(1);
    await act(async () => { await vi.advanceTimersByTimeAsync(5000); });
    expect(api).toHaveBeenCalledTimes(1);
    vi.mocked(api).mockResolvedValue(active);
    await act(async () => { button("刷新全部").click(); button("刷新全部").click(); });
    expect(vi.mocked(api).mock.calls.filter(([method]) => method === "POST")).toHaveLength(1);
    vi.mocked(api).mockResolvedValue({...active,status:"completed",completed:2});
    await act(async () => { await vi.advanceTimersByTimeAsync(1000); });
    const calls = vi.mocked(api).mock.calls.length;
    await act(async () => { await vi.advanceTimersByTimeAsync(5000); });
    expect(api).toHaveBeenCalledTimes(calls);
    expect(command).toHaveBeenCalledWith("refresh");
  });
});
