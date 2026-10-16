// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { api } from "./bridge";
import ConnectionStatusCard from "./ConnectionStatusCard";

vi.mock("./bridge", () => ({ api: vi.fn() }));
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

type ConnectionStatus = {
  state: string;
  endpoint: string;
  http_status: number | null;
  message: string;
  retryable: boolean;
  checked_at: string | null;
};

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

let container: HTMLDivElement;
let root: Root;

async function render(connectionKey: string) {
  await act(async () => root.render(<ConnectionStatusCard connectionKey={connectionKey} />));
}

async function settle() {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}

async function click(label: string) {
  const button = [...container.querySelectorAll<HTMLButtonElement>("button")].find(
    (item) => item.textContent?.includes(label),
  );
  if (!button) throw new Error(`Missing button: ${label}`);
  await act(async () => button.click());
}

beforeEach(() => {
  vi.resetAllMocks();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
});

describe("connection status card", () => {
  it("ignores a delayed response from the previous connection", async () => {
    const previous = deferred<ConnectionStatus>();
    const current = deferred<ConnectionStatus>();
    let request = 0;
    vi.mocked(api).mockImplementation(async () =>
      (request++ === 0 ? previous.promise : current.promise) as never,
    );

    await render("connection-old");
    await render("connection-current");

    await act(async () => {
      current.resolve({
        state: "ok",
        endpoint: "https://example.invalid/current",
        http_status: 200,
        message: "当前连接结果",
        retryable: false,
        checked_at: null,
      });
      await current.promise;
    });
    expect(container.textContent).toContain("当前连接结果");

    await act(async () => {
      previous.resolve({
        state: "network_unreachable",
        endpoint: "https://example.invalid/previous",
        http_status: null,
        message: "迟到的旧连接结果",
        retryable: true,
        checked_at: null,
      });
      await previous.promise;
    });

    expect(container.textContent).toContain("当前连接结果");
    expect(container.textContent).not.toContain("迟到的旧连接结果");
  });

  it("allows retry after a failed status request", async () => {
    const connected: ConnectionStatus = {
      state: "ok",
      endpoint: "https://example.invalid/status",
      http_status: 200,
      message: "重试已恢复",
      retryable: false,
      checked_at: null,
    };
    let request = 0;
    vi.mocked(api).mockImplementation(async () => {
      if (request++ === 0) throw new Error("fixture network failure");
      return connected as never;
    });

    await render("connection");
    await settle();
    expect(container.querySelector('[role="alert"]')?.textContent).toBe(
      "fixture network failure",
    );

    await click("重新检查");
    await settle();

    expect(api).toHaveBeenCalledTimes(2);
    expect(container.querySelector('[role="alert"]')).toBeNull();
    expect(container.textContent).toContain("重试已恢复");
  });
});
