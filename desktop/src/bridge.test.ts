// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

type FocusListener = (event: { payload: boolean }) => void;
const nativeWindow = vi.hoisted(() => ({
  getCurrentWindow: vi.fn(),
  onFocusChanged: vi.fn<(callback: FocusListener) => Promise<() => void>>(),
  isFocused: vi.fn<() => Promise<boolean>>(),
}));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: nativeWindow.getCurrentWindow,
}));
vi.mock("@tauri-apps/api/core", () => ({ Channel: vi.fn(), invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

let listener: FocusListener | undefined;
const unlisten = vi.fn(() => {
  listener = undefined;
});
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((yes) => {
    resolve = yes;
  });
  return { promise, resolve };
}
async function nativeFocusWatch() {
  Object.defineProperty(window, "__TAURI_INTERNALS__", {
    configurable: true,
    value: {},
  });
  return (await import("./bridge")).watchWindowFocus;
}

beforeEach(() => {
  vi.resetModules();
  vi.clearAllMocks();
  Reflect.deleteProperty(window, "__TAURI_INTERNALS__");
  listener = undefined;
  nativeWindow.getCurrentWindow.mockReturnValue(nativeWindow);
  nativeWindow.onFocusChanged.mockImplementation(async (callback) => {
    listener = callback;
    return unlisten;
  });
  nativeWindow.isFocused.mockResolvedValue(false);
});
afterEach(() => {
  Reflect.deleteProperty(window, "__TAURI_INTERNALS__");
  vi.restoreAllMocks();
});

describe("window focus bridge", () => {
  it("uses the native initial state and subsequent focus events, then releases the listener", async () => {
    const watchWindowFocus = await nativeFocusWatch();
    const changed = vi.fn();
    const stop = await watchWindowFocus(changed);
    expect(changed.mock.calls).toEqual([[false]]);
    listener?.({ payload: true });
    listener?.({ payload: false });
    expect(changed.mock.calls).toEqual([[false], [true], [false]]);

    stop();
    expect(unlisten).toHaveBeenCalledTimes(1);
    expect(listener).toBeUndefined();
    expect(nativeWindow.onFocusChanged).toHaveBeenCalledTimes(1);
    expect(nativeWindow.isFocused).toHaveBeenCalledTimes(1);
  });

  it.each([false, true])(
    "keeps a newer focus event (%s) when the initial native snapshot resolves late",
    async (focused) => {
      const readStarted = deferred<void>(),
        initial = deferred<boolean>();
      nativeWindow.isFocused.mockImplementation(() => {
        readStarted.resolve();
        return initial.promise;
      });
      const watchWindowFocus = await nativeFocusWatch();
      const changed = vi.fn();
      const subscription = watchWindowFocus(changed);
      await readStarted.promise;
      listener?.({ payload: focused });
      initial.resolve(!focused);
      const stop = await subscription;
      expect(changed.mock.calls).toEqual([[focused]]);
      listener?.({ payload: !focused });
      expect(changed.mock.calls).toEqual([[focused], [!focused]]);
      stop();
      expect(unlisten).toHaveBeenCalledTimes(1);
    },
  );

  it("releases the registered native listener if the initial focus read fails", async () => {
    const failure = new Error("initial focus unavailable");
    nativeWindow.isFocused.mockRejectedValue(failure);
    const watchWindowFocus = await nativeFocusWatch();
    const changed = vi.fn();
    await expect(watchWindowFocus(changed)).rejects.toBe(failure);
    expect(unlisten).toHaveBeenCalledTimes(1);
    expect(listener).toBeUndefined();
    expect(changed).not.toHaveBeenCalled();
  });

  it("surfaces a native subscription failure before querying focus", async () => {
    const failure = new Error("native event subscription unavailable");
    nativeWindow.onFocusChanged.mockRejectedValue(failure);
    const watchWindowFocus = await nativeFocusWatch();
    await expect(watchWindowFocus(vi.fn())).rejects.toBe(failure);
    expect(nativeWindow.isFocused).not.toHaveBeenCalled();
    expect(unlisten).not.toHaveBeenCalled();
  });

  it("uses browser focus events in preview and removes both listeners on cleanup", async () => {
    vi.spyOn(document, "hasFocus").mockReturnValue(false);
    const { watchWindowFocus } = await import("./bridge");
    const changed = vi.fn();
    const stop = await watchWindowFocus(changed);
    expect(changed.mock.calls).toEqual([[false]]);
    window.dispatchEvent(new Event("focus"));
    window.dispatchEvent(new Event("blur"));
    expect(changed.mock.calls).toEqual([[false], [true], [false]]);
    expect(nativeWindow.getCurrentWindow).not.toHaveBeenCalled();

    stop();
    window.dispatchEvent(new Event("focus"));
    window.dispatchEvent(new Event("blur"));
    expect(changed.mock.calls).toEqual([[false], [true], [false]]);
  });
});
