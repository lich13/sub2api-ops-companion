import { listen } from "@tauri-apps/api/event";
import { preview } from "./bridge";
import type { AppUpdateSnapshot } from "./types";

export async function subscribeObserved(
  callback: (observedAt: string) => void,
): Promise<() => void> {
  if ("__TAURI_INTERNALS__" in window) {
    return listen<string>("ops-observed", (event) => callback(event.payload));
  }
  return () => {};
}

export async function subscribeAppUpdate(
  callback: (state: AppUpdateSnapshot) => void,
): Promise<() => void> {
  if ("__TAURI_INTERNALS__" in window) {
    return listen<AppUpdateSnapshot>("app-update", (event) =>
      callback(event.payload),
    );
  }
  if (import.meta.env.DEV && preview) {
    const demo = await import("./preview");
    return demo.subscribeAppUpdate(callback);
  }
  return () => {};
}
