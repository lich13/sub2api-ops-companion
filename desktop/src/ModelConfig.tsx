import { useEffect, useMemo, useRef, useState } from "react";
import { Check, Download, RefreshCw, RotateCcw, Search, X } from "lucide-react";
import { api } from "./bridge";
import { fullTime } from "./types";

type Fields = Record<string, unknown>;
type Allowlist = { enabled: boolean; models: string[] };
type Group = {
  id: number;
  name: string;
  platform: string;
  version: string;
  model_allowlist: Allowlist;
};
type Catalog = { models: (Fields & { slug: string })[] };
type GroupConfig = {
  group: Group;
  revision: string;
  overrides: Record<string, Fields>;
  candidates: string[];
  baseline: Catalog;
  effective: Catalog;
  baseline_status: string;
  updated_at?: string;
  status: { state: string; message: string };
};
type Import = {
  provider: string;
  provider_name: string;
  id: string;
  name: string;
  fields: Fields;
};
type Preview = {
  effective: Catalog;
  pending_models: string[];
  baseline_status: string;
};
const efforts = [
  "none",
  "minimal",
  "low",
  "medium",
  "high",
  "xhigh",
  "max",
  "ultra",
];

export function mergeFields(base: Fields, patch: Fields): Fields {
  const output = structuredClone(base);
  for (const [key, value] of Object.entries(patch)) {
    // JSON objects retain unknown keys as data, including explicit null/false/0.
    const old = output[key];
    Object.defineProperty(output, key, {
      value:
        value &&
        typeof value === "object" &&
        !Array.isArray(value) &&
        old &&
        typeof old === "object" &&
        !Array.isArray(old)
          ? mergeFields(old as Fields, value as Fields)
          : structuredClone(value),
      writable: true,
      enumerable: true,
      configurable: true,
    });
  }
  return output;
}

export default function ModelConfig({ online }: { online: boolean }) {
  const [groups, setGroups] = useState<Group[]>([]);
  const [groupId, setGroupId] = useState(0);
  const [config, setConfig] = useState<GroupConfig>();
  const [draft, setDraft] = useState<Allowlist>({ enabled: false, models: [] });
  const [patches, setPatches] = useState<Record<string, Fields>>({});
  const [model, setModel] = useState("");
  const [tab, setTab] = useState("fields");
  const [query, setQuery] = useState("");
  const [jsonDrafts, setJsonDrafts] = useState<Record<string, string>>({});
  const [jsonErrors, setJsonErrors] = useState<Record<string, string>>({});
  const [result, setResult] = useState<Preview>();
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [busy, setBusy] = useState("");
  const [imports, setImports] = useState<Import[]>([]);
  const [importOpen, setImportOpen] = useState(false);
  const [importQuery, setImportQuery] = useState("");
  const generation = useRef(0);
  const previewSequence = useRef(0);
  const live = useRef(true);
  useEffect(() => {
    live.current = true;
    return () => {
      live.current = false;
      generation.current++;
    };
  }, []);

  useEffect(() => {
    let active = true;
    if (online)
      api<{ groups: Group[]; status: { message: string } }>(
        "GET",
        "/model-groups",
      )
        .then((data) => {
          if (!active) return;
          setGroups(data.groups);
          setGroupId((id) => id || data.groups[0]?.id || 0);
          if (data.status.message) setError(data.status.message);
        })
        .catch((e) => active && setError(String(e)));
    return () => {
      active = false;
    };
  }, [online]);

  async function load(id: number, preserve = false) {
    if (!id || !online) return;
    const seq = ++generation.current;
    setBusy("load");
    setError("");
    try {
      const next = await api<GroupConfig>("GET", `/model-groups/${id}`);
      if (!live.current || seq !== generation.current) return;
      setConfig(next);
      if (!preserve) {
        setDraft(next.group.model_allowlist);
        setPatches(next.overrides);
        setJsonDrafts({});
        setJsonErrors({});
        setModel(next.effective.models[0]?.slug || next.candidates[0] || "");
        setResult({
          effective: next.effective,
          pending_models: [],
          baseline_status: next.baseline_status,
        });
      }
    } catch (e) {
      if (seq === generation.current) setError(String(e));
    } finally {
      if (seq === generation.current) setBusy("");
    }
  }
  useEffect(() => {
    setConfig(undefined);
    setNotice("");
    void load(groupId);
  }, [groupId]); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    const seq = ++previewSequence.current;
    if (
      !online ||
      !config ||
      config.group.id !== groupId ||
      Object.values(jsonErrors).some(Boolean)
    )
      return;
    const timeout = setTimeout(() => {
      api<Preview>("POST", `/model-groups/${groupId}/preview`, {
        allowlist: draft,
        overrides: patches,
      })
        .then((data) => {
          if (live.current && seq === previewSequence.current) {
            setResult(data);
            setError("");
          }
        })
        .catch((e) => {
          if (live.current && seq === previewSequence.current)
            setError(String(e));
        });
    }, 300);
    return () => {
      clearTimeout(timeout);
      previewSequence.current++;
    };
  }, [draft, patches, config, groupId, online, jsonErrors]);

  const allModels = useMemo(
    () =>
      [
        ...new Set([
          ...(config?.candidates || []),
          ...(config?.baseline.models.map((m) => m.slug) || []),
          ...draft.models,
          ...Object.keys(patches),
        ]),
      ].sort(),
    [config, draft.models, patches],
  );
  const visible = allModels.filter((id) =>
    id.toLowerCase().includes(query.toLowerCase()),
  );
  const base = config?.baseline.models.find((m) => m.slug === model) || {};
  const fields = mergeFields(base, patches[model] || {});
  const selectedEfforts = Array.isArray(fields.supported_reasoning_levels)
    ? (fields.supported_reasoning_levels as { effort: string }[]).map(
        (v) => v.effort,
      )
    : [];
  const modalities = Array.isArray(fields.input_modalities)
    ? (fields.input_modalities as string[])
    : [];
  const whitelistDirty =
    !!config &&
    JSON.stringify(draft) !== JSON.stringify(config.group.model_allowlist);
  const metadataDirty =
    !!config && JSON.stringify(patches) !== JSON.stringify(config.overrides);
  const invalid = Object.values(jsonErrors).some(Boolean);
  const locked = !online || !!busy;

  function patch(change: Fields, replace = false) {
    setPatches((current) => ({
      ...current,
      [model]: replace ? change : mergeFields(current[model] || {}, change),
    }));
    setJsonDrafts((current) => {
      const next = { ...current };
      delete next[model];
      return next;
    });
    setJsonErrors((current) => ({ ...current, [model]: "" }));
    setNotice("");
  }
  async function save(kind: "allowlist" | "overrides") {
    if (!config || locked) return;
    setBusy(kind);
    setError("");
    setNotice("");
    const seq = generation.current;
    try {
      if (kind === "allowlist") {
        const saved = await api<{
          version: string;
          model_allowlist: Allowlist;
        }>("PUT", `/model-groups/${groupId}/allowlist`, {
          expected_version: config.group.version,
          allowlist: draft,
        });
        if (seq !== generation.current || !live.current) return;
        setConfig(
          (old) => old && { ...old, group: { ...old.group, ...saved } },
        );
        await load(groupId, true);
        setNotice("白名单已保存");
      } else {
        const saved = await api<{
          revision: string;
          overrides: Record<string, Fields>;
          updated_at: string;
        }>("PUT", `/model-groups/${groupId}/overrides`, {
          expected_version: config.group.version,
          expected_revision: config.revision,
          overrides: patches,
        });
        if (seq !== generation.current || !live.current) return;
        setConfig((old) => old && { ...old, ...saved });
        setNotice("模型信息已保存");
      }
    } catch (e) {
      if (live.current && seq === generation.current) setError(String(e));
    } finally {
      if (live.current) setBusy("");
    }
  }
  async function upstream() {
    setBusy("upstream");
    setError("");
    const seq = generation.current;
    const target = model;
    try {
      const imported = await api<{ fields: Fields }>(
        "POST",
        `/model-groups/${groupId}/upstream-import`,
        { model },
      );
      if (live.current && seq === generation.current && target === model) {
        patch(imported.fields);
        setNotice("已导入草稿");
      }
    } catch (e) {
      if (seq === generation.current) setError(String(e));
    } finally {
      if (live.current) setBusy("");
    }
  }
  async function openCatalog() {
    setImportOpen(true);
    if (imports.length) return;
    setBusy("catalog");
    setError("");
    try {
      const data = await api<{ items: Import[] }>("GET", "/model-catalog");
      if (live.current) setImports(data.items);
    } catch (e) {
      if (live.current) setError(String(e));
    } finally {
      if (live.current) setBusy("");
    }
  }

  return (
    <section className="models-workspace">
      <div className="models-toolbar">
        <select
          aria-label="模型分组"
          value={groupId || ""}
          disabled={!!busy}
          onChange={(e) => setGroupId(Number(e.target.value))}
        >
          {!groups.length && <option value="">暂无分组</option>}
          {groups.map((g) => (
            <option key={g.id} value={g.id}>
              {g.name} · {g.platform}
            </option>
          ))}
        </select>
        <button
          aria-label="刷新模型配置"
          disabled={locked}
          onClick={() => void load(groupId, whitelistDirty || metadataDirty)}
        >
          <RefreshCw size={14} className={busy === "load" ? "spin" : ""} />
        </button>
        <span className="models-feedback" role="status">
          {notice && (
            <>
              <Check size={13} />
              {notice}
            </>
          )}
        </span>
        {config?.updated_at && <time>{fullTime(config.updated_at)}</time>}
      </div>
      {error && (
        <div className="models-error" role="alert">
          {error}
        </div>
      )}
      {config?.status.message && (
        <div className="models-error" role="status">
          {config.status.message}
        </div>
      )}
      {config?.baseline_status === "unavailable" && (
        <div className="models-error">原始目录暂不可读取</div>
      )}
      {!config ? (
        <div className="empty">{busy ? "正在读取分组模型" : "请选择分组"}</div>
      ) : (
        <div className="models-columns">
          <aside className="models-list">
            <div className="models-list-head">
              <label>
                <input
                  type="checkbox"
                  checked={draft.enabled}
                  onChange={(e) =>
                    setDraft((d) => ({ ...d, enabled: e.target.checked }))
                  }
                />
                白名单
              </label>
              <button
                disabled={locked || !whitelistDirty}
                onClick={() => void save("allowlist")}
              >
                保存白名单
              </button>
            </div>
            <label className="models-search">
              <Search size={14} />
              <input
                aria-label="搜索模型"
                placeholder="搜索模型"
                value={query}
                onChange={(e) => setQuery(e.target.value)}
              />
            </label>
            <div className="models-options">
              {visible.map((id) => (
                <div
                  className={`model-option ${id === model ? "selected" : ""}`}
                  key={id}
                >
                  <input
                    type="checkbox"
                    aria-label={`允许 ${id}`}
                    checked={draft.models.includes(id)}
                    onChange={(e) =>
                      setDraft((d) => ({
                        ...d,
                        models: e.target.checked
                          ? [...d.models, id]
                          : d.models.filter((v) => v !== id),
                      }))
                    }
                  />
                  <button
                    title={id}
                    disabled={!!busy}
                    onClick={() => setModel(id)}
                  >
                    <span>{id}</span>
                    {patches[id] && Object.keys(patches[id]).length > 0 && (
                      <i aria-label="已覆盖" />
                    )}
                  </button>
                </div>
              ))}
              {!visible.length && <div className="empty">没有匹配的模型</div>}
            </div>
            <form
              className="models-add"
              onSubmit={(e) => {
                e.preventDefault();
                const id = query.trim();
                if (id && !draft.models.includes(id))
                  setDraft((d) => ({ ...d, models: [...d.models, id] }));
                setModel(id);
                setQuery("");
              }}
            >
              <button
                disabled={!query.trim() || allModels.includes(query.trim())}
              >
                添加“{query.trim() || "模型 ID"}”
              </button>
            </form>
          </aside>
          <div className="model-editor">
            {!model ? (
              <div className="empty">选择一个模型</div>
            ) : (
              <>
                <div className="model-editor-head">
                  <strong title={model}>{model}</strong>
                  <button disabled={locked} onClick={() => void upstream()}>
                    <Download size={13} />
                    上游导入
                  </button>
                  <button disabled={locked} onClick={() => void openCatalog()}>
                    models.dev
                  </button>
                </div>
                <div className="model-tabs">
                  {[
                    ["fields", "模型信息"],
                    ["json", "完整 JSON"],
                    ["preview", "最终预览"],
                  ].map(([id, label]) => (
                    <button
                      key={id}
                      className={tab === id ? "active" : ""}
                      onClick={() => setTab(id)}
                    >
                      {label}
                    </button>
                  ))}
                  <button
                    className="inherit"
                    onClick={() => {
                      setPatches((current) => {
                        const next = { ...current };
                        delete next[model];
                        return next;
                      });
                      setJsonDrafts((current) => {
                        const next = { ...current };
                        delete next[model];
                        return next;
                      });
                      setJsonErrors((current) => ({ ...current, [model]: "" }));
                    }}
                  >
                    <RotateCcw size={12} />
                    恢复继承
                  </button>
                </div>
                {tab === "fields" && (
                  <div className="model-fields">
                    <label>
                      名称
                      <input
                        value={String(fields.display_name ?? "")}
                        onChange={(e) =>
                          patch({ display_name: e.target.value })
                        }
                      />
                    </label>
                    <label>
                      说明
                      <textarea
                        rows={2}
                        value={String(fields.description ?? "")}
                        onChange={(e) => patch({ description: e.target.value })}
                      />
                    </label>
                    <div className="model-field-pair">
                      {[
                        ["context_window", "上下文窗口"],
                        ["max_context_window", "最大上下文"],
                      ].map(([key, label]) => (
                        <label key={key}>
                          {label}
                          <input
                            type="number"
                            min={1}
                            value={
                              typeof fields[key] === "number"
                                ? (fields[key] as number)
                                : ""
                            }
                            onChange={(e) =>
                              patch({
                                [key]: e.target.value
                                  ? Number(e.target.value)
                                  : null,
                              })
                            }
                          />
                        </label>
                      ))}
                    </div>
                    <fieldset>
                      <legend>推理等级</legend>
                      <div className="model-choices">
                        {[...new Set([...efforts, ...selectedEfforts])].map(
                          (effort) => (
                            <label key={effort}>
                              <input
                                type="checkbox"
                                checked={selectedEfforts.includes(effort)}
                                onChange={(e) => {
                                  const values = e.target.checked
                                    ? [...selectedEfforts, effort]
                                    : selectedEfforts.filter(
                                        (v) => v !== effort,
                                      );
                                  const levels = values.map((v) => ({
                                    effort: v,
                                    description:
                                      (
                                        (fields.supported_reasoning_levels as {
                                          effort: string;
                                          description?: string;
                                        }[]) || []
                                      ).find((r) => r.effort === v)
                                        ?.description || "",
                                  }));
                                  patch({
                                    supported_reasoning_levels: levels,
                                    default_reasoning_level: values.includes(
                                      String(fields.default_reasoning_level),
                                    )
                                      ? fields.default_reasoning_level
                                      : (values[0] ?? null),
                                  });
                                }}
                              />
                              {effort}
                            </label>
                          ),
                        )}
                      </div>
                    </fieldset>
                    <label>
                      默认推理等级
                      <select
                        value={String(fields.default_reasoning_level ?? "")}
                        onChange={(e) =>
                          patch({
                            default_reasoning_level: e.target.value || null,
                          })
                        }
                      >
                        <option value="">无</option>
                        {selectedEfforts.map((value) => (
                          <option key={value}>{value}</option>
                        ))}
                      </select>
                    </label>
                    <fieldset>
                      <legend>输入类型</legend>
                      <div className="model-choices">
                        {["text", "image"].map((value) => (
                          <label key={value}>
                            <input
                              type="checkbox"
                              checked={modalities.includes(value)}
                              disabled={
                                modalities.length === 1 &&
                                modalities[0] === value
                              }
                              onChange={(e) =>
                                patch({
                                  input_modalities: e.target.checked
                                    ? [...modalities, value]
                                    : modalities.filter((v) => v !== value),
                                })
                              }
                            />
                            {value}
                          </label>
                        ))}
                      </div>
                    </fieldset>
                  </div>
                )}
                {tab === "json" && (
                  <>
                    <textarea
                      className="model-json"
                      aria-label="模型 JSON"
                      spellCheck={false}
                      value={
                        jsonDrafts[model] ??
                        JSON.stringify(
                          Object.fromEntries(
                            Object.entries(fields).filter(
                              ([k]) => k !== "slug" && k !== "id",
                            ),
                          ),
                          null,
                          2,
                        )
                      }
                      onChange={(e) => {
                        const value = e.target.value;
                        setJsonDrafts((current) => ({
                          ...current,
                          [model]: value,
                        }));
                        try {
                          const parsed = JSON.parse(value);
                          if (
                            !parsed ||
                            Array.isArray(parsed) ||
                            typeof parsed !== "object" ||
                            "id" in parsed ||
                            "slug" in parsed
                          )
                            throw Error(
                              "模型 JSON 必须为对象且不能修改 id / slug",
                            );
                          setPatches((current) => ({
                            ...current,
                            [model]: parsed,
                          }));
                          setJsonErrors((current) => ({
                            ...current,
                            [model]: "",
                          }));
                        } catch (error) {
                          setJsonErrors((current) => ({
                            ...current,
                            [model]: String(error),
                          }));
                        }
                      }}
                    />
                    {jsonErrors[model] && (
                      <div className="models-error" role="alert">
                        {jsonErrors[model]}
                      </div>
                    )}
                  </>
                )}
                {tab === "preview" && (
                  <>
                    {result?.pending_models.includes(model) ? (
                      <div className="models-error">
                        该模型当前未出现在原始目录；保存白名单后重新读取
                      </div>
                    ) : (
                      <pre className="model-json">
                        {JSON.stringify(
                          result?.effective.models.find(
                            (m) => m.slug === model,
                          ) ?? { status: "不在当前白名单" },
                          null,
                          2,
                        )}
                      </pre>
                    )}
                  </>
                )}
                <div className="model-editor-footer">
                  <span>
                    {
                      Object.keys(patches).filter(
                        (id) => Object.keys(patches[id]).length,
                      ).length
                    }{" "}
                    个模型覆盖
                  </span>
                  <button
                    className="primary"
                    disabled={
                      locked || !metadataDirty || invalid || whitelistDirty
                    }
                    onClick={() => void save("overrides")}
                  >
                    保存模型信息
                  </button>
                </div>
              </>
            )}
          </div>
        </div>
      )}
      {importOpen && (
        <div className="modal-backdrop">
          <div
            className="model-import"
            role="dialog"
            aria-label="models.dev 目录"
          >
            <header>
              <strong>models.dev</strong>
              <button
                aria-label="关闭目录"
                onClick={() => setImportOpen(false)}
              >
                <X size={16} />
              </button>
            </header>
            <input
              autoFocus
              aria-label="搜索目录"
              placeholder="供应商 / 模型"
              value={importQuery}
              onChange={(e) => setImportQuery(e.target.value)}
            />
            <div className="model-import-results">
              {imports
                .filter((item) =>
                  `${item.provider} ${item.provider_name} ${item.id} ${item.name}`
                    .toLowerCase()
                    .includes(importQuery.toLowerCase()),
                )
                .slice(0, 100)
                .map((item) => (
                  <button
                    key={`${item.provider}/${item.id}`}
                    onClick={() => {
                      patch(item.fields);
                      setImportOpen(false);
                      setNotice("已导入草稿");
                    }}
                  >
                    <span>
                      <strong>{item.name}</strong>
                      <small>
                        {item.provider_name} · {item.id}
                      </small>
                    </span>
                    <span>
                      {String(item.fields.context_window ?? "—")}
                      <small>
                        {(
                          (item.fields.supported_reasoning_levels as {
                            effort: string;
                          }[]) || []
                        )
                          .map((v) => v.effort)
                          .join(" / ") || "—"}
                      </small>
                    </span>
                  </button>
                ))}
              {busy === "catalog" && <div className="empty">正在读取目录</div>}
            </div>
          </div>
        </div>
      )}
    </section>
  );
}
