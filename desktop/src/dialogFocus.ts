import {
  useEffect,
  useRef,
  type KeyboardEvent as ReactKeyboardEvent,
  type MouseEvent as ReactMouseEvent,
  type PointerEvent as ReactPointerEvent,
  type RefObject,
} from "react";

export function isCleanEscape(
  event: KeyboardEvent | ReactKeyboardEvent,
): boolean {
  if (event.key !== "Escape" || event.defaultPrevented) return false;
  const composing =
    "nativeEvent" in event
      ? event.nativeEvent.isComposing ||
        (event as unknown as { isComposing?: boolean }).isComposing
      : event.isComposing;
  return !composing;
}

export function useBackdropDismiss(onDismiss: () => void) {
  const pressedBackdrop = useRef<boolean | null>(null);
  return {
    onPointerDown: (event: ReactPointerEvent<HTMLElement>) => {
      pressedBackdrop.current = event.target === event.currentTarget;
    },
    onMouseDown: (event: ReactMouseEvent<HTMLElement>) => {
      if (pressedBackdrop.current === null) {
        pressedBackdrop.current = event.target === event.currentTarget;
      }
    },
    onClick: (event: ReactMouseEvent<HTMLElement>) => {
      const startedOnBackdrop = pressedBackdrop.current;
      pressedBackdrop.current = null;
      if (event.target === event.currentTarget && startedOnBackdrop !== false) {
        onDismiss();
      }
    },
  };
}

// Keep keyboard editing within the active editor and return to its trigger.
export function useDialogFocus(
  ref: RefObject<HTMLElement | null>,
  active = true,
) {
  useEffect(() => {
    if (!active) return;
    const dialog = ref.current;
    if (!dialog) return;
    const previous = document.activeElement;
    const controls = () =>
      Array.from(
        dialog.querySelectorAll<HTMLElement>(
          'button:not(:disabled), input:not(:disabled), select:not(:disabled), textarea:not(:disabled), summary, [tabindex="0"]',
        ),
      );
    if (!dialog.contains(document.activeElement)) controls()[0]?.focus();
    const trap = (event: KeyboardEvent) => {
      if (event.key !== "Tab") return;
      const items = controls(),
        first = items[0],
        last = items.at(-1);
      if (!first || !last) {
        event.preventDefault();
        return;
      }
      if (
        event.shiftKey &&
        (document.activeElement === first ||
          !dialog.contains(document.activeElement))
      ) {
        event.preventDefault();
        last.focus();
      } else if (
        !event.shiftKey &&
        (document.activeElement === last ||
          !dialog.contains(document.activeElement))
      ) {
        event.preventDefault();
        first.focus();
      }
    };
    dialog.addEventListener("keydown", trap);
    return () => {
      dialog.removeEventListener("keydown", trap);
      if (previous instanceof HTMLElement && previous.isConnected)
        previous.focus({ preventScroll: true });
    };
  }, [ref, active]);
}
