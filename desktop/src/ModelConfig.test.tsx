// @vitest-environment jsdom
import React, { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import ModelConfig from "./ModelConfig";
import { api } from "./bridge";
vi.mock("./bridge", () => ({ api: vi.fn() }));
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
let container: HTMLDivElement, root: Root;
const fixture = () => ({
  group: { id: 7, name: "Codex", platform: "openai", version: "a".repeat(64) },
  revision: "b".repeat(64),
  items: [],
  status: { state: "ready", message: "" },
});
let limited = false,
  missing = false,
  native = false;
beforeEach(() => {
  vi.clearAllMocks();
  vi.useFakeTimers();
  limited = missing = native = false;
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  vi.mocked(api).mockImplementation(async (method, path, payload) => {
    if (path === "/model-groups") return { groups: [fixture().group] } as never;
    if (path.endsWith("/resolve")) {
      const p = payload as {
        model: string;
        efforts?: string[];
        default_effort?: string;
      };
      return {
        group: fixture().group,
        revision: fixture().revision,
        model: p.model,
        binding: "c".repeat(64),
        efforts: p.efforts || (missing ? [] : ["low", "high"]),
        default_effort: p.default_effort || (missing ? "" : "high"),
        source: missing ? "manual" : "upstream",
        needs_allowlist: !limited && !native,
        descriptor_available: !missing,
        native_efforts: native ? ["low", "high"] : [],
        native_default: native ? "high" : "",
        forwarding: {
          state: limited ? "limited" : "verified",
          reason: "实际转发规则",
          version: "0.2.10",
        },
      } as never;
    }
    if (method === "PUT") throw new Error("配置已变更，请刷新");
    return fixture() as never;
  });
});
afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.useRealTimers();
});
function button(text: string) {
  const b = [...container.querySelectorAll("button")].find(
    (b) => b.textContent === text,
  );
  if (!b) throw Error(text);
  return b;
}
async function click(text: string) {
  await act(async () => button(text).click());
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
async function input(value: string) {
  await act(async () => {
    const e = container.querySelector<HTMLInputElement>(
      '[aria-label="模型 ID"]',
    )!;
    Object.getOwnPropertyDescriptor(
      HTMLInputElement.prototype,
      "value",
    )!.set!.call(e, value);
    e.dispatchEvent(new Event("input", { bubbles: true }));
  });
}
async function settle() {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(400);
  });
}
async function add() {
  await start();
  await click("添加模型");
  await input("future-model");
  await settle();
  await settle();
}

it("only loads supplements, prefills the exact model and confirms one allowlist append", async () => {
  await add();
  expect(container.textContent).not.toContain("完整 JSON");
  expect(container.textContent).not.toContain("上下文");
  expect(container.querySelectorAll("[aria-pressed=true]")).toHaveLength(2);
  expect(button("保存").disabled).toBe(true);
  await act(async () =>
    container.querySelector<HTMLInputElement>("input[type=checkbox]")!.click(),
  );
  await click("保存");
  const write = vi.mocked(api).mock.calls.find(([method]) => method === "PUT")!;
  expect(write[1]).toBe("/model-groups/7/reasoning");
  expect(write[2]).toMatchObject({
    model: "future-model",
    efforts: ["low", "high"],
    default_effort: "high",
    confirm_allowlist: true,
  });
  expect(
    container.querySelector<HTMLInputElement>('[aria-label="模型 ID"]')!.value,
  ).toBe("future-model");
  expect(container.querySelector("[role=alert]")?.textContent).toContain(
    "配置已变更",
  );
  expect(button("保存").disabled).toBe(true);
  await click("重新核对");
  await settle();
  expect(
    container.querySelector<HTMLInputElement>('[aria-label="模型 ID"]')!.value,
  ).toBe("future-model");
});

it("missing capabilities stay unselected and default must belong to selected levels", async () => {
  missing = true;
  await add();
  expect(container.querySelectorAll("[aria-pressed=true]")).toHaveLength(0);
  expect(button("保存草稿").disabled).toBe(true);
  await click("max");
  expect(button("保存草稿").disabled).toBe(true);
  await act(async () => {
    const select = container.querySelector<HTMLSelectElement>(
      '[aria-label="默认档位"]',
    )!;
    select.value = "max";
    select.dispatchEvent(new Event("change", { bubbles: true }));
  });
  await settle();
  expect(button("保存草稿").disabled).toBe(false);
  await click("max");
  expect(
    container.querySelector<HTMLSelectElement>('[aria-label="默认档位"]')!
      .value,
  ).toBe("");
  expect(button("保存草稿").disabled).toBe(true);
});

it("limited forwarding saves drafts, offline blocks writes, native matches need no supplement", async () => {
  limited = true;
  await add();
  expect(container.textContent).toContain("转发受限");
  expect(container.querySelector("input[type=checkbox]")).toBeNull();
  expect(button("保存草稿").disabled).toBe(false);
  await act(async () =>
    root.render(
      <React.StrictMode>
        <ModelConfig online={false} />
      </React.StrictMode>,
    ),
  );
  expect(button("保存草稿").disabled).toBe(true);
  expect(
    vi.mocked(api).mock.calls.filter(([method]) => method === "PUT"),
  ).toHaveLength(0);
  await click("取消");
  limited = false;
  native = true;
  await act(async () =>
    root.render(
      <React.StrictMode>
        <ModelConfig online />
      </React.StrictMode>,
    ),
  );
  await click("添加模型");
  await input("native-model");
  await settle();
  await settle();
  expect(container.textContent).toContain("原生已支持");
  expect(button("恢复原生").disabled).toBe(false);
});

it("ignores stale model lookup responses", async () => {
  let finish: ((value: unknown) => void) | undefined;
  const original = vi.mocked(api).getMockImplementation()!;
  vi.mocked(api).mockImplementation((method, path, payload) => {
    if (
      path.endsWith("/resolve") &&
      (payload as { model: string }).model === "slow-model"
    )
      return new Promise((resolve) => {
        finish = resolve;
      }) as never;
    return original(method, path, payload);
  });
  await start();
  await click("添加模型");
  await input("slow-model");
  await settle();
  await input("future-model");
  await settle();
  await settle();
  await act(async () =>
    finish?.({
      model: "slow-model",
      efforts: ["ultra"],
      default_effort: "ultra",
    }),
  );
  expect(
    container.querySelector<HTMLInputElement>('[aria-label="模型 ID"]')!.value,
  ).toBe("future-model");
  expect(container.querySelector("[aria-pressed=true]")?.textContent).toBe(
    "low",
  );
});
