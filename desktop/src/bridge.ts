import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { ViewState } from "./types";
const native = "__TAURI_INTERNALS__" in window;
export const preview = !native && import.meta.env.DEV;
export async function command<T>(
  name: string,
  args: Record<string, unknown> = {},
): Promise<T> {
  if (native) return invoke<T>(name, args);
  if (preview) {
    const demo = await import("./preview");
    return demo.run(name, args) as Promise<T>;
  }
  throw new Error("请在 Sub2Ops 客户端中打开");
}
export async function subscribe(
  callback: (state: ViewState) => void,
): Promise<() => void> {
  if (native)
    return listen<ViewState>("ops-state", (event) => callback(event.payload));
  if (import.meta.env.DEV) {
    const demo = await import("./preview");
    return demo.subscribe(callback);
  }
  return () => {};
}
export async function updates(
  callback: (message: string) => void,
): Promise<() => void> {
  return native
    ? listen<string>("update-result", (e) => callback(e.payload))
    : () => {};
}
export function api<T>(method: string, path: string, body?: unknown) {
  return command<T>("api_request", { method, path, body: body ?? null });
}
