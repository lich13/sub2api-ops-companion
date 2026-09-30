import React, {
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import { Check, ChevronDown, Search, X } from "lucide-react";
import { api } from "./bridge";
import type { RecordOption, RecordOptionPage } from "./records";
import { useBackAction } from "./mobile";

export default function RecordFilter({
  kind,
  active,
  value,
  userId,
  onChange,
}: {
  kind: "users" | "api_keys";
  active: boolean;
  value: RecordOption | null;
  userId?: number;
  onChange: (value: RecordOption | null) => void;
}) {
  const label = kind === "users" ? "用户" : "API 密钥";
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [items, setItems] = useState<RecordOption[]>([]);
  const [cursor, setCursor] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const [focused, setFocused] = useState(-1);
  const [placement, setPlacement] = useState({ left: 0, width: 328 });
  const root = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const input = useRef<HTMLInputElement>(null);
  const list = useRef<HTMLDivElement>(null);
  const queue = useRef<Promise<unknown>>(Promise.resolve());
  const pending = useRef(new Set<string>());
  const epoch = useRef(0);
  const context = `${kind}:${userId ?? ""}:${open}:${active}:${query.trim()}`;
  const current = useRef(context);
  current.current = context;
  const listId = useId();
  const close = () => {
    setOpen(false);
    trigger.current?.focus({ preventScroll: true });
  };
  useBackAction(open, close);

  function read(page: string | null, generation = epoch.current) {
    const identity = `${generation}:${page ?? ""}`;
    const valid = () =>
      epoch.current === generation &&
      current.current === context &&
      open &&
      active;
    if (pending.current.has(identity) || !valid()) return;
    pending.current.add(identity);
    setLoading(true);
    setError("");
    const task = queue.current
      .catch(() => {})
      .then(async () => {
        if (!valid()) return;
        const params = new URLSearchParams({ kind, limit: "50" });
        if (query.trim()) params.set("q", query.trim());
        if (kind === "api_keys" && userId !== undefined)
          params.set("user_id", String(userId));
        if (page) params.set("cursor", page);
        try {
          const result = await api<RecordOptionPage>(
            "GET",
            `/usage-record-options?${params}`,
          );
          if (!valid()) return;
          setItems((old) =>
            page
              ? [
                  ...new Map(
                    [...old, ...result.items].map((item) => [item.id, item]),
                  ).values(),
                ]
              : result.items,
          );
          setCursor(result.next_cursor);
        } catch (cause) {
          if (valid())
            setError(cause instanceof Error ? cause.message : String(cause));
        } finally {
          if (valid()) setLoading(false);
        }
      })
      .finally(() => pending.current.delete(identity));
    queue.current = task;
  }

  useEffect(() => {
    const generation = ++epoch.current;
    setItems([]);
    setCursor(null);
    setError("");
    setFocused(-1);
    if (list.current) list.current.scrollTop = 0;
    if (!open || !active) {
      setLoading(false);
      return;
    }
    setLoading(true);
    const timer = setTimeout(
      () => read(null, generation),
      query.trim() ? 250 : 0,
    );
    return () => {
      clearTimeout(timer);
      ++epoch.current;
    };
  }, [context]);
  useEffect(() => {
    if (!active) setOpen(false);
  }, [active]);
  useLayoutEffect(() => {
    if (!open) return;
    const position = () => {
      const rect = root.current?.getBoundingClientRect();
      if (!rect) return;
      const container = root.current
        ?.closest(".records-view")
        ?.getBoundingClientRect();
      const left = container?.width ? container.left : 8;
      const right = container?.width ? container.right : window.innerWidth - 8;
      const width = Math.min(328, right - left);
      setPlacement({
        width,
        left: Math.max(
          left - rect.left,
          Math.min(0, right - rect.left - width),
        ),
      });
    };
    position();
    input.current?.focus({ preventScroll: true });
    window.addEventListener("resize", position);
    const outside = (event: PointerEvent) => {
      if (!root.current?.contains(event.target as Node)) setOpen(false);
    };
    document.addEventListener("pointerdown", outside);
    return () => {
      document.removeEventListener("pointerdown", outside);
      window.removeEventListener("resize", position);
    };
  }, [open]);
  useEffect(() => {
    const box = list.current;
    const option = box?.querySelector<HTMLElement>(`[data-index="${focused}"]`);
    if (!box || !option) return;
    const view = box.getBoundingClientRect();
    const item = option.getBoundingClientRect();
    // Keep keyboard navigation inside the menu; scrolling ancestors moves the table.
    if (item.top < view.top) box.scrollTop += item.top - view.top;
    else if (item.bottom > view.bottom)
      box.scrollTop += item.bottom - view.bottom;
  }, [focused]);

  const digits = query.trim().replace(/^#/, "");
  const id = /^[1-9]\d*$/.test(digits) ? Number(digits) : 0;
  const direct =
    Number.isSafeInteger(id) && id > 0 && !items.some((item) => item.id === id)
      ? {
          id,
          name: null,
          deleted: false,
          ...(kind === "api_keys" && userId ? { user_id: userId } : {}),
        }
      : null;
  const choices: (RecordOption | null)[] = [
    null,
    ...items,
    ...(direct ? [direct] : []),
  ];
  function choose(item: RecordOption | null) {
    onChange(item);
    close();
  }
  const selection = value
    ? `${value.name || value.email || label} #${value.id}`
    : `全部${label}`;
  return (
    <div
      className="record-filter"
      ref={root}
      onBlur={(event) => {
        if (!event.currentTarget.contains(event.relatedTarget)) setOpen(false);
      }}
    >
      <button
        type="button"
        className="record-filter-trigger"
        ref={trigger}
        aria-label={`${label}筛选`}
        aria-haspopup="listbox"
        aria-expanded={open}
        title={selection}
        disabled={!active}
        onClick={() => {
          setQuery("");
          setOpen(!open);
        }}
      >
        <span>{selection}</span>
        <ChevronDown size={13} />
      </button>
      {open && (
        <div
          className="record-filter-popover"
          style={placement}
          onKeyDown={(event) => {
            if (event.key === "Tab") {
              // Continue the page's normal tab order from the trigger, even when
              // the browser makes a scrollable list keyboard-focusable.
              close();
            } else if (event.key === "Escape") {
              event.preventDefault();
              event.stopPropagation();
              close();
            }
          }}
        >
          <header className="mobile-sheet-heading"><strong>选择{label}</strong><button type="button" className="icon-button" aria-label={`关闭${label}选择`} onClick={close}><X size={20}/></button></header>
          <div className="record-filter-search">
            <Search size={14} />
            <input
              ref={input}
              value={query}
              maxLength={200}
              role="combobox"
              aria-label={`搜索${label}`}
              aria-controls={listId}
              aria-expanded="true"
              aria-autocomplete="list"
              aria-activedescendant={
                focused >= 0 ? `${listId}-${focused}` : undefined
              }
              placeholder={kind === "users" ? "名称、邮箱或 ID" : "名称或 ID"}
              onChange={(event) => {
                setQuery(event.target.value);
                setFocused(-1);
              }}
              onKeyDown={(event) => {
                if (event.key === "ArrowDown" || event.key === "ArrowUp") {
                  event.preventDefault();
                  setFocused((old) =>
                    old < 0
                      ? event.key === "ArrowDown"
                        ? 0
                        : choices.length - 1
                      : (old +
                          (event.key === "ArrowDown" ? 1 : choices.length - 1) +
                          choices.length) %
                        choices.length,
                  );
                } else if (event.key === "Enter") {
                  event.preventDefault();
                  if (focused >= 0 && focused < choices.length)
                    choose(choices[focused]);
                  else if (id > 0 && Number.isSafeInteger(id))
                    choose(items.find((item) => item.id === id) || direct);
                  else if (items.length === 1) choose(items[0]);
                }
              }}
            />
          </div>
          <div
            className="record-filter-list"
            id={listId}
            role="listbox"
            tabIndex={-1}
            aria-label={label}
            ref={list}
            aria-busy={loading}
            onScroll={(event) => {
              const box = event.currentTarget;
              if (
                cursor &&
                !loading &&
                !error &&
                box.scrollHeight - box.scrollTop - box.clientHeight < 40
              )
                read(cursor);
            }}
          >
            {choices.map((item, index) => {
              const selected = (item?.id ?? null) === (value?.id ?? null);
              const isDirect = item !== null && item === direct;
              const owner =
                item?.user_id != null
                  ? `${item.user_name || item.user_email || "用户"} #${item.user_id}`
                  : item?.email;
              return (
                <button
                  type="button"
                  role="option"
                  tabIndex={-1}
                  key={item?.id ?? "all"}
                  id={`${listId}-${index}`}
                  data-index={index}
                  aria-selected={selected}
                  className={`record-filter-option${focused === index ? " focused" : ""}`}
                  onMouseEnter={() => setFocused(index)}
                  onClick={() => choose(item)}
                >
                  <span className="record-filter-option-text">
                    <span>
                      {isDirect ? (
                        `按 ID #${item.id} 筛选`
                      ) : item ? (
                        <>
                          <span>{item.name || item.email || label}</span>
                          <small>#{item.id}</small>
                        </>
                      ) : (
                        `全部${label}`
                      )}
                    </span>
                    {owner && <small title={owner}>{owner}</small>}
                  </span>
                  {item?.deleted ? (
                    <small className="record-option-state">已删除</small>
                  ) : item?.status && item.status !== "active" ? (
                    <small className="record-option-state">停用</small>
                  ) : null}
                  {selected && <Check size={13} />}
                </button>
              );
            })}
            {loading && (
              <div className="record-filter-message" role="status">
                正在读取…
              </div>
            )}
            {!loading && !items.length && !direct && !error && (
              <div className="record-filter-message">无匹配项</div>
            )}
            {error && (
              <div className="record-filter-error" role="alert">
                <span>{error}</span>
                <button type="button" onClick={() => read(cursor)}>
                  重试
                </button>
              </div>
            )}
            {cursor && !loading && !error && (
              <button
                type="button"
                className="record-filter-more"
                onClick={() => read(cursor)}
              >
                加载更多
              </button>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
