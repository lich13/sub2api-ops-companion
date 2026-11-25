import React, {
  Suspense,
  useEffect,
  useEffectEvent,
  useMemo,
  useState,
} from "react";
import { Check, LoaderCircle, Plug, Plus, Trash2, Unplug } from "lucide-react";
import { api, command } from "./bridge";
import { bridgeFailure } from "./bridgeError";
import {
  savedConnections,
  type Account,
  type Config,
  type ConfigSection,
  type SavedConnection,
  type ViewState,
} from "./types";
import RecoveryAccounts from "./RecoveryAccounts";
import DetectionPlatformSettings from "./DetectionPlatformSettings";
import FingerprintBankStatus from "./FingerprintBankStatus";
import AccountModelField from "./AccountModelField";
import ConnectionView from "./ConnectionView";
import ClientUpdateCard from "./ClientUpdateCard";
import { SwitchControl } from "./SwitchControl";

const LazySub2ApiConfig = React.lazy(() => import("./Sub2ApiConfig"));
const LazyProxyManagement = React.lazy(() => import("./ProxyManagement"));

const oauthLabels: Record<string, string> = {
  oauth_recovery_monitor_enabled: "额度恢复监控",
  oauth_early_reset_enabled: "提前重置检测",
  oauth_early_reset_interval_seconds: "检测间隔（秒）",
  oauth_auto_reset_credit_enabled: "7d 100% 且 429 时自动用卡",
  oauth_daily_test_enabled: "每日定时测活",
  oauth_daily_test_time: "测活时间（北京时间）",
  oauth_usage_refresh_concurrency: "额度查询并发",
  oauth_recovery_test_concurrency: "测活并发",
  connection_test_retry_interval_seconds: "连接测试重试间隔（秒）",
  connection_test_max_retries: "连接测试最大重试次数",
  oauth_recovery_test_model_id: "测活模型",
  model_detection_model_id: "自动检测模型",
};

export default function SettingsView({
  page,
  config,
  accounts,
  state,
  runtimeVersion,
  onChange,
  onMessage,
  onError,
  beforeConnectionChange,
  onSwitchConnection,
  onOpenConnectionModal,
  switchingUrl = null,
}: {
  page: "features" | "settings";
  config: Config;
  accounts: Account[];
  state: ViewState;
  runtimeVersion: string | null;
  onChange: (k: string, c: ConfigSection) => void;
  onMessage: (m: string) => void;
  onError: (e: unknown) => void;
  beforeConnectionChange: () => Promise<boolean>;
  onSwitchConnection?: (item: SavedConnection) => void;
  onOpenConnectionModal?: (target: {
    baseUrl: string;
    name: string;
    requireKey?: boolean;
  }) => void;
  switchingUrl?: string | null;
}) {
  const [featureTab, setFeatureTab] = useState("recovery");
  const [visitedTabs, setVisitedTabs] = useState<Record<string, boolean>>({
    recovery: true,
  });
  const [removingUrl, setRemovingUrl] = useState<string | null>(null);
  const selectFeatureTab = (next: string) => {
    setFeatureTab(next);
    setVisitedTabs((old) => (old[next] ? old : { ...old, [next]: true }));
  };
  const connectionKey = `${state.preferences.base_url}:${state.connection_revision ?? 0}`;
  const connections = useMemo(
    () => savedConnections(state.preferences),
    [state.preferences],
  );
  const activeUrl = state.preferences.base_url.trim().replace(/\/+$/, "");
  async function action(name: string) {
    try {
      const result = await api<{ message?: string }>(
        "POST",
        `/actions/${name}`,
      );
      onMessage(result.message || "操作完成");
    } catch (e) {
      onError(e);
    }
  }
  async function removeInstance(item: SavedConnection) {
    if (removingUrl) return;
    setRemovingUrl(item.base_url);
    try {
      await command("remove_connection", {
        baseUrl: item.base_url,
      });
      onMessage(`已删除实例 ${item.name}`);
    } catch (e) {
      onError(e);
    } finally {
      setRemovingUrl(null);
    }
  }
  return (
    <div
      className={`settings-stack ${page === "features" ? "features-stack" : ""}`}
    >
      {page === "features" ? (
        <>
          <div className="feature-tabs" role="tablist" aria-label="功能分区">
            {[
              ["recovery", "恢复与检测"],
              ["ip", "IP 管理"],
              ["sub2api", "Sub2API 配置"],
            ].map(([key, label]) => (
              <button
                key={key}
                id={`feature-tab-${key}`}
                role="tab"
                aria-selected={featureTab === key}
                aria-controls={`feature-panel-${key}`}
                tabIndex={featureTab === key ? 0 : -1}
                onClick={() => selectFeatureTab(key)}
                onKeyDown={(e) => {
                  const keys = ["recovery", "ip", "sub2api"];
                  const index = keys.indexOf(key);
                  const next =
                    e.key === "ArrowRight"
                      ? keys[(index + 1) % keys.length]
                      : e.key === "ArrowLeft"
                        ? keys[(index + keys.length - 1) % keys.length]
                        : e.key === "Home"
                          ? keys[0]
                          : e.key === "End"
                            ? keys[keys.length - 1]
                            : null;
                  if (next) {
                    e.preventDefault();
                    selectFeatureTab(next);
                    document.getElementById(`feature-tab-${next}`)?.focus();
                  }
                }}
              >
                {label}
              </button>
            ))}
          </div>
          <div
            role="tabpanel"
            id="feature-panel-recovery"
            aria-labelledby="feature-tab-recovery"
            hidden={featureTab !== "recovery"}
          >
            <ConfigForm
              section="oauth"
              title="恢复、测活与模型检测"
              value={config.oauth}
              online={state.online}
              onChange={onChange}
              onMessage={onMessage}
              onError={onError}
            >
              {(draft, set) => (
                <>
                  {Object.entries(oauthLabels).map(([key, label]) =>
                    key === "oauth_recovery_test_model_id" ||
                    key === "model_detection_model_id" ? (
                      <AccountModelField
                        key={`${connectionKey}:${key}`}
                        label={label}
                        value={draft[key]}
                        accounts={accounts}
                        online={state.online}
                        set={(v) => set(key, v)}
                      />
                    ) : (
                      <Field
                        key={key}
                        label={label}
                        value={draft[key]}
                        type={
                          key === "oauth_daily_test_time" ? "time" : undefined
                        }
                        min={
                          key === "oauth_early_reset_interval_seconds"
                            ? 2
                            : undefined
                        }
                        disabled={
                          key === "oauth_early_reset_interval_seconds" &&
                          draft.oauth_early_reset_enabled !== true
                        }
                        set={(v) => set(key, v)}
                      />
                    ),
                  )}
                  <DetectionPlatformSettings
                    value={draft.model_detection_platforms}
                    set={(v) => set("model_detection_platforms", v)}
                  />
                  {featureTab === "recovery" &&
                    (["modeltrace", "lm_detector"] as const).map((engine) => (
                      <FingerprintBankStatus
                        key={connectionKey + engine}
                        engine={engine}
                        online={state.online}
                      />
                    ))}
                  <RecoveryAccounts
                    accounts={accounts}
                    connection={
                      (draft.oauth_recovery_connection_account_ids as number[]) ??
                      []
                    }
                    models={
                      (draft.oauth_recovery_model_account_ids as number[]) ?? []
                    }
                    change={(connection, models) => {
                      set("oauth_recovery_connection_account_ids", connection);
                      set("oauth_recovery_model_account_ids", models);
                    }}
                  />
                </>
              )}
            </ConfigForm>
          </div>
          <div
            role="tabpanel"
            id="feature-panel-sub2api"
            aria-labelledby="feature-tab-sub2api"
            hidden={featureTab !== "sub2api"}
          >
            {(featureTab === "sub2api" || visitedTabs.sub2api) && (
              <Suspense fallback={null}>
                <LazySub2ApiConfig
                  key={connectionKey}
                  connectionKey={connectionKey}
                  active={
                    featureTab === "sub2api" && state.foreground !== false
                  }
                  online={state.online}
                />
              </Suspense>
            )}
          </div>
          <div
            role="tabpanel"
            id="feature-panel-ip"
            aria-labelledby="feature-tab-ip"
            hidden={featureTab !== "ip"}
          >
            {(featureTab === "ip" || visitedTabs.ip) && (
              <Suspense fallback={null}>
                <LazyProxyManagement
                  key={connectionKey}
                  connectionKey={connectionKey}
                  active={featureTab === "ip"}
                  online={state.online}
                />
              </Suspense>
            )}
          </div>
        </>
      ) : (
        <>
          <section
            className="settings-card instance-management-card"
            aria-label="Sub2API 实例管理"
          >
            <div className="settings-heading">
              <h2>Sub2API 实例管理</h2>
              {onOpenConnectionModal && (
                <button
                  type="button"
                  onClick={() =>
                    onOpenConnectionModal({
                      baseUrl: "",
                      name: "",
                      requireKey: true,
                    })
                  }
                >
                  <Plus size={14} />
                  新增实例
                </button>
              )}
            </div>
            {connections.length > 0 ? (
              <div className="saved-instances-list">
                {connections.map((item) => {
                  const isCurrent =
                    state.connected && item.base_url === activeUrl;
                  const isSwitching = switchingUrl === item.base_url;
                  const isRemoving = removingUrl === item.base_url;
                  return (
                    <article
                      key={item.base_url}
                      className={`saved-instance-row${isCurrent ? " current" : ""}`}
                    >
                      <div className="saved-instance-info">
                        <div className="saved-instance-title">
                          <span
                            className={`status-dot ${isCurrent ? (state.online ? "good" : "warning") : "muted"}`}
                          />
                          <strong>{item.name}</strong>
                          <span
                            className={`instance-badge ${isCurrent ? "active" : ""}`}
                          >
                            {isCurrent ? "当前连接" : "已保存"}
                          </span>
                        </div>
                        <small>{item.base_url}</small>
                      </div>
                      <div className="saved-instance-actions">
                        {!isCurrent && onSwitchConnection && (
                          <button
                            type="button"
                            disabled={switchingUrl !== null || isRemoving}
                            onClick={() => onSwitchConnection(item)}
                          >
                            {isSwitching ? (
                              <LoaderCircle size={13} className="spin" />
                            ) : (
                              <Plug size={13} />
                            )}
                            切换连接
                          </button>
                        )}
                        {onOpenConnectionModal && (
                          <button
                            type="button"
                            onClick={() =>
                              onOpenConnectionModal({
                                baseUrl: item.base_url,
                                name: item.name,
                                requireKey: false,
                              })
                            }
                          >
                            更新 Key / 重命名
                          </button>
                        )}
                        <button
                          type="button"
                          className="danger-text"
                          disabled={
                            isCurrent || switchingUrl !== null || isRemoving
                          }
                          title={
                            isCurrent
                              ? "当前实例正在连接或使用中，请先切换到其他实例或断开连接后再删除"
                              : "删除此实例并清除对应的本机 Key"
                          }
                          onClick={() => void removeInstance(item)}
                        >
                          <Trash2 size={13} />
                          删除实例
                        </button>
                      </div>
                    </article>
                  );
                })}
              </div>
            ) : (
              <p className="muted">尚未保存任何 Sub2API 实例。</p>
            )}
          </section>
          <ClientUpdateCard runtimeVersion={runtimeVersion} onError={onError} />
          <ConfigForm
            section="bark"
            title="Bark 事件推送"
            value={config.bark}
            online={state.online}
            onChange={onChange}
            onMessage={onMessage}
            onError={onError}
          >
            {(draft, set) => (
              <>
                <Field
                  label="启用推送"
                  value={draft.enabled}
                  set={(v) => set("enabled", v)}
                />
                <Field
                  label={`Device Key${config.bark.device_key_set ? "（已保存，留空保留）" : ""}`}
                  value={draft.device_key ?? ""}
                  type="password"
                  set={(v) => set("device_key", v)}
                />
                <button
                  type="button"
                  disabled={!state.online}
                  onClick={() => void action("bark-test")}
                >
                  发送测试消息
                </button>
              </>
            )}
          </ConfigForm>
          <section className="settings-card">
            <ConnectionView
              state={state}
              onError={onError}
              beforeChange={beforeConnectionChange}
              onSwitchSaved={onSwitchConnection}
              switchingUrl={switchingUrl}
            />
            <button
              className="danger-text"
              onClick={() =>
                void beforeConnectionChange()
                  .then((allowed) => {
                    if (allowed) return command("disconnect");
                  })
                  .catch(onError)
              }
            >
              <Unplug size={14} />
              断开连接并删除本机 Key
            </button>
          </section>
        </>
      )}
    </div>
  );
}

function Field({
  label,
  value,
  type,
  min,
  disabled,
  set,
}: {
  label: string;
  value: unknown;
  type?: string;
  min?: number;
  disabled?: boolean;
  set: (v: unknown) => void;
}) {
  const numeric = typeof value === "number";
  const [raw, setRaw] = useState(() =>
    numeric ? (Number.isFinite(value) ? String(value) : "") : "",
  );
  useEffect(() => {
    if (!numeric || !Number.isFinite(value)) return;
    setRaw((current) =>
      current.trim() !== "" && Number(current) === value
        ? current
        : String(value),
    );
  }, [numeric, value]);
  return (
    <label className="field">
      <span>{label}</span>
      {typeof value === "boolean" ? (
        <SwitchControl
          label={label}
          on={value}
          disabled={disabled}
          onChange={() => set(!value)}
        />
      ) : (
        <input
          type={type || (numeric ? "number" : "text")}
          value={numeric ? raw : String(value ?? "")}
          autoComplete="off"
          min={min}
          step={min == null ? undefined : 1}
          disabled={disabled}
          onChange={(e) => {
            const text = e.currentTarget.value;
            if (numeric) {
              setRaw(text);
              if (text.trim() !== "" && Number.isFinite(Number(text))) {
                set(Number(text));
              }
            } else {
              set(text);
            }
          }}
          onBlur={() => {
            if (
              numeric &&
              (raw.trim() === "" || !Number.isFinite(Number(raw))) &&
              Number.isFinite(value)
            ) {
              setRaw(String(value));
            }
          }}
        />
      )}
    </label>
  );
}

function ConfigForm({
  section,
  title,
  value,
  online,
  onChange,
  onMessage,
  onError,
  children,
}: {
  section: string;
  title: string;
  value: ConfigSection;
  online: boolean;
  onChange: (k: string, c: ConfigSection) => void;
  onMessage: (m: string) => void;
  onError: (e: unknown) => void;
  children: (
    draft: ConfigSection,
    set: (key: string, v: unknown) => void,
  ) => React.ReactNode;
}) {
  const [draft, setDraft] = useState(value),
    [changes, setChanges] = useState<Record<string, unknown>>({}),
    [busy, setBusy] = useState(false),
    [conflict, setConflict] = useState(false);
  const receiveConfig = useEffectEvent(() => {
    if (!Object.keys(changes).length) {
      setDraft(value);
      setConflict(false);
    } else if (draft.revision !== value.revision) setConflict(true);
  });
  useEffect(() => {
    receiveConfig();
  }, [value]);
  const set = (key: string, v: unknown) => {
    setDraft((d) => ({ ...d, [key]: v }));
    setChanges((c) => ({ ...c, [key]: v }));
  };
  return (
    <form
      className="settings-card"
      onSubmit={(e) => {
        e.preventDefault();
        setBusy(true);
        void api<ConfigSection>("PUT", `/config/${section}`, {
          expected_revision: draft.revision,
          changes,
        })
          .then((c) => {
            setDraft(c);
            setChanges({});
            setConflict(false);
            onChange(section, c);
            onMessage("设置已保存");
          })
          .catch((e) => {
            onError(e);
            if (bridgeFailure(e)?.status === 409) setConflict(true);
          })
          .finally(() => setBusy(false));
      }}
    >
      <div className="settings-heading">
        <div>
          <h2>{title}</h2>
        </div>
        <button
          className="primary small"
          disabled={!online || busy || !Object.keys(changes).length || conflict}
        >
          {busy ? (
            <LoaderCircle size={13} className="spin" />
          ) : (
            <Check size={13} />
          )}
          保存
        </button>
      </div>
      <fieldset disabled={!online || busy}>{children(draft, set)}</fieldset>
      {conflict && (
        <div className="conflict">
          云端配置已变化。
          <button
            type="button"
            onClick={() =>
              void api<Config>("GET", "/config")
                .then((c) => {
                  setChanges({});
                  setDraft(c[section]);
                  setConflict(false);
                  onChange(section, c[section]);
                })
                .catch(onError)
            }
          >
            放弃本地修改并刷新
          </button>
        </div>
      )}
    </form>
  );
}
