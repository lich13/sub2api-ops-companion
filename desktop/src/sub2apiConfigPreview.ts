import catalog from "../../server/resources/sub2api-config.json";
import type { ConfigDocument } from "./Sub2ApiConfig";
const configured: Record<string, unknown> = {
  "server.port": 8080,
  "jwt.expire_hour": 24,
  "log.level": "info",
  "gateway.scheduling.load_batch_cache_ttl_ms": 200,
};
const environment: Record<string, unknown> = {
  "server.port": 8080,
  "jwt.expire_hour": 168,
};
let revision = 1,
  status = "idle";
export function configPreview(
  path: string,
  body: Record<string, unknown>,
): unknown {
  if (path === "/sub2api-config")
    return {
      available: true,
      editable: true,
      reason: "",
      version: catalog.version,
      commit: catalog.revision,
      healthy: true,
      revision: String(revision).repeat(64),
      runtime_revision: "a".repeat(64),
      catalog,
      unknown: [
        {
          path: "rate_limit.requests_per_minute",
          message: "当前版本不识别此字段，不会产生全局每分钟请求限制。",
        },
      ],
      values: Object.fromEntries(
        catalog.fields
          .filter((f) => !f.internal)
          .map((f) => [
            f.path,
            {
              configured: f.path in configured,
              yaml: configured[f.path] ?? null,
              effective:
                environment[f.path] ?? configured[f.path] ?? f.default_value,
              source:
                f.path in environment
                  ? "environment"
                  : f.path in configured
                    ? "yaml"
                    : "default",
              environment: f.path in environment ? f.env[0] : null,
              editable: f.editable && !(f.path in environment),
            },
          ]),
      ),
    } satisfies ConfigDocument;
  if (path.endsWith("/validate")) return { valid: true, errors: [] };
  if (path.endsWith("/apply")) {
    for (const c of body.changes as {
      path: string;
      remove: boolean;
      value: unknown;
    }[]) {
      if (c.remove) delete configured[c.path];
      else configured[c.path] = c.value;
    }
    ++revision;
    status = "succeeded";
    return { status: "restarting", message: "正在重启 Sub2API" };
  }
  if (path.endsWith("/operation"))
    return {
      status,
      message: status === "succeeded" ? "配置已应用，Sub2API 已恢复健康" : "",
    };
  throw new Error("不支持的配置请求");
}

export function systemPreview(path: string) {
  if (path === "/sub2api-system/version") return { version: catalog.version };
  if (path.startsWith("/sub2api-system/check-updates")) return { current_version: catalog.version, latest_version: catalog.version, has_update: false, cached: false, build_type: "release", release_info: { name: `v${catalog.version}`, body: "", published_at: "", html_url: `https://github.com/Wei-Shaw/sub2api/releases/tag/v${catalog.version}` } };
  if (path.endsWith("/update")) return { message: "Already up to date", already_up_to_date: true, current_version: catalog.version, latest_version: catalog.version };
  if (path.endsWith("/restart")) return { message: "Service restart initiated" };
  throw new Error("不支持的版本请求");
}
