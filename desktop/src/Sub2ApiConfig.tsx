import { useEffectEvent, useCallback } from "react";
import { memo, useEffect, useMemo, useRef, useState } from "react";
import { ChevronRight, Eye, EyeOff, RefreshCw, Search, X } from "lucide-react";
import { parseDocument, stringify } from "yaml";
import { api } from "./bridge";
import { errorMessage as errorText, ignoredFailure } from "./bridgeError";
import type {
  HostConfigHeader,
  HostFieldIssue,
  HostOperationReply,
  HostValidationReply,
} from "./protocol";
import Sub2ApiVersion from "./Sub2ApiVersion";
import { ProxyDialog } from "./ProxyDialogs";
import { NumberFieldInput } from "./NumberFieldInput";
import "./sub2api-config.css";

type ConfigField = {
  path: string;
  group: string;
  type: string;
  type_label: string;
  description: string;
  range: string;
  default_text: string;
  default_value: unknown;
  has_default: boolean;
  nullable?: boolean;
  sensitive: boolean;
  internal: boolean;
  editable: boolean;
  status: string;
  source_url: string;
  env: string[];
  choices?: string[];
};
type FieldValue = {
  configured: boolean;
  yaml: unknown;
  effective: unknown;
  source: string;
  environment: string | null;
  editable: boolean;
};
export type ConfigDocument = HostConfigHeader & {
  available: true;
  version: string;
  commit: string;
  healthy: boolean;
  revision: string;
  runtime_revision: string;
  catalog: {
    version: string;
    revision: string;
    groups: { id: string; label: string }[];
    fields: ConfigField[];
  };
  values: Record<string, FieldValue>;
  unknown: HostFieldIssue[];
};
type ConfigRead = ConfigDocument | (HostConfigHeader & { available: false });
type Change = { path: string; remove: boolean; value?: unknown };
type Issue = HostFieldIssue;
type Operation = Omit<HostOperationReply, "message"> & {
  message?: string | null;
};
const running = (operation: Operation | null) =>
  !!operation &&
  ["saving", "restarting", "restoring"].includes(operation.status);
const display = (value: unknown) =>
  value === undefined || value === null
    ? "未设置"
    : typeof value === "string"
      ? value || '""'
      : JSON.stringify(value);
const equal = (a: unknown, b: unknown) =>
  JSON.stringify(a) === JSON.stringify(b);
const containsPath = (parent: string, path: string) =>
  path === parent ||
  path.startsWith(`${parent}.`) ||
  path.startsWith(`${parent}[`);
function pathAncestors(path: string): string[] {
  const out = [path];
  for (let i = path.length - 1; i > 0; i--) {
    if (path[i] === "." || path[i] === "[") out.push(path.slice(0, i));
  }
  return out;
}
const summary = (description: string) => {
  const first = description.split(/[。\n]/)[0];
  return first.length > 90 ? `${first.slice(0, 90)}…` : first;
};
const subsection = (field: ConfigField) =>
  field.path.split(".").length > 2
    ? field.path.split(".").slice(0, 2).join(".")
    : field.group;
export const structuredText = (value: unknown) =>
  typeof value === "string" ? value : value == null ? "" : stringify(value);
export function structuredValue(text: string, type: string) {
  const document = parseDocument(text, { uniqueKeys: true });
  if (document.errors.length)
    return { value: text, error: "YAML 语法无效，请检查缩进和重复字段" };
  let value: unknown;
  try {
    value = document.toJS({ maxAliasCount: 100 });
  } catch {
    return { value: text, error: "YAML 别名数量过多" };
  }
  const valid =
    type === "array"
      ? Array.isArray(value)
      : !!value && typeof value === "object" && !Array.isArray(value);
  return {
    value: valid ? value : text,
    error: valid ? "" : type === "array" ? "需要 YAML 列表" : "需要 YAML 映射",
  };
}

function FieldEditor({
  field,
  value,
  unconfigured = false,
  disabled,
  change,
  invalid = false,
}: {
  field: ConfigField;
  value: unknown;
  unconfigured?: boolean;
  disabled: boolean;
  change: (value: unknown) => void;
  invalid?: boolean;
}) {
  const [reveal, setReveal] = useState(false);
  const structured = field.type === "array" || field.type === "object";
  const numeric = field.type === "integer" || field.type === "number";
  const [yaml, setYaml] = useState(() => structuredText(value));
  const [error, setError] = useState(() =>
    structured &&
    typeof value === "string" &&
    !(unconfigured && value.trim() === "")
      ? structuredValue(value, field.type).error
      : "",
  );
  const dirty = useRef(
    structured && typeof value === "string" && !(unconfigured && value === ""),
  );
  const emitted = useRef<unknown>(value);
  useEffect(() => {
    if (structured && emitted.current !== value) {
      setYaml(structuredText(value));
      const hasError =
        typeof value === "string" && !(unconfigured && value.trim() === "");
      setError(hasError ? structuredValue(value, field.type).error : "");
      dirty.current =
        typeof value === "string" && !(unconfigured && value === "");
      emitted.current = value;
    }
  }, [value, structured, field.type, unconfigured]);
  const id = `config-${field.path}`;
  const a11y = {
    "aria-describedby": `${id}-help ${id}-errors${error && !invalid ? ` ${id}-syntax` : ""}`,
    "aria-invalid": invalid || !!error,
  };
  if (field.type === "boolean")
    return (
      <div className="config-boolean">
        <button
          type="button"
          role="switch"
          className={`switch ${value === true ? "on" : ""}`}
          id={id}
          aria-label={field.path}
          aria-checked={value === true}
          {...a11y}
          disabled={disabled}
          onClick={() => change(value !== true)}
        >
          <span />
        </button>
        <span>{value == null ? "未配置" : value ? "开启" : "关闭"}</span>
      </div>
    );
  if (field.choices)
    return (
      <select
        id={id}
        aria-label={field.path}
        {...a11y}
        disabled={disabled}
        value={String(value ?? "")}
        onChange={(e) => change(e.target.value)}
      >
        {!field.choices.includes(String(value ?? "")) && (
          <option value={String(value ?? "")}>{display(value)}</option>
        )}
        {field.choices.map((v) => (
          <option key={v} value={v}>
            {v}
          </option>
        ))}
      </select>
    );
  if (structured)
    return (
      <div className="config-structured">
        <textarea
          id={id}
          aria-label={field.path}
          {...a11y}
          disabled={disabled}
          rows={4}
          value={yaml}
          placeholder={
            field.type === "array"
              ? "输入 YAML 列表，例如 []"
              : "输入 YAML 映射，例如 {}"
          }
          onChange={(e) => {
            const text = e.target.value;
            if (unconfigured && text === "") {
              dirty.current = false;
              setYaml("");
              if (error) setError("");
              emitted.current = null;
              change("");
              return;
            }
            dirty.current = true;
            setYaml(text);
            if (error) setError("");
            emitted.current = text;
            change(text);
          }}
          onBlur={() => {
            if (!dirty.current) return;
            if (unconfigured && yaml.trim() === "") {
              dirty.current = false;
              setYaml("");
              setError("");
              emitted.current = null;
              change("");
              return;
            }
            const result = structuredValue(yaml, field.type);
            setError(result.error);
            dirty.current = !!result.error;
            emitted.current = result.value;
            change(result.value);
          }}
        />
        {error && !invalid && (
          <small id={`${id}-syntax`} role="alert" className="bad-text">
            {error}
          </small>
        )}
      </div>
    );
  return (
    <div className="config-input">
      {numeric && (!field.sensitive || reveal) ? (
        <NumberFieldInput
          id={id}
          aria-label={field.path}
          {...a11y}
          disabled={disabled}
          step={field.type === "integer" ? 1 : "any"}
          value={typeof value === "number" ? value : null}
          allowEmpty
          autoComplete="off"
          spellCheck={false}
          placeholder={value == null ? "未配置" : undefined}
          onChange={(next) => change(next)}
        />
      ) : (
        <input
          id={id}
          aria-label={field.path}
          {...a11y}
          disabled={disabled}
          type={field.sensitive && !reveal ? "password" : "text"}
          value={value == null ? "" : String(value)}
          autoComplete="off"
          spellCheck={false}
          placeholder={
            value == null
              ? "未配置"
              : typeof value === "string" && !value
                ? "空字符串"
                : undefined
          }
          onChange={(e) => {
            const text = e.target.value;
            if (numeric) {
              change(text === "" ? null : Number(text));
            } else {
              change(text);
            }
          }}
        />
      )}
      {field.sensitive && (
        <button
          type="button"
          aria-label={reveal ? "隐藏字段值" : "显示字段值"}
          onClick={() => setReveal((v) => !v)}
        >
          {reveal ? <EyeOff size={16} /> : <Eye size={16} />}
        </button>
      )}
    </div>
  );
}

const EMPTY_CHILDREN: ConfigField[] = [];
const EMPTY_ISSUES: Issue[] = [];

const ConfigFieldRow = memo(function ConfigFieldRow({
  field,
  current,
  change,
  children,
  issues,
  expand,
  blocked,
  locked,
  onEdit,
  onRemove,
  onUndo,
}: {
  field: ConfigField;
  current?: FieldValue;
  change?: Change;
  children: ConfigField[];
  issues: Issue[];
  expand: boolean;
  blocked: boolean;
  locked: boolean;
  onEdit: (field: ConfigField, value: unknown, explicit?: boolean) => void;
  onRemove: (field: ConfigField) => void;
  onUndo: (path: string) => void;
}) {
  const [details, setDetails] = useState(false);
  useEffect(() => {
    if (expand) setDetails(true);
  }, [expand]);
  const handleEdit = useCallback(
    (val: unknown, explicit?: boolean) => onEdit(field, val, explicit),
    [onEdit, field],
  );
  const handleRemove = useCallback(() => onRemove(field), [onRemove, field]);
  const handleUndo = useCallback(
    () => onUndo(field.path),
    [onUndo, field.path],
  );
  const configured = change ? !change.remove : !!current?.configured;
  const value = change
    ? change.remove
      ? field.default_value
      : change.value
    : current?.source === "environment"
      ? current.effective
      : current?.configured
        ? current.yaml
        : field.default_value;
  const overridden = current?.source === "environment";
  const editable = !!current?.editable && field.editable;
  const source = overridden
    ? "环境变量 · 只读"
    : field.status === "reserved"
      ? "保留字段 · 只读"
      : !editable
        ? "只读"
        : configured
          ? "YAML"
          : field.has_default
            ? "使用默认值"
            : "未配置";
  const id = `config-${field.path}`;
  return (
    <article
      className={`config-field${change ? " changed" : ""}${issues.length ? " invalid" : ""}`}
      data-config-path={field.path}
    >
      <div className="config-field-intro">
        <label htmlFor={id}>{summary(field.description)}</label>
        <code className="config-path">{field.path}</code>
        <div className="config-field-tags">
          <span>{field.type_label}</span>
          <span className={overridden || !editable ? "config-source" : ""}>
            {source}
          </span>
          {change && (
            <span className="config-dirty">
              {change.remove ? "待恢复默认" : "已修改"}
            </span>
          )}
        </div>
        {overridden && (
          <p className="config-override">
            由 <code>{current.environment}</code> 覆盖，不能在此修改。
            <br />
            YAML：
            {field.sensitive
              ? current.configured
                ? "已遮蔽"
                : "未配置"
              : current.configured
                ? display(current.yaml)
                : "未配置"}
          </p>
        )}
        {!editable && !overridden && (
          <p className="config-override">
            {field.status === "reserved"
              ? "当前版本保留此字段，不支持编辑。"
              : "当前版本不支持修改此字段。"}
          </p>
        )}
      </div>
      <div className="config-field-control">
        <FieldEditor
          key={`${field.path}:${change?.remove ?? false}:${current?.configured}`}
          field={field}
          value={value}
          unconfigured={
            !current?.configured && !change?.remove && !field.has_default
          }
          disabled={blocked || !editable || overridden}
          invalid={issues.length > 0}
          change={handleEdit}
        />
        <div id={`${id}-help`} className="config-value-meta">
          <span>
            默认：<span>{field.default_text}</span>
          </span>
          <span>
            范围：
            <span>
              {field.range.startsWith("官方未声明")
                ? "官方未声明额外范围"
                : field.range}
            </span>
          </span>
        </div>
        <div id={`${id}-errors`} className="config-field-errors">
          {issues.map((issue, index) => (
            <p key={index} role="alert" className="bad-text">
              {issue.message}
            </p>
          ))}
        </div>
        {(change || (editable && configured)) && (
          <div className="config-field-actions">
            {editable && configured && (
              <button type="button" disabled={blocked} onClick={handleRemove}>
                恢复默认
              </button>
            )}
            {change && (
              <button type="button" disabled={locked} onClick={handleUndo}>
                撤销此项
              </button>
            )}
          </div>
        )}
      </div>
      <details
        className="config-field-details"
        open={details}
        onToggle={(e) => setDetails(e.currentTarget.open)}
      >
        <summary>
          <ChevronRight size={14} className="config-summary-arrow" />
          <span>
            完整说明{children.length ? `与内部字段（${children.length}）` : ""}
          </span>
        </summary>
        <div className="config-detail-body">
          <p>{field.description}</p>
          <p>允许范围：{field.range}</p>
          {children.map((child) => (
            <div className="config-child" key={child.path}>
              <code>{child.path}</code>
              <p>{child.description}</p>
              <span>
                {child.type_label} · {child.range}
              </span>
            </div>
          ))}
          <div className="config-detail-actions">
            <a href={field.source_url} target="_blank" rel="noreferrer">
              官方源码
            </a>
            {editable && !configured && field.has_default && (
              <button
                type="button"
                disabled={blocked}
                onClick={() => handleEdit(field.default_value, true)}
              >
                写入默认值
              </button>
            )}
          </div>
        </div>
      </details>
    </article>
  );
});

export default function Sub2ApiConfig({
  active,
  online,
  connectionKey = "",
}: {
  active: boolean;
  online: boolean;
  connectionKey?: string;
}) {
  const [response, setResponse] = useState<ConfigRead | null>(null),
    [draft, setDraft] = useState<Record<string, Change>>({});
  const document = response?.available ? response : null;
  const [query, setQuery] = useState(""),
    [filter, setFilter] = useState("all"),
    [group, setGroup] = useState("server"),
    [scopeGroup, setScopeGroup] = useState(""),
    [focusTarget, setFocusTarget] = useState<{ path: string } | null>(null);
  const rootRef = useRef<HTMLElement>(null),
    fieldsRef = useRef<HTMLDivElement>(null);
  const [busy, setBusy] = useState(false),
    [error, setError] = useState(""),
    [issues, setIssues] = useState<Issue[]>([]),
    [message, setMessage] = useState("");
  const [confirm, setConfirm] = useState(false),
    [operation, setOperation] = useState<Operation | null>(null);
  const alive = useRef(true),
    loadSeq = useRef(0),
    draftRef = useRef(draft),
    documentRef = useRef(document),
    conflicted = useRef(false),
    statusLoaded = useRef(false),
    uncertain = useRef(false),
    applying = running(operation);
  draftRef.current = draft;
  documentRef.current = document;
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  async function load(reset = false, protectDraft = false) {
    const seq = ++loadSeq.current;
    setBusy(true);
    setError("");
    try {
      const next = await api<ConfigRead>("GET", "/sub2api-config");
      if (alive.current && seq === loadSeq.current) {
        const currentDoc = documentRef.current;
        if (reset || !Object.keys(draftRef.current).length)
          conflicted.current = false;
        else if (
          protectDraft &&
          currentDoc &&
          next.available &&
          (currentDoc.revision !== next.revision ||
            currentDoc.runtime_revision !== next.runtime_revision)
        )
          conflicted.current = true;
        setResponse(
          conflicted.current
            ? {
                ...next,
                editable: false,
                reason:
                  "文件或运行进程已变更，草稿已保留。请撤销草稿并刷新后重新核对。",
              }
            : next,
        );
        if (reset) {
          setDraft({});
          setIssues([]);
        }
      }
    } catch (e) {
      if (alive.current && seq === loadSeq.current) {
        setError(errorText(e));
        setResponse((old) =>
          old ? { ...old, editable: false, reason: errorText(e) } : old,
        );
      }
    } finally {
      if (alive.current && seq === loadSeq.current) setBusy(false);
    }
  }
  const refreshDocument = useEffectEvent(() => {
    if (!applying) void load(false, true);
  });
  useEffect(() => {
    if (active && online) refreshDocument();
  }, [active, online]);
  useEffect(() => {
    if (!active || statusLoaded.current) return;
    statusLoaded.current = true;
    void api<Operation>("GET", "/sub2api-config/operation")
      .then((value) => {
        if (alive.current) setOperation(value);
      })
      .catch((err: unknown) => {
        if (alive.current && !ignoredFailure(err)) setError(errorText(err));
      });
  }, [active]);
  useEffect(() => {
    if (!applying) return;
    let cancelled = false,
      inFlight = false,
      timer: ReturnType<typeof setTimeout> | undefined;
    const clearScheduled = () => {
      if (timer !== undefined) {
        clearTimeout(timer);
        timer = undefined;
      }
    };
    async function poll() {
      if (cancelled || inFlight) return;
      clearScheduled();
      inFlight = true;
      let shouldContinue = true;
      try {
        const value = await api<Operation>("GET", "/sub2api-config/operation");
        if (cancelled || !alive.current) return;
        if (!running(value) && uncertain.current) {
          shouldContinue = false;
          setOperation({
            status: "failed",
            message: `提交结果未确认；宿主机当前状态：${value.message || (value.status === "idle" ? "无操作记录" : value.status)}。草稿已保留。`,
          });
          uncertain.current = false;
          await load();
          return;
        }
        setOperation(value);
        setError("");
        if (!running(value)) {
          shouldContinue = false;
          if (value.status === "succeeded") await load(true);
          else await load();
          return;
        }
      } catch (e) {
        if (!cancelled && alive.current)
          setError(`${errorText(e)}；应用结果待确认，请保持此页或稍后查询状态`);
      } finally {
        inFlight = false;
      }
      if (
        shouldContinue &&
        !cancelled &&
        alive.current &&
        !globalThis.document.hidden
      ) {
        clearScheduled();
        timer = setTimeout(() => {
          timer = undefined;
          void poll();
        }, 1500);
      }
    }
    const onVisibility = () => {
      clearScheduled();
      if (!cancelled && !globalThis.document.hidden && !inFlight) void poll();
    };
    globalThis.document.addEventListener("visibilitychange", onVisibility);
    if (!globalThis.document.hidden) void poll();
    return () => {
      cancelled = true;
      clearScheduled();
      globalThis.document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [applying]);
  const fieldByPath = useMemo(
    () => new Map(document?.catalog.fields.map((f) => [f.path, f]) ?? []),
    [document],
  );
  const changes = Object.values(draft),
    blocked = busy || applying || !online || !document?.editable;
  const edit = useCallback(
    (field: ConfigField, value: unknown, explicit = false) => {
      const numeric = field.type === "integer" || field.type === "number";
      const structured = field.type === "array" || field.type === "object";
      setDraft((old) => {
        const next = { ...old },
          current = documentRef.current?.values[field.path];
        if (
          !explicit &&
          (current?.configured
            ? equal(current.yaml, value)
            : numeric && value === null && !field.nullable
              ? true
              : structured && !field.has_default && value === ""
                ? true
                : field.has_default && equal(field.default_value, value))
        )
          delete next[field.path];
        else next[field.path] = { path: field.path, remove: false, value };
        return next;
      });
      setIssues((old) => (old.length ? [] : old));
      setMessage("");
    },
    [],
  );
  function payload(resolvedChanges = changes) {
    return {
      expected_revision: document!.revision,
      runtime_revision: document!.runtime_revision,
      changes: resolvedChanges,
    };
  }
  async function validate(openConfirm: boolean) {
    if (blocked || !changes.length) return;
    const resolvedChanges: Change[] = [];
    const localIssues: Issue[] = [];
    let normalizedDraft: Record<string, Change> | null = null;
    for (const c of changes) {
      const field = fieldByPath.get(c.path)!;
      if (
        !c.remove &&
        (field.type === "array" || field.type === "object") &&
        typeof c.value === "string"
      ) {
        const current = document!.values[field.path];
        if (
          !current?.configured &&
          !field.has_default &&
          c.value.trim() === ""
        ) {
          normalizedDraft ??= { ...draft };
          delete normalizedDraft[field.path];
          continue;
        }
        const parsed = structuredValue(c.value, field.type);
        if (parsed.error) {
          localIssues.push({
            path: c.path,
            message: parsed.error || "需要有效的 YAML 结构",
          });
          resolvedChanges.push(c);
          continue;
        }
        normalizedDraft ??= { ...draft };
        if (
          current?.configured
            ? equal(current.yaml, parsed.value)
            : field.has_default && equal(field.default_value, parsed.value)
        ) {
          delete normalizedDraft[field.path];
        } else {
          const nextChange: Change = { ...c, value: parsed.value };
          normalizedDraft[field.path] = nextChange;
          resolvedChanges.push(nextChange);
        }
        continue;
      }
      if (
        !c.remove &&
        (field.type === "integer" || field.type === "number") &&
        (c.value === null
          ? !field.nullable
          : typeof c.value !== "number" ||
            !Number.isFinite(c.value) ||
            (field.type === "integer" && !Number.isInteger(c.value)))
      ) {
        localIssues.push({
          path: c.path,
          message:
            c.value === null
              ? "使用恢复默认移除配置或填写有效数值"
              : field.type === "integer"
                ? "需要有效整数"
                : "需要有效数值",
        });
      }
      resolvedChanges.push(c);
    }
    if (normalizedDraft) setDraft(normalizedDraft);
    if (localIssues.length) {
      setIssues(localIssues);
      locate(localIssues[0].path);
      return;
    }
    if (!resolvedChanges.length) return;
    setBusy(true);
    setError("");
    setMessage("");
    try {
      const result = await api<HostValidationReply>(
        "POST",
        "/sub2api-config/validate",
        payload(resolvedChanges),
      );
      if (!alive.current) return;
      setIssues(result.errors);
      if (!result.valid && result.errors[0]) locate(result.errors[0].path);
      if (result.valid) {
        setMessage("配置检查通过");
        if (openConfirm) setConfirm(true);
      }
    } catch (e) {
      if (alive.current) setError(errorText(e));
    } finally {
      if (alive.current) setBusy(false);
    }
  }
  async function apply() {
    if (blocked) return;
    setBusy(true);
    setError("");
    uncertain.current = false;
    try {
      const result = await api<Operation>(
        "POST",
        "/sub2api-config/apply",
        payload(),
      );
      if (alive.current) {
        setOperation(result);
        setConfirm(false);
        setMessage("");
      }
    } catch (e) {
      if (alive.current) {
        uncertain.current = true;
        setConfirm(false);
        setError(errorText(e));
        setOperation({
          status: "restarting",
          message: "提交结果待确认，正在查询操作状态",
        });
      }
    } finally {
      if (alive.current) setBusy(false);
    }
  }
  const { searchIndex, childrenByPath, splitGroups } = useMemo(() => {
    const index = new Map<string, string>();
    const children = new Map<string, ConfigField[]>();
    const groupTotals = new Map<string, number>();
    const catalogFields = document?.catalog.fields ?? [];
    for (const f of catalogFields) {
      index.set(f.path, `${f.path} ${f.description}`.toLowerCase());
      if (!f.internal) {
        children.set(f.path, EMPTY_CHILDREN);
        groupTotals.set(f.group, (groupTotals.get(f.group) ?? 0) + 1);
      }
    }
    for (const c of catalogFields) {
      if (!c.internal || c.status !== "internal") continue;
      for (const anc of pathAncestors(c.path)) {
        const list = children.get(anc);
        if (!list) continue;
        if (list === EMPTY_CHILDREN) children.set(anc, [c]);
        else list.push(c);
      }
    }
    const split = new Set<string>();
    for (const [gid, total] of groupTotals) {
      if (total >= 24) split.add(gid);
    }
    return { searchIndex: index, childrenByPath: children, splitGroups: split };
  }, [document]);
  const needle = query.trim().toLowerCase();
  const matches = useCallback(
    (field: ConfigField) =>
      (searchIndex.get(field.path) ?? "").includes(needle),
    [searchIndex, needle],
  );
  const configuredOnly = filter === "configured";
  const changedOnly = filter === "changed";
  const baseFields = useMemo(() => {
    if (!document) return [];
    return document.catalog.fields.filter((field) => {
      const value = document.values[field.path];
      return (
        !field.internal &&
        field.status === "supported" &&
        field.editable &&
        value?.editable &&
        value.source !== "environment" &&
        (!needle ||
          matches(field) ||
          childrenByPath.get(field.path)?.some(matches)) &&
        (!configuredOnly || value?.configured)
      );
    });
  }, [document, needle, matches, childrenByPath, configuredOnly]);
  const fields = useMemo(
    () =>
      changedOnly ? baseFields.filter((f) => !!draft[f.path]) : baseFields,
    [baseFields, changedOnly, draft],
  );
  const issuesByPath = useMemo(() => {
    if (!issues.length || !document) return new Map<string, Issue[]>();
    const map = new Map<string, Issue[]>();
    for (const issue of issues) {
      if (!issue.path) continue;
      for (const anc of pathAncestors(issue.path)) {
        const target = fieldByPath.get(anc);
        if (!target || target.internal) continue;
        const list = map.get(anc);
        if (list) list.push(issue);
        else map.set(anc, [issue]);
      }
    }
    return map;
  }, [issues, document, fieldByPath]);
  const { groupCounts, fieldsByGroup } = useMemo(() => {
    const counts = new Map<string, number>();
    const grouped = new Map<
      string,
      { list: ConfigField[]; sections: Map<string, ConfigField[]> }
    >();
    for (const field of fields) {
      counts.set(field.group, (counts.get(field.group) ?? 0) + 1);
      let bucket = grouped.get(field.group);
      if (!bucket) {
        bucket = { list: [], sections: new Map() };
        grouped.set(field.group, bucket);
      }
      bucket.list.push(field);
      const secKey = splitGroups.has(field.group)
        ? subsection(field)
        : field.group;
      const secList = bucket.sections.get(secKey);
      if (secList) secList.push(field);
      else bucket.sections.set(secKey, [field]);
    }
    return { groupCounts: counts, fieldsByGroup: grouped };
  }, [fields, splitGroups]);
  const groups = useMemo(
    () =>
      document
        ? document.catalog.groups.filter(
            (g) => (groupCounts.get(g.id) ?? 0) > 0,
          )
        : [],
    [document, groupCounts],
  );
  const aggregate = !!needle || filter !== "all";
  const selectedGroup = aggregate
    ? groups.some((g) => g.id === scopeGroup)
      ? scopeGroup
      : ""
    : groups.some((g) => g.id === group)
      ? group
      : (groups[0]?.id ?? "");
  const shownGroups = groups.filter(
    (g) => !selectedGroup || g.id === selectedGroup,
  );
  const count = (id: string) => groupCounts.get(id) ?? 0;
  const shownCount = selectedGroup ? count(selectedGroup) : fields.length;
  function chooseGroup(id: string) {
    if (aggregate) setScopeGroup(id);
    else setGroup(id);
  }
  function search(value: string) {
    setQuery(value);
    setScopeGroup("");
  }
  function chooseFilter(value: string) {
    setFilter(value);
    setScopeGroup("");
  }
  const findPublicOwner = useCallback(
    (path: string): ConfigField | undefined => {
      for (const anc of pathAncestors(path)) {
        const target = fieldByPath.get(anc);
        if (target && !target.internal) return target;
      }
      return undefined;
    },
    [fieldByPath],
  );
  function locate(path: string) {
    const field = findPublicOwner(path);
    if (!field) return;
    setQuery(field.path);
    setFilter("all");
    setScopeGroup("");
    setFocusTarget({ path });
  }
  useEffect(() => {
    if (fieldsRef.current) fieldsRef.current.scrollTop = 0;
    const workspace = rootRef.current?.querySelector(".config-workspace");
    if (workspace) workspace.scrollTop = 0;
  }, [query, filter, selectedGroup]);
  useEffect(() => {
    if (!focusTarget) return;
    const field =
      findPublicOwner(focusTarget.path) ??
      document?.catalog.fields.find(
        (f) => !f.internal && containsPath(f.path, focusTarget.path),
      );
    const input =
      field &&
      rootRef.current?.ownerDocument.getElementById(`config-${field.path}`);
    input?.focus({ preventScroll: true });
    input?.scrollIntoView?.({ block: "nearest" });
  }, [focusTarget, document?.catalog.fields, findPublicOwner]);
  const undo = useCallback((path: string) => {
    setDraft((old) => {
      const next = { ...old };
      delete next[path];
      return next;
    });
    setIssues((old) =>
      old.some((issue) => containsPath(path, issue.path))
        ? old.filter((issue) => !containsPath(path, issue.path))
        : old,
    );
    setMessage("");
  }, []);
  const remove = useCallback(
    (field: ConfigField) => {
      if (documentRef.current?.values[field.path]?.configured)
        setDraft((old) => ({
          ...old,
          [field.path]: { path: field.path, remove: true },
        }));
      else undo(field.path);
      setIssues((old) =>
        old.some((issue) => containsPath(field.path, issue.path))
          ? old.filter((issue) => !containsPath(field.path, issue.path))
          : old,
      );
      setMessage("");
    },
    [undo],
  );
  function scrollToSection(id: string) {
    rootRef.current?.ownerDocument
      .getElementById(`config-section-${id}`)
      ?.scrollIntoView({ block: "start" });
  }
  return (
    <section ref={rootRef} className="sub2api-config" aria-label="Sub2API 配置">
      <div className="config-workspace">
        <header className="config-heading">
          <div className="config-heading-main">
            <h2>Sub2API 配置</h2>
            {document && (
              <div className="config-runtime">
                <code>{document.commit.slice(0, 8)}</code>
                <span className={document.healthy ? "good-text" : "bad-text"}>
                  {document.healthy ? "服务健康" : "服务未就绪"}
                </span>
                <span>说明 {document.catalog.version}</span>
              </div>
            )}
          </div>
          <button
            type="button"
            className="config-refresh"
            disabled={busy || applying || !online || changes.length > 0}
            onClick={() => void load(true)}
          >
            <RefreshCw size={15} className={busy ? "spin" : ""} />
            <span>刷新</span>
          </button>
        </header>
        <Sub2ApiVersion
          active={active}
          online={online}
          connectionKey={connectionKey}
          knownVersion={document?.version}
          onRuntimeChanged={() => void load(false, true)}
        />
        {document ? (
          <>
            <div className="config-filters">
              <label className="config-search">
                <Search size={16} />
                <input
                  aria-label="搜索配置字段"
                  placeholder="搜索字段、路径或中文含义"
                  value={query}
                  onChange={(e) => search(e.target.value)}
                />
                {query && (
                  <button
                    type="button"
                    aria-label="清空配置搜索"
                    onClick={() => search("")}
                  >
                    <X size={15} />
                  </button>
                )}
              </label>
              <select
                className="config-mobile-group"
                aria-label="选择配置分组"
                value={selectedGroup}
                onChange={(e) => chooseGroup(e.target.value)}
              >
                {aggregate && (
                  <option value="">全部分组（{fields.length}）</option>
                )}
                {groups.map((g) => (
                  <option key={g.id} value={g.id}>
                    {g.label}（{count(g.id)}）
                  </option>
                ))}
                {!groups.length && <option value="">没有匹配分组</option>}
              </select>
              <select
                aria-label="配置字段筛选"
                value={filter}
                onChange={(e) => chooseFilter(e.target.value)}
              >
                <option value="all">全部字段</option>
                <option value="configured">已配置</option>
                <option value="changed">已修改</option>
              </select>
            </div>
            <div className="config-layout">
              <div
                ref={fieldsRef}
                className="config-fields"
                tabIndex={0}
                aria-label="配置字段列表"
              >
                {!online && (
                  <p role="alert" className="config-notice bad-text">
                    连接已断开，草稿已保留；重新连接后才能检查和应用。
                  </p>
                )}
                {(error || document.reason) && (
                  <p
                    role={error || !document.editable ? "alert" : "status"}
                    className={`config-notice${error || !document.editable ? " bad-text" : ""}`}
                  >
                    {error || document.reason}
                  </p>
                )}
                {operation && operation.status !== "idle" && (
                  <p
                    role="status"
                    className={`config-notice${["failed", "restore_failed", "restored"].includes(operation.status) ? " bad-text" : ""}`}
                  >
                    {operation.message || operation.status}
                  </p>
                )}
                {issues.length > 0 && (
                  <div
                    role="alert"
                    className="config-notice config-issues bad-text"
                  >
                    {issues.map((issue, index) => (
                      <p key={index}>
                        <button
                          type="button"
                          className="config-issue"
                          onClick={() => locate(issue.path)}
                        >
                          {issue.path || "配置"}
                        </button>
                        ：{issue.message}
                      </p>
                    ))}
                  </div>
                )}
                <div className="config-results">
                  <span>
                    {aggregate
                      ? `${shownCount} 项匹配`
                      : `${shownCount} 个字段`}
                  </span>
                  <details className="config-source-help">
                    <summary>配置值来源</summary>
                    <p>
                      显示值由
                      YAML、进程环境变量与版本默认值推导。环境变量覆盖项只读；后台业务设置仍可能覆盖部分参数。
                    </p>
                  </details>
                </div>
                {!fields.length && (
                  <div className="config-empty">
                    <p>没有匹配的配置字段</p>
                    <button
                      type="button"
                      onClick={() => {
                        search("");
                        chooseFilter("all");
                      }}
                    >
                      清除搜索与筛选
                    </button>
                  </div>
                )}
                {shownGroups.map((g) => {
                  const bucket = fieldsByGroup.get(g.id);
                  const groupFields = bucket?.list ?? EMPTY_CHILDREN;
                  const sections =
                    bucket?.sections ?? new Map<string, ConfigField[]>();
                  return (
                    <section
                      className="config-field-group"
                      aria-label={g.label}
                      key={g.id}
                    >
                      <header className="config-group-heading">
                        <h3>{g.label}</h3>
                        <span>{groupFields.length}</span>
                        {sections.size > 1 && (
                          <select
                            className="config-subsection-picker"
                            aria-label={`${g.label}子节`}
                            value=""
                            onChange={(e) => scrollToSection(e.target.value)}
                          >
                            <option value="" disabled>
                              跳转子节
                            </option>
                            {[...sections.keys()].map((id) => (
                              <option key={id} value={id}>
                                {id === g.id
                                  ? "基础参数"
                                  : id.slice(g.id.length + 1)}
                              </option>
                            ))}
                          </select>
                        )}
                      </header>
                      {[...sections].map(([id, sectionFields]) => (
                        <div
                          key={id}
                          id={`config-section-${id}`}
                          className="config-subsection"
                        >
                          {sections.size > 1 && id !== g.id && (
                            <h4>{id === g.id ? "基础参数" : id}</h4>
                          )}
                          {sectionFields.map((field) => (
                            <ConfigFieldRow
                              key={field.path}
                              field={field}
                              current={document.values[field.path]}
                              change={draft[field.path]}
                              children={
                                childrenByPath.get(field.path) ?? EMPTY_CHILDREN
                              }
                              expand={
                                (!!needle &&
                                  (childrenByPath
                                    .get(field.path)
                                    ?.some(matches) ??
                                    false)) ||
                                (!!focusTarget &&
                                  focusTarget.path !== field.path &&
                                  containsPath(field.path, focusTarget.path))
                              }
                              issues={
                                issuesByPath.get(field.path) ?? EMPTY_ISSUES
                              }
                              blocked={blocked}
                              locked={busy || applying}
                              onEdit={edit}
                              onRemove={remove}
                              onUndo={undo}
                            />
                          ))}
                        </div>
                      ))}
                    </section>
                  );
                })}
              </div>
            </div>
          </>
        ) : (
          <div className="config-empty">
            {error || response?.reason ? (
              <p role="alert" className="bad-text">
                {error || response?.reason}
              </p>
            ) : (
              <p>{busy ? "正在读取配置…" : "连接后读取配置"}</p>
            )}
            {operation && operation.status !== "idle" && (
              <p role="status">{operation.message || operation.status}</p>
            )}
          </div>
        )}
      </div>
      {document && (
        <footer className="config-footer">
          <div className="config-footer-state">
            {changes.length ? (
              <>
                <button
                  type="button"
                  className="config-change-count"
                  onClick={() => {
                    search("");
                    chooseFilter("changed");
                  }}
                >
                  {changes.length} 项修改
                </button>
                <button
                  type="button"
                  disabled={busy || applying}
                  onClick={() => {
                    setDraft({});
                    setIssues([]);
                    setMessage("");
                  }}
                >
                  撤销全部
                </button>
              </>
            ) : (
              <span>没有待应用的修改</span>
            )}
            <span
              className={issues.length ? "bad-text" : "good-text"}
              role="status"
            >
              {applying
                ? "正在应用…"
                : issues.length
                  ? `${issues.length} 项检查错误`
                  : message}
            </span>
          </div>
          <div className="config-footer-actions">
            <button
              type="button"
              disabled={blocked || !changes.length}
              onClick={() => void validate(false)}
            >
              检查配置
            </button>
            <button
              type="button"
              className="primary"
              disabled={blocked || !changes.length}
              onClick={() => void validate(true)}
            >
              应用并重启
            </button>
          </div>
        </footer>
      )}
      {confirm && document && (
        <ProxyDialog
          title="应用配置并重启"
          close={() => setConfirm(false)}
          busy={busy}
          wide
        >
          <p>
            Sub2API 会短暂中断服务；未恢复健康时将自动恢复上次配置并重启一次。
          </p>
          <ul className="config-diff">
            {changes.map((c) => {
              const field = fieldByPath.get(c.path)!;
              const current = document.values[c.path];
              return (
                <li key={c.path}>
                  <div className="config-diff-title">
                    <strong>
                      {c.remove
                        ? "恢复默认"
                        : current?.configured
                          ? "修改"
                          : "新增"}
                    </strong>
                    <code>{c.path}</code>
                  </div>
                  <dl>
                    <div>
                      <dt>当前</dt>
                      <dd>
                        {current?.configured
                          ? field.sensitive
                            ? "已遮蔽"
                            : display(current.yaml)
                          : field.has_default
                            ? `默认值：${field.default_text}`
                            : "未配置"}
                      </dd>
                    </div>
                    <div>
                      <dt>应用后</dt>
                      <dd>
                        {c.remove
                          ? `删除 YAML 项，${field.has_default ? `使用默认值：${field.default_text}` : "恢复为未配置"}`
                          : field.sensitive
                            ? "替换为新值（已遮蔽）"
                            : display(c.value)}
                      </dd>
                    </div>
                  </dl>
                </li>
              );
            })}
          </ul>
          <footer>
            <button
              type="button"
              disabled={busy}
              onClick={() => setConfirm(false)}
            >
              返回修改
            </button>
            <button
              type="button"
              className="primary"
              disabled={blocked}
              onClick={() => void apply()}
            >
              确认应用并重启
            </button>
          </footer>
        </ProxyDialog>
      )}
    </section>
  );
}
