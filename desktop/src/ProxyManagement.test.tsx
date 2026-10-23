// @vitest-environment jsdom
import { act, StrictMode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import ProxyManagement from "./ProxyManagement";
import { ProxyEditor, ProxyBatchAdd } from "./ProxyDialogs";
import { api } from "./bridge";
import type { ProxyRow } from "./proxyData";
vi.mock("./bridge", () => ({
  api: vi.fn(),
  command: vi.fn(async () => {}),
  preview: true,
}));
vi.mock("./mobile", () => ({ useBackAction: vi.fn() }));
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
let container: HTMLDivElement, root: Root;
const row: ProxyRow = {
  id: 7,
  name: "Fixture",
  protocol: "socks5",
  host: "::1",
  port: 1080,
  status: "active",
  username_set: true,
  password_set: true,
  account_count: 2,
  expires_at: "2031-04-05T10:00:25Z",
};
beforeEach(() => {
  vi.useFakeTimers();
  vi.clearAllMocks();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  vi.mocked(api).mockResolvedValue({
    items: [row, { ...row, id: 8, name: "Other" }],
    page: 1,
    page_size: 20,
    total: 21,
    pages: 2,
  } as never);
});
afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.useRealTimers();
});
const click = async (el: Element) => {
  await act(async () => (el as HTMLElement).click());
};
const button = (text: string) =>
  [...container.querySelectorAll<HTMLButtonElement>("button")].find(
    (b) => b.textContent === text,
  )!;
async function change(
  input: HTMLInputElement | HTMLSelectElement | HTMLTextAreaElement,
  value: string,
) {
  await act(async () => {
    Object.getOwnPropertyDescriptor(
      Object.getPrototypeOf(input),
      "value",
    )!.set!.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
    input.dispatchEvent(new Event("change", { bubbles: true }));
  });
}
async function render(online = true, key = "fixture") {
  await act(async () =>
    root.render(
      <StrictMode>
        <ProxyManagement active online={online} connectionKey={key} />
      </StrictMode>,
    ),
  );
  await act(async () => vi.advanceTimersByTimeAsync(200));
}
describe("IP administration", () => {
  it("loads filters and pages without reading authentication", async () => {
    await render();
    expect(container.textContent).toContain("[::1]:1080");
    expect(
      vi.mocked(api).mock.calls.every(([, p]) => !p.endsWith("/credentials")),
    ).toBe(true);
    await change(container.querySelector('[aria-label="筛选协议"]')!, "socks5");
    await act(async () => vi.advanceTimersByTimeAsync(200));
    expect(vi.mocked(api).mock.calls.at(-1)?.[1]).toContain("protocol=socks5");
    await click(button("下一页"));
    await act(async () => vi.advanceTimersByTimeAsync(200));
    expect(vi.mocked(api).mock.calls.at(-1)?.[1]).toContain("page=2");
    await change(container.querySelector('[aria-label="搜索 IP"]')!, "new");
    await act(async () => vi.advanceTimersByTimeAsync(200));
    expect(vi.mocked(api).mock.calls.at(-1)?.[1]).toContain("page=1");
  });
  it("preserves credentials and precise expiry unless explicitly cleared", async () => {
    const saved = vi.fn();
    await act(async () =>
      root.render(
        <ProxyEditor row={row} online close={() => {}} saved={saved} />,
      ),
    );
    await click(button("保存"));
    expect(api).toHaveBeenCalledWith(
      "PUT",
      "/proxy-management/7",
      expect.not.objectContaining({ password: expect.anything() }),
    );
    const body = vi.mocked(api).mock.calls[0][2] as object;
    expect(body).not.toHaveProperty("username");
    expect(body).not.toHaveProperty("expires_at");
    await click(container.querySelector('input[type="checkbox"]')!);
    await click(button("保存"));
    expect(vi.mocked(api).mock.calls.at(-1)?.[2]).toMatchObject({
      username: "",
      password: "",
    });
    expect(saved).toHaveBeenCalledTimes(2);
  });
  it("shows per-item batch failure without repeating uncertain writes", async () => {
    const saved = vi.fn();
    vi.mocked(api)
      .mockResolvedValueOnce({ created: 1, skipped: 0 } as never)
      .mockRejectedValueOnce(new Error("fixture"));
    await act(async () =>
      root.render(<ProxyBatchAdd online close={() => {}} saved={saved} />),
    );
    await change(
      container.querySelector("textarea")!,
      "socks5://[::1]:1080\nhttp://127.0.0.1:8080",
    );
    await click(button("添加 2 个"));
    expect(api).toHaveBeenCalledTimes(2);
    expect(saved).not.toHaveBeenCalled();
    expect(container.textContent).toContain("结果未确认");
    expect(container.textContent).toContain("已添加");
    expect(button("添加 2 个")).toBeUndefined();
  });
  it("reports linked-account deletion failures and only deletes selected rows", async () => {
    await render();
    await click(container.querySelector('[aria-label="全选 IP"]')!);
    const original = vi.mocked(api).getMockImplementation()!;
    vi.mocked(api).mockImplementation(async (m, p, b) =>
      p.endsWith("batch-delete")
        ? ({
            deleted_ids: [7],
            skipped: [{ id: 8, reason: "代理仍有关联账号" }],
          } as never)
        : original(m, p, b),
    );
    await click(container.querySelector(".proxy-selection .danger-text")!);
    await click(container.querySelector(".proxy-dialog .danger")!);
    expect(api).toHaveBeenCalledWith("POST", "/proxy-management/batch-delete", {
      ids: [7, 8],
    });
    expect(container.textContent).toContain("代理仍有关联账号");
  });
  it("discards stale authentication on connection change and disables writes offline", async () => {
    await render();
    let resolve!: (v: unknown) => void;
    vi.mocked(api).mockImplementation(
      async () =>
        new Promise((r) => {
          resolve = r;
        }) as never,
    );
    await click(container.querySelector('[aria-label="查看 Fixture 认证"]')!);
    await render(false, "another");
    await act(async () =>
      resolve({ username: "fixture-private", password: "fixture-private" }),
    );
    expect(container.textContent).not.toContain("fixture-private");
    expect(button("新增 IP").disabled).toBe(true);
  });
  it("uses existing diagnostic routes and shows the quality report", async () => {
    await render();
    vi.mocked(api).mockResolvedValueOnce({
      score: 90,
      grade: "A",
      summary: "fixture report",
      exit_ip: "::1",
      country: "",
      base_latency_ms: 24,
      items: [
        {
          target: "example.invalid",
          status: "pass",
          http_status: 200,
          latency_ms: 24,
          message: "通过",
        },
      ],
    } as never);
    await click(container.querySelectorAll(".proxy-row-actions button")[1]);
    expect(api).toHaveBeenCalledWith("POST", "/proxies/7/quality-check");
    expect(container.querySelector('[role="dialog"]')?.textContent).toContain(
      "fixture report",
    );
  });
});

it("closes batch add only after every proxy is confirmed", async () => {
  const saved=vi.fn();
  vi.mocked(api).mockResolvedValue({created:1,skipped:0} as never);
  await act(async()=>root.render(<ProxyBatchAdd online close={vi.fn()} saved={saved}/>));
  await change(container.querySelector("textarea")!, "socks5://[::1]:1080\nhttp://127.0.0.1:8080");
  await click(button("添加 2 个"));
  expect(saved).toHaveBeenCalledOnce(); expect(api).toHaveBeenCalledTimes(2);
});
