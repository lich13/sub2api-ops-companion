import { useEffect, useId, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { MoreHorizontal } from "lucide-react";
import { useBackAction } from "./mobile";

const openedEvent = "sub2ops-account-menu-opened";

export default function AccountActionMenu({ label, children, iconOnly = false, active = true, contextKey, disabled = false }: {
  label: string; children: ReactNode; iconOnly?: boolean; active?: boolean; contextKey?: string; disabled?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const [position, setPosition] = useState<{ top: number; left: number }>();
  const trigger = useRef<HTMLButtonElement>(null);
  const popup = useRef<HTMLDivElement>(null);
  const id = useId();
  const visible = open && active && !disabled;
  useBackAction(visible, () => { setOpen(false); trigger.current?.focus({ preventScroll: true }); });
  useEffect(() => { setOpen(false); setPosition(undefined); }, [active, contextKey, disabled]);
  useEffect(() => {
    const closeOther = (event: Event) => { if ((event as CustomEvent<string>).detail !== id) setOpen(false); };
    document.addEventListener(openedEvent, closeOther);
    return () => document.removeEventListener(openedEvent, closeOther);
  }, [id]);

  useLayoutEffect(() => {
    if (!visible || !trigger.current || !popup.current) return;
    const anchor = trigger.current.getBoundingClientRect();
    const menu = popup.current.getBoundingClientRect();
    const below = window.innerHeight - anchor.bottom - 8;
    const above = anchor.top - 8;
    const top = below >= menu.height || below >= above ? anchor.bottom + 4 : anchor.top - menu.height - 4;
    setPosition({
      top: Math.max(8, Math.min(top, window.innerHeight - menu.height - 8)),
      left: Math.max(8, Math.min(anchor.right - menu.width, window.innerWidth - menu.width - 8)),
    });
  }, [visible]);

  useLayoutEffect(() => {
    if (visible && position) popup.current?.querySelector<HTMLButtonElement>("button:not(:disabled)")?.focus({ preventScroll: true });
  }, [visible, position]);

  useEffect(() => {
    if (!visible) return;
    const outside = (event: PointerEvent) => {
      if (!popup.current?.contains(event.target as Node) && !trigger.current?.contains(event.target as Node)) setOpen(false);
    };
    const scroll = (event: Event) => {
      if (!popup.current?.contains(event.target as Node)) setOpen(false);
    };
    const resize = () => setOpen(false);
    const key = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      event.stopPropagation();
      setOpen(false);
      trigger.current?.focus({ preventScroll: true });
    };
    document.addEventListener("pointerdown", outside);
    document.addEventListener("scroll", scroll, true);
    document.addEventListener("keydown", key, true);
    window.addEventListener("resize", resize);
    return () => {
      document.removeEventListener("pointerdown", outside);
      document.removeEventListener("scroll", scroll, true);
      document.removeEventListener("keydown", key, true);
      window.removeEventListener("resize", resize);
    };
  }, [visible]);

  return <>
    <button ref={trigger} type="button" className={`account-more-trigger${iconOnly ? " icon-only" : ""}`} aria-label={label} aria-expanded={visible} aria-controls={visible ? id : undefined}
      disabled={disabled || !active} onPointerDown={(event) => event.stopPropagation()} onKeyDown={(event) => event.stopPropagation()}
      onClick={(event) => {
        event.stopPropagation();
        if (!visible) document.dispatchEvent(new CustomEvent(openedEvent, { detail: id }));
        setPosition(undefined); setOpen(!visible);
      }}>
      <MoreHorizontal size={iconOnly ? 17 : 12} aria-hidden="true" />{!iconOnly && "更多"}
    </button>
    {visible && createPortal(
      <div ref={popup} id={id} role="group" aria-label={label} className="account-action-popup"
        style={{ top: position?.top ?? 0, left: position?.left ?? 0, visibility: position ? "visible" : "hidden" }}
        onPointerDown={(event) => event.stopPropagation()}
        onClick={(event) => {
          event.stopPropagation();
          const button = (event.target as Element).closest("button");
          if (button && !button.disabled) setOpen(false);
        }}
        onBlur={(event) => {
          if (event.relatedTarget && !event.currentTarget.contains(event.relatedTarget as Node) && event.relatedTarget !== trigger.current) setOpen(false);
        }}
        onKeyDown={(event) => {
          event.stopPropagation();
          if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) return;
          const buttons = [...event.currentTarget.querySelectorAll<HTMLButtonElement>("button:not(:disabled)")];
          if (!buttons.length) return;
          event.preventDefault();
          const current = buttons.indexOf(document.activeElement as HTMLButtonElement);
          const next = event.key === "Home" ? 0 : event.key === "End" ? buttons.length - 1 :
            (current + (event.key === "ArrowDown" ? 1 : -1) + buttons.length) % buttons.length;
          buttons[next].focus({ preventScroll: true });
        }}>{children}</div>, document.body)}
  </>;
}
