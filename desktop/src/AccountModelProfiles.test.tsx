// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import AccountModelProfiles from "./AccountModelProfiles";
import { api } from "./bridge";

vi.mock("./bridge", () => ({ api: vi.fn() }));
vi.mock("./mobile", () => ({ useBackAction: vi.fn() }));
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
let root: Root, container: HTMLDivElement;
const config = { version: "a".repeat(64), configured: true,
  normal: { whitelist: ["gpt-6-sol"], mappings: [] },
  degraded: { whitelist: ["gpt-6-luna"], mappings: [{ source: "gpt-6-astra", target: "gpt-6-luna" }] } };
const preview = { version: "b".repeat(64), items: [{ account_id: 387, name: "yx", marked: true,
  before: config.normal, after: { whitelist: [], mappings: [] }, status: "queued", unrestricted: true }] };
const click = async (text: string) => { const button = [...container.querySelectorAll("button")].find((b) => b.textContent?.trim() === text); expect(button).toBeTruthy(); await act(async () => button!.click()); };
beforeEach(() => {
  vi.resetAllMocks(); container = document.createElement("div"); document.body.append(container); root = createRoot(container);
  vi.mocked(api).mockImplementation(async (method, path) => {
    if (method === "GET" && path === "/account-model-profiles") return structuredClone(config) as never;
    if (path === "/account-model-profiles/preview") return structuredClone(preview) as never;
    if (path === "/account-model-profiles/apply") return { id: "f".repeat(32), items: preview.items.map((i) => ({ ...i, status: "applied" })) } as never;
    throw new Error("版本冲突，请重新读取");
  });
});
afterEach(async () => { await act(async () => root.unmount()); container.remove(); });

describe("account model profiles", () => {
  it("keeps both editable templates and previews unrestricted changes before writing", async () => {
    await act(async () => root.render(<AccountModelProfiles online/>));
    expect(container.textContent).toContain("未标记降智"); expect(container.textContent).toContain("已标记降智");
    expect(container.querySelectorAll("textarea")).toHaveLength(4);
    expect(vi.mocked(api).mock.calls.every(([method]) => method === "GET")).toBe(true);
    await click("一键应用");
    expect(container.querySelector('[role="dialog"]')?.textContent).toContain("不限制模型");
    expect(vi.mocked(api).mock.calls.some(([method]) => method === "POST")).toBe(false);
    await click("应用变更 1");
    expect(vi.mocked(api).mock.calls.find(([method]) => method === "POST")?.[2]).toMatchObject({ preview_version: preview.version });
    expect(container.textContent).toContain("已应用");
  });
  it("keeps conflicts local and never silently overwrites an edited template", async () => {
    await act(async () => root.render(<AccountModelProfiles online/>));
    const input = container.querySelector<HTMLTextAreaElement>('textarea[aria-label="未标记降智白名单 1"]')!;
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!.call(input, "gpt-6.1-sol");
      input.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await click("保存模板");
    expect(container.querySelector('[role="alert"]')?.textContent).toContain("版本冲突");
    expect(input.value).toBe("gpt-6.1-sol");
    expect(vi.mocked(api).mock.calls.find(([method]) => method === "PUT")?.[2]).toMatchObject({ expected_version: config.version });
    await click("放弃"); expect(input.value).toBe("gpt-6-sol");
  });
  it("drops a late response from an unmounted connection", async () => {
    let resolve!: (value: unknown) => void;
    vi.mocked(api).mockImplementationOnce(() => new Promise((done) => { resolve = done; }) as never);
    await act(async () => root.render(<AccountModelProfiles key="old" online/>));
    await act(async () => root.render(<AccountModelProfiles key="new" online/>));
    await act(async () => resolve({ ...config, normal: { whitelist: ["stale-connection-model"], mappings: [] } }));
    expect(container.querySelector<HTMLTextAreaElement>('textarea[aria-label="未标记降智白名单 1"]')?.value).toBe("gpt-6-sol");
  });
});
