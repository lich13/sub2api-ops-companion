// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import AccountOperations from "./OperationPanel";
import {
  bindOperationConnection,
  getOperations,
  setOperations,
  type Operation,
} from "./accountOperations";
import { api, command } from "./bridge";
import type { Account } from "./types";

vi.mock("./bridge", () => ({ api: vi.fn(), command: vi.fn() }));
vi.mock("./mobile", () => ({ useBackAction: vi.fn() }));

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

const account: Account = {
  id: 1,
  name: "OpenAI OAuth",
  priority: 1,
  platform: "openai",
  type: "oauth",
  status: "active",
  schedulable: true,
  available: true,
  group_ids: [],
  blockers: [],
  version: "account-version-1",
  last_success_at: null,
  last_error_at: null,
  last_error_id: null,
  last_error_code: null,
  last_error_status: null,
  error_message: "",
  success_after_error: false,
  usage_windows: [],
};

function operation(overrides: Partial<Operation> = {}): Operation {
  return {
    id: "operation-1",
    account_id: account.id,
    account_name: account.name,
    action: "account_template",
    status: "failed",
    requested: { template_id: "alpha" },
    ...overrides,
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

let container: HTMLDivElement;
let root: Root;
let operationConnectionSequence = 0;

function resetOperationState() {
  bindOperationConnection("operation-panel-test-" + ++operationConnectionSequence);
  setOperations([]);
}

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(command).mockResolvedValue(undefined as never);
  resetOperationState();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  resetOperationState();
  container.remove();
});

function buttonByText(text: string) {
  const result = [...container.querySelectorAll<HTMLButtonElement>("button")].find(
    (node) => node.textContent?.includes(text),
  );
  if (!result) throw new Error("missing button: " + text);
  return result;
}

function entryButton() {
  const result = container.querySelector<HTMLButtonElement>('button[aria-label^="操作待办"]');
  if (!result) throw new Error("missing operation entry");
  return result;
}

async function flush() {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();
  });
}

async function renderPanel(connectionKey: string) {
  await act(async () => {
    root.render(
      <AccountOperations
        accounts={[account]}
        online
        connectionKey={connectionKey}
        report={() => {}}
        modelTest={() => {}}
        template={() => {}}
        priorities={() => {}}
      />,
    );
  });
  await flush();
}

describe("operation panel batch history", () => {
  it("does not start a new series on another connection after a delayed stop response", async () => {
    const job = operation({ action: "test", status: "needs_confirmation", repeat: "quota_cycle", repeat_state: "needs_confirmation", requested: { confirmed: true } });
    const stopped = deferred<Operation>();
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method === "GET") return { items: [job] } as never;
      if (path.endsWith("/cancel")) return stopped.promise as never;
      throw new Error(path);
    });
    await renderPanel("repeat-old-connection");
    await act(async () => entryButton().click());
    await act(async () => buttonByText("重新预约").click());
    await act(async () => { bindOperationConnection("repeat-new-connection"); stopped.resolve({ ...job, status: "cancelled", repeat_state: "stopped" }); });
    expect(vi.mocked(api).mock.calls.filter(([method]) => method === "POST")).toHaveLength(1);
    expect(getOperations()).toEqual([]);
  });

  it("keeps a continuous appointment stoppable while dispatched and shows its stop reason", async () => {
    const job = operation({ action: "test", status: "running", started_at: "2030-01-02T01:31:00Z", scheduled_at: "2030-01-02T01:31:00Z", repeat: "quota_cycle", repeat_state: "active", repeat_series_id: "fixture-series", requested: {} });
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method === "GET") return { items: [job] } as never;
      if (path.endsWith("/cancel")) return { ...job, repeat_state: "stopped", repeat_reason: "已停止后续预约，当前测试保留结果" } as never;
      throw new Error(path);
    });
    await renderPanel("panel-continuous");
    await act(async () => entryButton().click());
    await act(async () => buttonByText("停止连续预约").click());
    expect(api).toHaveBeenCalledWith("POST", "/account-operations/operation-1/cancel", {});
    expect(getOperations()[0].status).toBe("running");
    expect(getOperations()[0].repeat_state).toBe("stopped");
    expect(container.textContent).toContain("当前测试保留结果");
    expect([...container.querySelectorAll("button")].some(button => button.textContent === "停止连续预约")).toBe(false);
  });

  it("restarts a confirmed continuous appointment as a new series without replaying its old time", async () => {
    const job = operation({ action: "test", status: "completed", repeat: "quota_cycle", repeat_state: "needs_confirmation", repeat_reason: "周期无法确认", requested: { confirmed: true, mode: "default", model_id: "fixture-model" } });
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method === "GET") return { items: [job] } as never;
      if (path.endsWith("/cancel")) return { ...job, status: "cancelled", repeat_state: "stopped" } as never;
      if (path.endsWith("/operations")) return { ...job, id: "new-operation", status: "queued", repeat_state: "active", repeat_series_id: "new-operation" } as never;
      throw new Error(path);
    });
    await act(async () => root.render(<AccountOperations accounts={[{ ...account, operation_versions: { test: "a".repeat(64) } }]} online connectionKey="panel-repeat-confirm" report={() => {}} modelTest={() => {}} template={() => {}} priorities={() => {}}/>));
    await act(async () => entryButton().click());
    expect(container.textContent).toContain("周期无法确认");
    await act(async () => buttonByText("重新预约").click());
    const posts = vi.mocked(api).mock.calls.filter(([method]) => method === "POST");
    expect(posts.map(([, path]) => path)).toEqual(["/account-operations/operation-1/cancel", "/accounts/1/operations"]);
    expect(posts[1][2]).toMatchObject({ repeat: "quota_cycle", payload: job.requested });
    expect(posts[1][2]).not.toHaveProperty("scheduled_at");
    expect(getOperations().find(row => row.id === job.id)?.status).toBe("cancelled");
    expect(getOperations().find(row => row.id === "new-operation")?.repeat_series_id).toBe("new-operation");
  });

  it.each(["running", "needs_confirmation"])(
    "keeps cancellation for single tests and queued batches while a dispatched batch is %s",
    async (status) => {
      const jobs = [
        operation({
          id: "batch-dispatched",
          account_name: "Dispatched batch account",
          action: "test",
          batch_id: "connection-batch",
          status,
          started_at: "2026-10-13T02:00:00Z",
          requested: { confirmed: true },
        }),
        operation({
          id: "single-dispatched",
          account_name: "Single test account",
          action: "test",
          status,
          started_at: "2026-10-13T02:00:00Z",
          requested: { confirmed: true },
        }),
        operation({
          id: "batch-queued",
          account_name: "Queued batch account",
          action: "test",
          batch_id: "connection-batch",
          status: "queued",
          requested: { confirmed: true },
        }),
        operation({
          id: "scheduled-dispatched",
          action: "test",
          status,
          scheduled_at: "2030-01-02T01:31:00+00:00",
          started_at: "2030-01-02T01:31:00+00:00",
          requested: { confirmed: true, model_id: "fixture-model" },
        }),
      ];
      vi.mocked(api).mockImplementation(async (method, path) => {
        if (method === "GET" && path === "/account-operations") return { items: jobs } as never;
        if (method === "POST" && path === "/account-operations/single-dispatched/cancel") {
          return { ...jobs[1], status: "cancelled" } as never;
        }
        throw new Error("unexpected " + method + " " + path);
      });

      await renderPanel("panel-test-cancellation");
      await act(async () => entryButton().click());
      await flush();
      const articles = [...container.querySelectorAll('[role="dialog"] .operation-list article')];
      expect(articles).toHaveLength(4);
      const cancelButtons = articles.map((article) => [...article.querySelectorAll<HTMLButtonElement>("button")]
        .find((button) => button.textContent === "取消"));
      expect(cancelButtons[0]).toBeUndefined();
      expect(cancelButtons[1]).toBeDefined();
      expect(cancelButtons[2]).toBeDefined();
      expect(cancelButtons[3]).toBeUndefined();
      expect(articles[3].querySelector("time")?.textContent).toContain("09:31");

      await act(async () => cancelButtons[1]!.click());
      await flush();
      expect(api).toHaveBeenCalledWith("POST", "/account-operations/single-dispatched/cancel", {});
      expect(getOperations().find((job) => job.id === "single-dispatched")?.status).toBe("cancelled");
      expect(getOperations().find((job) => job.id === "batch-dispatched")?.status).toBe(status);
    },
  );

  it("shows every item in a batch larger than thirty and returns to all operations", async () => {
    const sourceOperation = operation({
      id: "operation-summary",
      account_name: "Batch source operation",
      batch_id: "batch-36",
      status: "failed",
      reason: "批次中有失败项",
    });
    const batchItems = Array.from({ length: 36 }, (_, index) =>
      operation({
        id: "batch-operation-" + index,
        batch_id: "batch-36",
        account_id: index + 1,
        account_name: "Batch account " + (index + 1),
        status: "completed",
      }),
    );
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method === "GET" && path === "/account-operations") return { items: [sourceOperation] } as never;
      if (method === "GET" && path === "/account-operations?batch_id=batch-36") {
        return { batch_id: "batch-36", items: batchItems, pending: 0 } as never;
      }
      throw new Error("unexpected " + method + " " + path);
    });

    bindOperationConnection("panel-batch-history");
    setOperations([sourceOperation]);
    await renderPanel("panel-batch-history");
    await act(async () => entryButton().click());
    await flush();

    await act(async () => buttonByText("查看整批").click());
    await flush();

    expect(vi.mocked(api).mock.calls.some(
      ([method, path]) => method === "GET" && path === "/account-operations?batch_id=batch-36",
    )).toBe(true);
    expect(container.querySelectorAll('[role="dialog"] .operation-list article')).toHaveLength(36);
    expect(container.textContent).toContain("Batch account 36");

    await act(async () => buttonByText("返回全部待办").click());
    await flush();

    expect(container.querySelectorAll('[role="dialog"] .operation-list article')).toHaveLength(1);
    expect(container.textContent).toContain("Batch source operation");
  });

  it("drops a batch response that arrives after the connection changes", async () => {
    const sourceOperation = operation({
      id: "operation-summary",
      batch_id: "batch-stale",
      status: "queued",
    });
    const staleBatchRead = deferred<{ batch_id: string; items: Operation[]; pending: number }>();
    const currentConnectionOperation = operation({
      id: "operation-current",
      account_id: 99,
      account_name: "Current connection operation",
      status: "queued",
    });
    const staleBatchOperation = operation({
      id: "operation-stale-batch",
      account_name: "Stale batch account",
      batch_id: "batch-stale",
      status: "completed",
    });
    let serverConnection = "panel-old-connection";
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method === "GET" && path === "/account-operations") {
        return {
          items: serverConnection === "panel-old-connection"
            ? [sourceOperation]
            : [currentConnectionOperation],
        } as never;
      }
      if (method === "GET" && path === "/account-operations?batch_id=batch-stale") {
        return staleBatchRead.promise as never;
      }
      throw new Error("unexpected " + method + " " + path);
    });

    bindOperationConnection(serverConnection);
    setOperations([sourceOperation]);
    await renderPanel(serverConnection);
    await act(async () => entryButton().click());
    await flush();
    await act(async () => buttonByText("查看整批").click());
    await flush();

    expect(vi.mocked(api).mock.calls.some(
      ([method, path]) => method === "GET" && path === "/account-operations?batch_id=batch-stale",
    )).toBe(true);

    await act(async () => {
      serverConnection = "panel-new-connection";
      bindOperationConnection(serverConnection);
      setOperations([currentConnectionOperation]);
      root.render(
        <AccountOperations
          accounts={[account]}
          online
          connectionKey={serverConnection}
          report={() => {}}
          modelTest={() => {}}
          template={() => {}}
          priorities={() => {}}
        />,
      );
    });
    await flush();

    await act(async () => {
      staleBatchRead.resolve({
        batch_id: "batch-stale",
        items: [staleBatchOperation],
        pending: 0,
      });
      await staleBatchRead.promise;
    });
    await flush();

    expect(getOperations()).toEqual([currentConnectionOperation]);
    expect(container.textContent).not.toContain("Stale batch account");
    expect(container.querySelector('[role="dialog"]')).toBeNull();
  });

  it("uses the template callback to reopen a template preview for the operation account", async () => {
    const needsConfirmation = operation({
      id: "operation-template",
      status: "needs_confirmation",
      reason: "配置版本已变化",
    });
    const template = vi.fn();
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method === "GET" && path === "/account-operations") return { items: [needsConfirmation] } as never;
      throw new Error("unexpected " + method + " " + path);
    });

    await act(async () => {
      root.render(
        <AccountOperations
          accounts={[account]}
          online
          connectionKey="panel-template-preview"
          report={() => {}}
          modelTest={() => {}}
          template={template}
          priorities={() => {}}
        />,
      );
    });
    await flush();
    await act(async () => entryButton().click());
    await flush();

    await act(async () => buttonByText("重新预览模板").click());

    expect(template).toHaveBeenCalledWith(account);
    expect(container.querySelector('[role="dialog"]')).toBeNull();
  });
});
