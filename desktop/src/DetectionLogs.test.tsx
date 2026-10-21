// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import DetectionLogs from "./DetectionLogs";
import { api } from "./bridge";
import type { DetectionLog } from "./useDetectionLogFeed";

vi.mock("./bridge", () => ({ api: vi.fn() }));
vi.mock("./mobile", () => ({ useBackAction: vi.fn() }));

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

let container: HTMLDivElement;
let root: Root;

type LogOverrides = Omit<Partial<DetectionLog>, "summary"> & {
  summary?: Partial<DetectionLog["summary"]>;
};

function log(jobId: string, name: string, overrides: LogOverrides = {}): DetectionLog {
  const base: DetectionLog = {
    job_id: jobId,
    account_id: 17,
    account_name: name,
    model_id: "fixture-model",
    status: "completed",
    created_at: "2026-10-20T12:00:00Z",
    started_at: "2026-10-20T11:59:00Z",
    completed_at: "2026-10-20T12:00:00Z",
    triggers: ["scheduled"],
    error: "",
    details_available: true,
    summary: {
      outcome: "normal",
      analysis_available: true,
      first_prediction: "fixture-first-prediction",
      prediction: "fixture-summary-prediction",
      probability: 0.91,
      used_outputs: 3,
      first_downgrade: false,
      aggregate_downgrade: false,
      disposition: null,
      recovery: null,
      verification: null,
      restoration: null,
    },
    notification: {
      status: "delivered",
      merged: false,
      delivered_at: "2026-10-20T12:00:01Z",
      reason: "",
    },
  };
  return {
    ...base,
    ...overrides,
    summary: { ...base.summary, ...overrides.summary },
    notification: overrides.notification === undefined ? base.notification : overrides.notification,
  };
}

function receipt(status: string, overrides: Partial<NonNullable<DetectionLog["summary"]["recovery"]>> = {}) {
  return {
    status,
    marked: null,
    schedule_verified: null,
    mark_cleared: null,
    reason: "",
    completed_at: "2026-10-20T12:00:00Z",
    ...overrides,
  };
}

function page(items: DetectionLog[], next_cursor: string | null = null) {
  return { items, next_cursor };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

async function settle() {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();
  });
}

async function render({
  active = true,
  online = true,
  connectionKey = "fixture-connection",
}: { active?: boolean; online?: boolean; connectionKey?: string } = {}) {
  await act(async () => root.render(<DetectionLogs active={active} online={online} connectionKey={connectionKey} />));
  await settle();
}

async function rerender({
  active = true,
  online = true,
  connectionKey = "fixture-connection",
}: { active?: boolean; online?: boolean; connectionKey?: string } = {}) {
  await act(async () => root.render(<DetectionLogs active={active} online={online} connectionKey={connectionKey} />));
  await settle();
}

function rowNames() {
  return [...container.querySelectorAll(".detection-log-row")].map((node) => node.querySelector("td:nth-child(2) button span")?.textContent || node.querySelector("td:nth-child(2)")?.textContent || "");
}

function row(name: string) {
  const match = [...container.querySelectorAll<HTMLTableRowElement>(".detection-log-row")]
    .find((node) => node.textContent?.includes(name));
  if (!match) throw new Error(`missing log row: ${name}`);
  return match;
}

function rowButton(name: string) {
  const button = row(name).querySelector<HTMLButtonElement>("td:nth-child(2) button");
  if (!button) throw new Error(`missing log account button: ${name}`);
  return button;
}

function fact(label: string, scope: ParentNode = container) {
  const term = [...scope.querySelectorAll(".detection-meta dt")].find((node) => node.textContent === label);
  return term?.nextElementSibling?.textContent ?? null;
}

beforeEach(() => {
  vi.clearAllMocks();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  vi.mocked(api).mockImplementation(async (_method, path) => {
    if (path === "/capabilities") return { features: { detection_logs: true } } as never;
    if (path === "/detection-logs?limit=50") return page([]) as never;
    if (path.startsWith("/model-tests/")) return { id: path.slice("/model-tests/".length), groups: [] } as never;
    throw new Error(`Unexpected fixture request: ${path}`);
  });
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
});

describe("automatic detection log feed", () => {
  it("shows the upgrade prompt when the server lacks log capability and retries capability detection", async () => {
    let supported = false;
    vi.mocked(api).mockImplementation(async (_method, path) => {
      if (path === "/capabilities") return { features: { detection_logs: supported } } as never;
      if (path === "/detection-logs?limit=50") return page([]) as never;
      throw new Error(`Unexpected fixture request: ${path}`);
    });

    await render();
    expect(container.querySelector('[role="alert"]')?.textContent).toContain("请升级服务端以查看自动检测日志");
    expect(vi.mocked(api).mock.calls).not.toContainEqual(["GET", "/detection-logs?limit=50"]);

    supported = true;
    await act(async () => container.querySelector<HTMLButtonElement>('[role="alert"] button')!.click());
    await settle();
    expect(vi.mocked(api).mock.calls.filter(([, path]) => path === "/capabilities")).toHaveLength(2);
    expect(vi.mocked(api).mock.calls).toContainEqual(["GET", "/detection-logs?limit=50"]);
    expect(container.querySelector('[role="alert"]')).toBeNull();
  });

  it("renders each result as a ten-column table row with a dedicated status badge and account opener", async () => {
    const jobId = "fixture-job-table";
    const item = log(jobId, "表格账号", {
      triggers: ["scheduled", "recovery"],
      summary: { prediction: "汇总预测", probability: 0.84 },
    });
    vi.mocked(api).mockImplementation(async (_method, path) => {
      if (path === "/capabilities") return { features: { detection_logs: true } } as never;
      if (path === "/detection-logs?limit=50") return page([item]) as never;
      if (path === `/model-tests/${jobId}`) return { id: jobId, groups: [] } as never;
      throw new Error(`Unexpected fixture request: ${path}`);
    });

    await render();
    const table = container.querySelector("table")!;
    const headers = [...table.querySelectorAll("thead th")].map((cell) => cell.textContent?.trim());
    expect(headers).toEqual(["结果时间", "账号", "模型", "触发", "执行", "结论", "预测", "匹配度", "处置／恢复", "通知"]);
    const resultRow = row("表格账号");
    expect(resultRow.tagName).toBe("TR");
    expect(resultRow.parentElement?.tagName).toBe("TBODY");
    const cells = resultRow.querySelectorAll("td");
    expect(cells).toHaveLength(10);
    const accountButton = cells[1].querySelector<HTMLButtonElement>("button");
    expect(accountButton).not.toBeNull();
    expect(accountButton?.textContent).toContain("表格账号");
    const status = cells[4].querySelector("span");
    expect(status?.textContent).toBe("已完成");
    expect(status?.className).toMatch(/status|badge/);
    expect(cells[5].textContent).toContain("未确认降智");
    expect(cells[6].textContent).toContain("汇总预测");
    expect(cells[7].textContent).toContain("84.0%");

    await act(async () => accountButton!.click());
    await settle();
    expect(api).toHaveBeenCalledWith("GET", `/model-tests/${jobId}`);
  });

  it.each([
    { status: "completed", statusLabel: "已完成", noticeStatus: "delivered", noticeLabel: "已推送" },
    { status: "failed", statusLabel: "失败", noticeStatus: "failed", noticeLabel: "投递失败" },
    { status: "cancelled", statusLabel: "已取消", noticeStatus: "suppressed", noticeLabel: "已取消", merged: true },
    { status: "interrupted", statusLabel: "已中断", noticeStatus: "retry", noticeLabel: "待重试" },
    { status: "needs_confirmation", statusLabel: "待确认", noticeStatus: "pending", noticeLabel: "待推送" },
  ])("keeps $status status, result, receipts, and notification distinct", async ({ status, statusLabel, noticeStatus, noticeLabel, merged }) => {
    const row = log(`job-${status}`, `账号 ${status}`, {
      status,
      triggers: ["recheck", "recovery"],
      summary: {
        outcome: "normal",
        recovery: receipt("completed", { mark_cleared: true }),
        verification: receipt("passed"),
        restoration: receipt("restored"),
      },
      notification: {
        status: noticeStatus,
        merged: merged ?? false,
        delivered_at: "2026-10-20T12:01:00Z",
        reason: "fixture notification reason",
      },
    });
    vi.mocked(api).mockImplementation(async (_method, path) => {
      if (path === "/capabilities") return { features: { detection_logs: true } } as never;
      if (path === "/detection-logs?limit=50") return page([row]) as never;
      if (path === `/model-tests/${row.job_id}`) return { id: row.job_id, groups: [] } as never;
      throw new Error(`Unexpected fixture request: ${path}`);
    });

    await render();
    expect(rowButton(`账号 ${status}`).closest("tr")?.querySelector("td:nth-child(6)")?.textContent).toContain("未确认降智");
    await act(async () => rowButton(`账号 ${status}`).click());
    await settle();

    const dialog = container.querySelector<HTMLElement>('[role="dialog"][aria-label="自动检测日志详情"]')!;
    expect(fact("执行状态", dialog)).toBe(statusLabel);
    expect(fact("检测结论", dialog)).toBe("未确认降智");
    expect(fact("复查恢复", dialog)).toBe("已取消降智标记");
    expect(fact("恢复验证", dialog)).toBe("模型验证通过");
    expect(fact("调度恢复", dialog)).toContain("已恢复调度");
    expect(fact("通知", dialog)).toContain(noticeLabel);
    expect(fact("通知", dialog)).toContain(merged ? "合并用卡通知" : "");
    expect(fact("推送时间", dialog)).not.toBeNull();
  });

  it("does not infer verification or restored scheduling from a normal outcome", async () => {
    const row = log("job-normal-only", "正常结论账号", {
      triggers: ["recovery", "recheck"],
      summary: {
        outcome: "normal",
        recovery: receipt("completed", { mark_cleared: true }),
        verification: receipt("pending"),
        restoration: receipt("pending"),
      },
    });
    vi.mocked(api).mockImplementation(async (_method, path) => {
      if (path === "/capabilities") return { features: { detection_logs: true } } as never;
      if (path === "/detection-logs?limit=50") return page([row]) as never;
      if (path === `/model-tests/${row.job_id}`) return { id: row.job_id, groups: [] } as never;
      throw new Error(`Unexpected fixture request: ${path}`);
    });

    await render();
    await act(async () => rowButton("正常结论账号").click());
    await settle();
    const dialog = container.querySelector<HTMLElement>('[role="dialog"][aria-label="自动检测日志详情"]')!;
    expect(fact("检测结论", dialog)).toBe("未确认降智");
    expect(fact("复查恢复", dialog)).toBe("已取消降智标记");
    expect(fact("恢复验证", dialog)).toBe("验证回执不可用");
    expect(fact("调度恢复", dialog)).toContain("恢复待核对");
    expect(dialog.textContent).not.toContain("模型验证通过");
    expect(dialog.textContent).not.toContain("已恢复调度");
  });

  it("requests the exact job ID and retains the summary if detail history is missing", async () => {
    const jobId = "fixture-job-404";
    const row = log(jobId, "摘要保留账号", {
      summary: { prediction: "保留的汇总预测", probability: 0.84 },
    });
    vi.mocked(api).mockImplementation(async (_method, path) => {
      if (path === "/capabilities") return { features: { detection_logs: true } } as never;
      if (path === "/detection-logs?limit=50") return page([row]) as never;
      if (path === `/model-tests/${jobId}`) throw new Error("404 not found");
      throw new Error(`Unexpected fixture request: ${path}`);
    });

    await render();
    await act(async () => rowButton("摘要保留账号").click());
    await settle();

    expect(api).toHaveBeenCalledWith("GET", `/model-tests/${jobId}`);
    expect(container.querySelector('[role="status"]')?.textContent).toContain("任务详情暂不可用，已保留结果摘要");
    const dialog = container.querySelector<HTMLElement>('[role="dialog"][aria-label="自动检测日志详情"]')!;
    expect(fact("汇总预测", dialog)).toBe("保留的汇总预测");
    expect(fact("检测结论", dialog)).toBe("未确认降智");
  });

  it("encodes cursors while deduplicating overlapping pagination rows by job ID", async () => {
    const overlappingCursor = "2026-10-20T11:00:00Z /cursor";
    const first = [
      log("job-a", "记录 A 原版", { created_at: "2026-10-20T12:00:00Z" }),
      log("job-b", "记录 B", { created_at: "2026-10-20T11:00:00Z" }),
    ];
    const second = [
      log("job-a", "记录 A 后版", { created_at: "2026-10-20T12:00:00Z" }),
      log("job-c", "记录 C", { created_at: "2026-10-20T10:00:00Z" }),
    ];
    vi.mocked(api).mockImplementation(async (_method, path) => {
      if (path === "/capabilities") return { features: { detection_logs: true } } as never;
      if (path === "/detection-logs?limit=50") return page(first, overlappingCursor) as never;
      if (path === `/detection-logs?limit=50&before=${encodeURIComponent(overlappingCursor)}`) return page(second) as never;
      throw new Error(`Unexpected fixture request: ${path}`);
    });

    await render();
    expect(rowNames()).toEqual(["记录 A 原版", "记录 B"]);
    await act(async () => container.querySelector<HTMLButtonElement>(".load-more")!.click());
    await settle();

    expect(api).toHaveBeenCalledWith("GET", `/detection-logs?limit=50&before=${encodeURIComponent(overlappingCursor)}`);
    expect(rowNames()).toEqual(["记录 A 后版", "记录 B", "记录 C"]);
    expect(container.querySelectorAll(".detection-log-row")).toHaveLength(3);
    expect(container.querySelector(".load-more")).toBeNull();
  });

  it("loads a switched connection before the old request settles and ignores stale results", async () => {
    const oldPage = deferred<{ items: DetectionLog[]; next_cursor: string | null }>();
    const offlinePage = deferred<{ items: DetectionLog[]; next_cursor: string | null }>();
    let pageReads = 0;
    vi.mocked(api).mockImplementation(async (_method, path) => {
      if (path === "/capabilities") return { features: { detection_logs: true } } as never;
      if (path.startsWith("/detection-logs?limit=50")) {
        pageReads += 1;
        if (pageReads === 1) return oldPage.promise as never;
        if (pageReads === 2) return page([log("job-new", "新连接记录")]) as never;
        return offlinePage.promise as never;
      }
      throw new Error(`Unexpected fixture request: ${path}`);
    });

    await render({ connectionKey: "connection-old" });
    expect(pageReads).toBe(1);
    await rerender({ connectionKey: "connection-new" });
    expect(pageReads).toBe(2);
    expect(rowNames()).toEqual(["新连接记录"]);

    await act(async () => oldPage.resolve(page([log("job-old", "旧连接记录")])));
    await settle();
    expect(pageReads).toBe(2);
    expect(rowNames()).toEqual(["新连接记录"]);

    await act(async () => container.querySelector<HTMLButtonElement>(".section-heading button")!.click());
    await settle();
    expect(pageReads).toBe(3);
    await rerender({ online: false, connectionKey: "connection-new" });
    await act(async () => offlinePage.resolve(page([log("job-late", "离线迟到记录")])));
    await settle();
    expect(rowNames()).toEqual(["新连接记录"]);
    expect(container.textContent).not.toContain("离线迟到记录");
  });

  it("ignores exact-job detail responses after close and after going offline", async () => {
    const row = log("fixture-job-detail", "任务详情账号");
    const closedDetail = deferred<{ id: string; groups: { index: number; status: string }[] }>();
    const offlineDetail = deferred<{ id: string; groups: { index: number; status: string }[] }>();
    let detailReads = 0;
    vi.mocked(api).mockImplementation(async (_method, path) => {
      if (path === "/capabilities") return { features: { detection_logs: true } } as never;
      if (path === "/detection-logs?limit=50") return page([row]) as never;
      if (path === `/model-tests/${row.job_id}`) {
        detailReads += 1;
        return (detailReads === 1 ? closedDetail.promise : offlineDetail.promise) as never;
      }
      throw new Error(`Unexpected fixture request: ${path}`);
    });

    await render();
    await act(async () => rowButton("任务详情账号").click());
    await settle();
    expect(api).toHaveBeenCalledWith("GET", `/model-tests/${row.job_id}`);
    await act(async () => container.querySelector<HTMLButtonElement>('[aria-label="关闭日志详情"]')!.click());
    await settle();
    await act(async () => closedDetail.resolve({ id: row.job_id, groups: [{ index: 71, status: "completed" }] }));
    await settle();
    expect(container.querySelector('[role="dialog"][aria-label="自动检测日志详情"]')).toBeNull();
    expect(container.textContent).not.toContain("第 71 组");

    await act(async () => rowButton("任务详情账号").click());
    await settle();
    expect(detailReads).toBe(2);
    await rerender({ online: false });
    await act(async () => offlineDetail.resolve({ id: row.job_id, groups: [{ index: 88, status: "completed" }] }));
    await settle();

    expect(container.querySelector('[role="dialog"][aria-label="自动检测日志详情"]')).not.toBeNull();
    expect(container.textContent).not.toContain("第 88 组");
  });

  it("does not refetch an old selected job after switching connections and ignores its late response", async () => {
    const oldJobId = "job-old-connection";
    const newJobId = "job-new-connection";
    const oldRow = log(oldJobId, "旧连接账号");
    const newRow = log(newJobId, "新连接账号");
    const pendingOldDetail = deferred<{ id: string; groups: { index: number; status: string }[] }>();
    let pageReads = 0;
    let oldDetailReads = 0;
    let newDetailReads = 0;
    vi.mocked(api).mockImplementation(async (_method, path) => {
      if (path === "/capabilities") return { features: { detection_logs: true } } as never;
      if (path === "/detection-logs?limit=50") {
        pageReads += 1;
        return page([pageReads === 1 ? oldRow : newRow]) as never;
      }
      if (path === `/model-tests/${oldJobId}`) {
        oldDetailReads += 1;
        return pendingOldDetail.promise as never;
      }
      if (path === `/model-tests/${newJobId}`) {
        newDetailReads += 1;
        return { id: newJobId, groups: [{ index: 2, status: "completed" }] } as never;
      }
      throw new Error(`Unexpected fixture request: ${path}`);
    });

    await render({ connectionKey: "connection-old" });
    expect(rowNames()).toEqual(["旧连接账号"]);
    await act(async () => rowButton("旧连接账号").click());
    await settle();
    expect(oldDetailReads).toBe(1);

    await rerender({ connectionKey: "connection-new" });
    expect(rowNames()).toEqual(["新连接账号"]);
    expect(container.querySelector('[role="dialog"][aria-label="自动检测日志详情"]')).toBeNull();
    expect(oldDetailReads).toBe(1);

    await act(async () => pendingOldDetail.resolve({ id: oldJobId, groups: [{ index: 71, status: "completed" }] }));
    await settle();
    expect(rowNames()).toEqual(["新连接账号"]);
    expect(container.textContent).not.toContain("第 71 组");

    await act(async () => rowButton("新连接账号").click());
    await settle();
    expect(newDetailReads).toBe(1);
    expect(api).toHaveBeenCalledWith("GET", `/model-tests/${newJobId}`);
    const dialog = container.querySelector<HTMLElement>('[role="dialog"][aria-label="自动检测日志详情"]')!;
    expect(dialog.textContent).toContain("新连接账号");
    expect(dialog.textContent).toContain("第 2 组：已完成");
  });
});
