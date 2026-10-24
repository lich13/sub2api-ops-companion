// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import Sub2ApiConfig, { type ConfigDocument } from "./Sub2ApiConfig";
import { configPreview } from "./sub2apiConfigPreview";
import { api } from "./bridge";
vi.mock("./bridge", () => ({ api: vi.fn() }));
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
let container: HTMLDivElement, root: Root, fixture: ConfigDocument;
const button = (text: string) =>
  [...container.querySelectorAll<HTMLButtonElement>("button")].find(
    (b) => b.textContent === text,
  )!;
const input = (path: string) =>
  container.querySelector<HTMLInputElement>(`[aria-label="${path}"]`)!;
async function fill(el: HTMLInputElement, value: string) {
  await act(async () => {
    Object.getOwnPropertyDescriptor(
      HTMLInputElement.prototype,
      "value",
    )!.set!.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
}
beforeEach(() => {
  fixture = structuredClone(
    configPreview("/sub2api-config", {}) as ConfigDocument,
  );
  vi.mocked(api).mockImplementation(async (_method, path) => {
    if (path === "/sub2api-config") return fixture as never;
    if (path.endsWith("/validate")) return { valid: true, errors: [] } as never;
    if (path.endsWith("/apply"))
      return { status: "restarting", message: "正在重启" } as never;
    if (path.endsWith("/operation"))
      return { status: "restored", message: "已恢复上次配置" } as never;
    throw new Error(path);
  });
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});
afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.clearAllMocks();
});
it("shows runtime version, explains overrides and keeps them read only", async () => {
  await act(async () => root.render(<Sub2ApiConfig active online />));
  expect(container.textContent).toContain("运行 0.2.18");
  expect(input("server.port").disabled).toBe(true);
  expect(container.textContent).toContain("SERVER_PORT");
  await fill(input("搜索配置字段"), "jwt.expire_hour");
  expect(input("jwt.expire_hour").value).toBe("168");
  expect(input("jwt.expire_hour").disabled).toBe(true);
});
it("keeps edits across tab changes and preserves draft after automatic restoration", async () => {
  await act(async () => root.render(<Sub2ApiConfig active online />));
  await fill(input("搜索配置字段"), "load_batch_cache_ttl_ms");
  await fill(input("gateway.scheduling.load_batch_cache_ttl_ms"), "300");
  await act(async () => root.render(<Sub2ApiConfig active={false} online />));
  await act(async () => root.render(<Sub2ApiConfig active online />));
  expect(input("gateway.scheduling.load_batch_cache_ttl_ms").value).toBe("300");
  await act(async () => button("应用并重启").click());
  expect(container.querySelector('[role="dialog"]')?.textContent).toContain(
    "300",
  );
  await act(async () => button("确认应用并重启").click());
  expect(container.textContent).toContain("已恢复上次配置");
  expect(input("gateway.scheduling.load_batch_cache_ttl_ms").value).toBe("300");
  expect(
    vi.mocked(api).mock.calls.find((c) => c[1].endsWith("/apply"))?.[2],
  ).toMatchObject({
    changes: [
      {
        path: "gateway.scheduling.load_batch_cache_ttl_ms",
        remove: false,
        value: 300,
      },
    ],
  });
});
it("blocks writes for an unmatched version", async () => {
  fixture.editable = false;
  fixture.reason = "运行版本没有匹配的配置目录";
  await act(async () => root.render(<Sub2ApiConfig active online />));
  expect(container.textContent).toContain(fixture.reason);
  expect(button("应用并重启").disabled).toBe(true);
});
it("refreshes runtime identity on reopening and stops stale drafts", async () => {
  await act(async () => root.render(<Sub2ApiConfig active online />));
  await fill(input("搜索配置字段"), "load_batch_cache_ttl_ms");
  await fill(input("gateway.scheduling.load_batch_cache_ttl_ms"), "300");
  await act(async () => root.render(<Sub2ApiConfig active={false} online />));
  fixture = { ...fixture, runtime_revision: "b".repeat(64) };
  await act(async () => root.render(<Sub2ApiConfig active online />));
  expect(container.textContent).toContain("文件或运行进程已变更");
  expect(input("gateway.scheduling.load_batch_cache_ttl_ms").value).toBe("300");
  expect(button("应用并重启").disabled).toBe(true);
});
it("locates field validation errors and keeps the draft", async () => {
  const original = vi.mocked(api).getMockImplementation()!;
  vi.mocked(api).mockImplementation(async (...args) =>
    args[1].endsWith("/validate")
      ? ({
          valid: false,
          errors: [
            {
              path: "gateway.scheduling.load_batch_cache_ttl_ms",
              message: "必须大于 0",
            },
          ],
        } as never)
      : original(...args),
  );
  await act(async () => root.render(<Sub2ApiConfig active online />));
  await fill(input("搜索配置字段"), "load_batch_cache_ttl_ms");
  await fill(input("gateway.scheduling.load_batch_cache_ttl_ms"), "-1");
  await act(async () => button("应用并重启").click());
  expect(container.textContent).toContain("必须大于 0");
  expect(container.querySelector('[role="dialog"]')).toBeNull();
  expect(input("gateway.scheduling.load_batch_cache_ttl_ms").value).toBe("-1");
});
it("can recover an ongoing operation after reopening while the upstream is offline", async () => {
  const original = vi.mocked(api).getMockImplementation()!;
  let reads = 0;
  vi.mocked(api).mockImplementation(async (...args) =>
    args[1].endsWith("/operation")
      ? ({
          status: ++reads === 1 ? "restarting" : "restored",
          message: reads === 1 ? "正在重启" : "已恢复上次配置",
        } as never)
      : original(...args),
  );
  await act(async () => root.render(<Sub2ApiConfig active online={false} />));
  expect(reads).toBe(2);
  expect(container.textContent).toContain("已恢复上次配置");
});
