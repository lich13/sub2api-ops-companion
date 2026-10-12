// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import ProxyOptions, { type ProxyOption } from "./ProxyOptions";
import { api } from "./bridge";

vi.mock("./bridge", () => ({ api: vi.fn() }));
vi.mock("./accountOperations", () => ({ operationEpoch: vi.fn(() => 1) }));
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

const proxies: ProxyOption[] = [
  { id: 2, name: "Fixture proxy", protocol: "http", status: "active" },
  { id: 3, name: "Offline proxy", protocol: "socks5", status: "disabled" },
  { id: 4, name: "Other fixture proxy", protocol: "https", status: "active" },
];
const connection = {
  success: true,
  message: "代理连接正常",
  latency_ms: 31,
  ip_address: "203.0.113.7",
  city: "Taipei",
  region: "Taiwan",
  country: "Taiwan",
  country_code: "TW",
};
const quality = {
  score: 88,
  grade: "yellow",
  summary: "一个目标需要关注",
  exit_ip: "203.0.113.8",
  country: "Taiwan",
  base_latency_ms: 20,
  items: [{ target: "example.invalid", status: "warn", http_status: 403, latency_ms: 42, message: "返回挑战页" }],
};

let container: HTMLDivElement;
let root: Root;
const click = async (element: Element) => {
  await act(async () => (element as HTMLElement).click());
};
const diagnosticButton = (name: string) => container.querySelector<HTMLButtonElement>(`button[aria-label="${name}"]`)!;
const clearProxyLabel = () => [...container.querySelectorAll(".proxy-options label")]
  .find((label) => label.textContent?.includes("不使用代理"))!;

async function flush() {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}

function render(value: number | null = 2, onChange = vi.fn(), online = true, connectionKey = "fixture-connection") {
  return act(async () => root.render(
    <ProxyOptions proxies={proxies} value={value} onChange={onChange} online={online} connectionKey={connectionKey}/>,
  ));
}

beforeEach(() => {
  vi.clearAllMocks();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  vi.mocked(api).mockImplementation(async (_method, path) => {
    if (path.endsWith("/test")) return structuredClone(connection) as never;
    if (path.endsWith("/quality-check")) return structuredClone(quality) as never;
    throw new Error(`Unexpected fixture request: ${path}`);
  });
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
});

describe("shared proxy options", () => {
  it("keeps diagnostics separate from proxy selection and renders safe connection details", async () => {
    const onChange = vi.fn();
    await render(2, onChange);
    expect(clearProxyLabel().querySelector("button")).toBeNull();

    await click(diagnosticButton("检测代理 4"));

    expect(api).toHaveBeenCalledWith("POST", "/proxies/4/test");
    expect(onChange).not.toHaveBeenCalled();
    expect([...container.querySelectorAll<HTMLInputElement>('input[type="radio"]')].filter((input) => input.checked)).toHaveLength(1);
    expect(container.textContent).toContain("203.0.113.7");
    expect(container.textContent).toContain("31 ms");
  });

  it("runs quality checks once and renders the safe per-target result", async () => {
    let resolveQuality!: (value: typeof quality) => void;
    const pendingQuality = new Promise<typeof quality>((resolve) => { resolveQuality = resolve; });
    vi.mocked(api).mockImplementation(async (_method, path) => {
      if (path.endsWith("/quality-check")) return pendingQuality as never;
      if (path.endsWith("/test")) return structuredClone(connection) as never;
      throw new Error(`Unexpected fixture request: ${path}`);
    });
    await render();
    const check = diagnosticButton("质量检查代理 4");

    await click(check);
    expect(check.disabled).toBe(true);
    await click(check);

    expect(api).toHaveBeenCalledTimes(1);
    expect(api).toHaveBeenCalledWith("POST", "/proxies/4/quality-check");
    await act(async () => resolveQuality(quality));
    await flush();
    expect(container.textContent).toContain("88 分 · yellow");
    expect(container.textContent).toContain("example.invalid");
    expect(container.textContent).toContain("返回挑战页");
  });

  it("disables diagnostic submissions while offline", async () => {
    await render(2, vi.fn(), false);

    expect(diagnosticButton("检测代理 2").disabled).toBe(true);
    expect(diagnosticButton("质量检查代理 2").disabled).toBe(true);
    await click(diagnosticButton("检测代理 2"));
    expect(api).not.toHaveBeenCalled();
  });

  it("drops a late diagnostic result after the connection changes", async () => {
    let resolveOld!: (value: typeof connection) => void;
    const oldResponse = new Promise<typeof connection>((resolve) => { resolveOld = resolve; });
    vi.mocked(api).mockReturnValueOnce(oldResponse as never);
    await render();
    await click(diagnosticButton("检测代理 2"));

    await render(2, vi.fn(), true, "new-fixture-connection");
    await act(async () => resolveOld({ ...connection, ip_address: "198.51.100.99", message: "late fixture result" }));
    await flush();

    expect(container.textContent).not.toContain("198.51.100.99");
    expect(container.textContent).not.toContain("late fixture result");
  });

  it("shows an explicit unsupported-interface response without retrying", async () => {
    vi.mocked(api).mockRejectedValueOnce(new Error("Sub2API 不支持此代理检测接口"));
    await render();
    await click(diagnosticButton("检测代理 2"));
    await flush();

    expect(container.querySelector('[role="alert"]')?.textContent).toContain("不支持");
    expect(api).toHaveBeenCalledTimes(1);
  });
});
