import { useEffect, useRef, useState } from "react";
import { Plus, RefreshCw, X } from "lucide-react";
import { api } from "./bridge";
import { fullTime } from "./types";
import { useBackAction } from "./mobile";

type Group = { id: number; name: string; platform: string; version: string };
type Item = {
  model: string;
  efforts: string[];
  default_effort: string;
  state: string;
  reason: string;
  source: string;
  updated_at?: string;
};
type Config = {
  group: Group;
  revision: string;
  items: Item[];
  status: { state: string; message: string };
};
type Resolution = {
  group: Group;
  revision: string;
  model: string;
  binding: string;
  efforts: string[];
  default_effort: string;
  source: string;
  needs_allowlist: boolean;
  descriptor_available: boolean;
  native_efforts: string[];
  native_default: string;
  forwarding: { state: string; reason: string; version: string };
};
type Draft = {
  model: string;
  efforts: string[];
  default_effort: string;
  editing: boolean;
  loaded: boolean;
};
const EFFORTS = [
  "none",
  "minimal",
  "low",
  "medium",
  "high",
  "xhigh",
  "max",
  "ultra",
];
const stateLabels: Record<string, string> = {
  active: "目录已补全",
  native: "原生已支持",
  limited: "转发受限",
  unverified: "转发未核实",
  draft: "待保存",
};
const sourceLabels: Record<string, string> = {
  upstream: "真实上游",
  native: "Sub2API 原生目录",
  manual: "手动填写",
  legacy: "旧配置待核对",
};
const signature = (model: string, efforts: string[], value: string) =>
  JSON.stringify([model, [...efforts].sort(), value]);

export default function ModelConfig({ online }: { online: boolean }) {
  const [groups, setGroups] = useState<Group[]>([]);
  const [groupId, setGroupId] = useState(0);
  const [config, setConfig] = useState<Config>();
  const [draft, setDraft] = useState<Draft>();
  const [resolution, setResolution] = useState<Resolution>();
  const [checked, setChecked] = useState("");
  const [confirm, setConfirm] = useState(false);
  const [error, setError] = useState("");
  const [formError, setFormError] = useState("");
  const [notice, setNotice] = useState("");
  const [busy, setBusy] = useState("");
  const [resolving, setResolving] = useState(false);
  const [checkRevision, setCheckRevision] = useState(0);
  const alive = useRef(true);
  const loadSequence = useRef(0);
  const resolveSequence = useRef(0);
  const modal = useRef<HTMLDivElement>(null);
  const focusReturn = useRef<HTMLElement | null>(null);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
      loadSequence.current++;
      resolveSequence.current++;
    };
  }, []);
  useEffect(() => {
    let active = true;
    if (online)
      api<{ groups: Group[] }>("GET", "/model-groups")
        .then((data) => {
          if (!active) return;
          setGroups(data.groups);
          setGroupId((id) =>
            data.groups.some((g) => g.id === id) ? id : data.groups[0]?.id || 0,
          );
        })
        .catch((e) => active && setError(String(e)));
    return () => {
      active = false;
    };
  }, [online]);

  async function load(id: number) {
    if (!id || !online) return;
    const seq = ++loadSequence.current;
    setBusy("load");
    setError("");
    try {
      const next = await api<Config>("GET", `/model-groups/${id}/reasoning`);
      if (alive.current && seq === loadSequence.current) setConfig(next);
    } catch (e) {
      if (alive.current && seq === loadSequence.current) setError(String(e));
    } finally {
      if (alive.current && seq === loadSequence.current) setBusy("");
    }
  }
  useEffect(() => {
    setConfig(undefined);
    setNotice("");
    void load(groupId);
  }, [groupId, online]); // eslint-disable-line react-hooks/exhaustive-deps

  const selectedKey = draft
    ? signature(draft.model, draft.efforts, draft.default_effort)
    : "";
  useEffect(() => {
    const seq = ++resolveSequence.current;
    setConfirm(false);
    if (!draft || !online || !draft.model || /\s|\*/.test(draft.model)) {
      setResolution(undefined);
      setChecked("");
      setResolving(false);
      return;
    }
    if (
      draft.loaded &&
      (!draft.efforts.length || !draft.efforts.includes(draft.default_effort))
    ) {
      setChecked("");
      setResolving(false);
      return;
    }
    setResolving(true);
    const timer = setTimeout(() => {
      const payload = draft.loaded
        ? {
            model: draft.model,
            efforts: draft.efforts,
            default_effort: draft.default_effort,
          }
        : { model: draft.model };
      api<Resolution>(
        "POST",
        `/model-groups/${groupId}/reasoning/resolve`,
        payload,
      )
        .then((data) => {
          if (!alive.current || seq !== resolveSequence.current) return;
          setResolution(data);
          setChecked(signature(data.model, data.efforts, data.default_effort));
          if (!draft.loaded)
            setDraft(
              (current) =>
                current && {
                  ...current,
                  efforts: data.efforts,
                  default_effort: data.default_effort,
                  loaded: true,
                },
            );
          setFormError("");
        })
        .catch((e) => {
          if (alive.current && seq === resolveSequence.current) {
            setFormError(String(e));
            setChecked("");
          }
        })
        .finally(() => {
          if (alive.current && seq === resolveSequence.current)
            setResolving(false);
        });
    }, 350);
    return () => {
      clearTimeout(timer);
      resolveSequence.current++;
    };
  }, [selectedKey, groupId, online, checkRevision]); // eslint-disable-line react-hooks/exhaustive-deps

  const isOpen = !!draft;
  useEffect(() => {
    if (!isOpen) return;
    focusReturn.current = document.activeElement as HTMLElement;
    const first = modal.current?.querySelector<HTMLElement>("input,button");
    first?.focus();
    return () => focusReturn.current?.focus();
  }, [isOpen]);

  function open(item?: Item) {
    setFormError("");
    setNotice("");
    setResolution(undefined);
    setChecked("");
    setConfirm(false);
    setDraft({
      model: item?.model || "",
      efforts: item?.efforts || [],
      default_effort: item?.default_effort || "",
      editing: !!item,
      loaded: !!item,
    });
  }
  function close() {
    if (busy !== "save") {
      setDraft(undefined);
      setFormError("");
    }
  }
  useBackAction(!!draft, close);
  function changeEffort(effort: string) {
    if (!draft) return;
    const next = draft.efforts.includes(effort)
      ? draft.efforts.filter((e) => e !== effort)
      : EFFORTS.filter((e) => draft.efforts.includes(e) || e === effort);
    setDraft({
      ...draft,
      loaded: true,
      efforts: next,
      default_effort: next.includes(draft.default_effort)
        ? draft.default_effort
        : "",
    });
  }
  const valid =
    !!draft?.efforts.length && draft.efforts.includes(draft.default_effort);
  const verified =
    resolution?.forwarding.state === "verified" &&
    resolution.descriptor_available;
  const native =
    verified &&
    resolution?.native_default === draft?.default_effort &&
    [...(resolution?.native_efforts || [])].sort().join() ===
      [...(draft?.efforts || [])].sort().join() &&
    !resolution?.needs_allowlist;
  const needsAllowlist = verified && resolution?.needs_allowlist && !native;
  const ready =
    online && valid && checked === selectedKey && !resolving && !busy;

  async function save() {
    if (!draft || !resolution || !ready || (needsAllowlist && !confirm)) return;
    setBusy("save");
    setFormError("");
    try {
      const result = await api<{
        outcome: string;
        message: string;
        state: Config;
      }>("PUT", `/model-groups/${groupId}/reasoning`, {
        model: draft.model,
        efforts: draft.efforts,
        default_effort: draft.default_effort,
        expected_version: resolution.group.version,
        expected_revision: resolution.revision,
        expected_binding: resolution.binding,
        confirm_allowlist: confirm,
      });
      if (!alive.current) return;
      setConfig(result.state);
      if (result.outcome === "partial") {
        setFormError(result.message);
        setChecked("");
      } else {
        setDraft(undefined);
        setNotice(result.message);
      }
    } catch (e) {
      if (alive.current) {
        setFormError(String(e));
        setChecked("");
      }
    } finally {
      if (alive.current) setBusy("");
    }
  }
  async function remove(item: Item) {
    if (!config || !online || busy) return;
    setBusy(item.model);
    setError("");
    setNotice("");
    try {
      const next = await api<Config>(
        "DELETE",
        `/model-groups/${groupId}/reasoning`,
        {
          model: item.model,
          expected_version: config.group.version,
          expected_revision: config.revision,
        },
      );
      if (alive.current) {
        setConfig(next);
        setNotice("已恢复原生");
      }
    } catch (e) {
      if (alive.current) setError(String(e));
    } finally {
      if (alive.current) setBusy("");
    }
  }
  const status = native
    ? "原生已支持"
    : resolution &&
      (resolution.forwarding.state !== "verified"
        ? stateLabels[resolution.forwarding.state]
        : resolution.descriptor_available
          ? "可补全目录"
          : "转发未核实");
  return (
    <section className="reasoning-workspace" aria-label="模型思考档位">
      <div className="reasoning-toolbar">
        <select
          aria-label="分组"
          value={groupId}
          disabled={!!draft || !!busy || !online}
          onChange={(e) => setGroupId(Number(e.target.value))}
        >
          {!groups.length && <option value={0}>暂无分组</option>}
          {groups.map((g) => (
            <option key={g.id} value={g.id}>
              {g.name}
            </option>
          ))}
        </select>
        <button
          aria-label="刷新模型"
          title="刷新"
          disabled={!!busy || !!draft || !online || !groupId}
          onClick={() => void load(groupId)}
        >
          <RefreshCw size={14} />
        </button>
        <span className="reasoning-notice" role="status">
          {notice}
        </span>
        <button
          className="primary"
          disabled={!online || !config || !!busy}
          onClick={() => open()}
        >
          <Plus size={14} />
          添加模型
        </button>
      </div>
      {(error || config?.status.message) && (
        <div className="models-error" role="alert">
          {error || config?.status.message}
        </div>
      )}
      <div className="reasoning-list">
        <div className="reasoning-row reasoning-head">
          <span>模型 ID</span>
          <span>思考档位</span>
          <span>默认</span>
          <span>状态</span>
          <span />
        </div>
        {config?.items.map((item) => (
          <div className="reasoning-row" key={item.model}>
            <strong className="reasoning-id" title={item.model}>
              {item.model}
            </strong>
            <div className="reasoning-levels">
              {item.efforts.map((e) => (
                <span key={e}>{e}</span>
              ))}
            </div>
            <span className="reasoning-default"><span className="mobile-field-label">默认</span>{item.default_effort}</span>
            <span
              className={`reasoning-state ${item.state}`}
              title={`${item.reason}${item.updated_at ? "\n" + fullTime(item.updated_at) : ""}`}
            >
              {stateLabels[item.state] || "转发未核实"}
            </span>
            <div className="reasoning-actions">
              <button disabled={!online || !!busy} onClick={() => open(item)}>
                编辑
              </button>
              <button
                disabled={!online || !!busy}
                onClick={() => void remove(item)}
              >
                {item.state === "native" ? "恢复原生" : "移除"}
              </button>
            </div>
          </div>
        ))}
        {!config?.items.length && (
          <div className="reasoning-empty">
            {busy === "load" ? "读取中…" : "暂无补全"}
          </div>
        )}
      </div>
      {draft && (
        <div className="modal-backdrop">
          <div
            className="reasoning-modal"
            role="dialog"
            aria-modal="true"
            aria-labelledby="reasoning-title"
            ref={modal}
            onKeyDown={(e) => {
              if (e.key === "Escape") {
                e.stopPropagation();
                close();
              }
              if (e.key === "Tab") {
                const elements = [
                  ...(modal.current?.querySelectorAll<HTMLElement>(
                    "button:not(:disabled),input:not(:disabled),select:not(:disabled),summary",
                  ) || []),
                ];
                const index = elements.indexOf(
                  document.activeElement as HTMLElement,
                );
                if (
                  (e.shiftKey && index <= 0) ||
                  (!e.shiftKey && index === elements.length - 1)
                ) {
                  e.preventDefault();
                  elements[e.shiftKey ? elements.length - 1 : 0]?.focus();
                }
              }
            }}
          >
            <header>
              <h2 id="reasoning-title">
                {draft.editing ? "编辑补全" : "添加模型"}
              </h2>
              <button
                aria-label="关闭"
                disabled={busy === "save"}
                onClick={close}
              >
                <X size={16} />
              </button>
            </header>
            <label>
              模型 ID
              <input
                aria-label="模型 ID"
                placeholder="精确模型 ID"
                value={draft.model}
                disabled={draft.editing || busy === "save"}
                spellCheck={false}
                onChange={(e) => {
                  setResolution(undefined);
                  setFormError("");
                  setDraft({
                    ...draft,
                    model: e.target.value.trim(),
                    efforts: [],
                    default_effort: "",
                    loaded: false,
                  });
                }}
              />
            </label>
            <fieldset disabled={busy === "save"}>
              <legend>支持的思考档位</legend>
              <div className="reasoning-choices">
                {EFFORTS.map((e) => (
                  <button
                    key={e}
                    type="button"
                    aria-pressed={draft.efforts.includes(e)}
                    onClick={() => changeEffort(e)}
                  >
                    {e}
                  </button>
                ))}
              </div>
            </fieldset>
            <label>
              默认档位
              <select
                aria-label="默认档位"
                value={draft.default_effort}
                disabled={!draft.efforts.length || busy === "save"}
                onChange={(e) =>
                  setDraft({
                    ...draft,
                    loaded: true,
                    default_effort: e.target.value,
                  })
                }
              >
                <option value="">请选择</option>
                {draft.efforts.map((e) => (
                  <option key={e} value={e}>
                    {e}
                  </option>
                ))}
              </select>
            </label>
            {resolving ? (
              <div className="reasoning-status" role="status">
                读取模型信息…
              </div>
            ) : (
              resolution && (
                <details className="reasoning-details">
                  <summary>
                    <span
                      className={`reasoning-state ${native ? "native" : resolution.forwarding.state}`}
                    >
                      {status}
                    </span>
                    <span>来源与详情</span>
                  </summary>
                  <p>{sourceLabels[resolution.source] || resolution.source}</p>
                  <p>{resolution.forwarding.reason}</p>
                  {!resolution.descriptor_available && (
                    <p>未取得该模型自身的完整目录描述，暂存草稿。</p>
                  )}
                </details>
              )
            )}
            {needsAllowlist && (
              <label className="reasoning-confirm">
                <input
                  type="checkbox"
                  checked={confirm}
                  disabled={busy === "save"}
                  onChange={(e) => setConfirm(e.target.checked)}
                />
                仅将 {draft.model} 追加到分组白名单
              </label>
            )}
            {formError && (
              <div className="models-error" role="alert">
                {formError}
              </div>
            )}
            <footer>
              {formError && (
                <button
                  disabled={!online || !!busy || resolving}
                  onClick={() => setCheckRevision((n) => n + 1)}
                >
                  重新核对
                </button>
              )}
              <button disabled={busy === "save"} onClick={close}>
                取消
              </button>
              <button
                className="primary"
                disabled={!ready || !!(needsAllowlist && !confirm)}
                onClick={() => void save()}
              >
                {busy === "save"
                  ? "保存中…"
                  : native
                    ? "恢复原生"
                    : verified
                      ? "保存"
                      : "保存草稿"}
              </button>
            </footer>
          </div>
        </div>
      )}
    </section>
  );
}
