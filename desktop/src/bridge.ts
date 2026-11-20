import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import type { ViewState, Preferences } from "./types";
import { bridgeFailure, BridgeError } from "./bridgeError";

type Commands = {
  get_state: [[], ViewState];
  refresh: [[], void];
  disconnect: [[], void];
  background: [[], void];
  check_updates: [[], string];
  connect: [[{ baseUrl: string; apiKey: string; name?: string }], void];
  save_connection_profile: [[{ baseUrl: string; name: string }], void];
  remove_connection: [[{ baseUrl: string }], void];
  preferences: [
    [
      {
        pinned: boolean;
        launchAtLogin: boolean;
        recordColumns?: string[] | null;
        modelTestConcurrency?: number | null;
        modelTestPreferences?: Preferences["model_test_preferences"];
      },
    ],
    void,
  ];
  open_authorization_url: [[{ url: string }], void];
  open_sub2api_release: [[{ url: string }], void];
  api_request: [[{ method: string; path: string; body: unknown }], unknown];
};
export type CommandName = keyof Commands;

const native = "__TAURI_INTERNALS__" in window;
export const preview = !native && import.meta.env.DEV;
export async function watchWindowFocus(
  callback: (focused: boolean) => void,
): Promise<() => void> {
  if (!native) {
    const focus = () => callback(true);
    const blur = () => callback(false);
    window.addEventListener("focus", focus);
    window.addEventListener("blur", blur);
    callback(document.hasFocus());
    return () => {
      window.removeEventListener("focus", focus);
      window.removeEventListener("blur", blur);
    };
  }
  const current = getCurrentWindow();
  let revision = 0;
  const unlisten = await current.onFocusChanged(({ payload }) => {
    ++revision;
    callback(payload);
  });
  try {
    const observed = revision;
    const focused = await current.isFocused();
    // A focus event during the initial read is newer than that snapshot.
    if (revision === observed) callback(focused);
    return unlisten;
  } catch (error) {
    unlisten();
    throw error;
  }
}
export async function command<K extends CommandName>(
  name: K,
  ...parameters: Commands[K][0]
): Promise<Commands[K][1]> {
  const args = parameters[0] ?? {};
  try {
    if (native) return await invoke<Commands[K][1]>(name, args);
    if (import.meta.env.DEV && preview) {
      const demo = await import("./preview");
      return (await demo.run(name, args)) as Commands[K][1];
    }
    throw new Error("请在 Sub2Ops 客户端中打开");
  } catch (error) {
    const failure = bridgeFailure(error);
    throw failure ? new BridgeError(failure) : error;
  }
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
  return command("api_request", {
    method,
    path,
    body: body ?? null,
  }) as Promise<T>;
}
