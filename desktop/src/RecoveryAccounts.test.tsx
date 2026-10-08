// @vitest-environment jsdom
import { act, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import RecoveryAccounts from "./RecoveryAccounts";
import type { Account } from "./types";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
let container: HTMLDivElement;
let root: Root;

const accounts = [
  { id: 1, name: "Alpha OAuth", platform: "openai", type: "oauth" },
  { id: 2, name: "Beta OAuth", platform: "openai", type: "oauth", recovery_selectable: true },
  { id: 3, name: "Unavailable OAuth", platform: "openai", type: "oauth", recovery_selectable: false },
  { id: 4, name: "OpenAI Key", platform: "openai", type: "apikey" },
  { id: 5, name: "Grok OAuth", platform: "grok", type: "oauth" },
] as Account[];

beforeEach(() => {
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});
afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
});

async function search(value: string) {
  const input = container.querySelector<HTMLInputElement>('[aria-label="搜索恢复账号"]')!;
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

function selection(mode: "测试连接" | "模型测试", name: string) {
  const column = [...container.querySelectorAll("fieldset")].find((node) => node.querySelector("legend")?.textContent === mode)!;
  return [...column.querySelectorAll("label")].find((node) => node.textContent?.includes(name))!.querySelector<HTMLInputElement>('input[type="checkbox"]')!;
}

describe("recovery account selection", () => {
  it("searches eligible OpenAI OAuth accounts by name or ID without changing selections", async () => {
    const change = vi.fn();
    await act(async () => root.render(<RecoveryAccounts accounts={accounts} connection={[1]} models={[2]} change={change}/>));
    expect(container.querySelectorAll('input[type="checkbox"]')).toHaveLength(4);
    expect(container.textContent).not.toContain("Unavailable OAuth");
    expect(container.textContent).not.toContain("OpenAI Key");
    expect(container.textContent).not.toContain("Grok OAuth");
    await search("  ALPHA ");
    expect(container.querySelectorAll('input[type="checkbox"]')).toHaveLength(2);
    expect(selection("测试连接", "Alpha OAuth").checked).toBe(true);
    expect(container.textContent).not.toContain("Beta OAuth");
    await search("#2");
    expect(selection("模型测试", "Beta OAuth").checked).toBe(true);
    expect(container.textContent).not.toContain("Alpha OAuth");
    await search("no-match");
    expect(container.querySelectorAll('input[type="checkbox"]')).toHaveLength(0);
    expect(container.textContent).toContain("没有匹配的账号");
    expect(change).not.toHaveBeenCalled();
  });

  it("moves an account between recovery modes and retains selections hidden by search", async () => {
    const change = vi.fn();
    function Selection() {
      const [connection, setConnection] = useState([1]);
      const [models, setModels] = useState([2]);
      return <RecoveryAccounts accounts={accounts} connection={connection} models={models} change={(nextConnection, nextModels) => {
        change(nextConnection, nextModels);
        setConnection(nextConnection);
        setModels(nextModels);
      }}/>;
    }
    await act(async () => root.render(<Selection/>));
    await search("Alpha");
    await act(async () => selection("模型测试", "Alpha OAuth").click());
    expect(change).toHaveBeenLastCalledWith([], [2, 1]);
    expect(selection("测试连接", "Alpha OAuth").checked).toBe(false);
    expect(selection("模型测试", "Alpha OAuth").checked).toBe(true);
    await search("");
    expect(selection("模型测试", "Beta OAuth").checked).toBe(true);
    await act(async () => selection("测试连接", "Beta OAuth").click());
    expect(change).toHaveBeenLastCalledWith([2], [1]);
    expect(selection("模型测试", "Beta OAuth").checked).toBe(false);
    await act(async () => selection("测试连接", "Beta OAuth").click());
    expect(change).toHaveBeenLastCalledWith([], [1]);
    expect(selection("模型测试", "Alpha OAuth").checked).toBe(true);
  });
});
