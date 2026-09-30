import { useEffect, useRef, useState } from "react";
import { CalendarDays, ChevronDown, X } from "lucide-react";
import { useBackAction } from "./mobile";
import {
  datePresets,
  dateRangeLabel,
  presetRange,
  validDateRange,
  type RecordDateRange,
} from "./recordDates";

export default function RecordDatePicker({
  value,
  disabled,
  onChange,
}: {
  value: RecordDateRange;
  disabled: boolean;
  onChange: (value: RecordDateRange) => void;
}) {
  const [open, setOpen] = useState(false),
    [draft, setDraft] = useState(value),
    [error, setError] = useState("");
  const root = useRef<HTMLDivElement>(null),
    trigger = useRef<HTMLButtonElement>(null);
  const close = () => {
    setOpen(false);
    trigger.current?.focus({ preventScroll: true });
  };
  useBackAction(open, close);
  useEffect(() => {
    if (disabled) setOpen(false);
  }, [disabled]);
  useEffect(() => {
    if (!open) return;
    const outside = (event: PointerEvent) => {
      if (!root.current?.contains(event.target as Node)) setOpen(false);
    };
    document.addEventListener("pointerdown", outside);
    return () => document.removeEventListener("pointerdown", outside);
  }, [open]);
  return (
    <div
      className="records-date-picker"
      ref={root}
      onKeyDown={(event) => {
        if (event.key === "Escape" && open) {
          event.stopPropagation();
          close();
        }
      }}
    >
      <button
        type="button"
        ref={trigger}
        aria-label="时间范围"
        aria-expanded={open}
        aria-haspopup="dialog"
        disabled={disabled}
        className="records-date-trigger"
        onClick={() => {
          setDraft(value);
          setError("");
          setOpen(!open);
        }}
      >
        <CalendarDays size={16} />
        <span>{dateRangeLabel(value)}</span>
        <ChevronDown size={14} />
      </button>
      {open && (
        <div
          className="records-date-popover"
          role="dialog"
          aria-label="时间范围"
        >
          <header className="mobile-sheet-heading">
            <strong>时间范围</strong>
            <button
              type="button"
              className="icon-button"
              aria-label="关闭时间范围"
              onClick={close}
            >
              <X size={20} />
            </button>
          </header>
          <div className="records-date-presets">
            {datePresets.map(([key, label]) => (
              <button
                type="button"
                key={key}
                aria-pressed={draft.preset === key}
                onClick={() => {
                  setDraft(presetRange(key));
                  setError("");
                }}
              >
                {label}
              </button>
            ))}
          </div>
          <div className="records-date-inputs">
            <label>
              开始日期
              <input
                type="date"
                aria-label="开始日期"
                value={draft.start}
                max={draft.end || undefined}
                onChange={(event) =>
                  setDraft({
                    ...draft,
                    preset: null,
                    start: event.target.value,
                  })
                }
              />
            </label>
            <label>
              结束日期
              <input
                type="date"
                aria-label="结束日期"
                value={draft.end}
                min={draft.start || undefined}
                onChange={(event) =>
                  setDraft({ ...draft, preset: null, end: event.target.value })
                }
              />
            </label>
          </div>
          {error && (
            <div className="records-error" role="alert">
              {error}
            </div>
          )}
          <footer>
            <button
              type="button"
              className="primary"
              onClick={() => {
                const next = draft.preset ? presetRange(draft.preset) : draft;
                if (!validDateRange(next.start, next.end)) {
                  setError("请选择有效的日期范围");
                  return;
                }
                onChange(next);
                close();
              }}
            >
              应用
            </button>
          </footer>
        </div>
      )}
    </div>
  );
}
