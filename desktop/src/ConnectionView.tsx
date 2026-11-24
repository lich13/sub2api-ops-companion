import { useEffect, useMemo, useState } from "react";
import { LoaderCircle, Plug, X } from "lucide-react";
import { command } from "./bridge";
import {
  savedConnections,
  type SavedConnection,
  type ViewState,
} from "./types";

export default function ConnectionView({
  state,
  onError,
  beforeChange = async () => true,
  initialUrl,
  initialName,
  requireKey = false,
  showNameField = false,
  onConnected,
  onCancel,
  onSwitchSaved,
  switchingUrl = null,
}: {
  state: ViewState;
  onError: (e: unknown) => void;
  beforeChange?: () => Promise<boolean>;
  initialUrl?: string;
  initialName?: string;
  requireKey?: boolean;
  showNameField?: boolean;
  onConnected?: () => void;
  onCancel?: () => void;
  onSwitchSaved?: (item: SavedConnection) => void;
  switchingUrl?: string | null;
}) {
  const saved = useMemo(
    () => savedConnections(state.preferences),
    [state.preferences],
  );
  const defaultUrl =
    initialUrl !== undefined ? initialUrl : state.preferences.base_url || "";
  const defaultName =
    initialName !== undefined
      ? initialName
      : saved.find((item) => item.base_url === defaultUrl)?.name || "";
  const [url, setUrl] = useState(defaultUrl),
    [name, setName] = useState(defaultName),
    [nameOpen, setNameOpen] = useState(showNameField || Boolean(initialName)),
    [key, setKey] = useState(""),
    [busy, setBusy] = useState(false);
  useEffect(() => {
    setUrl(defaultUrl);
    setName(defaultName);
    if (initialName) setNameOpen(true);
  }, [defaultUrl, defaultName, initialName]);
  const normalizedUrl = url.trim().replace(/\/+$/, "");
  const normalizedInitial = (initialUrl ?? "").trim().replace(/\/+$/, "");
  const isEditingSaved = Boolean(
    showNameField &&
    normalizedInitial &&
    !requireKey &&
    normalizedUrl === normalizedInitial,
  );
  return (
    <form
      className="connection-form"
      onSubmit={async (e) => {
        e.preventDefault();
        const trimmedName = name.trim();
        if (isEditingSaved && !key.trim()) {
          setBusy(true);
          void command("save_connection_profile", {
            baseUrl: url,
            name: trimmedName,
          })
            .then(() => {
              onConnected?.();
            })
            .catch(onError)
            .finally(() => setBusy(false));
          return;
        }
        if (!(await beforeChange())) return;
        setBusy(true);
        const payload = {
          baseUrl: url,
          apiKey: key,
          name: trimmedName || undefined,
        };
        void command("connect", payload)
          .then(() => {
            setKey("");
            onConnected?.();
          })
          .catch(onError)
          .finally(() => setBusy(false));
      }}
    >
      <div className="connection-form-top">
        <span className="connection-icon">
          <Plug size={26} />
        </span>
        {onCancel && (
          <button
            type="button"
            className="icon-button"
            aria-label="关闭连接窗口"
            onClick={onCancel}
          >
            <X size={16} />
          </button>
        )}
      </div>
      <h2>
        {initialUrl === "" && showNameField
          ? "新增 Sub2API 实例"
          : state.connected
            ? "更换或添加连接"
            : "连接你的运维服务"}
      </h2>
      {!state.connected && saved.length > 0 && onSwitchSaved && (
        <div
          className="saved-connections-quick"
          aria-label="已保存实例快捷连接"
        >
          <span className="saved-connections-label">已保存实例</span>
          <div className="saved-connections-chips">
            {saved.map((item) => (
              <div key={item.base_url} className="saved-connection-chip">
                <button
                  type="button"
                  className="saved-connection-connect"
                  disabled={busy || switchingUrl !== null}
                  onClick={() => onSwitchSaved(item)}
                >
                  {switchingUrl === item.base_url ? (
                    <LoaderCircle size={13} className="spin" />
                  ) : (
                    <Plug size={13} />
                  )}
                  <strong>{item.name}</strong>
                  <small>{item.base_url}</small>
                </button>
                <button
                  type="button"
                  className="text-button"
                  onClick={() => {
                    setUrl(item.base_url);
                    setName(item.name);
                    setNameOpen(true);
                  }}
                >
                  填入
                </button>
              </div>
            ))}
          </div>
        </div>
      )}
      <label>
        服务地址
        <input
          type="url"
          required
          value={url}
          onChange={(e) => setUrl(e.target.value)}
          placeholder="https://example.com/sub2ops"
          autoComplete="url"
        />
      </label>
      <label>
        管理员 API Key
        <input
          type="password"
          required={!isEditingSaved}
          value={key}
          onChange={(e) => setKey(e.target.value)}
          placeholder={
            isEditingSaved ? "留空则仅保存实例名称" : "粘贴已有的管理员 Key"
          }
          autoComplete="off"
          spellCheck={false}
        />
      </label>
      {nameOpen ? (
        <label>
          实例名称（可选）
          <input
            type="text"
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder="例如：主力生产节点"
            autoComplete="off"
          />
        </label>
      ) : (
        <button
          type="button"
          className="text-button connection-name-toggle"
          onClick={() => setNameOpen(true)}
        >
          设置实例备注名
        </button>
      )}
      <div className="connection-form-actions">
        {onCancel && (
          <button type="button" onClick={onCancel} disabled={busy}>
            取消
          </button>
        )}
        <button className="primary" disabled={busy}>
          {busy ? (
            <LoaderCircle className="spin" size={15} />
          ) : (
            <Plug size={15} />
          )}
          {isEditingSaved && !key.trim() ? "保存实例名称" : "验证并连接"}
        </button>
      </div>
    </form>
  );
}
