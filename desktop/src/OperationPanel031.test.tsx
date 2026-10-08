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
  managed: false,
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
      />,
    );
  });
  await flush();
}

describe("operation panel batch history", () => {
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
