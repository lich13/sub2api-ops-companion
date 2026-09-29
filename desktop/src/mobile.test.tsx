// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vitest";
import { handleBack, useBackAction } from "./mobile";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
function Surface({ open, action }: { open: boolean; action: () => void }) {
  useBackAction(open, action);
  return null;
}
it("Back closes the most recent surface, uses current callbacks, and releases closed surfaces", async () => {
  const node = document.createElement("div"), root = createRoot(node);
  const parent = vi.fn(), child = vi.fn(), updated = vi.fn(), fallback = vi.fn();
  await act(async () => root.render(<><Surface open action={parent}/><Surface open action={child}/></>));
  handleBack(fallback);
  expect(child).toHaveBeenCalledOnce();
  expect(parent).not.toHaveBeenCalled();
  await act(async () => root.render(<><Surface open action={updated}/><Surface open={false} action={child}/></>));
  handleBack(fallback);
  expect(updated).toHaveBeenCalledOnce();
  await act(async () => root.unmount());
  handleBack(fallback);
  expect(fallback).toHaveBeenCalledOnce();
});
