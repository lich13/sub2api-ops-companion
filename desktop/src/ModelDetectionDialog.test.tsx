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
});
