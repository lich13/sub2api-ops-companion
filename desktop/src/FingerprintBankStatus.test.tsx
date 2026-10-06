// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import FingerprintBankStatus, { type BankVersion } from "./FingerprintBankStatus";
import { api } from "./bridge";

vi.mock("./bridge", () => ({ api: vi.fn() }));

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

const currentVersion: BankVersion = {
  revision: "current-revision-1234",
  sha256: "current-sha256",
  built_at: "2026-10-06T01:00:00Z",
  analyzer_version: 8,
};

const taskVersion: BankVersion = {
  revision: "task-revision-5678",
  sha256: "task-sha256",
  built_at: "2026-10-05T01:00:00Z",
  analyzer_version: 7,
};

const bank = (overrides: Record<string, unknown> = {}) => ({
  version: currentVersion,
  source: "bundled",
  status: "ready",
  result: "",
  checked_at: 1791248400,
  synced_at: 1791244800,
  ...overrides,
});

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  vi.clearAllMocks();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.useRealTimers();
});

async function flush() {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}

describe("fingerprint bank status", () => {
  it("renders the current bank details and the task's fixed version", async () => {
    vi.mocked(api).mockResolvedValue(bank() as never);
    await act(async () => root.render(
      <FingerprintBankStatus online taskVersion={taskVersion} />,
    ));
    await flush();

    expect(container.textContent).toContain("指纹库 current-");
    expect(container.textContent).toContain("内置库");
    expect(container.textContent).toContain("本轮使用");
    expect(container.textContent).toContain("task-rev");
    expect(container.textContent).toContain("分析器 7");
    expect(container.textContent).toContain("task-sha256");
    expect(vi.mocked(api).mock.calls[0]).toEqual(["GET", "/modeltrace/fingerprint-bank"]);
  });

  it("checks for an update, follows the checking state, and reports the updated bank", async () => {
    vi.useFakeTimers();
    let reads = 0;
    vi.mocked(api).mockImplementation(async (method, path) => {
      if (method === "GET" && path === "/modeltrace/fingerprint-bank") {
        reads += 1;
        if (reads === 1) return bank() as never;
        if (reads === 2) return bank({ status: "checking", result: "" }) as never;
        return bank({
          version: { ...currentVersion, revision: "updated-revision-9999" },
          result: "updated",
        }) as never;
      }
      if (method === "POST" && path === "/modeltrace/fingerprint-bank/sync") return {} as never;
      throw new Error("unexpected " + method + " " + path);
    });
    await act(async () => root.render(<FingerprintBankStatus online />));
    await flush();
    await act(async () => container.querySelector<HTMLButtonElement>("button")!.click());
    expect(container.textContent).toContain("检查中");
    expect(vi.mocked(api).mock.calls).toContainEqual(["POST", "/modeltrace/fingerprint-bank/sync", {}]);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(1000);
    });
    expect(container.textContent).toContain("已更新");
    expect(container.textContent).toContain("updated-");
    expect(reads).toBe(3);
  });
});
