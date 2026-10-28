// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import ReauthorizeAccountDialog from "./ReauthorizeAccountDialog";
import { api, command } from "./bridge";
import type { Account } from "./types";
vi.mock("./bridge", () => ({ api: vi.fn(), command: vi.fn() }));
vi.mock("./mobile", () => ({ useBackAction: vi.fn() }));
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
let root: Root, container: HTMLDivElement;
const close = vi.fn(), report = vi.fn();
const account = { id: 7, name: "Fixture", platform: "openai", type: "oauth" } as Account;
const button = (label: string) => [...container.querySelectorAll("button")].find(b => b.textContent === label)!;
async function click(label: string) { await act(async () => button(label).click()); }
async function input(value: string) { await act(async () => { const field = container.querySelector("textarea")!; Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!.call(field, value); field.dispatchEvent(new Event("input", { bubbles: true })); }); }
async function render(key = "one") { await act(async () => root.render(<ReauthorizeAccountDialog key={key} account={account} online close={close} report={report}/>)); }
const baseline = async (_m: string, p: string): Promise<never> => {
  if (p.endsWith("/settings")) return { proxy_id: 11 } as never;
  if (p.endsWith("/authorize")) return { auth_url: "https://example.invalid/authorize?state=fixture-state", session_id: "fixture-session" } as never;
  if (p.endsWith("/exchange")) return { access_token: "fixture-access", refresh_token: "fixture-refresh", chatgpt_account_id: "fixture-id" } as never;
  if (p.endsWith("/apply-oauth-credentials")) return { account_id: 7, verified: true, status: "active" } as never;
  throw new Error("unexpected route");
};
beforeEach(() => { vi.clearAllMocks(); container = document.createElement("div"); document.body.append(container); root = createRoot(container); vi.mocked(api).mockImplementation(baseline); vi.mocked(command).mockResolvedValue(undefined as never); });
afterEach(async () => { await act(async () => root.unmount()); container.remove(); });

it("uses the account proxy, verifies callback state and applies identity only once without storage", async () => {
  const storage = vi.spyOn(Storage.prototype, "setItem");
  await render(); await click("生成授权链接"); await click("浏览器授权");
  expect(command).toHaveBeenCalledWith("open_authorization_url", { url: "https://example.invalid/authorize?state=fixture-state" });
  expect(api).toHaveBeenCalledWith("POST", "/account-creation/auth/openai/authorize", { proxy_id: 11 });
  await input("https://example.invalid/callback?code=fixture-code&state=wrong"); await click("完成授权");
  expect(container.textContent).toContain("不属于本次授权");
  await input("https://example.invalid/callback?code=fixture-code&state=fixture-state");
  await act(async () => { button("完成授权").click(); button("完成授权").click(); });
  expect(api).toHaveBeenCalledWith("POST", "/account-creation/auth/openai/exchange", { session_id: "fixture-session", code: "fixture-code", state: "fixture-state", proxy_id: 11 });
  expect(vi.mocked(api).mock.calls.filter(([, p]) => p.endsWith("/apply-oauth-credentials"))).toHaveLength(1);
  expect(close).toHaveBeenCalledTimes(1); expect(storage).not.toHaveBeenCalled(); storage.mockRestore();
});
it("discards a late exchange after closing or changing connection", async () => {
  let resolve!: (v: unknown) => void;
  vi.mocked(api).mockImplementation((m, p) => p.endsWith("/exchange") ? new Promise(r => { resolve = r; }) as never : baseline(m, p));
  await render(); await click("生成授权链接"); await input("fixture-code"); await click("完成授权");
  await render("two"); await act(async () => resolve({ access_token: "fixture-late" }));
  expect(container.querySelector("textarea")).toBeNull();
  expect(vi.mocked(api).mock.calls.some(([, p]) => p.endsWith("/apply-oauth-credentials"))).toBe(false);
});
it("does not replay an uncertain write or show sensitive upstream errors", async () => {
  vi.mocked(api).mockImplementation((m, p) => p.endsWith("/apply-oauth-credentials") ? Promise.reject(new Error("fixture-access-secret")) : baseline(m, p));
  await render(); await click("生成授权链接"); await input("fixture-code"); await click("完成授权");
  expect(container.textContent).toContain("未重放请求"); expect(container.textContent).not.toContain("fixture-access-secret");
  expect(button("完成授权").disabled).toBe(true); expect(container.querySelector("textarea")).toBeNull();
});
