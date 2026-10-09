// @vitest-environment jsdom
import { act, type ComponentProps, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import AccountActionMenu from "./AccountActionMenu";
import { handleBack } from "./mobile";

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

async function renderMenu(
  children: ReactNode,
  props: Partial<Omit<ComponentProps<typeof AccountActionMenu>, "label" | "children">> = {},
) {
  await act(async () =>
    root.render(
      <div className="clipped-table" style={{ height: 24, overflow: "hidden" }}>
        <AccountActionMenu label={label} {...props}>{children}</AccountActionMenu>
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

  it("keeps only one portal open when another account trigger is activated", async () => {
    await act(async () => root.render(<>
      <AccountActionMenu label="第一个账号操作"><button type="button">查看第一个</button></AccountActionMenu>
      <AccountActionMenu label="第二个账号操作"><button type="button">查看第二个</button></AccountActionMenu>
    </>));
    const triggers = [...container.querySelectorAll<HTMLButtonElement>(".account-more-trigger")];
    await click(triggers[0]);
    expect(document.body.querySelectorAll(".account-action-popup")).toHaveLength(1);

    await click(triggers[1]);

    expect(document.body.querySelectorAll(".account-action-popup")).toHaveLength(1);
    expect(document.body.querySelector(".account-action-popup")?.textContent).toBe("查看第二个");
    expect(triggers[0].getAttribute("aria-expanded")).toBe("false");
    expect(triggers[1].getAttribute("aria-expanded")).toBe("true");
  });

  it("closes on Android Back without invoking the page fallback", async () => {
    const fallback = vi.fn();
    await renderMenu(<button type="button">查看</button>);
    await click(trigger());

    await act(async () => handleBack(fallback));

    expect(popup()).toBeNull();
    expect(fallback).not.toHaveBeenCalled();
    await act(async () => handleBack(fallback));
    expect(fallback).toHaveBeenCalledOnce();
  });

  it("closes when focus leaves the menu", async () => {
    await renderMenu(<button type="button">查看</button>);
    await click(trigger());

    await act(async () => container.querySelector<HTMLButtonElement>("[data-outside]")!.focus());

    expect(popup()).toBeNull();
  });

  it("closes on deactivation and context changes without reopening on return", async () => {
    const action = <button type="button">查看</button>;
    await renderMenu(action, { active: true, contextKey: "connection-1:openai" });
    await click(trigger());
    await renderMenu(action, { active: false, contextKey: "connection-1:openai" });
    expect(popup()).toBeNull();
    await renderMenu(action, { active: true, contextKey: "connection-1:openai" });
    expect(popup()).toBeNull();

    await click(trigger());
    await renderMenu(action, { active: true, contextKey: "connection-1:grok" });
    expect(popup()).toBeNull();
    await click(trigger());
    await renderMenu(action, { active: true, contextKey: "connection-2:grok" });
    expect(popup()).toBeNull();
  });

  it("does not let trigger or portal events reach a group destination", async () => {
    const destinationClick = vi.fn();
    const destinationKey = vi.fn();
    const action = vi.fn();
    await act(async () => root.render(
      <section onClick={destinationClick} onKeyDown={destinationKey}>
        <AccountActionMenu label={label} iconOnly>
          <button type="button" onClick={action}>测试连接</button>
        </AccountActionMenu>
      </section>,
    ));
    await press(trigger(), "Enter");
    await click(trigger());
    const option = popup()!.querySelector<HTMLButtonElement>("button")!;
    await press(option, "Enter");
    await press(option, " ");
    await click(option);

    expect(action).toHaveBeenCalledOnce();
    expect(destinationClick).not.toHaveBeenCalled();
    expect(destinationKey).not.toHaveBeenCalled();
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
