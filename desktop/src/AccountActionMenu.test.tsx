// @vitest-environment jsdom
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import AccountActionMenu from "./AccountActionMenu";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });

const label = "账号操作";
let container: HTMLDivElement;
let root: Root;
let mounted = true;

const trigger = () =>
  container.querySelector<HTMLButtonElement>(".account-more-trigger")!;
const popup = () =>
  document.body.querySelector<HTMLDivElement>(
    '[role="group"][aria-label="账号操作"]',
  );

async function renderMenu(children: ReactNode) {
  await act(async () =>
    root.render(
      <div className="clipped-table" style={{ height: 24, overflow: "hidden" }}>
        <AccountActionMenu label={label}>{children}</AccountActionMenu>
        <button type="button" data-outside>
          外部
        </button>
      </div>,
    ),
  );
}

async function click(element: HTMLElement) {
  await act(async () => element.click());
}

async function press(target: HTMLElement, key: string) {
  const event = new KeyboardEvent("keydown", {
    key,
    bubbles: true,
    cancelable: true,
  });
  await act(async () => target.dispatchEvent(event));
  return event;
}

beforeEach(() => {
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  mounted = true;
});

afterEach(async () => {
  if (mounted) await act(async () => root.unmount());
  container.remove();
  vi.restoreAllMocks();
});

describe("account action menu", () => {
  it("portals the labeled menu outside a clipped table and toggles from its trigger", async () => {
    await renderMenu(<button type="button">查看</button>);

    await click(trigger());

    const menu = popup();
    expect(menu).not.toBeNull();
    expect(menu?.parentElement).toBe(document.body);
    expect(container.contains(menu)).toBe(false);
    expect(menu?.getAttribute("aria-label")).toBe(label);
    expect(trigger().getAttribute("aria-expanded")).toBe("true");
    expect(trigger().getAttribute("aria-controls")).toBe(menu?.id);

    await click(trigger());

    expect(popup()).toBeNull();
    expect(trigger().getAttribute("aria-expanded")).toBe("false");
  });

  it("closes on Escape and restores focus to the trigger", async () => {
    await renderMenu(
      <>
        <button type="button" disabled>
          不可用
        </button>
        <button type="button">编辑</button>
      </>,
    );
    await click(trigger());

    const edit = popup()!.querySelector<HTMLButtonElement>("button:not(:disabled)")!;
    expect(document.activeElement).toBe(edit);
    const event = await press(edit, "Escape");

    expect(event.defaultPrevented).toBe(true);
    expect(popup()).toBeNull();
    expect(document.activeElement).toBe(trigger());
  });

  it("closes on outside pointerdown and after an enabled menu action is clicked", async () => {
    await renderMenu(
      <button type="button" data-action="delete">
        删除
      </button>,
    );
    await click(trigger());

    await act(async () => {
      container
        .querySelector<HTMLButtonElement>("[data-outside]")!
        .dispatchEvent(new Event("pointerdown", { bubbles: true }));
    });
    expect(popup()).toBeNull();

    await click(trigger());
    await click(popup()!.querySelector<HTMLButtonElement>("[data-action=delete]")!);

    expect(popup()).toBeNull();
    expect(trigger().getAttribute("aria-expanded")).toBe("false");
  });

  it("moves among enabled buttons with arrows, Home, and End", async () => {
    await renderMenu(
      <>
        <button type="button">编辑</button>
        <button type="button" disabled>
          不可用
        </button>
        <button type="button">复制</button>
        <button type="button" disabled>
          暂不可用
        </button>
        <button type="button">删除</button>
      </>,
    );
    await click(trigger());

    const enabled = [
      ...popup()!.querySelectorAll<HTMLButtonElement>("button:not(:disabled)"),
    ];
    expect(document.activeElement).toBe(enabled[0]);

    expect((await press(enabled[0], "ArrowDown")).defaultPrevented).toBe(true);
    expect(document.activeElement).toBe(enabled[1]);
    await press(enabled[1], "ArrowDown");
    expect(document.activeElement).toBe(enabled[2]);
    await press(enabled[2], "ArrowDown");
    expect(document.activeElement).toBe(enabled[0]);
    await press(enabled[0], "ArrowUp");
    expect(document.activeElement).toBe(enabled[2]);
    await press(enabled[1], "End");
    expect(document.activeElement).toBe(enabled[2]);
    await press(enabled[2], "Home");
    expect(document.activeElement).toBe(enabled[0]);
  });

  it("keeps the menu open for internal scrolling and closes for outer scrolling", async () => {
    await renderMenu(
      <div style={{ maxHeight: 20, overflowY: "auto" }}>
        <button type="button">查看</button>
      </div>,
    );
    await click(trigger());

    await act(async () => {
      popup()!.dispatchEvent(new Event("scroll", { bubbles: true }));
    });
    expect(popup()).not.toBeNull();

    await act(async () => {
      container
        .querySelector(".clipped-table")!
        .dispatchEvent(new Event("scroll", { bubbles: true }));
    });
    expect(popup()).toBeNull();
  });

  it("removes its portal and global listeners on unmount", async () => {
    await renderMenu(<button type="button">查看</button>);
    const removeDocumentListener = vi.spyOn(document, "removeEventListener");
    const removeWindowListener = vi.spyOn(window, "removeEventListener");
    await click(trigger());
    expect(popup()).not.toBeNull();

    await act(async () => root.unmount());
    mounted = false;

    expect(popup()).toBeNull();
    for (const type of ["pointerdown", "scroll", "keydown"])
      expect(
        removeDocumentListener.mock.calls.some(([removedType]) => removedType === type),
      ).toBe(true);
    expect(
      removeWindowListener.mock.calls.some(([removedType]) => removedType === "resize"),
    ).toBe(true);
  });
});
