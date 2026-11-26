import { useRef } from "react";
import {
  isCleanEscape,
  useBackdropDismiss,
  useDialogFocus,
} from "./dialogFocus";

export function ConfirmDiscardDialog({
  open,
  onCancel,
  onConfirm,
}: {
  open: boolean;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  const dialogRef = useRef<HTMLElement>(null);
  useDialogFocus(dialogRef, open);
  const backdrop = useBackdropDismiss(onCancel);
  if (!open) return null;
  return (
    <div className="modal-backdrop" {...backdrop}>
      <section
        ref={dialogRef}
        className="discard-groups-dialog"
        role="alertdialog"
        aria-modal="true"
        aria-label="放弃未保存的更改？"
        tabIndex={-1}
        onClick={(e) => e.stopPropagation()}
        onKeyDown={(e) => {
          if (isCleanEscape(e)) {
            e.preventDefault();
            e.stopPropagation();
            onCancel();
          }
        }}
      >
        <h2>放弃未保存的更改？</h2>
        <div>
          <button type="button" onClick={onCancel}>
            继续编辑
          </button>
          <button type="button" className="primary" onClick={onConfirm}>
            放弃修改
          </button>
        </div>
      </section>
    </div>
  );
}
