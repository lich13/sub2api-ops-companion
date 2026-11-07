// @vitest-environment jsdom
import { act, type ComponentProps } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import Sub2ApiVersion from "./Sub2ApiVersion";
import { api, command } from "./bridge";
vi.mock("./bridge", () => ({ api: vi.fn(), command: vi.fn() }));
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
let root: Root, container: HTMLDivElement;
let running: string, hasUpdate: boolean, buildType: string, update: Record<string,unknown>;
const button = (name: string) => [...container.querySelectorAll<HTMLButtonElement>("button")].find(b => b.textContent === name)!;
const clicks = (path: string) => vi.mocked(api).mock.calls.filter(([m,p]) => m === "POST" && p === path);
const refreshed = vi.fn();
async function click(name: string) { await act(async () => button(name).click()); }
async function render(active = true, props: Partial<ComponentProps<typeof Sub2ApiVersion>> = {}) { await act(async () => root.render(<Sub2ApiVersion active={active} online onRuntimeChanged={refreshed} {...props} />)); }
beforeEach(() => {
  vi.clearAllMocks(); vi.useFakeTimers();
  running = "0.2.20"; hasUpdate = true; buildType = "release"; update = { message: "Update completed", need_restart: true };
  vi.mocked(api).mockImplementation(async (_method, path) => {
    if (path === "/capabilities") return { features: { sub2api_update: true } } as never;
    if (path.endsWith("/version")) return { version: running } as never;
    if (path.endsWith("check-updates?force=true")) return { current_version: running, latest_version: "0.2.21", has_update: hasUpdate, build_type: buildType, cached: false } as never;
    if (path.endsWith("/update")) return update as never;
    if (path.endsWith("/restart")) return { message: "Restarting" } as never;
    throw new Error(path);
  });
  vi.mocked(command).mockResolvedValue(undefined as never);
  container = document.createElement("div"); document.body.append(container); root = createRoot(container);
});
afterEach(async () => { await act(async () => root.unmount()); container.remove(); vi.useRealTimers(); });
it("checks updates and accepts the already latest response without requiring need_restart", async () => {
  await render(); expect(container.textContent).toContain("运行 0.2.20");
  await click("检查更新");
  update = { message: "Already up to date", already_up_to_date: true, current_version: "0.2.21", latest_version: "0.2.21" };
  await click("更新 Sub2API");
  expect(container.textContent).toContain("已经是最新版本"); expect(button("重启生效")).toBeUndefined();
  expect(clicks("/sub2api-system/update")).toHaveLength(1);
});
it("disables online updates for source builds and opens the native release page", async () => {
  buildType = "source"; await render(); await click("检查更新");
  expect(button("更新 Sub2API").disabled).toBe(true); expect(container.textContent).toContain("源码构建");
  await click("查看发布"); expect(command).toHaveBeenCalledWith("open_sub2api_release", { url: "https://github.com/Wei-Shaw/sub2api/releases" });
});
it("restarts once and checks only the current page until the updated version runs", async () => {
  await render(); await click("检查更新"); await click("更新 Sub2API"); await click("重启生效");
  await act(async () => vi.advanceTimersByTimeAsync(1500));
  expect(container.textContent).toContain("正在等待重启生效");
  running = "0.2.21"; await act(async () => vi.advanceTimersByTimeAsync(3000));
  expect(container.textContent).toContain("更新已生效"); expect(button("重启生效")).toBeUndefined();
  expect(clicks("/sub2api-system/restart")).toHaveLength(1);
  expect(vi.getTimerCount()).toBe(0);
  expect(refreshed).toHaveBeenCalledTimes(1);
});
it("stops restart checks on leaving the page and never repeats disconnected writes", async () => {
  await render(); await click("检查更新"); await click("更新 Sub2API");
  const original = vi.mocked(api).getMockImplementation()!;
  vi.mocked(api).mockImplementation((m,p,b) => p.endsWith("/restart") ? Promise.reject(new Error("连接暂时中断")) : original(m,p,b));
  await click("重启生效"); expect(container.textContent).toContain("连接暂时中断");
  await render(false); const calls = vi.mocked(api).mock.calls.length;
  await act(async () => vi.advanceTimersByTimeAsync(90000));
  expect(vi.mocked(api).mock.calls).toHaveLength(calls); expect(clicks("/sub2api-system/restart")).toHaveLength(1);
});
it("shows an update failure without retrying or clearing version information", async () => {
  await render(); await click("检查更新");
  const original = vi.mocked(api).getMockImplementation()!;
  vi.mocked(api).mockImplementation((m,p,b) => p.endsWith("/update") ? Promise.reject(new Error("fixture update failed")) : original(m,p,b));
  await click("更新 Sub2API"); await act(async () => vi.advanceTimersByTimeAsync(60000));
  expect(container.textContent).toContain("fixture update failed"); expect(container.textContent).toContain("运行 0.2.20");
  expect(clicks("/sub2api-system/update")).toHaveLength(1);
});

it("keeps a manual check available after restart verification times out", async () => {
  await render(); await click("检查更新"); await click("更新 Sub2API"); await click("重启生效");
  await act(async () => vi.advanceTimersByTimeAsync(62000));
  expect(container.textContent).toContain("尚未确认运行版本，请手动刷新");
  expect(button("检查更新").disabled).toBe(false); expect(vi.getTimerCount()).toBe(0);
  expect(clicks("/sub2api-system/restart")).toHaveLength(1);
  running = "0.2.21"; await click("检查更新");
  expect(refreshed).toHaveBeenCalledTimes(1);
  expect(button("重启生效")).toBeUndefined();
});

it("reuses the parent version and capabilities across page activation", async () => {
  await render(true, { loadingVersion: true });
  expect(api).not.toHaveBeenCalledWith("GET", "/sub2api-system/version");
  await render(true, { knownVersion: "0.2.20" });
  await render(false, { knownVersion: "0.2.20" });
  await render(true, { knownVersion: "0.2.20" });
  expect(container.textContent).toContain("运行 0.2.20");
  expect(vi.mocked(api).mock.calls.filter(([, path]) => path === "/capabilities")).toHaveLength(1);
  expect(api).not.toHaveBeenCalledWith("GET", "/sub2api-system/version");
});

it("reads the native version when the parent host cannot provide it", async () => {
  await render(true, { loadingVersion: true });
  await render(true, { loadingVersion: false });
  expect(container.textContent).toContain("运行 0.2.20");
  expect(vi.mocked(api).mock.calls.filter(([, path]) => path === "/sub2api-system/version")).toHaveLength(1);
  await render(false); await render();
  expect(vi.mocked(api).mock.calls.filter(([, path]) => path === "/sub2api-system/version")).toHaveLength(1);
});

it("falls back to the native version if a previously available configuration host fails", async () => {
  await render(true, { knownVersion: "0.2.20" });
  running = "0.2.21";
  await render(true, { loadingVersion: true });
  await render();
  expect(container.textContent).toContain("运行 0.2.21");
  expect(vi.mocked(api).mock.calls.filter(([, path]) => path === "/sub2api-system/version")).toHaveLength(1);
});
