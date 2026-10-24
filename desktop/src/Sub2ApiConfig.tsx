import { useEffect, useRef, useState } from "react";
import { Eye, EyeOff, RefreshCw, Search } from "lucide-react";
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

function FieldEditor({
  field,
  value,
  disabled,
  change,
}: {
  field: ConfigField;
  value: unknown;
  disabled: boolean;
  change: (value: unknown) => void;
}) {
  const [reveal, setReveal] = useState(false);
  const structured = field.type === "array" || field.type === "object";
  const [yaml, setYaml] = useState(() =>
    stringify(value ?? (field.type === "array" ? [] : {})),
  );
  const [error, setError] = useState("");
  const emitted = useRef<unknown>(value);
  useEffect(() => {
    if (structured && emitted.current !== value) {
      setYaml(stringify(value ?? (field.type === "array" ? [] : {})));
      setError("");
    }
  }, [value, structured, field.type]);
  const id = `config-${field.path}`;
  if (field.type === "boolean")
    return (
      <select
        id={id}
        aria-label={field.path}
        disabled={disabled}
        value={String(value ?? false)}
        onChange={(e) => change(e.target.value === "true")}
      >
        <option value="true">开启 · true</option>
        <option value="false">关闭 · false</option>
      </select>
    );
  if (field.choices)
    return (
      <select
        id={id}
        aria-label={field.path}
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
          disabled={disabled}
          rows={5}
          value={yaml}
          aria-invalid={!!error}
          onChange={(e) => {
            const text = e.target.value;
            setYaml(text);
            const document = parseDocument(text, { uniqueKeys: true });
            if (document.errors.length) {
              setError("YAML 语法无效，请检查缩进和重复字段");
              emitted.current = text;
              change(text);
              return;
            }
            let parsed: unknown;
            try {
              parsed = document.toJS({ maxAliasCount: 100 });
            } catch {
              setError("YAML 别名数量过多");
              emitted.current = text;
              change(text);
              return;
            }
            const valid =
              field.type === "array"
                ? Array.isArray(parsed)
                : !!parsed &&
                  typeof parsed === "object" &&
                  !Array.isArray(parsed);
            setError(
              valid
                ? ""
                : field.type === "array"
                  ? "需要 YAML 列表"
                  : "需要 YAML 映射",
            );
            emitted.current = parsed;
            change(parsed);
          }}
        />
        {error && (
          <small role="alert" className="bad-text">
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
        disabled={disabled}
        type={
          field.sensitive && !reveal ? "password" : numeric ? "number" : "text"
        }
        step={field.type === "integer" ? 1 : "any"}
        value={value == null ? "" : String(value)}
        autoComplete="off"
        spellCheck={false}
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
    [group, setGroup] = useState("server");
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
  function edit(field: ConfigField, value: unknown) {
    setDraft((old) => {
      const next = { ...old },
        current = document?.values[field.path];
      if (
        current?.configured &&
        JSON.stringify(current.yaml) === JSON.stringify(value)
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
  function shown(field: ConfigField) {
    const value = document!.values[field.path];
    return (
      !field.internal &&
      (!query ||
        `${field.path} ${field.description}`
          .toLowerCase()
          .includes(query.toLowerCase())) &&
      (filter === "all" ||
        (filter === "configured" && value?.configured) ||
        (filter === "changed" && !!draft[field.path]) ||
        (filter === "environment" && value?.source === "environment"))
    );
  }
  const fields = document?.available
    ? document.catalog.fields.filter(shown)
    : [];
  const groups = document?.available
    ? document.catalog.groups.filter((g) =>
        fields.some((f) => f.group === g.id),
      )
    : [];
  const selectedGroup = groups.some((g) => g.id === group)
    ? group
    : groups[0]?.id;
  return (
    <section className="sub2api-config" aria-label="Sub2API 配置">
      <header className="config-heading">
        <div>
          <h2>Sub2API 配置</h2>
          {document?.available && (
            <div className="config-runtime">
              <b>运行 {document.version}</b>
              <code>{document.commit.slice(0, 8)}</code>
              <span className={document.healthy ? "good-text" : "bad-text"}>
                {document.healthy ? "服务健康" : "服务未就绪"}
              </span>
              <span>配置说明 {document.catalog.version}</span>
            </div>
          )}
        </div>
        <button
          disabled={busy || applying || !online || changes.length > 0}
          onClick={() => void load(true)}
        >
          <RefreshCw size={15} />
          刷新
        </button>
      </header>
      {(error || document?.reason) && (
        <p role="alert" className="config-notice bad-text">
          {error || document?.reason}
        </p>
      )}
      {!document && !error && (
        <p className="config-notice">
          {busy ? "正在读取配置…" : "连接后读取配置"}
        </p>
      )}
      {operation && operation.status !== "idle" && (
        <div role="status" className="config-notice">
          <b>{operation.message || operation.status}</b>
        </div>
      )}
      {document?.available && (
        <>
          <p className="config-notice muted">
            配置值按
            YAML、进程环境变量与默认值推导；后台业务设置仍可能覆盖部分参数。
          </p>
          <div className="config-filters">
            <label className="config-search">
              <Search size={16} />
              <input
                aria-label="搜索配置字段"
                placeholder="搜索字段、路径或中文含义"
                value={query}
                onChange={(e) => setQuery(e.target.value)}
              />
            </label>
            <select
              aria-label="配置字段筛选"
              value={filter}
              onChange={(e) => setFilter(e.target.value)}
            >
              <option value="all">全部字段</option>
              <option value="configured">已配置</option>
              <option value="changed">已修改</option>
              <option value="environment">环境变量覆盖</option>
            </select>
          </div>
          <div className="config-layout">
            <nav className="config-groups" aria-label="配置分组">
              {groups.map((g) => (
                <button
                  key={g.id}
                  aria-current={g.id === selectedGroup ? "true" : undefined}
                  onClick={() => setGroup(g.id)}
                >
                  <span>{g.label}</span>
                  <small>{g.id}</small>
                </button>
              ))}
            </nav>
            <div className="config-fields">
              <select
                className="config-mobile-group"
                aria-label="选择配置分组"
                value={selectedGroup ?? ""}
                onChange={(e) => setGroup(e.target.value)}
              >
                {groups.map((g) => (
                  <option key={g.id} value={g.id}>
                    {g.label}
                  </option>
                ))}
              </select>
              {!fields.length && (
                <p className="config-notice">没有匹配的配置字段</p>
              )}
              {fields
                .filter((f) => f.group === selectedGroup)
                .map((f) => {
                  const current = document.values[f.path],
                    change = draft[f.path],
                    configured = change ? !change.remove : current?.configured;
                  const value = change
                    ? change.remove
                      ? f.default_value
                      : change.value
                    : current?.source === "environment"
                      ? current.effective
                      : current?.configured
                        ? current.yaml
                        : f.default_value;
                  const editable = !blocked && current?.editable;
                  const children = document.catalog.fields.filter(
                    (c) =>
                      c.internal &&
                      (c.path.startsWith(`${f.path}.`) ||
                        c.path.startsWith(`${f.path}[]`)),
                  );
                  return (
                    <article
                      className={`config-field${change ? " changed" : ""}`}
                      key={f.path}
                    >
                      <div className="config-field-title">
                        <label htmlFor={`config-${f.path}`}>
                          <code>{f.path}</code>
                        </label>
                        <span className="muted">{f.type_label}</span>
                        {current?.source === "environment" ? (
                          <span className="config-source">
                            环境变量覆盖 · 只读
                          </span>
                        ) : f.status === "reserved" ? (
                          <span className="config-source">保留字段</span>
                        ) : (
                          <span className="muted">
                            {configured ? "YAML" : "默认值"}
                          </span>
                        )}
                      </div>
                      <p className="config-explanation">{f.description}</p>
                      <FieldEditor
                        key={`${f.path}:${change?.remove ?? false}:${current?.configured}`}
                        field={f}
                        value={value}
                        disabled={
                          !editable ||
                          (!configured && !change) ||
                          current?.source === "environment"
                        }
                        change={(v) => edit(f, v)}
                      />
                      <dl className="config-field-meta">
                        <div>
                          <dt>默认值</dt>
                          <dd>{f.default_text}</dd>
                        </div>
                        <div>
                          <dt>允许范围</dt>
                          <dd>{f.range}</dd>
                        </div>
                        {current?.environment && (
                          <div>
                            <dt>覆盖来源</dt>
                            <dd>
                              <code>{current.environment}</code>
                              <span>
                                {" "}
                                · YAML：
                                {f.sensitive ? "已遮蔽" : display(current.yaml)}
                              </span>
                            </dd>
                          </div>
                        )}
                        <div>
                          <dt>配置来源</dt>
                          <dd>
                            <a
                              href={f.source_url}
                              target="_blank"
                              rel="noreferrer"
                            >
                              官方源码
                            </a>
                          </dd>
                        </div>
                      </dl>
                      {children.length > 0 && (
                        <details>
                          <summary>列表／映射内部字段说明</summary>
                          {children.map((c) => (
                            <div key={c.path} className="config-child">
                              <code>{c.path}</code>
                              <p>{c.description}</p>
                              <small>
                                {c.type_label} · {c.range}
                              </small>
                            </div>
                          ))}
                        </details>
                      )}
                      {issues
                        .filter((e) => e.path === f.path)
                        .map((e, i) => (
                          <p key={i} role="alert" className="bad-text">
                            {e.message}
                          </p>
                        ))}
                      <div className="config-field-actions">
                        {current?.editable && !configured && (
                          <button
                            disabled={blocked}
                            onClick={() =>
                              edit(
                                f,
                                f.has_default
                                  ? f.default_value
                                  : f.type === "array"
                                    ? []
                                    : f.type === "object"
                                      ? {}
                                      : f.type === "boolean"
                                        ? false
                                        : f.type === "integer" ||
                                            f.type === "number"
                                          ? 0
                                          : "",
                              )
                            }
                          >
                            添加参数
                          </button>
                        )}
                        {current?.editable && configured && (
                          <button
                            disabled={blocked}
                            onClick={() =>
                              setDraft((old) => ({
                                ...old,
                                [f.path]: { path: f.path, remove: true },
                              }))
                            }
                          >
                            恢复默认
                          </button>
                        )}
                        {change && (
                          <button
                            disabled={busy || applying}
                            onClick={() =>
                              setDraft((old) => {
                                const next = { ...old };
                                delete next[f.path];
                                return next;
                              })
                            }
                          >
                            撤销此项
                          </button>
                        )}
                      </div>
                    </article>
                  );
                })}
            </div>
          </div>
          {document.unknown.length > 0 && (
            <details className="config-notice">
              <summary>未识别或失效字段 · {document.unknown.length}</summary>
              {document.unknown.map((v) => (
                <p key={v.path}>
                  <code>{v.path}</code> · {v.message}
                </p>
              ))}
            </details>
          )}
          {issues.length > 0 && (
            <div role="alert" className="config-notice bad-text">
              {issues.map((e, i) => (
                <p key={i}>
                  <button
                    className="config-issue"
                    onClick={() => {
                      setQuery(e.path);
                      setFilter("all");
                      const f = document.catalog.fields.find(
                        (f) => f.path === e.path,
                      );
                      if (f) setGroup(f.group);
                    }}
                  >
                    {e.path || "配置"}
                  </button>
                  ：{e.message}
                </p>
              ))}
            </div>
          )}
          <footer className="config-footer">
            <span>
              {changes.length} 项修改{message && ` · ${message}`}
            </span>
            <div>
              <button
                disabled={!changes.length || busy || applying}
                onClick={() => {
                  setDraft({});
                  setIssues([]);
                  setMessage("");
                }}
              >
                撤销全部
              </button>
              <button
                disabled={blocked || !changes.length}
                onClick={() => void validate(false)}
              >
                检查配置
              </button>
              <button
                className="primary"
                disabled={blocked || !changes.length}
                onClick={() => void validate(true)}
              >
                应用并重启
              </button>
            </div>
          </footer>
        </>
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
              const f = document.catalog.fields.find((f) => f.path === c.path)!;
              return (
                <li key={c.path}>
                  <code>{c.path}</code>
                  <div>
                    <span>
                      {f.sensitive
                        ? "已遮蔽"
                        : display(document.values[c.path]?.yaml)}
                    </span>
                    <span>→</span>
                    <strong>
                      {c.remove
                        ? "删除 YAML 项，使用默认值"
                        : f.sensitive
                          ? "替换为新值（已遮蔽）"
                          : display(c.value)}
                    </strong>
                  </div>
                </li>
              );
            })}
          </ul>
          <footer>
            <button disabled={busy} onClick={() => setConfirm(false)}>
              返回修改
            </button>
            <button
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
