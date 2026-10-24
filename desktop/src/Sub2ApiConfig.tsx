import { useEffect, useMemo, useRef, useState } from "react";
import { Eye, EyeOff, RefreshCw, Search, X } from "lucide-react";
import { parseDocument, stringify } from "yaml";
import { api } from "./bridge";
import { ProxyDialog } from "./ProxyDialogs";
import "./sub2api-config.css";

export type ConfigField = {
  path: string;
  group: string;
  type: string;
  type_label: string;
  description: string;
  range: string;
  default_text: string;
  default_value: unknown;
  has_default: boolean;
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
export type ConfigDocument = {
  available: boolean;
  editable: boolean;
  reason: string;
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
  unknown: { path: string; message: string }[];
};
type Change = { path: string; remove: boolean; value?: unknown };
type Issue = { path: string; message: string };
type Operation = { status: string; message?: string };
const running = (operation: Operation | null) =>
  !!operation &&
  ["saving", "restarting", "restoring"].includes(operation.status);
const display = (value: unknown) =>
  value === undefined || value === null
    ? "未设置"
    : typeof value === "string"
      ? value || '""'
      : JSON.stringify(value);
const errorText = (e: unknown) => String(e).replace(/^Error: /, "");
const equal = (a: unknown, b: unknown) =>
  JSON.stringify(a) === JSON.stringify(b);
const containsPath = (parent: string, path: string) =>
  path === parent ||
  path.startsWith(`${parent}.`) ||
  path.startsWith(`${parent}[`);
const summary = (description: string) => {
  const first = description.split(/[。\n]/)[0];
  return first.length > 90 ? `${first.slice(0, 90)}…` : first;
};
const subsection = (field: ConfigField) =>
  field.path.split(".").length > 2
    ? field.path.split(".").slice(0, 2).join(".")
    : field.group;
const structuredText = (value: unknown) =>
  typeof value === "string" ? value : value == null ? "" : stringify(value);
function structuredValue(text: string, type: string) {
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
  disabled,
  change,
  invalid = false,
}: {
  field: ConfigField;
  value: unknown;
  disabled: boolean;
  change: (value: unknown) => void;
  invalid?: boolean;
}) {
  const [reveal, setReveal] = useState(false);
  const structured = field.type === "array" || field.type === "object";
  const [yaml, setYaml] = useState(() => structuredText(value));
  const [error, setError] = useState(() =>
    structured && typeof value === "string"
      ? structuredValue(value, field.type).error
      : "",
  );
  const emitted = useRef<unknown>(value);
  useEffect(() => {
    if (structured && emitted.current !== value) {
      setYaml(structuredText(value));
      setError(
        typeof value === "string"
          ? structuredValue(value, field.type).error
          : "",
      );
    }
  }, [value, structured, field.type]);
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
            setYaml(text);
            const result = structuredValue(text, field.type);
            setError(result.error);
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
  const numeric = field.type === "integer" || field.type === "number";
  return (
    <div className="config-input">
      <input
        id={id}
        aria-label={field.path}
        {...a11y}
        disabled={disabled}
        type={
          field.sensitive && !reveal ? "password" : numeric ? "number" : "text"
        }
        step={field.type === "integer" ? 1 : "any"}
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
        onChange={(e) =>
          change(
            numeric
              ? e.target.value === ""
                ? null
                : Number(e.target.value)
              : e.target.value,
          )
        }
      />
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

function ConfigFieldRow({
  field,
  current,
  change,
  children,
  issues,
  expand,
  blocked,
  locked,
  edit,
  remove,
  undo,
}: {
  field: ConfigField;
  current?: FieldValue;
  change?: Change;
  children: ConfigField[];
  issues: Issue[];
  expand: boolean;
  blocked: boolean;
  locked: boolean;
  edit: (value: unknown, explicit?: boolean) => void;
  remove: () => void;
  undo: () => void;
}) {
  const [details, setDetails] = useState(false);
  useEffect(() => {
    if (expand) setDetails(true);
  }, [expand]);
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
          disabled={blocked || !editable || overridden}
          invalid={issues.length > 0}
          change={edit}
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
              <button type="button" disabled={blocked} onClick={remove}>
                恢复默认
              </button>
            )}
            {change && (
              <button type="button" disabled={locked} onClick={undo}>
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
          完整说明{children.length ? `与内部字段（${children.length}）` : ""}
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
                onClick={() => edit(field.default_value, true)}
              >
                写入默认值
              </button>
            )}
          </div>
        </div>
      </details>
    </article>
  );
}

export default function Sub2ApiConfig({
  active,
  online,
}: {
  active: boolean;
  online: boolean;
}) {
  const [document, setDocument] = useState<ConfigDocument | null>(null),
    [draft, setDraft] = useState<Record<string, Change>>({});
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
    conflicted = useRef(false),
    statusLoaded = useRef(false),
    uncertain = useRef(false),
    applying = running(operation);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  async function load(reset = false, protectDraft = false) {
    setBusy(true);
    setError("");
    try {
      const next = await api<ConfigDocument>("GET", "/sub2api-config");
      if (alive.current) {
        if (reset || !Object.keys(draft).length) conflicted.current = false;
        else if (
          protectDraft &&
          document?.available &&
          next.available &&
          (document.revision !== next.revision ||
            document.runtime_revision !== next.runtime_revision)
        )
          conflicted.current = true;
        setDocument(
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
      if (alive.current) {
        setError(errorText(e));
        setDocument((old) =>
          old ? { ...old, editable: false, reason: errorText(e) } : old,
        );
      }
    } finally {
      if (alive.current) setBusy(false);
    }
  }
  useEffect(() => {
    if (active && online && !applying) void load(false, true);
  }, [active, online]);
  useEffect(() => {
    if (!active || statusLoaded.current) return;
    statusLoaded.current = true;
    void api<Operation>("GET", "/sub2api-config/operation")
      .then((value) => {
        if (alive.current) setOperation(value);
      })
      .catch(() => {});
  }, [active]);
  useEffect(() => {
    if (!applying) return;
    let cancelled = false,
      timer: ReturnType<typeof setTimeout>;
    async function poll() {
      try {
        const value = await api<Operation>("GET", "/sub2api-config/operation");
        if (cancelled || !alive.current) return;
        if (!running(value) && uncertain.current) {
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
          if (value.status === "succeeded") await load(true);
          else await load();
          return;
        }
      } catch (e) {
        if (!cancelled && alive.current)
          setError(`${errorText(e)}；应用结果待确认，请保持此页或稍后查询状态`);
      }
      if (!cancelled) timer = setTimeout(() => void poll(), 1500);
    }
    void poll();
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [applying]);
  const changes = Object.values(draft),
    blocked = busy || applying || !online || !document?.editable;
  function edit(field: ConfigField, value: unknown, explicit = false) {
    setDraft((old) => {
      const next = { ...old },
        current = document?.values[field.path];
      if (
        !explicit &&
        (current?.configured
          ? equal(current.yaml, value)
          : field.has_default && equal(field.default_value, value))
      )
        delete next[field.path];
      else next[field.path] = { path: field.path, remove: false, value };
      return next;
    });
    setIssues([]);
    setMessage("");
  }
  function payload() {
    return {
      expected_revision: document!.revision,
      runtime_revision: document!.runtime_revision,
      changes,
    };
  }
  async function validate(openConfirm: boolean) {
    if (blocked || !changes.length) return;
    const localIssues = changes.flatMap((c) => {
      const field = document!.catalog.fields.find((f) => f.path === c.path)!;
      if (
        !c.remove &&
        (field.type === "array" || field.type === "object") &&
        typeof c.value === "string"
      )
        return [
          {
            path: c.path,
            message:
              structuredValue(c.value, field.type).error ||
              "需要有效的 YAML 结构",
          },
        ];
      return [];
    });
    if (localIssues.length) {
      setIssues(localIssues);
      locate(localIssues[0].path);
      return;
    }
    setBusy(true);
    setError("");
    setMessage("");
    try {
      const result = await api<{ valid: boolean; errors: Issue[] }>(
        "POST",
        "/sub2api-config/validate",
        payload(),
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
  const childrenByPath = useMemo(() => {
    const fields = document?.catalog?.fields ?? [];
    return new Map(
      fields
        .filter((f) => !f.internal)
        .map((f) => [
          f.path,
          fields.filter((c) => c.internal && c.status === "internal" && containsPath(f.path, c.path)),
        ]),
    );
  }, [document]);
  const needle = query.trim().toLowerCase();
  const matches = (field: ConfigField) =>
    `${field.path} ${field.description}`.toLowerCase().includes(needle);
  const fields = document?.available
    ? document.catalog.fields.filter((field) => {
        const value = document.values[field.path];
        return (
          !field.internal && field.status === "supported" && field.editable &&
          value?.editable && value.source !== "environment" &&
          (!needle ||
            matches(field) ||
            childrenByPath.get(field.path)?.some(matches)) &&
          (filter === "all" ||
            (filter === "configured" && value?.configured) ||
            (filter === "changed" && !!draft[field.path]))
        );
      })
    : [];
  const groups = document?.available
    ? document.catalog.groups.filter((g) =>
        fields.some((f) => f.group === g.id),
      )
    : [];
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
  const count = (id: string) => fields.filter((f) => f.group === id).length;
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
  function locate(path: string) {
    const field = document?.catalog.fields.find(
      (f) => !f.internal && containsPath(f.path, path),
    );
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
    const field = document?.catalog.fields.find(
      (f) => !f.internal && containsPath(f.path, focusTarget.path),
    );
    const input =
      field &&
      rootRef.current?.ownerDocument.getElementById(`config-${field.path}`);
    input?.focus({ preventScroll: true });
    input?.scrollIntoView?.({ block: "nearest" });
  }, [focusTarget]);
  function undo(path: string) {
    setDraft((old) => {
      const next = { ...old };
      delete next[path];
      return next;
    });
    setIssues((old) => old.filter((issue) => !containsPath(path, issue.path)));
    setMessage("");
  }
  function remove(field: ConfigField) {
    if (document?.values[field.path]?.configured)
      setDraft((old) => ({
        ...old,
        [field.path]: { path: field.path, remove: true },
      }));
    else undo(field.path);
    setIssues((old) =>
      old.filter((issue) => !containsPath(field.path, issue.path)),
    );
    setMessage("");
  }
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
            {document?.available && (
              <div className="config-runtime">
                <b>运行 {document.version}</b>
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
        {document?.available ? (
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
              <nav className="config-groups" aria-label="配置分组">
                {aggregate && (
                  <button
                    type="button"
                    aria-current={!selectedGroup ? "true" : undefined}
                    onClick={() => chooseGroup("")}
                  >
                    <span>全部分组</span>
                    <small>{fields.length}</small>
                  </button>
                )}
                {groups.map((g) => (
                  <button
                    type="button"
                    key={g.id}
                    aria-label={g.label}
                    aria-current={g.id === selectedGroup ? "true" : undefined}
                    onClick={() => chooseGroup(g.id)}
                  >
                    <span>{g.label}</span>
                    <small>{count(g.id)}</small>
                  </button>
                ))}
                {!groups.length && (
                  <p className="config-notice muted">没有匹配分组</p>
                )}
              </nav>
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
                  <p role="alert" className="config-notice bad-text">
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
                  const groupFields = fields.filter((f) => f.group === g.id);
                  const split =
                    document.catalog.fields.filter(
                      (f) => !f.internal && f.group === g.id,
                    ).length >= 24;
                  const sections = new Map<string, ConfigField[]>();
                  for (const field of groupFields) {
                    const key = split ? subsection(field) : g.id;
                    sections.set(key, [...(sections.get(key) ?? []), field]);
                  }
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
                              children={childrenByPath.get(field.path) ?? []}
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
                              issues={issues.filter((issue) =>
                                containsPath(field.path, issue.path),
                              )}
                              blocked={blocked}
                              locked={busy || applying}
                              edit={(value, explicit) =>
                                edit(field, value, explicit)
                              }
                              remove={() => remove(field)}
                              undo={() => undo(field.path)}
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
            {error || document?.reason ? (
              <p role="alert" className="bad-text">
                {error || document?.reason}
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
      {document?.available && (
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
              const field = document.catalog.fields.find(
                (f) => f.path === c.path,
              )!;
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
