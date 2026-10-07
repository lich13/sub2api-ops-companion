import { useEffect, useId, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { MoreHorizontal } from "lucide-react";

export default function AccountActionMenu({ label, children }: { label: string; children: ReactNode }) {
  const [open, setOpen] = useState(false);
  const [position, setPosition] = useState<{ top: number; left: number }>();
  const trigger = useRef<HTMLButtonElement>(null);
  const popup = useRef<HTMLDivElement>(null);
  const id = useId();

  useLayoutEffect(() => {
    if (!open || !trigger.current || !popup.current) return;
    const anchor = trigger.current.getBoundingClientRect();
    const menu = popup.current.getBoundingClientRect();
    const below = window.innerHeight - anchor.bottom - 8;
    const above = anchor.top - 8;
    const top = below >= menu.height || below >= above ? anchor.bottom + 4 : anchor.top - menu.height - 4;
    setPosition({
      top: Math.max(8, Math.min(top, window.innerHeight - menu.height - 8)),
      left: Math.max(8, Math.min(anchor.right - menu.width, window.innerWidth - menu.width - 8)),
    });
  }, [open]);

  useLayoutEffect(() => {
    if (open && position) popup.current?.querySelector<HTMLButtonElement>("button:not(:disabled)")?.focus({ preventScroll: true });
  }, [open, position]);

  useEffect(() => {
    if (!open) return;
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
  }, [open]);

  return <>
    <button ref={trigger} type="button" className="account-more-trigger" aria-label={label} aria-expanded={open} aria-controls={open ? id : undefined}
      onClick={() => { setPosition(undefined); setOpen(!open); }}>
      <MoreHorizontal size={12} aria-hidden="true" />更多
    </button>
    {open && createPortal(
      <div ref={popup} id={id} role="group" aria-label={label} className="account-action-popup"
        style={{ top: position?.top ?? 0, left: position?.left ?? 0, visibility: position ? "visible" : "hidden" }}
        onClick={(event) => {
          const button = (event.target as Element).closest("button");
          if (button && !button.disabled) setOpen(false);
        }}
        onBlur={(event) => {
          if (event.relatedTarget && !event.currentTarget.contains(event.relatedTarget as Node) && event.relatedTarget !== trigger.current) setOpen(false);
        }}
        onKeyDown={(event) => {
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
