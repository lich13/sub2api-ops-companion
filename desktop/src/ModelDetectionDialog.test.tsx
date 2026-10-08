// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import ModelDetectionDialog from "./ModelDetectionDialog";
import { api, command } from "./bridge";
import type { Account } from "./types";

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
  enabled: boolean;
  interval_minutes: number;
  model_id: string;
  version: string;
  next_at?: string | null;
  next_allowed_at?: string | null;
  disposition?: { marked: boolean; schedule_verified: boolean };
  disposition_notification?: { status: string } | null;
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

describe("scheduled model detection", () => {
  it("loads the default settings and saves the selected values with the current version", async () => {
    await renderDialog();
    expect(container.querySelector<HTMLInputElement>('input[type="checkbox"]')?.checked).toBe(false);
    expect(container.querySelector<HTMLInputElement>('[aria-label="检测间隔"]')?.value).toBe("30");
    expect(container.querySelector<HTMLSelectElement>('[aria-label="检测模型"]')?.value).toBe("gpt-6-sol");
    expect(container.textContent).toContain("等待下次检测");

    await act(async () => container.querySelector<HTMLInputElement>('input[type="checkbox"]')!.click());
    const interval = container.querySelector<HTMLInputElement>('[aria-label="检测间隔"]')!;
    await act(async () => {
      const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
      setter.call(interval, "45");
      interval.dispatchEvent(new Event("input", { bubbles: true }));
    });
    const model = container.querySelector<HTMLSelectElement>('[aria-label="检测模型"]')!;
    model.value = "gpt-6-luna";
    await act(async () => model.dispatchEvent(new Event("change", { bubbles: true })));

    const saved: Detection = {
      ...baseDetection,
      enabled: true,
      interval_minutes: 45,
      model_id: "gpt-6-luna",
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
      model_id: "gpt-6-luna",
    });
    expect(command).toHaveBeenCalledWith("refresh");
    expect(container.querySelector<HTMLInputElement>('input[type="checkbox"]')?.checked).toBe(true);
  });

  it("keeps an unavailable selected model visible and prevents enabling detection", async () => {
    await renderDialog({ ...baseDetection, enabled: true, model_id: "retired-model" }, [models[0]]);
    const model = container.querySelector<HTMLSelectElement>('[aria-label="检测模型"]')!;
    expect(model.value).toBe("retired-model");
    expect(model.textContent).toContain("retired-model（当前不可用）");
    expect(container.textContent).toContain("所选模型不在当前分组白名单中");
    expect(button("保存").disabled).toBe(true);
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
});
