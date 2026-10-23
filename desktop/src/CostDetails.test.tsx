// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import CostDetails, { costDetails } from "./CostDetails";
import type { UsageRecord } from "./records";
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
const record = (values: Partial<UsageRecord> = {}) => ({
  input_tokens: 3734, output_tokens: 1113, input_cost: "0.007468", output_cost: "0.006678",
  total_cost: "0.014722", actual_cost: "0.029444", account_cost: "0.022083",
  rate_multiplier: "2", account_rate_multiplier: "1.5", cache_read_cost: "0.000576",
  cache_creation_cost: null, image_input_tokens: null, image_output_tokens: null,
  service_tier: "default", billing_mode: "token", ...values,
} as UsageRecord);

it("uses each record's stored costs and token counts with the native precision", () => {
  const rows = Object.fromEntries(costDetails(record()));
  expect(rows).toMatchObject({ "输入费用": "$0.00746800", "输出费用": "$0.00667800", "输入单价": "$2.0000 / 1M Token", "输出单价": "$6.0000 / 1M Token", "缓存读取费用": "$0.00057600", "服务档位": "Standard", "倍率": "2.00x", "原始": "$0.01472200", "用户扣费": "$0.02944400", "账号倍率": "1.50x", "账号计费": "$0.02208300" });
});
it("does not invent costs or divide by zero when fields are missing", () => {
  const rows = Object.fromEntries(costDetails(record({ input_tokens: 0, output_tokens: null, input_cost: null, output_cost: "0", rate_multiplier: null, account_cost: null })));
  expect(rows["输入单价"]).toBeUndefined(); expect(rows["输出单价"]).toBeUndefined();
  expect(rows["输入费用"]).toBe("—"); expect(rows["输出费用"]).toBe("$0.00000000");
  expect(rows["倍率"]).toBe("—"); expect(rows["账号计费"]).toBe("—");
  expect(Object.fromEntries(costDetails(record({ actual_cost: "9007199254740993.123456789" })))["用户扣费"]).toBe("$9007199254740993.12345679");
});
it("separates image token costs, legacy image records and per-request billing", () => {
  const mixed = Object.fromEntries(costDetails(record({ input_tokens: 150, image_input_tokens: 50, input_cost: "0.0002", image_input_cost: "0.0004", output_tokens: 100, image_output_tokens: 80, output_cost: "0.00006", image_output_cost: "0.0008", service_tier: "FAST" })));
  expect(mixed["输入单价"]).toBe("$2.0000 / 1M Token"); expect(mixed["图片输入单价"]).toBe("$8.0000 / 1M Token");
  expect(mixed["输出单价"]).toBe("$3.0000 / 1M Token"); expect(mixed["图片输出单价"]).toBe("$10.0000 / 1M Token");
  expect(mixed["服务档位"]).toBe("Priority");
  const image = Object.fromEntries(costDetails(record({ billing_mode: null, image_count: 2, total_cost: "0.123456789" })));
  expect(image["图片单价"]).toBe("$0.06172839"); expect(image["输入单价"]).toBeUndefined();
  expect(Object.fromEntries(costDetails(record({ billing_mode: "per_request", total_cost: "0.5" })))["每次费用"]).toBe("$0.50000000");
});

describe("cost popover", () => {
  let container: HTMLDivElement, root: Root;
  const details = vi.fn();
  const pointer = (target: Element, type: string, pointerType: string) => {
    const event = new Event(type, { bubbles: true }); Object.defineProperty(event, "pointerType", { value: pointerType }); target.dispatchEvent(event);
  };
  const tooltip = () => document.querySelector('[role="tooltip"]');
  beforeEach(async () => {
    details.mockClear(); container = document.createElement("div"); document.body.append(container); root = createRoot(container);
    await act(async () => root.render(<div onClick={details} onKeyDown={details} style={{ overflow: "hidden" }}><CostDetails row={record()} /></div>));
  });
  afterEach(async () => { await act(async () => root.unmount()); container.remove(); vi.unstubAllGlobals(); });
  it("portals hover details outside the table and closes on pointer exit", async () => {
    const button = container.querySelector("button")!;
    await act(async () => pointer(button, "pointerover", "mouse"));
    expect(tooltip()?.textContent).toContain("$2.0000 / 1M Token");
    expect(tooltip()?.parentElement).toBe(document.body);
    await act(async () => pointer(button, "pointerout", "mouse")); expect(tooltip()).toBeNull();
  });
  it("opens with keyboard focus and dismisses with Escape without opening record details", async () => {
    const button = container.querySelector("button")!;
    await act(async () => button.focus()); expect(tooltip()).not.toBeNull();
    await act(async () => button.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true })));
    expect(tooltip()).toBeNull(); expect(document.activeElement).toBe(button); expect(details).not.toHaveBeenCalled();
  });
  it("ignores emulated mouse hover on touch-only screens while retaining tap access", async () => {
    vi.stubGlobal("matchMedia", vi.fn(() => ({ matches: false })));
    const button = container.querySelector("button")!;
    await act(async () => pointer(button, "pointerover", "mouse")); expect(tooltip()).toBeNull();
    await act(async () => { pointer(button, "pointerdown", "touch"); button.focus(); button.click(); });
    expect(tooltip()).not.toBeNull(); expect(details).not.toHaveBeenCalled();
  });
  it("opens on a touch tap, toggles on another tap, and closes on outside input", async () => {
    const button = container.querySelector("button")!;
    await act(async () => { pointer(button, "pointerdown", "touch"); button.focus(); button.click(); });
    expect(tooltip()).not.toBeNull(); expect(details).not.toHaveBeenCalled();
    await act(async () => { (tooltip() as HTMLElement).focus(); tooltip()!.dispatchEvent(new Event("scroll")); });
    expect(tooltip()).not.toBeNull();
    await act(async () => button.click()); expect(tooltip()).toBeNull();
    await act(async () => button.click()); expect(tooltip()).not.toBeNull();
    await act(async () => pointer(document.body, "pointerdown", "touch")); expect(tooltip()).toBeNull();
  });
  it("closes when focus moves to a control outside the record", async () => {
    const button = container.querySelector("button")!;
    const outside = document.createElement("button");
    document.body.append(outside);
    await act(async () => button.focus());
    expect(tooltip()).not.toBeNull();
    await act(async () => outside.focus());
    expect(tooltip()).toBeNull();
    outside.remove();
  });
});
