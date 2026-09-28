// @vitest-environment jsdom
import React, { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import ModelConfig, { mergeFields } from "./ModelConfig";
import { api } from "./bridge";
vi.mock("./bridge", () => ({ api: vi.fn() }));
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
let container: HTMLDivElement, root: Root;
const base = {
  slug: "gpt-6-astra",
  display_name: "Astra",
  context_window: 400000,
  max_context_window: 1000000,
  input_modalities: ["text", "image"],
};
const fixture = () => ({
  group: {
    id: 7,
    name: "Codex",
    platform: "openai",
    version: "a".repeat(64),
    model_allowlist: { enabled: true, models: [base.slug, "gpt-6-sol"] },
  },
  revision: "b".repeat(64),
  overrides: {},
  candidates: [base.slug, "gpt-6-sol"],
  baseline: { models: [base] },
  effective: { models: [base] },
  baseline_status: "native",
  status: { state: "ready", message: "" },
});
beforeEach(() => {
  vi.clearAllMocks();
  vi.useFakeTimers();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  vi.mocked(api).mockImplementation(async (method, path) => {
    if (path === "/model-groups")
      return { groups: [fixture().group], status: { message: "" } } as never;
    if (path.endsWith("/preview"))
      return {
        effective: { models: [base] },
        pending_models: [],
        baseline_status: "native",
      } as never;
    if (method === "PUT") throw new Error("配置已变更，请刷新");
    return fixture() as never;
  });
});
afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.useRealTimers();
});
async function click(text: string) {
  await act(async () => {
    const b = [...container.querySelectorAll("button")].find(
      (b) => b.textContent === text,
    );
    if (!b) throw Error(text);
    b.click();
  });
}
async function start() {
  await act(async () =>
    root.render(
      <React.StrictMode>
        <ModelConfig online />
      </React.StrictMode>,
    ),
  );
}
async function input(element: HTMLTextAreaElement, value: string) {
  await act(async () => {
    Object.getOwnPropertyDescriptor(
      HTMLTextAreaElement.prototype,
      "value",
    )!.set!.call(element, value);
    element.dispatchEvent(new Event("input", { bubbles: true }));
  });
}
it("merges explicit falsy values, arrays and prototype-named data without prototype mutation", () => {
  const result = mergeFields(
    { nested: { a: 1, b: 2 }, values: [1, 2] },
    JSON.parse(
      '{"nested":{"a":0},"values":[],"off":false,"nil":null,"__proto__":{"x":1}}',
    ),
  );
  expect(result.nested).toEqual({ a: 0, b: 2 });
  expect(result.values).toEqual([]);
  expect(result.off).toBe(false);
  expect(result.nil).toBeNull();
  expect(Object.getPrototypeOf(result)).toBe(Object.prototype);
  expect(Object.hasOwn(result, "__proto__")).toBe(true);
});
it("loads under StrictMode, keeps model drafts and rejects invalid JSON", async () => {
  await start();
  expect(
    [...container.querySelectorAll("input")].some(
      (input) => input.value === "Astra",
    ),
  ).toBe(true);
  await click("完整 JSON");
  const area = container.querySelector("textarea")!;
  await input(area, '{"display_name":"Changed","future":{"flag":false}}');
  await click("gpt-6-sol");
  await click("gpt-6-astra");
  expect(container.querySelector("textarea")!.value).toContain("Changed");
  await input(container.querySelector("textarea")!, "{invalid");
  expect(container.querySelector('[role="alert"]')).not.toBeNull();
  expect(
    [...container.querySelectorAll("button")].find(
      (b) => b.textContent === "保存模型信息",
    )!.disabled,
  ).toBe(true);
});
it("save conflicts preserve JSON drafts and offline never sends writes", async () => {
  await start();
  await click("完整 JSON");
  await input(
    container.querySelector("textarea")!,
    '{"display_name":"Keep draft"}',
  );
  await click("保存模型信息");
  expect(container.textContent).toContain("配置已变更");
  expect(container.querySelector("textarea")!.value).toContain("Keep draft");
  await act(async () => root.render(<ModelConfig online={false} />));
  expect(
    vi.mocked(api).mock.calls.filter(([method]) => method === "PUT"),
  ).toHaveLength(1);
});
