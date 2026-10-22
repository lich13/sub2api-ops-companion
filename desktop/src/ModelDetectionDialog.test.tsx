// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import ModelDetectionDialog from "./ModelDetectionDialog";
import { api, command } from "./bridge";
import { fullTime, type Account } from "./types";

vi.mock("./bridge", () => ({ api: vi.fn(), command: vi.fn() }));
vi.mock("./mobile", () => ({ useBackAction: vi.fn() }));

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

const account: Account = {
  id: 42,
  name: "检测账号",
  priority: 42,
  platform: "openai",
  type: "oauth",
  status: "active",
  schedulable: true,
  available: true,
  group_ids: [7],
  blockers: [],
  managed: false,
  version: "account-version-42",
  last_success_at: null,
  last_error_at: null,
  last_error_id: null,
  last_error_code: null,
  last_error_status: null,
  error_message: "",
  success_after_error: false,
  usage_windows: [],
};

type Detection = {
  last_automatic_started_at?: string | null;
  enabled: boolean;
  interval_minutes: number;
  model_id: string;
  model_source?: "global" | "account";
  model_override?: string | null;
  global_model_id?: string;
  version: string;
  generation?: number;
  next_at?: string | null;
  next_allowed_at?: string | null;
  disposition?: { marked: boolean; schedule_verified: boolean };
  disposition_notification?: { status: string } | null;
  recheck_recovery?: { status: string; generation: number; mark_cleared: boolean; reason?: string } | null;
  recovery_notification?: { status: string } | null;
  status: string;
  reason?: string;
  job_id?: string;
  last_result?: { status?: string; report?: { prediction_name?: string } } | null;
};

const models = [
  { id: "gpt-6-sol", display_name: "GPT-6 Sol" },
  { id: "gpt-6-luna", display_name: "GPT-6 Luna" },
];

const baseDetection: Detection = {
  enabled: false,
  interval_minutes: 30,
  model_id: "gpt-6-sol",
  model_source: "global",
  version: "detection-version-1",
  next_at: null,
  status: "waiting",
  last_result: null,
};

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  vi.clearAllMocks();
  vi.useFakeTimers();
  vi.mocked(command).mockResolvedValue(undefined as never);
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.useRealTimers();
});

function button(text: string) {
  const result = [...container.querySelectorAll<HTMLButtonElement>("button")].find(
    (node) => node.textContent?.includes(text),
  );
  if (!result) throw new Error("missing button: " + text);
  return result;
}

function lastDetectionTime() {
  return [...container.querySelectorAll(".detection-meta dt")]
    .find((node) => node.textContent === "上次检测（开始）")
    ?.nextElementSibling?.textContent;
}

async function renderDialog(
  detection: Detection = baseDetection,
  candidates = models,
  report = vi.fn(),
) {
  vi.mocked(api).mockImplementation(async (method, path) => {
    if (method === "GET" && path === "/accounts/42/model-detection") return detection as never;
    if (method === "GET" && path === "/accounts/42/models?purpose=model_test") return candidates as never;
    throw new Error("unexpected " + method + " " + path);
  });
  await act(async () => root.render(
    <ModelDetectionDialog
      account={account}
      online
      close={() => {}}
      report={report}
    />,
  ));
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}

async function settle() {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}

describe("scheduled model detection", () => {
  it("shows the last automatic start with the shared full time format", async () => {
    const startedAt = "2026-10-08T00:00:00Z";
    await renderDialog({ ...baseDetection, last_automatic_started_at: startedAt });

    expect(lastDetectionTime()).toBe(fullTime(startedAt));
  });

  it("shows no record when neither a start time nor a result exists", async () => {
    await renderDialog({ ...baseDetection, last_automatic_started_at: null, last_result: null });

    expect(lastDetectionTime()).toBe("暂无记录");
  });

  it("shows an unknown time when history exists without a valid start time", async () => {
    await renderDialog({
      ...baseDetection,
      last_automatic_started_at: "",
      last_result: { status: "failed" },
    });

    expect(lastDetectionTime()).toBe("时间未知");
  });

  it("shows the global model as read-only and saves only enabled and interval", async () => {
    await renderDialog();
    expect(container.querySelector<HTMLInputElement>('input[type="checkbox"]')?.checked).toBe(false);
    expect(container.querySelector<HTMLInputElement>('[aria-label="检测间隔"]')?.value).toBe("30");
    expect(container.querySelector('[aria-label="检测模型"]')).toBeNull();
    expect(container.textContent).toContain("等待下次检测");

    await act(async () => container.querySelector<HTMLInputElement>('input[type="checkbox"]')!.click());
    const interval = container.querySelector<HTMLInputElement>('[aria-label="检测间隔"]')!;
    await act(async () => {
      const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
      setter.call(interval, "45");
      interval.dispatchEvent(new Event("input", { bubbles: true }));
    });
    const saved: Detection = {
      ...baseDetection,
      enabled: true,
      interval_minutes: 45,
      version: "detection-version-2",
      status: "waiting",
    };
    vi.mocked(api).mockImplementation(async (method, path, body) => {
      if (method === "PUT" && path === "/accounts/42/model-detection") return saved as never;
      if (method === "GET" && path === "/accounts/42/model-detection") return baseDetection as never;
      if (method === "GET" && path === "/accounts/42/models?purpose=model_test") return models as never;
      throw new Error("unexpected " + method + " " + path);
    });
    await act(async () => button("保存").click());

    expect(vi.mocked(api).mock.calls.find(([method]) => method === "PUT")?.[2]).toEqual({
      expected_version: "detection-version-1",
      enabled: true,
      interval_minutes: 45,
    });
    expect(command).toHaveBeenCalledWith("refresh");
    expect(container.querySelector<HTMLInputElement>('input[type="checkbox"]')?.checked).toBe(true);
  });

  it("shows an unavailable global model and prevents enabling detection", async () => {
    await renderDialog({ ...baseDetection, enabled: true, model_id: "retired-model" }, [models[0]]);
    expect(container.querySelector('[aria-label="检测模型"]')).toBeNull();
    expect(container.textContent).toContain("当前账号上游模型目录");
    expect(button("保存").disabled).toBe(true);
  });

  it("allows saving disabled settings when the model directory cannot be read", async () => {
    await renderDialog({ ...baseDetection, enabled: false });
    const interval = container.querySelector<HTMLInputElement>('[aria-label="检测间隔"]')!;
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(interval, "45");
      interval.dispatchEvent(new Event("input", { bubbles: true }));
    });

    const original = vi.mocked(api).getMockImplementation()!;
    vi.mocked(api).mockImplementation(async (method, path, body) => {
      if (path === "/accounts/42/models?purpose=model_test") throw new Error("目录暂不可用");
      return original(method, path, body);
    });
    await act(async () => button("重新读取").click());
    await settle();
    expect(container.textContent).toContain("目录暂不可用");
    expect(button("保存").disabled).toBe(false);

    vi.mocked(api).mockImplementation(async (method, path, body) => {
      if (method === "PUT" && path === "/accounts/42/model-detection") return {
        ...baseDetection, interval_minutes: 45, version: "detection-version-2",
      } as never;
      return original(method, path, body);
    });
    await act(async () => button("保存").click());
    expect(vi.mocked(api).mock.calls.find(([method]) => method === "PUT")?.[2]).toEqual({
      expected_version: "detection-version-1",
      enabled: false,
      interval_minutes: 45,
    });
  });

  it("requires a global model source from the server before allowing edits", async () => {
    const oldServer: Detection = { ...baseDetection };
    delete oldServer.model_source;
    await renderDialog(oldServer);

    const enabled = container.querySelector<HTMLInputElement>('input[type="checkbox"]')!;
    const interval = container.querySelector<HTMLInputElement>('[aria-label="检测间隔"]')!;
    expect(enabled.disabled).toBe(true);
    expect(interval.disabled).toBe(true);
    expect(container.textContent).toContain("请升级服务端");
    expect(button("保存").disabled).toBe(true);
  });

  it("keeps an edited draft when a directory reread fails", async () => {
    await renderDialog({ ...baseDetection, enabled: true });
    const interval = container.querySelector<HTMLInputElement>('[aria-label="检测间隔"]')!;
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(interval, "45");
      interval.dispatchEvent(new Event("input", { bubbles: true }));
    });

    let failDirectory = true;
    const original = vi.mocked(api).getMockImplementation()!;
    vi.mocked(api).mockImplementation(async (method, path, body) => {
      if (failDirectory && path === "/accounts/42/models?purpose=model_test") throw new Error("目录暂不可用");
      return original(method, path, body);
    });
    await act(async () => button("重新读取").click());
    await settle();

    expect(interval.value).toBe("45");
    expect(container.textContent).toContain("目录暂不可用");
    expect(button("保存").disabled).toBe(true);

    failDirectory = false;
    await act(async () => button("重试").click());
    await settle();
    expect(interval.value).toBe("45");
    expect(container.textContent).not.toContain("目录暂不可用");
    expect(button("保存").disabled).toBe(false);
  });

  it("waits for a candidate reread before marking a saved model unavailable", async () => {
    const detection = { ...baseDetection, enabled: true, model_id: "retired-model" };
    let resolveModels!: (value: unknown) => void;
    const pendingModels = new Promise((resolve) => { resolveModels = resolve; });
    let reload = false;
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method === "GET" && path === "/accounts/42/model-detection") return detection as never;
      if (method === "GET" && path === "/accounts/42/models?purpose=model_test") {
        return (reload ? pendingModels : [models[0]]) as never;
      }
      throw new Error("unexpected " + method + " " + path);
    });
    await act(async () => root.render(
      <ModelDetectionDialog account={account} online close={() => {}} report={() => {}} />,
    ));
    await settle();

    reload = true;
    await act(async () => button("重新读取").click());
    await settle();
    expect(container.querySelector('[aria-label="检测模型"]')).toBeNull();
    expect(container.textContent).not.toContain("所选模型不在当前账号上游模型目录中");
    expect(button("保存").disabled).toBe(true);

    await act(async () => resolveModels([]));
    await settle();
    expect(container.querySelector('[aria-label="检测模型"]')).toBeNull();
    expect(container.textContent).toContain("所选模型不在当前账号上游模型目录中");
    expect(button("保存").disabled).toBe(true);
  });

  it("adopts the server draft and version after an explicit successful reread", async () => {
    let refreshed = false;
    const serverDetection: Detection = {
      ...baseDetection,
      interval_minutes: 60,
      model_id: "gpt-6-luna",
      version: "detection-version-server",
    };
    vi.mocked(api).mockImplementation(async (method, path, body) => {
      if (method === "GET" && path === "/accounts/42/model-detection") return (refreshed ? serverDetection : baseDetection) as never;
      if (method === "GET" && path === "/accounts/42/models?purpose=model_test") return models as never;
      if (method === "PUT" && path === "/accounts/42/model-detection") return serverDetection as never;
      throw new Error("unexpected " + method + " " + path);
    });
    await act(async () => root.render(
      <ModelDetectionDialog account={account} online close={() => {}} report={() => {}} />,
    ));
    await settle();

    const interval = container.querySelector<HTMLInputElement>('[aria-label="检测间隔"]')!;
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(interval, "45");
      interval.dispatchEvent(new Event("input", { bubbles: true }));
    });
    refreshed = true;
    await act(async () => button("重新读取").click());
    await settle();

    expect(interval.value).toBe("60");
    expect(container.querySelector('[aria-label="检测模型"]')).toBeNull();
    await act(async () => button("保存").click());
    expect(vi.mocked(api).mock.calls.find(([method]) => method === "PUT")?.[2]).toEqual({
      expected_version: "detection-version-server",
      enabled: false,
      interval_minutes: 60,
    });
  });

  it("keeps the draft through an offline reconnect reread", async () => {
    let reconnect = false;
    let resolveReconnect!: (value: Detection) => void;
    const pendingReconnect = new Promise<Detection>((resolve) => { resolveReconnect = resolve; });
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method === "GET" && path === "/accounts/42/model-detection") return (reconnect ? pendingReconnect : baseDetection) as never;
      if (method === "GET" && path === "/accounts/42/models?purpose=model_test") return models as never;
      throw new Error("unexpected " + method + " " + path);
    });
    await act(async () => root.render(
      <ModelDetectionDialog account={account} online close={() => {}} report={() => {}} />,
    ));
    await settle();
    const interval = container.querySelector<HTMLInputElement>('[aria-label="检测间隔"]')!;
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(interval, "45");
      interval.dispatchEvent(new Event("input", { bubbles: true }));
    });
    reconnect = true;
    await act(async () => root.render(
      <ModelDetectionDialog account={account} online={false} close={() => {}} report={() => {}} />,
    ));
    await act(async () => root.render(
      <ModelDetectionDialog account={account} online close={() => {}} report={() => {}} />,
    ));
    await act(async () => resolveReconnect({ ...baseDetection, interval_minutes: 15, model_id: "gpt-6-sol", version: "reconnected-version" }));
    await settle();

    expect(interval.value).toBe("45");
    expect(container.querySelector('[aria-label="检测模型"]')).toBeNull();
  });

  it("ignores a late settings response after switching accounts", async () => {
    const otherAccount = { ...account, id: 43, name: "另一个检测账号", version: "account-version-43" };
    let resolveOldDetection!: (value: Detection) => void;
    let resolveOldModels!: (value: unknown) => void;
    const oldDetection = new Promise<Detection>((resolve) => { resolveOldDetection = resolve; });
    const oldModels = new Promise((resolve) => { resolveOldModels = resolve; });
    const freshDetection: Detection = { ...baseDetection, interval_minutes: 55, model_id: "gpt-6-luna", version: "fresh-version" };
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method !== "GET") throw new Error("unexpected " + method + " " + path);
      if (path === "/accounts/42/model-detection") return oldDetection as never;
      if (path === "/accounts/42/models?purpose=model_test") return oldModels as never;
      if (path === "/accounts/43/model-detection") return freshDetection as never;
      if (path === "/accounts/43/models?purpose=model_test") return [models[1]] as never;
      throw new Error("unexpected " + path);
    });
    await act(async () => root.render(
      <ModelDetectionDialog account={account} online close={() => {}} report={() => {}} />,
    ));
    await act(async () => root.render(
      <ModelDetectionDialog account={otherAccount} online close={() => {}} report={() => {}} />,
    ));
    await settle();
    expect(container.textContent).toContain("另一个检测账号 #43");
    expect(container.querySelector<HTMLInputElement>('[aria-label="检测间隔"]')!.value).toBe("55");
    expect(container.querySelector('[aria-label="检测模型"]')).toBeNull();

    await act(async () => {
      resolveOldDetection({ ...baseDetection, interval_minutes: 5, model_id: "gpt-6-sol", version: "stale-version" });
      resolveOldModels([{ id: "stale-account-model", display_name: "Stale account model" }]);
    });
    await settle();
    expect(container.textContent).toContain("另一个检测账号 #43");
    expect(container.querySelector<HTMLInputElement>('[aria-label="检测间隔"]')!.value).toBe("55");
    expect(container.querySelector('[aria-label="检测模型"]')).toBeNull();
  });

  it("ignores settings responses after the dialog is unmounted", async () => {
    let resolveDetection!: (value: Detection) => void;
    let resolveModels!: (value: unknown) => void;
    const pendingDetection = new Promise<Detection>((resolve) => { resolveDetection = resolve; });
    const pendingModels = new Promise((resolve) => { resolveModels = resolve; });
    const report = vi.fn();
    const close = vi.fn(() => root.render(null));
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method === "GET" && path === "/accounts/42/model-detection") return pendingDetection as never;
      if (method === "GET" && path === "/accounts/42/models?purpose=model_test") return pendingModels as never;
      throw new Error("unexpected " + method + " " + path);
    });
    await act(async () => root.render(
      <ModelDetectionDialog account={account} online close={close} report={report} />,
    ));
    await act(async () => container.querySelector<HTMLButtonElement>('button[aria-label="关闭定时检测"]')!.click());
    expect(close).toHaveBeenCalledOnce();
    await act(async () => {
      resolveDetection(baseDetection);
      resolveModels(models);
    });
    await settle();
    expect(container.textContent).toBe("");
    expect(report).not.toHaveBeenCalled();
  });

  it("cancels a queued task and refreshes the terminal task state", async () => {
    let cancelled = false;
    const running: Detection = {
      ...baseDetection,
      enabled: true,
      status: "running",
      job_id: "model-job-42",
    };
    const done: Detection = { ...running, status: "cancelled", job_id: undefined };
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method === "GET" && path === "/accounts/42/model-detection") return (cancelled ? done : running) as never;
      if (method === "GET" && path === "/accounts/42/models?purpose=model_test") return models as never;
      if (method === "POST" && path === "/model-tests/model-job-42/cancel") {
        cancelled = true;
        return done as never;
      }
      throw new Error("unexpected " + method + " " + path);
    });
    await act(async () => root.render(
      <ModelDetectionDialog account={account} online close={() => {}} report={() => {}} />,
    ));
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(button("取消任务")).toBeTruthy();
    await act(async () => button("取消任务").click());

    expect(vi.mocked(api).mock.calls).toContainEqual(["POST", "/model-tests/model-job-42/cancel", {}]);
    expect(container.textContent).toContain("已取消");
    expect(container.querySelector("button")?.textContent).not.toContain("取消任务");
    expect(button("保存").disabled).toBe(false);
  });

  it("updates cooldown, disposition and notification results without overwriting an unsaved interval", async () => {
    vi.setSystemTime(new Date("2026-10-08T00:00:00Z"));
    const handling: Detection = {
      ...baseDetection,
      enabled: true,
      status: "handling",
      next_allowed_at: "2026-10-08T00:00:01Z",
      disposition: { marked: false, schedule_verified: false },
      disposition_notification: { status: "queued" },
    };
    await renderDialog(handling);
    const detail = (label: string) => [...container.querySelectorAll(".detection-meta dt")].find((node) => node.textContent === label)?.nextElementSibling?.textContent;
    expect(detail("状态")).toBe("正在处置");
    expect(detail("冷却至")).toBe("10-08 08:00:01");
    expect(detail("处置")).toBe("标记待保存 · 停调度待确认");
    expect(detail("结果通知")).toBe("待推送");

    const interval = container.querySelector<HTMLInputElement>('[aria-label="检测间隔"]')!;
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(interval, "45");
      interval.dispatchEvent(new Event("input", { bubbles: true }));
    });
    const original = vi.mocked(api).getMockImplementation()!;
    let notification = "retry";
    vi.mocked(api).mockImplementation(async (method, path, body) => {
      if (method === "GET" && path === "/accounts/42/model-detection") return {
        ...handling, status: "completed", disposition: { marked: true, schedule_verified: true },
        disposition_notification: { status: notification },
      } as never;
      return original(method, path, body);
    });
    await act(async () => { await vi.advanceTimersByTimeAsync(2000); });
    expect(detail("冷却至")).toBeUndefined();
    expect(detail("状态")).toBe("已完成");
    expect(detail("处置")).toBe("标记已保存 · 停调度已确认");
    expect(detail("结果通知")).toBe("待重试");
    expect(interval.value).toBe("45");
    notification = "delivered";
    await act(async () => { await vi.advanceTimersByTimeAsync(2000); });
    expect(detail("结果通知")).toBe("已推送");
    expect(interval.value).toBe("45");
    expect(vi.mocked(api).mock.calls.every(([method]) => method === "GET")).toBe(true);
  });

  it("shows a suppressed notification as cancelled and omits expired cooldown information", async () => {
    vi.setSystemTime(new Date("2026-10-08T00:00:00Z"));
    await renderDialog({
      ...baseDetection,
      next_allowed_at: "2026-10-07T23:59:59Z",
      disposition_notification: { status: "suppressed" },
    });
    expect(container.querySelector(".detection-meta")?.textContent).toContain("结果通知已取消");
    expect(container.querySelector(".detection-meta")?.textContent).not.toContain("冷却至");
  });

  it("polls same-generation recovery receipt and notification through queued, retry and delivered while preserving a draft", async () => {
    const pending: Detection = {
      ...baseDetection,
      enabled: true,
      status: "handling",
      generation: 4,
      disposition: { marked: true, schedule_verified: true },
      disposition_notification: { status: "queued" },
      recheck_recovery: { status: "pending", generation: 4, mark_cleared: false, reason: "正在取消降智标记" },
      recovery_notification: { status: "queued" },
    };
    let current: Detection = pending;
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method === "GET" && path === "/accounts/42/model-detection") return current as never;
      if (method === "GET" && path === "/accounts/42/models?purpose=model_test") return models as never;
      throw new Error("unexpected " + method + " " + path);
    });
    await act(async () => root.render(
      <ModelDetectionDialog account={account} online close={() => {}} report={() => {}} />,
    ));
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });

    const detail = (label: string) => [...container.querySelectorAll(".detection-meta dt")]
      .find((node) => node.textContent === label)?.nextElementSibling?.textContent;
    const interval = container.querySelector<HTMLInputElement>('[aria-label="检测间隔"]')!;
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(interval, "45");
      interval.dispatchEvent(new Event("input", { bubbles: true }));
    });
    expect(detail("复查结果")).toBe("正在取消降智标记");
    expect(detail("结果通知")).toBe("待推送");

    current = {
      ...pending,
      interval_minutes: 15,
      status: "completed",
      recheck_recovery: { status: "completed", generation: 4, mark_cleared: true, reason: "检测恢复正常，已取消降智标记" },
      recovery_notification: { status: "retry" },
    };
    await act(async () => { await vi.advanceTimersByTimeAsync(2000); });
    expect(detail("复查结果")).toBe("已取消降智标记 · 调度保持不变");
    expect(detail("结果通知")).toBe("待重试");
    expect(interval.value).toBe("45");
    expect(container.querySelector(".detection-meta")?.textContent).not.toContain("停调度已确认");

    current = { ...current, recovery_notification: { status: "delivered" } };
    await act(async () => { await vi.advanceTimersByTimeAsync(2000); });
    expect(detail("复查结果")).toBe("已取消降智标记 · 调度保持不变");
    expect(detail("结果通知")).toBe("已推送");
    expect(interval.value).toBe("45");
    expect(vi.mocked(api).mock.calls.every(([method]) => method === "GET")).toBe(true);
  });

  it("ignores a recovery receipt from an older generation and keeps the prior disposition", async () => {
    await renderDialog({
      ...baseDetection,
      generation: 9,
      disposition: { marked: true, schedule_verified: true },
      disposition_notification: { status: "retry" },
      recheck_recovery: { status: "completed", generation: 8, mark_cleared: true, reason: "检测恢复正常，已取消降智标记" },
      recovery_notification: { status: "delivered" },
    });
    const meta = container.querySelector(".detection-meta")!;
    expect(meta.querySelector("dt")?.textContent).toBe("状态");
    expect(meta.textContent).not.toContain("复查结果");
    expect(meta.textContent).not.toContain("已取消降智标记 · 调度保持不变");
    expect(meta.textContent).toContain("标记已保存 · 停调度已确认");
    expect(meta.textContent).toContain("待重试");
  });
});


describe("OpenAI Key detection models", () => {
  it("saves an upstream model override and can return to the global model", async () => {
    let current: Detection = { ...baseDetection, model_override: null, global_model_id: "gpt-6-sol" };
    vi.mocked(api).mockImplementation(async (method, path, body) => {
      if (path.endsWith("/models?purpose=model_test")) return models as never;
      if (path.endsWith("/model-detection")) {
        if (method === "PUT") {
          const override = (body as { model_override: string | null }).model_override;
          current = { ...current, model_override: override, model_id: override ?? "gpt-6-sol", model_source: override ? "account" : "global", version: "saved-version" };
        }
        return current as never;
      }
      throw new Error("unexpected " + path);
    });
    await act(async () => root.render(<ModelDetectionDialog account={{ ...account, type: "apikey" }} online close={() => {}} report={vi.fn()}/>));
    const select = container.querySelector<HTMLSelectElement>('[aria-label="检测模型"]')!;
    expect([...select.options].map(option => option.value)).toEqual(["", ...models.map(model => model.id)]);
    await act(async () => { select.value = "gpt-6-luna"; select.dispatchEvent(new Event("change", { bubbles: true })); });
    expect(button("保存").disabled).toBe(false);
    await act(async () => button("保存").click());
    expect(api).toHaveBeenCalledWith("PUT", "/accounts/42/model-detection", {
      expected_version: baseDetection.version, enabled: false, interval_minutes: 30, model_override: "gpt-6-luna",
    });
    expect(select.value).toBe("gpt-6-luna");
    await act(async () => { select.value = ""; select.dispatchEvent(new Event("change", { bubbles: true })); });
    await act(async () => button("保存").click());
    expect(api).toHaveBeenCalledWith("PUT", "/accounts/42/model-detection", {
      expected_version: "saved-version", enabled: false, interval_minutes: 30, model_override: null,
    });
    expect(select.value).toBe("");
    expect(vi.mocked(api).mock.calls.some(([method]) => method === "POST")).toBe(false);
  });

  it("keeps an unavailable custom model and allows disabling it without a model substitution", async () => {
    const current: Detection = { ...baseDetection, enabled: true, model_id: "retired-model", model_source: "account", model_override: "retired-model", global_model_id: "gpt-6-sol" };
    vi.mocked(api).mockImplementation(async (_method, path) => path.endsWith("/model-detection") ? current as never : models as never);
    await act(async () => root.render(<ModelDetectionDialog account={{ ...account, type: "apikey" }} online close={() => {}} report={vi.fn()}/>));
    const select = container.querySelector<HTMLSelectElement>('[aria-label="检测模型"]')!;
    expect(select.value).toBe("retired-model");
    expect(select.textContent).toContain("retired-model（当前不可用）");
    expect(button("保存").disabled).toBe(true);
    await act(async () => container.querySelector<HTMLInputElement>('input[type="checkbox"]')!.click());
    expect(button("保存").disabled).toBe(false);
    await act(async () => button("保存").click());
    expect(api).toHaveBeenCalledWith("PUT", "/accounts/42/model-detection", {
      expected_version: baseDetection.version, enabled: false, interval_minutes: 30, model_override: "retired-model",
    });
  });
});
