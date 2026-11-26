import { useEffect, useState } from "react";
import {
  Download,
  LoaderCircle,
  RefreshCw,
  ShieldAlert,
  X,
} from "lucide-react";
import { command } from "./bridge";
import { errorMessage } from "./bridgeError";
import { subscribeAppUpdate } from "./tauriEvents";
import type { AppUpdateSnapshot } from "./types";
import { version as appVersion } from "../package.json";

function formatBytes(bytes: number): string {
  if (bytes >= 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  if (bytes >= 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${bytes} B`;
}

export default function ClientUpdateCard({
  runtimeVersion,
  onError,
}: {
  runtimeVersion?: string | null;
  onError?: (error: unknown) => void;
}) {
  const [update, setUpdate] = useState<AppUpdateSnapshot | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let disposed = false;
    let unlisten = () => {};
    void (async () => {
      unlisten = await subscribeAppUpdate((next) => {
        if (!disposed) setUpdate(next);
      });
      const initial = await command("get_app_update_state");
      if (disposed) unlisten();
      else if (initial) setUpdate(initial);
    })().catch((error) => {
      if (disposed) return;
      const message = errorMessage(error);
      if (message === "请在 Sub2Ops 客户端中打开") return;
      setUpdate(
        (prev) =>
          prev ?? {
            phase: "error",
            current_version: appVersion,
            latest_version: null,
            downloaded_bytes: 0,
            total_bytes: null,
            message,
          },
      );
    });
    return () => {
      disposed = true;
      unlisten();
    };
  }, []);

  const currentVer = runtimeVersion ?? update?.current_version ?? appVersion;
  const phase = update?.phase ?? "idle";
  const inProgress =
    phase === "checking" || phase === "downloading" || phase === "verifying";
  const totalBytes = update?.total_bytes ?? null;
  const downloadedBytes = update?.downloaded_bytes ?? 0;
  const percent =
    totalBytes && totalBytes > 0
      ? Math.min(100, Math.round((downloadedBytes / totalBytes) * 100))
      : null;

  async function runAction(
    cmd: "start_app_update" | "cancel_app_update" | "install_app_update",
  ) {
    if (busy) return;
    setBusy(true);
    try {
      const next = await command(cmd);
      if (next) setUpdate(next);
    } catch (error) {
      onError?.(error);
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="settings-card client-update-card">
      <h2>客户端</h2>
      <div className="field">
        <span>当前版本</span>
        <div className="client-update-actions">
          <code className="client-version-chip">{currentVer}</code>
          {inProgress ? (
            <button
              type="button"
              disabled={busy}
              onClick={() => void runAction("cancel_app_update")}
            >
              <X size={14} />
              取消下载
            </button>
          ) : phase === "permission_required" ? (
            <>
              <button
                type="button"
                className="primary"
                disabled={busy}
                onClick={() => void runAction("install_app_update")}
              >
                <ShieldAlert size={14} />
                授权后继续安装
              </button>
              <button
                type="button"
                disabled={busy}
                onClick={() => void runAction("cancel_app_update")}
              >
                取消
              </button>
            </>
          ) : (
            <button
              type="button"
              disabled={busy}
              onClick={() => void runAction("start_app_update")}
            >
              {busy ? (
                <LoaderCircle size={14} className="spin" />
              ) : phase === "error" ? (
                <RefreshCw size={14} />
              ) : (
                <Download size={14} />
              )}
              {phase === "error" ? "重试更新" : "检查更新"}
            </button>
          )}
        </div>
      </div>
      {(phase === "downloading" || phase === "verifying") && (
        <div className="client-update-progress">
          <progress
            max={totalBytes ?? 100}
            value={totalBytes ? downloadedBytes : undefined}
            aria-label="更新下载进度"
          />
          <span className="client-update-bytes">
            {totalBytes
              ? `${formatBytes(downloadedBytes)} / ${formatBytes(totalBytes)}${percent !== null ? ` (${percent}%)` : ""}`
              : formatBytes(downloadedBytes)}
          </span>
        </div>
      )}
      {update?.message && (
        <p
          role="status"
          className={`client-update-status ${phase === "error" ? "bad-text" : phase === "up_to_date" ? "good-text" : "muted"}`}
        >
          {inProgress && <LoaderCircle size={13} className="spin" />}
          <span>{update.message}</span>
        </p>
      )}
    </section>
  );
}
