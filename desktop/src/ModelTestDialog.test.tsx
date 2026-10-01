// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import ModelTestDialog from "./ModelTestDialog";
import { api } from "./bridge";
import type { Account } from "./types";

vi.mock("./bridge", () => ({ api: vi.fn() }));
vi.mock("./mobile", () => ({ useBackAction: vi.fn() }));

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

const account: Account = {
  id: 387,
  name: "yx",
  priority: 50,
  platform: "openai",
  type: "oauth",
  status: "active",
  schedulable: true,
  available: true,
  group_ids: [13],
  blockers: [],
  managed: false,
  version: "a".repeat(64),
  last_success_at: null,
  last_error_at: null,
  last_error_id: null,
  last_error_code: null,
  last_error_status: null,
  error_message: "",
  success_after_error: false,
  usage_windows: [],
};

let root: Root;
let container: HTMLDivElement;

beforeEach(() => {
  vi.clearAllMocks();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  vi.mocked(api).mockImplementation(async (_method, path) => {
    if (path === "/accounts/387/models?purpose=model_test") {
      return ["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna", "gpt-6.1-sol", "gpt-6-sol", "gpt-6-luna", "gpt-6-astra", "codex-auto-review"].map((id) => ({ id, display_name: id, type: "model" })) as never;
    }
    if (path === "/accounts/387/model-tests/latest") return null as never;
    throw new Error(`unexpected ${path}`);
  });
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
});

describe("ModelTestDialog candidates", () => {
  it("requests and renders the exact group whitelist candidates", async () => {
    await act(async () => root.render(<ModelTestDialog account={account} online close={() => {}} report={() => {}} />));
    await act(async () => Promise.resolve());
    expect(vi.mocked(api).mock.calls[0]).toEqual(["GET", "/accounts/387/models?purpose=model_test"]);
    expect(container.querySelectorAll('select[aria-label="测试模型"] option')).toHaveLength(8);
    expect(container.querySelector("button")?.hasAttribute("disabled")).toBe(false);
  });

  it("disables start and offers retry when candidate loading fails", async () => {
    vi.mocked(api).mockRejectedValueOnce(new Error("目录失败"));
    await act(async () => root.render(<ModelTestDialog account={account} online close={() => {}} report={() => {}} />));
    await act(async () => Promise.resolve());
    expect(container.querySelector('[role="alert"]')?.textContent).toContain("目录失败");
    const start = [...container.querySelectorAll("button")].find((button) => button.textContent?.includes("开始测试"));
    expect(start).toBeTruthy();
    expect(start).toHaveProperty("disabled", true);
    const retry = [...container.querySelectorAll("button")].find((button) => button.textContent?.includes("重试"));
    expect(retry).toBeTruthy();
    await act(async () => retry!.click());
    expect(vi.mocked(api).mock.calls.some((call) => call[1] === "/accounts/387/models?purpose=model_test")).toBe(true);
  });

  it("submits the selected concurrency and preserves the exact requested model", async () => {
    const save = vi.fn();
    await act(async () => root.render(<ModelTestDialog account={account} online close={() => {}} report={() => {}} concurrency={2} saveConcurrency={save}/>));
    const slots = container.querySelector<HTMLSelectElement>('select[aria-label="测试并发"]')!;
    expect(slots.value).toBe("2");
    await act(async () => { slots.value = "3"; slots.dispatchEvent(new Event("change", { bubbles: true })); });
    expect(save).toHaveBeenCalledWith(3);
    vi.mocked(api).mockResolvedValueOnce({ id: "f".repeat(32), status: "completed", requested_model: "gpt-5.6-sol", forwarded_model: "gpt-5.6-sol", returned_models: [], completed_groups: 3, valid_groups: 0, attempts: 3, duration_ms: 12 } as never);
    const start = [...container.querySelectorAll("button")].find((button) => button.textContent?.includes("开始测试"))!;
    await act(async () => start.click());
    expect(vi.mocked(api).mock.calls.find(([method]) => method === "POST")?.[2]).toMatchObject({ model_id: "gpt-5.6-sol", concurrency: 3 });
  });

  it("allows a conflicted task to be cancelled", async () => {
    const original = vi.mocked(api).getMockImplementation()!;
    const job = { id: "f".repeat(32), status: "needs_confirmation", requested_model: "gpt-6-luna", forwarded_model: "gpt-6-luna", returned_models: [], completed_groups: 0, valid_groups: 0, attempts: 0, duration_ms: 0 };
    vi.mocked(api).mockImplementation(async (method, path, body) => {
      if (path.endsWith("/latest")) return job as never;
      if (path.endsWith("/cancel")) return { ...job, status: "cancelled" } as never;
      return original(method, path, body);
    });
    await act(async () => root.render(<ModelTestDialog account={account} online close={() => {}} report={() => {}}/>));
    const cancel = [...container.querySelectorAll("button")].find((button) => button.textContent === "取消任务")!;
    expect(cancel).toBeTruthy();
    await act(async () => cancel.click());
    expect(container.textContent).toContain("已停止");
    expect(vi.mocked(api).mock.calls.filter(([method]) => method === "POST")).toHaveLength(1);
  });
});
