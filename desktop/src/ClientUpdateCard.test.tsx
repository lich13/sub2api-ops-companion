// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { command, subscribe } from "./bridge";
import ClientUpdateCard from "./ClientUpdateCard";
import App from "./main";
import { subscribeAppUpdate } from "./tauriEvents";
import type { AppUpdateSnapshot, ViewState } from "./types";

vi.mock("./bridge", () => ({
  api: vi.fn(),
  command: vi.fn(),
  subscribe: vi.fn(),
  preview: false,
}));

vi.mock("./tauriEvents", () => ({
  subscribeObserved: vi.fn(async () => () => {}),
  subscribeAppUpdate: vi.fn(),
}));

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

let container: HTMLDivElement;
let root: Root;
let pushUpdate: ((next: AppUpdateSnapshot) => void) | null = null;

function snapshot(
  overrides: Partial<AppUpdateSnapshot> = {},
): AppUpdateSnapshot {
  return {
    phase: "idle",
    current_version: "0.1.105",
    latest_version: null,
    downloaded_bytes: 0,
    total_bytes: null,
    message: "",
    ...overrides,
  };
}

async function flush() {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}

beforeEach(() => {
  vi.resetAllMocks();
  pushUpdate = null;
  vi.mocked(subscribe).mockImplementation(async () => () => {});
  vi.mocked(subscribeAppUpdate).mockImplementation(async (cb) => {
    pushUpdate = cb;
    return () => {
      if (pushUpdate === cb) pushUpdate = null;
    };
  });
  vi.mocked(command).mockImplementation(
    async (...[name]: Parameters<typeof command>) => {
      if (name === "get_app_update_state") return snapshot() as never;
      return undefined as never;
    },
  );
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
});

describe("ClientUpdateCard", () => {
  it("reports up-to-date status in-app without opening any external browser URL", async () => {
    vi.mocked(command).mockImplementation(
      async (...[name]: Parameters<typeof command>) => {
        if (name === "get_app_update_state") return snapshot() as never;
        if (name === "start_app_update") {
          return snapshot({
            phase: "up_to_date",
            latest_version: "0.1.105",
            message: "当前已是最新版本 0.1.105",
          }) as never;
        }
        throw new Error(`Unexpected command: ${name}`);
      },
    );

    await act(async () => {
      root.render(<ClientUpdateCard runtimeVersion="0.1.105" />);
    });
    await flush();

    expect(container.querySelector(".client-version-chip")?.textContent).toBe(
      "0.1.105",
    );
    const checkBtn = container.querySelector("button")!;
    expect(checkBtn.textContent).toContain("检查更新");

    await act(async () => {
      checkBtn.click();
    });
    await flush();

    const status = container.querySelector('[role="status"]');
    expect(status?.textContent).toContain("当前已是最新版本 0.1.105");
    expect(status?.className).toContain("good-text");
    expect(
      vi
        .mocked(command)
        .mock.calls.some(
          ([name]) =>
            name === "open_authorization_url" ||
            name === "open_sub2api_release",
        ),
    ).toBe(false);
  });

  it("streams download progress and supports cancelling an active download", async () => {
    vi.mocked(command).mockImplementation(
      async (...[name]: Parameters<typeof command>) => {
        if (name === "get_app_update_state") return snapshot() as never;
        if (name === "cancel_app_update") {
          return snapshot({
            phase: "idle",
            latest_version: "0.1.106",
            message: "已取消更新下载",
          }) as never;
        }
        throw new Error(`Unexpected command: ${name}`);
      },
    );

    await act(async () => {
      root.render(<ClientUpdateCard runtimeVersion="0.1.105" />);
    });
    await flush();

    await act(async () => {
      pushUpdate?.(
        snapshot({
          phase: "downloading",
          latest_version: "0.1.106",
          downloaded_bytes: 5 * 1024 * 1024,
          total_bytes: 20 * 1024 * 1024,
          message: "正在下载 v0.1.106 安装包",
        }),
      );
    });

    const progress = container.querySelector("progress");
    expect(progress).not.toBeNull();
    expect(container.querySelector(".client-update-bytes")?.textContent).toBe(
      "5.0 MB / 20.0 MB (25%)",
    );

    const cancelBtn = container.querySelector("button")!;
    expect(cancelBtn.textContent).toContain("取消下载");
    await act(async () => {
      cancelBtn.click();
    });
    await flush();

    expect(command).toHaveBeenCalledWith("cancel_app_update");
    expect(container.querySelector("progress")).toBeNull();
    expect(container.querySelector('[role="status"]')?.textContent).toContain(
      "已取消更新下载",
    );
  });

  it("handles Android unknown-sources permission prompt and resumes installation", async () => {
    vi.mocked(command).mockImplementation(
      async (...[name]: Parameters<typeof command>) => {
        if (name === "get_app_update_state") {
          return snapshot({
            phase: "permission_required",
            latest_version: "0.1.106",
            downloaded_bytes: 24 * 1024 * 1024,
            total_bytes: 24 * 1024 * 1024,
            message: "请授权允许安装未知来源应用，返回后将自动拉起安装",
          }) as never;
        }
        if (name === "install_app_update") {
          return snapshot({
            phase: "installing",
            latest_version: "0.1.106",
            message: "正在拉起系统安装器（0.1.106）",
          }) as never;
        }
        throw new Error(`Unexpected command: ${name}`);
      },
    );

    await act(async () => {
      root.render(<ClientUpdateCard runtimeVersion="0.1.105" />);
    });
    await flush();

    const installBtn = [
      ...container.querySelectorAll<HTMLButtonElement>("button"),
    ].find((b) => b.textContent?.includes("授权后继续安装"))!;
    expect(installBtn).toBeTruthy();

    await act(async () => {
      installBtn.click();
    });
    await flush();

    expect(command).toHaveBeenCalledWith("install_app_update");
    expect(container.querySelector('[role="status"]')?.textContent).toContain(
      "正在拉起系统安装器（0.1.106）",
    );
  });

  it("surfaces verification/download errors with a retry button and forwards unexpected bridge failures", async () => {
    const onError = vi.fn();
    let shouldReject = false;
    vi.mocked(command).mockImplementation(
      async (...[name]: Parameters<typeof command>) => {
        if (name === "get_app_update_state") {
          return snapshot({
            phase: "error",
            latest_version: "0.1.106",
            message: "APK SHA-256 校验不匹配，已拒绝安装",
          }) as never;
        }
        if (name === "start_app_update") {
          if (shouldReject) throw new Error("桥接异常");
          return snapshot({
            phase: "checking",
            message: "正在检查 GitHub Releases 最新版本",
          }) as never;
        }
        throw new Error(`Unexpected command: ${name}`);
      },
    );

    await act(async () => {
      root.render(
        <ClientUpdateCard runtimeVersion="0.1.105" onError={onError} />,
      );
    });
    await flush();

    const status = container.querySelector('[role="status"]');
    expect(status?.className).toContain("bad-text");
    expect(status?.textContent).toContain("SHA-256 校验不匹配");

    const retryBtn = container.querySelector("button")!;
    expect(retryBtn.textContent).toContain("重试更新");

    shouldReject = true;
    await act(async () => {
      retryBtn.click();
    });
    await flush();
    expect(onError).toHaveBeenCalledOnce();
  });

  it("surfaces initial update state read failures instead of swallowing them", async () => {
    vi.mocked(command).mockRejectedValueOnce(new Error("读取更新状态失败"));
    await act(async () => {
      root.render(<ClientUpdateCard runtimeVersion="0.1.105" />);
    });
    await flush();
    const status = container.querySelector('[role="status"]');
    expect(status?.className).toContain("bad-text");
    expect(status?.textContent).toContain("读取更新状态失败");
    expect(container.querySelector("button")?.textContent).toContain(
      "重试更新",
    );
  });

  it("renders ClientUpdateCard on unconnected and offline-snapshot screens for self-recovery", async () => {
    let view: ViewState = {
      platform: "android",
      foreground: true,
      connected: false,
      online: false,
      error: "",
      preferences: { base_url: "" },
      snapshot: null,
    };
    let pushState: ((next: ViewState) => void) | null = null;
    vi.mocked(subscribe).mockImplementation(async (cb) => {
      pushState = cb;
      return () => {};
    });
    vi.mocked(command).mockImplementation(
      async (...[name]: Parameters<typeof command>) => {
        if (name === "get_state") return view as never;
        if (name === "get_app_update_state") return snapshot() as never;
        return undefined as never;
      },
    );

    await act(async () => {
      root.render(<App />);
    });
    await flush();

    expect(
      container.querySelector(".unconnected-shell .client-update-card"),
    ).not.toBeNull();

    view = {
      ...view,
      connected: true,
      error: "快照协议版本不兼容",
      preferences: { base_url: "https://ops.example.invalid" },
      snapshot: null,
    };
    await act(async () => {
      pushState?.(view);
    });
    await flush();

    expect(
      container.querySelector(".empty .client-update-card"),
    ).not.toBeNull();
  });
});
