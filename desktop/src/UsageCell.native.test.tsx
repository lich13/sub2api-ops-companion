// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import UsageCell, { usageRefreshEvent } from "./UsageCell";
import { api } from "./bridge";
import type { Account, UsageSummary } from "./types";
vi.mock("./bridge", () => ({ api: vi.fn() }));
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
const summary = (): UsageSummary => ({ branch: "apikey", native_read: true, windows: [], actions: ["query_usage"], reset_credits: null, today: { requests: 0, tokens: 0, cost: 0, standard_cost: 0, user_cost: 0 } });
const account = (overrides: Partial<Account> = {}) => ({ id: 1, name: "fixture", version: "changing-snapshot", operation_versions: { usage: "stable-identity" }, usage: summary(), ...overrides } as Account);
let root: Root, container: HTMLDivElement;
const render = async (row = account(), active = true, connectionKey = "fixture-a") => {
  await act(async () => root.render(<UsageCell account={row} active={active} online connectionKey={connectionKey} report={vi.fn()} refresh={async () => {}} />));
};
beforeEach(() => { vi.useFakeTimers(); vi.clearAllMocks(); container=document.createElement("div"); document.body.append(container); root=createRoot(container); });
afterEach(async () => { await act(async () => root.unmount()); container.remove(); vi.useRealTimers(); });
it("uses native reads only for supported rows and retains real zero local statistics", async () => {
  vi.mocked(api).mockResolvedValue({ ...summary(), status: "unavailable", message: "上游尚未提供用量窗口" });
  await render();
  expect(api).toHaveBeenCalledWith("GET", "/accounts/1/usage");
  expect(container.textContent).toContain("0 req"); expect(container.textContent).toContain("A $0.00");
  expect(container.textContent).toContain("上游尚未提供用量窗口");
  await render(account({ version: "new-snapshot" })); expect(api).toHaveBeenCalledTimes(1);
  await act(async () => vi.advanceTimersByTimeAsync(60_000)); await render(); expect(api).toHaveBeenCalledTimes(2);
  await render(account({ id: 2, usage: { ...summary(), native_read: false } })); expect(api).toHaveBeenCalledTimes(2);
});
it("keeps existing windows when a single native read fails", async () => {
  vi.mocked(api).mockRejectedValue(new Error("fixture failure"));
  await render(account({ usage: { ...summary(), windows: [{ key: "weekly", label: "7d", used_percent: 42, source: "upstream_snapshot", observed_at: null, reset_at: null, status: "stale" }] } }));
  expect(container.textContent).toContain("42%"); expect(container.textContent).toContain("异常"); expect(container.textContent).toContain("0 req");
});
it("keeps native statistics failures visible until a successful retry", async () => {
  const row = () => account({ usage: { ...summary(), stats_status: "ok" } });
  vi.mocked(api).mockResolvedValue({ ...summary(), stats_status: "error", today: null });
  await render(row());
  expect(container.textContent).toContain("统计暂不可用，请重试");
  expect(container.textContent).not.toContain("A $0.00");
  vi.mocked(api).mockResolvedValue({ ...summary(), stats_status: "ok" });
  await act(async () => vi.advanceTimersByTimeAsync(60_000)); await render(row());
  expect(container.textContent).not.toContain("统计暂不可用");
  expect(container.textContent).toContain("A $0.00");
});
it("retains the last native sample as error data after the next read fails", async () => {
  vi.mocked(api).mockResolvedValue({ ...summary(), windows: [{ key: "model", label: "模型", used_percent: 32, source: "upstream_usage", status: "known" }], balances: [{ currency: "CNY", balance: 2 }] });
  await render();
  vi.mocked(api).mockResolvedValue({ ...summary(), status: "error", message: "上游用量读取失败" });
  await act(async () => vi.advanceTimersByTimeAsync(60_000)); await render();
  expect(container.textContent).toContain("32%"); expect(container.textContent).toContain("CNY 2.00 · 异常");
});
it("applies the batch refresh result immediately without another upstream read", async () => {
  vi.mocked(api).mockResolvedValue(summary()); await render();
  const updated = { ...summary(), balances: [{ currency: "CNY", balance: 9 }] };
  const notify = (connectionKey: string) => document.dispatchEvent(new CustomEvent(usageRefreshEvent, { detail: { connectionKey, batch: { items: [{ account_id: 1, usage: updated }] } } }));
  await act(async () => notify("previous-connection")); expect(container.textContent).not.toContain("CNY 9.00");
  await act(async () => notify("fixture-a")); expect(container.textContent).toContain("CNY 9.00");
  expect(api).toHaveBeenCalledTimes(1);
});
it("discards a delayed response after connection changes and resumes cancelled reads", async () => {
  let resolve!: (value: UsageSummary) => void;
  vi.mocked(api).mockReturnValueOnce(new Promise<UsageSummary>(done => { resolve=done; }));
  await render();
  await render(account(), false);
  vi.mocked(api).mockResolvedValue({ ...summary(), balances: [{ currency: "CNY", balance: 3 }] });
  await render(account(), true, "fixture-b");
  expect(api).toHaveBeenCalledTimes(2);
  await act(async () => resolve({ ...summary(), balances: [{ currency: "USD", balance: 999 }] }));
  expect(container.textContent).toContain("CNY 3.00"); expect(container.textContent).not.toContain("999");
  await render(account(), false, "fixture-b");
  await render(account(), true, "fixture-b"); expect(api).toHaveBeenCalledTimes(3);
});
