import { useEffect, useRef } from "react";
import { onBackButtonPress } from "@tauri-apps/api/app";

const backActions: { action: () => void }[] = [];

// Dialogs register only while open. The most recently opened surface owns Back.
export function useBackAction(open: boolean, action: () => void) {
  const current = useRef(action);
  current.current = action;
  useEffect(() => {
    if (!open) return;
    const entry = { action: () => current.current() };
    backActions.push(entry);
    return () => { const i = backActions.indexOf(entry); if (i >= 0) backActions.splice(i, 1); };
  }, [open]);
}

export function handleBack(fallback: () => void) {
  const entry = backActions.at(-1);
  if (entry) entry.action();
  else fallback();
}

export async function listenBack(fallback: () => void) {
  if (!("__TAURI_INTERNALS__" in window)) return () => {};
  const listener = await onBackButtonPress(() => handleBack(fallback));
  return () => { void listener.unregister(); };
}
