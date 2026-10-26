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
  id: 101,
  name: "Fixture account",
  priority: 50,
  platform: "openai",
  type: "oauth",
  status: "active",
  schedulable: true,
  available: true,
  group_ids: [13],
  blockers: [],
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
    if (path === "/modeltrace/fingerprint-bank") return fingerprintBank as never;
    if (path === "/accounts/101/models?purpose=model_test") {
      return ["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna", "gpt-6.1-sol", "gpt-6-sol", "gpt-6-luna", "gpt-6-astra", "codex-auto-review"].map((id) => ({ id, display_name: id, type: "model" })) as never;
    }
    if (path === "/accounts/101/model-tests/latest") return null as never;
    throw new Error(`unexpected ${path}`);
  });
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.useRealTimers();
});

describe("ModelTestDialog candidates", () => {
  it("requests and renders the current account upstream directory candidates", async () => {
    await act(async () => root.render(<ModelTestDialog account={account} online close={() => {}} report={() => {}} />));
    await act(async () => Promise.resolve());
    expect(vi.mocked(api).mock.calls).toContainEqual(["GET", "/accounts/101/models?purpose=model_test"]);
    expect(container.querySelectorAll('select[aria-label="测试模型"] option')).toHaveLength(8);
    expect(container.querySelector("button")?.hasAttribute("disabled")).toBe(false);
  });

  it("keeps the header and actions outside the scrollable content", async () => {
    await act(async () => root.render(<ModelTestDialog account={account} online close={() => {}} report={() => {}} />));
    const dialog = container.querySelector<HTMLElement>(".model-test-dialog")!;
    const content = dialog.children[1];
    expect(dialog.firstElementChild?.tagName).toBe("HEADER");
    expect(dialog.lastElementChild?.tagName).toBe("FOOTER");
    expect(content.contains(container.querySelector('select[aria-label="测试模型"]'))).toBe(true);
    expect(content.contains(container.querySelector(".fingerprint-status"))).toBe(true);
  });

  it("disables start and offers retry when candidate loading fails", async () => {
    vi.mocked(api).mockImplementation(async (_method, path) => {
      if (path === "/modeltrace/fingerprint-bank") return fingerprintBank as never;
      if (path === "/accounts/101/models?purpose=model_test") throw new Error("目录失败");
      if (path === "/accounts/101/model-tests/latest") return {
        id: "f".repeat(32), status: "completed", requested_model: "retired-model", forwarded_model: "retired-model",
        returned_models: [], completed_groups: 3, valid_groups: 0, attempts: 3, duration_ms: 200,
      } as never;
      throw new Error("unexpected " + path);
    });
    await act(async () => root.render(<ModelTestDialog account={account} online close={() => {}} report={() => {}} />));
    await act(async () => Promise.resolve());
    expect(container.querySelector('[role="alert"]')?.textContent).toContain("目录失败");
    const model = container.querySelector<HTMLSelectElement>('select[aria-label="测试模型"]')!;
    expect(model.value).toBe("retired-model");
    expect(model.textContent).not.toContain("当前不可用");
    expect(container.textContent).not.toContain("所选模型不在当前账号上游模型目录中");
    const start = [...container.querySelectorAll("button")].find((button) => button.textContent?.includes("重新测试") || button.textContent?.includes("开始测试"));
    expect(start).toBeTruthy();
    expect(start).toHaveProperty("disabled", true);
    const retry = [...container.querySelectorAll("button")].find((button) => button.textContent?.includes("重试"));
    expect(retry).toBeTruthy();
    await act(async () => retry!.click());
    expect(vi.mocked(api).mock.calls.some((call) => call[1] === "/accounts/101/models?purpose=model_test")).toBe(true);
  });

  it("keeps an unavailable latest model visible and prevents starting a test", async () => {
    const original = vi.mocked(api).getMockImplementation()!;
    const latest = {
      id: "f".repeat(32), status: "completed", requested_model: "retired-model", forwarded_model: "retired-model",
      returned_models: [], completed_groups: 3, valid_groups: 0, attempts: 3, duration_ms: 200,
    };
    vi.mocked(api).mockImplementation(async (method, path, body) => {
      if (path.endsWith("/latest")) return latest as never;
      if (path === "/accounts/101/models?purpose=model_test") return [{ id: "current-model", display_name: "Current model", type: "model" }] as never;
      return original(method, path, body);
    });

    await act(async () => root.render(<ModelTestDialog account={account} online close={() => {}} report={() => {}} />));
    await settle();

    const model = container.querySelector<HTMLSelectElement>('select[aria-label="测试模型"]')!;
    expect(model.value).toBe("retired-model");
    expect(model.textContent).toContain("retired-model（当前不可用）");
    expect(container.textContent).toContain("当前账号上游模型目录");
    const start = [...container.querySelectorAll<HTMLButtonElement>("button")].find((button) => button.textContent?.includes("重新测试") || button.textContent?.includes("开始测试"))!;
    expect(start.disabled).toBe(true);
    await act(async () => start.click());
    expect(vi.mocked(api).mock.calls.some(([method, path]) => method === "POST" && path === "/accounts/101/model-tests")).toBe(false);
  });

  it("keeps the selected draft when a candidate reload fails", async () => {
    await act(async () => root.render(<ModelTestDialog account={account} online close={() => {}} report={() => {}} />));
    await settle();
    const model = container.querySelector<HTMLSelectElement>('select[aria-label="测试模型"]')!;
    await act(async () => {
      model.value = "gpt-6-astra";
      model.dispatchEvent(new Event("change", { bubbles: true }));
    });

    const original = vi.mocked(api).getMockImplementation()!;
    vi.mocked(api).mockImplementation(async (method, path, body) => {
      if (path === "/accounts/101/models?purpose=model_test") throw new Error("目录暂不可用");
      return original(method, path, body);
    });
    await act(async () => root.render(<ModelTestDialog account={account} online={false} close={() => {}} report={() => {}} />));
    const offlineModel = container.querySelector<HTMLSelectElement>('select[aria-label="测试模型"]')!;
    expect(offlineModel.textContent).not.toContain("当前不可用");
    expect(container.textContent).not.toContain("所选模型不在当前账号上游模型目录中");
    await act(async () => root.render(<ModelTestDialog account={account} online close={() => {}} report={() => {}} />));
    await settle();

    const reloadedModel = container.querySelector<HTMLSelectElement>('select[aria-label="测试模型"]')!;
    expect(reloadedModel.value).toBe("gpt-6-astra");
    expect(reloadedModel.textContent).not.toContain("当前不可用");
    expect(container.textContent).not.toContain("所选模型不在当前账号上游模型目录中");
    expect(container.textContent).toContain("目录暂不可用");
  });

  it("waits for a candidate reread before marking a saved model unavailable", async () => {
    const latest = {
      id: "f".repeat(32), status: "completed", requested_model: "retired-model", forwarded_model: "retired-model",
      returned_models: [], completed_groups: 3, valid_groups: 0, attempts: 3, duration_ms: 200,
    };
    let reload = false;
    let resolveModels!: (value: unknown) => void;
    const pendingModels = new Promise((resolve) => { resolveModels = resolve; });
    vi.mocked(api).mockImplementation(async (_method, path) => {
      if (path === "/modeltrace/fingerprint-bank") return fingerprintBank as never;
      if (path === "/accounts/101/model-tests/latest") return latest as never;
      if (path === "/accounts/101/models?purpose=model_test") {
        return (reload ? pendingModels : [{ id: "current-model", display_name: "Current model", type: "model" }]) as never;
      }
      throw new Error("unexpected " + path);
    });

    await act(async () => root.render(<ModelTestDialog account={account} online close={() => {}} report={() => {}} />));
    await settle();
    reload = true;
    await act(async () => root.render(<ModelTestDialog account={account} online={false} close={() => {}} report={() => {}} />));
    await act(async () => root.render(<ModelTestDialog account={account} online close={() => {}} report={() => {}} />));
    await settle();

    const loadingModel = container.querySelector<HTMLSelectElement>('select[aria-label="测试模型"]')!;
    expect(loadingModel.value).toBe("retired-model");
    expect(loadingModel.textContent).not.toContain("当前不可用");
    expect(container.textContent).not.toContain("所选模型不在当前账号上游模型目录中");
    const start = [...container.querySelectorAll<HTMLButtonElement>("button")]
      .find((button) => button.textContent?.includes("重新测试") || button.textContent?.includes("开始测试"))!;
    expect(start.disabled).toBe(true);

    await act(async () => resolveModels([]));
    await settle();
    const emptyModel = container.querySelector<HTMLSelectElement>('select[aria-label="测试模型"]')!;
    expect(emptyModel.textContent).toContain("retired-model（当前不可用）");
    expect(container.textContent).toContain("所选模型不在当前账号上游模型目录中");
    expect(start.disabled).toBe(true);
  });

  it("ignores a late candidate response after switching accounts", async () => {
    const otherAccount = { ...account, id: 202, name: "Other fixture account", version: "b".repeat(64) };
    let resolveOldModels!: (value: unknown) => void;
    const oldModels = new Promise((resolve) => { resolveOldModels = resolve; });
    vi.mocked(api).mockImplementation(async (_method, path) => {
      if (path === "/modeltrace/fingerprint-bank") return fingerprintBank as never;
      if (path === "/accounts/101/models?purpose=model_test") return oldModels as never;
      if (path === "/accounts/101/model-tests/latest") return null as never;
      if (path === "/accounts/202/models?purpose=model_test") return [{ id: "new-account-model", display_name: "New account model", type: "model" }] as never;
      if (path === "/accounts/202/model-tests/latest") return null as never;
      throw new Error("unexpected " + path);
    });

    await act(async () => root.render(<ModelTestDialog account={account} online close={() => {}} report={() => {}} />));
    await act(async () => root.render(<ModelTestDialog account={otherAccount} online close={() => {}} report={() => {}} />));
    await settle();
    const model = container.querySelector<HTMLSelectElement>('select[aria-label="测试模型"]')!;
    expect(container.textContent).toContain("Other fixture account");
    expect(model.value).toBe("new-account-model");

    await act(async () => resolveOldModels([{ id: "stale-account-model", display_name: "Stale account model", type: "model" }]));
    await settle();
    expect(model.value).toBe("new-account-model");
    expect(model.textContent).not.toContain("stale-account-model");
  });

  it("ignores a late poll response after going offline and reconnecting", async () => {
    vi.useFakeTimers();
    const original = vi.mocked(api).getMockImplementation()!;
    const job = {
      id: "poll-job-101", status: "running", requested_model: "gpt-6-luna", forwarded_model: "gpt-6-luna",
      returned_models: [], completed_groups: 0, valid_groups: 0, attempts: 1, duration_ms: 0,
    };
    let poll = 0;
    let resolveLate!: (value: unknown) => void;
    const latePoll = new Promise((resolve) => { resolveLate = resolve; });
    vi.mocked(api).mockImplementation(async (method, path, body) => {
      if (path.endsWith("/latest")) return job as never;
      if (path === `/model-tests/${job.id}`) {
        poll += 1;
        if (poll === 1) return latePoll as never;
        return { ...job, status: "completed", completed_groups: 3, valid_groups: 3, attempts: 3, duration_ms: 300 } as never;
      }
      return original(method, path, body);
    });

    let online = true;
    await act(async () => root.render(<ModelTestDialog account={account} online={online} close={() => {}} report={() => {}} />));
    await settle();
    await act(async () => { await vi.advanceTimersByTimeAsync(400); });
    expect(poll).toBe(1);
    online = false;
    await act(async () => root.render(<ModelTestDialog account={account} online={online} close={() => {}} report={() => {}} />));
    online = true;
    await act(async () => root.render(<ModelTestDialog account={account} online={online} close={() => {}} report={() => {}} />));
    await settle();
    await act(async () => { await vi.advanceTimersByTimeAsync(400); });
    expect(poll).toBe(2);
    expect(container.querySelector(".model-test-status")?.textContent).toContain("已完成");

    await act(async () => resolveLate({ ...job, status: "running", completed_groups: 0, valid_groups: 0 }));
    await settle();
    expect(container.querySelector(".model-test-status")?.textContent).toContain("已完成");
  });

  it("continues polling a running test when a failed model directory is retried", async () => {
    vi.useFakeTimers();
    const original = vi.mocked(api).getMockImplementation()!;
    const job = {
      id: "fixture-running", status: "running", requested_model: "gpt-6-luna", forwarded_model: "gpt-6-luna",
      returned_models: [], completed_groups: 0, valid_groups: 0, attempts: 1, duration_ms: 0,
    };
    let reads = 0;
    let polls = 0;
    vi.mocked(api).mockImplementation(async (method, path, body) => {
      if (path.endsWith("/latest")) return job as never;
      if (path === "/accounts/101/models?purpose=model_test" && ++reads === 1) throw new Error("目录失败");
      if (path === `/model-tests/${job.id}`) {
        polls += 1;
        return (polls === 1 ? job : { ...job, status: "completed", completed_groups: 3, valid_groups: 3 }) as never;
      }
      return original(method, path, body);
    });
    await act(async () => root.render(<ModelTestDialog account={account} online close={vi.fn()} report={vi.fn()}/>));
    await settle();
    await act(async () => { await vi.advanceTimersByTimeAsync(400); });
    expect(polls).toBe(1);
    const retry = [...container.querySelectorAll<HTMLButtonElement>("button")].find(button => button.textContent === "重试")!;
    await act(async () => retry.click());
    expect(reads).toBe(2);
    await act(async () => { await vi.advanceTimersByTimeAsync(2000); });
    expect(polls).toBe(2);
    expect(container.querySelector(".model-test-status")?.textContent).toContain("已完成");
    await act(async () => { await vi.advanceTimersByTimeAsync(4000); });
    expect(polls).toBe(2);
  });

  it.each(["switch", "close"] as const)("discards an outstanding poll after %s", async (action) => {
    vi.useFakeTimers();
    const original = vi.mocked(api).getMockImplementation()!;
    const job = { id: "fixture-old", status: "running", requested_model: "gpt-6-luna", returned_models: [], completed_groups: 0, valid_groups: 0, attempts: 1, duration_ms: 0 };
    let resolvePoll!: (value: unknown) => void;
    const pending = new Promise(resolve => { resolvePoll = resolve; });
    vi.mocked(api).mockImplementation(async (method, path, body) => {
      if (path === "/accounts/101/model-tests/latest") return job as never;
      if (path === `/model-tests/${job.id}`) return pending as never;
      if (path === "/accounts/202/models?purpose=model_test") return [{ id: "fixture-new", display_name: "New", type: "model" }] as never;
      if (path === "/accounts/202/model-tests/latest") return null as never;
      return original(method, path, body);
    });
    const report = vi.fn();
    await act(async () => root.render(<ModelTestDialog account={account} online close={() => root.render(null)} report={report}/>));
    await settle();
    await act(async () => { await vi.advanceTimersByTimeAsync(400); });
    if (action === "switch") {
      await act(async () => root.render(<ModelTestDialog account={{ ...account, id: 202 }} online close={vi.fn()} report={report}/>));
    } else {
      await act(async () => container.querySelector<HTMLButtonElement>('button[aria-label="关闭模型测试"]')!.click());
    }
    await settle();
    await act(async () => resolvePoll({ ...job, status: "completed" }));
    expect(container.querySelector(".model-test-status")).toBeNull();
    await act(async () => { await vi.advanceTimersByTimeAsync(4000); });
    expect(vi.mocked(api).mock.calls.filter(([, path]) => path === `/model-tests/${job.id}`)).toHaveLength(1);
    if (action === "switch") expect(container.querySelector<HTMLSelectElement>('select[aria-label="测试模型"]')!.value).toBe("fixture-new");
    else expect(container.textContent).toBe("");
    expect(report).not.toHaveBeenCalled();
  });

  it("ignores candidate responses after the dialog is unmounted", async () => {
    let resolveModels!: (value: unknown) => void;
    const pendingModels = new Promise((resolve) => { resolveModels = resolve; });
    const report = vi.fn();
    const close = vi.fn(() => root.render(null));
    vi.mocked(api).mockImplementation(async (_method, path) => {
      if (path === "/modeltrace/fingerprint-bank") return fingerprintBank as never;
      if (path === "/accounts/101/models?purpose=model_test") return pendingModels as never;
      if (path === "/accounts/101/model-tests/latest") return null as never;
      throw new Error("unexpected " + path);
    });
    await act(async () => root.render(<ModelTestDialog account={account} online close={close} report={report} />));
    await act(async () => container.querySelector<HTMLButtonElement>('button[aria-label="关闭模型测试"]')!.click());
    expect(close).toHaveBeenCalledOnce();
    await act(async () => resolveModels([{ id: "late-model", display_name: "Late model", type: "model" }]));
    await settle();
    expect(container.textContent).toBe("");
    expect(report).not.toHaveBeenCalled();
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

  it("restores the previous model and shows provider terminal diagnostics without launching a request", async () => {
    const original = vi.mocked(api).getMockImplementation()!;
    const job = { id: "f".repeat(32), status: "completed", requested_model: "gpt-6-luna", forwarded_model: "gpt-6-luna", returned_models: [], completed_groups: 3, valid_groups: 0, attempts: 3, duration_ms: 200,
      groups: [{ index: 1, status: "completed", attempts: 1, ttft_ms: null, duration_ms: 200, diagnostics: { first_event_type: "response.created", last_event_type: "response.incomplete", output_tokens: 4096, reasoning_tokens: 3890, max_output_tokens: 4096, end_reason: "max_output_tokens" } }] };
    vi.mocked(api).mockImplementation(async (method, path, body) => path.endsWith("/latest") ? job as never : original(method, path, body));
    await act(async () => root.render(<ModelTestDialog account={account} online close={() => {}} report={() => {}}/>));
    expect(container.querySelector<HTMLSelectElement>('select[aria-label="测试模型"]')!.value).toBe("gpt-6-luna");
    expect(container.textContent).toContain("response.incomplete");
    expect(container.textContent).toContain("4096 / 4096");
    expect(container.textContent).toContain("3890");
    expect(vi.mocked(api).mock.calls.every(([method]) => method === "GET")).toBe(true);
  });

  it("does not replace a user selection with a delayed previous result", async () => {
    const original = vi.mocked(api).getMockImplementation()!;
    let resolveLatest!: (value: unknown) => void;
    const delayed = new Promise((resolve) => { resolveLatest = resolve; });
    vi.mocked(api).mockImplementation(async (method, path, body) => path.endsWith("/latest") ? await delayed as never : original(method, path, body));
    await act(async () => root.render(<ModelTestDialog account={account} online close={() => {}} report={() => {}}/>));
    const model = container.querySelector<HTMLSelectElement>('select[aria-label="测试模型"]')!;
    await act(async () => { model.value = "gpt-6-astra"; model.dispatchEvent(new Event("change", { bubbles: true })); });
    await act(async () => resolveLatest({ id: "f".repeat(32), status: "completed", requested_model: "gpt-6-luna", forwarded_model: "gpt-6-luna", returned_models: [], completed_groups: 3, valid_groups: 0, attempts: 3, duration_ms: 200 }));
    expect(model.value).toBe("gpt-6-astra");
    expect(vi.mocked(api).mock.calls.every(([method]) => method === "GET")).toBe(true);
  });

  it("shows one-group verification progress and the final 1/1 result while polling an existing task", async () => {
    vi.useFakeTimers();
    const original = vi.mocked(api).getMockImplementation()!;
    const job = {
      id: "f".repeat(32), status: "running", requested_model: "gpt-6-luna", forwarded_model: "gpt-6-luna",
      returned_models: [], planned_groups: 1, completed_groups: 0, valid_groups: 0, attempts: 1, duration_ms: 0,
      groups: [{ index: 1, status: "running", attempts: 1, ttft_ms: null, duration_ms: null }],
    };
    let polls = 0;
    vi.mocked(api).mockImplementation(async (method, path, body) => {
      if (path.endsWith("/latest")) return job as never;
      if (path === `/model-tests/${job.id}`) {
        polls += 1;
        return {
          ...job, status: polls === 1 ? "handling" : "completed", completed_groups: 1, valid_groups: 1,
          completion_reason: "automatic_degradation", report: { used_outputs: 1, prediction_name: "Fixture prediction" },
          groups: [{ ...job.groups[0], status: "completed", duration_ms: 900 }],
        } as never;
      }
      return original(method, path, body);
    });
    await act(async () => root.render(<ModelTestDialog account={account} online close={() => {}} report={() => {}}/>));
    const validGroups = () => [...container.querySelectorAll("dt")].find((node) => node.textContent === "有效组数")?.nextElementSibling?.textContent;
    expect(container.querySelector(".model-test-status")?.textContent).toContain("0/1 组");
    expect(validGroups()).toBe("0/1");
    await act(async () => { await vi.advanceTimersByTimeAsync(400); });
    expect(container.querySelector(".model-test-status")?.textContent).toContain("1/1 组");
    expect(validGroups()).toBe("1/1");
    await act(async () => { await vi.advanceTimersByTimeAsync(400); });
    expect(container.querySelector(".model-test-status")?.textContent).toContain("首组命中降智模型");
    expect(validGroups()).toBe("1/1");
    expect(container.querySelectorAll(".model-test-groups > div")).toHaveLength(1);
    expect(container.textContent).not.toContain("1/3");
    expect(vi.mocked(api).mock.calls.every(([method]) => method === "GET")).toBe(true);
    await act(async () => { await vi.advanceTimersByTimeAsync(5000); });
    expect(polls).toBe(2);
  });

  it("shows only the recheck reason when a degraded account remains degraded", async () => {
    const original = vi.mocked(api).getMockImplementation()!;
    const job = {
      id: "e".repeat(32), status: "completed", requested_model: "gpt-6-luna", forwarded_model: "gpt-6-luna",
      returned_models: [], completed_groups: 1, valid_groups: 1, attempts: 1, duration_ms: 150,
      completion_reason: "automatic_degradation",
      automatic_disposition: {
        kind: "recheck", status: "still_degraded", mark_cleared: false,
        reason: "仍符合降智特征",
      },
    };
    vi.mocked(api).mockImplementation(async (method, path, body) => path.endsWith("/latest") ? job as never : original(method, path, body));
    await act(async () => root.render(<ModelTestDialog account={account} online close={() => {}} report={() => {}} />));
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(container.textContent).toContain("仍符合降智特征");
    expect(container.textContent).not.toContain("标记已保存");
    expect(container.textContent).not.toContain("标记待核对");
    expect(container.textContent).not.toContain("停调度");
  });
});
const fingerprintBank = {
  version: {
    revision: "fixture-revision",
    sha256: "fixture-sha256",
    built_at: "2026-10-06T00:00:00Z",
    analyzer_version: 1,
  },
  source: "bundled",
  status: "ready",
};

async function settle() {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}


describe("Key model test preference", () => {
  it("prefers the saved detection model over a completed historical test", async () => {
    const original = vi.mocked(api).getMockImplementation()!;
    vi.mocked(api).mockImplementation(async (method, path, body) => {
      if (path.endsWith("/model-detection")) return { model_override: "gpt-6-astra" } as never;
      if (path.endsWith("/latest")) return { id: "fixture-previous", status: "completed", requested_model: "gpt-6-luna", returned_models: [], completed_groups: 1, valid_groups: 1, duration_ms: 10 } as never;
      return original(method, path, body);
    });
    await act(async () => root.render(<ModelTestDialog account={{ ...account, type: "apikey" }} online close={() => {}} report={vi.fn()}/>));
    await settle();
    expect(container.querySelector<HTMLSelectElement>('select[aria-label="测试模型"]')!.value).toBe("gpt-6-astra");
    const start = [...container.querySelectorAll("button")].find(button => button.textContent?.includes("重新测试"))!;
    vi.mocked(api).mockResolvedValueOnce({ id: "fixture-new", status: "completed", requested_model: "gpt-6-astra", returned_models: [], completed_groups: 1, valid_groups: 1, duration_ms: 10 } as never);
    await act(async () => start.click());
    expect(vi.mocked(api).mock.calls.find(([method]) => method === "POST")?.[2]).toMatchObject({ model_id: "gpt-6-astra" });
  });

  it("preserves a manual model selection when the detection preference arrives late", async () => {
    const original = vi.mocked(api).getMockImplementation()!;
    let resolve!: (value: unknown) => void;
    const pending = new Promise(done => { resolve = done; });
    vi.mocked(api).mockImplementation(async (method, path, body) => path.endsWith("/model-detection") ? pending as never : original(method, path, body));
    await act(async () => root.render(<ModelTestDialog account={{ ...account, type: "apikey" }} online close={() => {}} report={vi.fn()}/>));
    const model = container.querySelector<HTMLSelectElement>('select[aria-label="测试模型"]')!;
    await act(async () => { model.value = "gpt-6-sol"; model.dispatchEvent(new Event("change", { bubbles: true })); });
    const start = [...container.querySelectorAll("button")].find(button => button.textContent?.includes("开始测试"))!;
    expect(start.disabled).toBe(true);
    await act(async () => resolve({ model_override: "gpt-6-astra" }));
    expect(model.value).toBe("gpt-6-sol");
    expect(start.disabled).toBe(false);
  });
});

describe("LM Detector manual preferences and reports", () => {
  it("keeps manual preferences apart from automatic history and saves only accepted submissions", async () => {
    const savePreference = vi.fn();
    let reject = true;
    const original = vi.mocked(api).getMockImplementation()!;
    const result = { id: "fixture-lm", status: "completed", requested_model: "gpt-6-sol", engine: "lm_detector", returned_models: [], completed_groups: 1, valid_groups: 1, attempts: 1, duration_ms: 10 };
    vi.mocked(api).mockImplementation(async (method, path, body) => {
      if (path.startsWith("/lm-detector/fingerprint-bank")) return fingerprintBank as never;
      if (path.endsWith("/latest")) return { ...result, automatic: true, planned_groups: 3 } as never;
      if (method === "POST" && path.endsWith("/model-tests")) { if (reject) throw new Error("fixture rejected"); return result as never; }
      return original(method, path, body);
    });
    const props = { account, online: true, close: vi.fn(), report: vi.fn(), preference: { engine: "modeltrace" as const, sample_count: 1 as const }, savePreference };
    await act(async () => root.render(<ModelTestDialog {...props}/>)); await settle();
    const source = container.querySelector<HTMLSelectElement>('select[aria-label="检测源"]')!;
    expect(source.value).toBe("modeltrace"); expect(savePreference).not.toHaveBeenCalled();
    await act(async () => { source.value = "lm_detector"; source.dispatchEvent(new Event("change", { bubbles: true })); });
    const count = container.querySelector<HTMLSelectElement>('select[aria-label="采样组数"]')!;
    await act(async () => { count.value = "3"; count.dispatchEvent(new Event("change", { bubbles: true })); container.querySelector<HTMLInputElement>('.lm-probe-option input')!.click(); });
    const submit = [...container.querySelectorAll("footer button")].find(b => b.textContent?.includes("重新测试")) as HTMLButtonElement;
    await act(async () => submit.click()); expect(savePreference).not.toHaveBeenCalled();
    reject = false; await act(async () => submit.click());
    expect(savePreference).toHaveBeenCalledWith({ engine: "lm_detector", sample_count: 3 });
    expect([...vi.mocked(api).mock.calls].reverse().find(([method, path]) => method === "POST" && path.endsWith("/model-tests"))?.[2]).toMatchObject({ engine: "lm_detector", sample_count: 3, tokenizer_probe: true, concurrency: 1 });
    await act(async () => root.render(<ModelTestDialog key="reopen" {...props} preference={{ engine: "lm_detector", sample_count: 3 }}/>)); await settle();
    expect(container.querySelector<HTMLInputElement>('.lm-probe-option input')!.checked).toBe(false);
  });
  it("shows only five candidates with three decimal scores and missing confidence", async () => {
    const original = vi.mocked(api).getMockImplementation()!;
    const names = ["gpt-6-astra/6.1-sol", "claude-opus-5", "gpt-6-sol", "grok-4", "deepseek-v4", "qwen-3"];
    vi.mocked(api).mockImplementation(async (method, path, body) => path.endsWith("/latest") ? {
      id: "fixture-lm", status: "completed", engine: "lm_detector", requested_model: "gpt-6-sol", returned_models: [], planned_groups: 1, completed_groups: 1, valid_groups: 1, attempts: 1, duration_ms: 10,
      report: { decision: "partial", prediction_name: names[0], merged_class: true, probability: null, used_outputs: 1, results: names.map((name, i) => ({ model: name, display_name: name, score: i === 1 ? null : 2.83452 - i * .1, probability: i === 2 ? .123456 : null })) },
    } as never : original(method, path, body));
    await act(async () => root.render(<ModelTestDialog account={account} online close={vi.fn()} report={vi.fn()}/>)); await settle();
    expect(container.querySelectorAll('.lm-candidates [role="row"]')).toHaveLength(6);
    expect(container.textContent).toContain("初步结果"); expect(container.textContent).toContain("合并类别，无法区分"); expect(container.textContent).toContain("2.835"); expect(container.textContent).not.toContain("0.0%");
    expect(container.textContent).toContain("12.35%");
    expect(container.querySelector(".lm-expand")).toBeNull();
    expect(container.textContent).not.toContain("qwen-3");
  });
});

describe("additional API Key platforms", () => {
  it.each(["anthropic", "grok", "deepseek"])("uses LM Detector and exposes safe retry for %s", async (platform) => {
    const latest = { id: "fixture-retry", account_id: account.id, status: "completed", engine: "lm_detector", requested_model: "gpt-6-sol", forwarded_model: "gpt-6-sol", returned_models: [], planned_groups: 3, completed_groups: 2, valid_groups: 2, attempts: 5, duration_ms: 10, can_retry: true };
    const original = vi.mocked(api).getMockImplementation()!;
    vi.mocked(api).mockImplementation(async (method, path, body) => {
      if (path.endsWith("/latest")) return latest as never;
      if (path.endsWith("/model-detection")) return { model_id: "gpt-6-sol" } as never;
      if (path.endsWith("/retry")) return { ...latest, can_retry: false } as never;
      return original(method, path, body);
    });
    await act(async () => root.render(<ModelTestDialog account={{ ...account, platform, type: "apikey" }} online close={vi.fn()} report={vi.fn()} preference={{ engine: "modeltrace", sample_count: 1 }}/>));
    await settle();
    const source = container.querySelector<HTMLSelectElement>('[aria-label="检测源"]')!;
    expect([...source.options].map(o => o.value)).toEqual(["lm_detector"]);
    const retry = [...container.querySelectorAll<HTMLButtonElement>("footer button")].find(b => b.textContent?.includes("重试失败组"))!;
    expect(retry.disabled).toBe(false);
    await act(async () => retry.click());
    expect(api).toHaveBeenCalledWith("POST", "/model-tests/fixture-retry/retry", expect.objectContaining({ request_id: expect.any(String) }));
  });
});
