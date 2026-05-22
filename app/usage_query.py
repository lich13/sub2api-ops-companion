from __future__ import annotations

import json
import math
import threading
import urllib.error
import urllib.request
from dataclasses import asdict, dataclass, replace
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Callable
from urllib.parse import urlsplit, urlunsplit

import quickjs


DEFAULT_SUB2API_TEMPLATE = """({
  request: {
    url: "{{baseUrl}}/v1/usage",
    method: "GET",
    headers: {
      "Authorization": "Bearer {{apiKey}}",
      "User-Agent": "cc-switch/1.0"
    }
  },
  extractor: function(response) {
    const asNumber = function(value) {
      const parsed = typeof value === "number" ? value : Number(value);
      return Number.isFinite(parsed) ? parsed : undefined;
    };
    const remaining = asNumber(response?.remaining ?? response?.quota?.remaining ?? response?.balance);
    const used = asNumber(response?.used ?? response?.quota?.used ?? response?.usage?.total?.actual_cost ?? response?.usage?.total?.cost);
    const explicitTotal = asNumber(response?.total ?? response?.quota?.total);
    const total = explicitTotal ?? (remaining !== undefined && used !== undefined ? remaining + used : undefined);
    const unit = response?.unit ?? response?.quota?.unit ?? "USD";
    return {
      isValid: response?.is_active ?? response?.isValid ?? true,
      planName: response?.planName ?? response?.plan_name ?? response?.quota?.planName ?? "",
      remaining,
      total,
      used,
      unit
    };
  }
})"""

LEGACY_SUB2API_TEMPLATES = (
    """({
  request: {
    url: "{{baseUrl}}/v1/usage",
    method: "GET",
    headers: {
      "Authorization": "Bearer {{apiKey}}"
    }
  },
  extractor: function(response) {
    const asNumber = function(value) {
      const parsed = typeof value === "number" ? value : Number(value);
      return Number.isFinite(parsed) ? parsed : undefined;
    };
    const remaining = asNumber(response?.remaining ?? response?.quota?.remaining ?? response?.balance);
    const used = asNumber(response?.used ?? response?.quota?.used ?? response?.usage?.total?.actual_cost ?? response?.usage?.total?.cost);
    const explicitTotal = asNumber(response?.total ?? response?.quota?.total);
    const total = explicitTotal ?? (remaining !== undefined && used !== undefined ? remaining + used : undefined);
    const unit = response?.unit ?? response?.quota?.unit ?? "USD";
    return {
      isValid: response?.is_active ?? response?.isValid ?? true,
      planName: response?.planName ?? response?.plan_name ?? response?.quota?.planName ?? "",
      remaining,
      total,
      used,
      unit
    };
  }
})""",
    """({
    request: {
      url: "{{baseUrl}}/v1/usage",
      method: "GET",
      headers: { "Authorization": "Bearer {{apiKey}}" }
    },
    extractor: function(response) {
      const remaining = response?.remaining ?? response?.quota?.remaining ?? response?.balance;
      const unit = response?.unit ?? response?.quota?.unit ?? "USD";
      return {
        isValid: response?.is_active ?? response?.isValid ?? true,
        remaining,
        unit
      };
    }
  })""",
    """({
  request: {
    url: "{{baseUrl}}/v1/usage",
    method: "GET",
    headers: {
      "Authorization": "Bearer {{apiKey}}"
    }
  },
  extractor: function(response) {
    const remaining = response?.remaining ?? response?.quota?.remaining ?? response?.balance;
    const total = response?.total ?? response?.quota?.total;
    const used = response?.used ?? response?.quota?.used;
    const unit = response?.unit ?? response?.quota?.unit ?? "USD";
    return {
      isValid: response?.is_active ?? response?.isValid ?? true,
      planName: response?.planName ?? response?.plan_name ?? response?.quota?.planName ?? "",
      remaining,
      total,
      used,
      unit
    };
  }
})""",
    """({
  request: {
    url: "{{baseUrl}}/user/balance",
    method: "GET",
    headers: {
      "Authorization": "Bearer {{apiKey}}",
      "User-Agent": "cc-switch/1.0"
    }
  },
  extractor: function(response) {
    return {
      isValid: response.is_active || true,
      remaining: response.balance,
      unit: "USD"
    };
  }
})""",
)

DEFAULT_NEWAPI_TEMPLATE = """({
  request: {
    url: "{{baseUrl}}/api/usage/token/",
    method: "GET",
    headers: {
      "Accept": "application/json",
      "Content-Type": "application/json",
      "Authorization": "Bearer {{newApiToken}}",
      "User-Agent": "cc-switch/1.0"
    },
  },
  extractor: function (response) {
    const quotaFactor = 500000;
    const isObject = function(value) {
      return value !== null && typeof value === "object" && !Array.isArray(value);
    };
    const asNumber = function(value) {
      const parsed = typeof value === "number" ? value : Number(value);
      return Number.isFinite(parsed) ? parsed : undefined;
    };
    const quotaToUsd = function(value) {
      return value === undefined ? undefined : Math.max(0, value) / quotaFactor;
    };
    if (response?.success === false || response?.code === false) {
      return {
        isValid: false,
        invalidMessage: response?.message || "查询失败"
      };
    }
    const data = isObject(response?.data) ? response.data : response;
    const totalGranted = asNumber(data?.total_granted);
    const totalUsed = asNumber(data?.total_used);
    const totalAvailable = asNumber(data?.total_available);
    const unlimitedQuota = data?.unlimited_quota === true || (totalGranted !== undefined && totalGranted < 0);
    if (
      !unlimitedQuota &&
      totalGranted === undefined &&
      totalUsed === undefined &&
      totalAvailable === undefined
    ) {
      return {
        isValid: false,
        invalidMessage: response?.message || "响应缺少 NewAPI token usage 字段"
      };
    }
    return {
      isValid: true,
      planName: data?.name || (unlimitedQuota ? "无限额度" : "Token 额度"),
      remaining: unlimitedQuota ? null : quotaToUsd(totalAvailable),
      used: quotaToUsd(totalUsed),
      total: unlimitedQuota ? null : quotaToUsd(totalGranted),
      unit: "USD",
      extra: unlimitedQuota ? "unlimited_quota" : ""
    };
  },
})"""

LEGACY_NEWAPI_TEMPLATES = (
    """({
  request: {
    url: "{{baseUrl}}/api/user/self",
    method: "GET",
    headers: {
      "Content-Type": "application/json",
      "Authorization": "Bearer {{accessToken}}",
      "User-Agent": "cc-switch/1.0",
      "New-Api-User": "{{userId}}"
    },
  },
  extractor: function (response) {
    if (response.success && response.data) {
      return {
        planName: response.data.group || "默认套餐",
        remaining: response.data.quota / 500000,
        used: response.data.used_quota / 500000,
        total: (response.data.quota + response.data.used_quota) / 500000,
        unit: "USD",
      };
    }
    return {
      isValid: false,
      invalidMessage: response.message || "查询失败"
    };
  },
})""",
)

DEFAULT_CUSTOM_TEMPLATE = """({
  request: {
    url: "",
    method: "GET",
    headers: {}
  },
  extractor: function(response) {
    return {
      remaining: 0,
      unit: "USD"
    };
  }
})"""

TEMPLATE_LABELS = {
    "sub2api": "Sub2API",
    "newapi": "NewAPI",
    "custom": "自定义",
}

DEFAULT_TEMPLATES = {
    "sub2api": DEFAULT_SUB2API_TEMPLATE,
    "newapi": DEFAULT_NEWAPI_TEMPLATE,
    "custom": DEFAULT_CUSTOM_TEMPLATE,
}

UsageOpener = Callable[[dict[str, Any], int], Any]
_STORE_LOCK = threading.Lock()
DEFAULT_AUTO_QUERY_INTERVAL_SECONDS = 3600
MAX_AUTO_QUERY_INTERVAL_SECONDS = 86400


class UsageQueryError(ValueError):
    pass


@dataclass
class UsageQueryConfig:
    account_id: int
    enabled: bool = False
    template_type: str = "sub2api"
    code: str = ""
    base_url: str = ""
    api_key: str = ""
    access_token: str = ""
    user_id: str = ""
    use_account_credentials: bool = True
    timeout_seconds: int = 10
    upstream_multiplier: float = 1.0
    guard_disable_on_zero: bool = False
    auto_query_interval_minutes: int = 60
    updated_at: str = ""

    def __post_init__(self) -> None:
        self.account_id = int(self.account_id)
        self.enabled = bool(self.enabled)
        self.template_type = normalize_template_type(self.template_type)
        self.base_url = normalize_base_url(str(self.base_url or ""), self.template_type)
        self.api_key = str(self.api_key or "")
        self.access_token = str(self.access_token or "")
        self.user_id = str(self.user_id or "").strip()
        self.use_account_credentials = bool(self.use_account_credentials)
        self.timeout_seconds = normalize_timeout(self.timeout_seconds)
        self.upstream_multiplier = normalize_multiplier(self.upstream_multiplier)
        self.guard_disable_on_zero = bool(self.guard_disable_on_zero)
        self.auto_query_interval_minutes = normalize_interval(self.auto_query_interval_minutes)
        self.code = normalize_default_template(self.template_type, str(self.code or "").strip())
        self.updated_at = str(self.updated_at or "")

    @classmethod
    def from_dict(cls, account_id: int, raw: dict[str, Any] | None) -> "UsageQueryConfig":
        payload = dict(raw or {})
        payload["account_id"] = account_id
        return cls(**payload)

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)


class UsageQueryStore:
    def __init__(self, path: str) -> None:
        self.path = Path(path)
        self._data = self._read()

    def _read(self) -> dict[str, Any]:
        try:
            data = json.loads(self.path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            return {"configs": {}, "results": {}, "settings": {}}
        if not isinstance(data, dict):
            return {"configs": {}, "results": {}, "settings": {}}
        if not isinstance(data.get("configs"), dict):
            data["configs"] = {}
        if not isinstance(data.get("results"), dict):
            data["results"] = {}
        if not isinstance(data.get("settings"), dict):
            data["settings"] = {}
        return data

    def _write(self) -> None:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        tmp = self.path.with_suffix(self.path.suffix + ".tmp")
        tmp.write_text(json.dumps(self._data, ensure_ascii=False, indent=2, sort_keys=True), encoding="utf-8")
        tmp.chmod(0o600)
        tmp.replace(self.path)
        self.path.chmod(0o600)

    def config(self, account_id: int) -> UsageQueryConfig:
        raw = (self._data.get("configs") or {}).get(str(int(account_id)))
        return UsageQueryConfig.from_dict(int(account_id), raw if isinstance(raw, dict) else {})

    def configs(self) -> list[UsageQueryConfig]:
        configs = self._data.get("configs") or {}
        if not isinstance(configs, dict):
            return []
        result: list[UsageQueryConfig] = []
        for key, raw in configs.items():
            try:
                account_id = int(key)
            except (TypeError, ValueError):
                continue
            result.append(UsageQueryConfig.from_dict(account_id, raw if isinstance(raw, dict) else {}))
        return result

    def usage_query_enabled(self) -> bool:
        settings = self._data.get("settings") or {}
        if not isinstance(settings, dict):
            return True
        return normalize_bool_setting(settings.get("usage_query_enabled"), True)

    def guard_disable_on_zero(self) -> bool:
        settings = self._data.get("settings") or {}
        if not isinstance(settings, dict):
            return True
        return normalize_bool_setting(settings.get("guard_disable_on_zero"), True)

    def auto_query_interval_seconds(self) -> int:
        settings = self._data.get("settings") or {}
        if not isinstance(settings, dict):
            return normalize_interval_seconds(None)
        if "auto_query_interval_seconds" in settings:
            return normalize_interval_seconds(settings.get("auto_query_interval_seconds"))
        legacy_minutes = normalize_interval(settings.get("auto_query_interval_minutes"))
        return normalize_interval_seconds(legacy_minutes * 60)

    def auto_query_interval_minutes(self) -> int:
        return self.auto_query_interval_seconds() // 60

    def save_usage_query_settings(
        self,
        *,
        usage_query_enabled: object | None = None,
        guard_disable_on_zero: object | None = None,
        auto_query_interval_seconds: object | None = None,
    ) -> None:
        with _STORE_LOCK:
            self._data = self._read()
            settings = self._data.setdefault("settings", {})
            if not isinstance(settings, dict):
                settings = {}
                self._data["settings"] = settings
            if usage_query_enabled is not None:
                settings["usage_query_enabled"] = normalize_bool_setting(usage_query_enabled, True)
            if guard_disable_on_zero is not None:
                settings["guard_disable_on_zero"] = normalize_bool_setting(guard_disable_on_zero, True)
            if auto_query_interval_seconds is not None:
                settings["auto_query_interval_seconds"] = normalize_interval_seconds(auto_query_interval_seconds)
                settings.pop("auto_query_interval_minutes", None)
            self._write()

    def save_auto_query_interval_minutes(self, value: object) -> None:
        self.save_usage_query_settings(auto_query_interval_seconds=normalize_interval(value) * 60)

    def save_config(self, config: UsageQueryConfig) -> None:
        with _STORE_LOCK:
            self._data = self._read()
            payload = config.to_dict()
            payload["updated_at"] = payload.get("updated_at") or utc_now_iso()
            self._data.setdefault("configs", {})[str(config.account_id)] = payload
            self._write()

    def delete_config(self, account_id: int) -> None:
        with _STORE_LOCK:
            self._data = self._read()
            self._data.setdefault("configs", {}).pop(str(int(account_id)), None)
            self._data.setdefault("results", {}).pop(str(int(account_id)), None)
            self._write()

    def result(self, account_id: int) -> dict[str, Any]:
        raw = (self._data.get("results") or {}).get(str(int(account_id))) or {}
        return raw if isinstance(raw, dict) else {}

    def results(self) -> dict[int, dict[str, Any]]:
        raw = self._data.get("results") or {}
        if not isinstance(raw, dict):
            return {}
        results: dict[int, dict[str, Any]] = {}
        for key, value in raw.items():
            try:
                account_id = int(key)
            except (TypeError, ValueError):
                continue
            if isinstance(value, dict):
                results[account_id] = value
        return results

    def save_result(self, account_id: int, result: dict[str, Any]) -> None:
        with _STORE_LOCK:
            self._data = self._read()
            self._data.setdefault("results", {})[str(int(account_id))] = dict(result)
            self._write()


def utc_now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


def normalize_template_type(value: object) -> str:
    raw = str(value or "").strip().lower().replace("-", "_")
    if raw in {"general", "sub2api"}:
        return "sub2api"
    if raw in {"new_api", "newapi"}:
        return "newapi"
    if raw == "custom":
        return "custom"
    return "sub2api"


def default_template(template_type: object) -> str:
    return DEFAULT_TEMPLATES[normalize_template_type(template_type)]


def normalize_default_template(template_type: object, code: str) -> str:
    if not code:
        return default_template(template_type)
    normalized_type = normalize_template_type(template_type)
    if normalized_type == "sub2api":
        legacy_codes = {normalize_template_code(template) for template in LEGACY_SUB2API_TEMPLATES}
        if normalize_template_code(code) in legacy_codes:
            return default_template("sub2api")
    if normalized_type == "newapi":
        legacy_codes = {normalize_template_code(template) for template in LEGACY_NEWAPI_TEMPLATES}
        if normalize_template_code(code) in legacy_codes:
            return default_template("newapi")
    return code


def normalize_template_code(code: str) -> str:
    return "".join(str(code or "").split())


def normalize_base_url(value: str, template_type: object = "") -> str:
    cleaned = str(value or "").strip().rstrip("/")
    if normalize_template_type(template_type) != "newapi":
        return cleaned
    parsed = urlsplit(cleaned)
    if parsed.scheme in {"http", "https"} and parsed.netloc and parsed.path.rstrip("/") == "/v1":
        return urlunsplit((parsed.scheme, parsed.netloc, "", "", ""))
    return cleaned


def normalize_timeout(value: object) -> int:
    try:
        parsed = int(str(value).strip())
    except (TypeError, ValueError):
        parsed = 10
    return max(2, min(30, parsed))


def normalize_interval(value: object) -> int:
    try:
        parsed = int(str(value).strip())
    except (TypeError, ValueError):
        parsed = 60
    return max(0, min(1440, parsed))


def normalize_interval_seconds(value: object) -> int:
    try:
        parsed = int(str(value).strip())
    except (TypeError, ValueError):
        parsed = DEFAULT_AUTO_QUERY_INTERVAL_SECONDS
    return max(0, min(MAX_AUTO_QUERY_INTERVAL_SECONDS, parsed))


def normalize_bool_setting(value: object, default: bool) -> bool:
    if value is None:
        return default
    if isinstance(value, bool):
        return value
    normalized = str(value).strip().lower()
    if normalized in {"1", "true", "yes", "on"}:
        return True
    if normalized in {"0", "false", "no", "off"}:
        return False
    return default


def normalize_multiplier(value: object) -> float:
    try:
        parsed = float(str(value).strip())
    except (TypeError, ValueError):
        parsed = 1.0
    if not math.isfinite(parsed) or parsed <= 0:
        parsed = 1.0
    return max(0.0001, min(1_000_000.0, parsed))


def actual_available(remaining: object, multiplier: object) -> float | None:
    numeric = numeric_or_none(remaining)
    if numeric is None:
        return None
    return numeric / normalize_multiplier(multiplier)


def numeric_or_none(value: object) -> float | None:
    if value is None or isinstance(value, bool):
        return None
    try:
        parsed = float(value)
    except (TypeError, ValueError):
        return None
    return parsed if math.isfinite(parsed) else None


def apply_account_credentials(config: UsageQueryConfig, account_row: dict[str, Any] | None) -> UsageQueryConfig:
    if not config.use_account_credentials or not account_row:
        return config
    credentials = account_credentials(account_row)
    base_url = config.base_url or credentials.get("base_url", "")
    api_key = config.api_key or credentials.get("api_key", "")
    access_token = config.access_token or credentials.get("access_token", "")
    if base_url == config.base_url and api_key == config.api_key and access_token == config.access_token:
        return config
    return replace(config, base_url=base_url, api_key=api_key, access_token=access_token)


def fill_account_credentials(config: UsageQueryConfig, account_row: dict[str, Any] | None) -> UsageQueryConfig:
    if not account_row:
        return config
    credentials = account_credentials(account_row)
    return replace(
        config,
        base_url=credentials.get("base_url", "") or config.base_url,
        api_key=credentials.get("api_key", "") or config.api_key,
        access_token=credentials.get("access_token", "") or config.access_token,
        use_account_credentials=False,
    )


def account_credentials(account_row: dict[str, Any]) -> dict[str, str]:
    payloads = [json_object(account_row.get("credentials")), json_object(account_row.get("extra")), account_row]
    return {
        "base_url": first_string(payloads, ("base_url", "baseUrl", "api_base", "apiBase", "api_url", "apiUrl", "url")),
        "api_key": first_string(payloads, ("api_key", "apiKey", "key", "token", "secret_key", "secretKey")),
        "access_token": first_string(payloads, ("access_token", "accessToken")),
    }


def json_object(value: Any) -> dict[str, Any]:
    if isinstance(value, dict):
        return value
    if isinstance(value, str):
        try:
            parsed = json.loads(value)
        except json.JSONDecodeError:
            return {}
        return parsed if isinstance(parsed, dict) else {}
    return {}


def first_string(payloads: list[dict[str, Any]], keys: tuple[str, ...]) -> str:
    for payload in payloads:
        for key in keys:
            value = payload.get(key)
            if value in (None, ""):
                continue
            return str(value).strip()
    return ""


def execute_usage_query(
    config: UsageQueryConfig,
    *,
    opener: UsageOpener | None = None,
    now: datetime | None = None,
) -> dict[str, Any]:
    queried_at = (now or datetime.now(timezone.utc)).astimezone(timezone.utc).isoformat()
    try:
        script = replace_template_vars(config.code or default_template(config.template_type), config)
        request = extract_request(script, config.timeout_seconds)
        validate_request(request, config)
        response = (opener or open_usage_request)(request, config.timeout_seconds)
        data = run_extractor(script, response, config.timeout_seconds)
        items = normalize_usage_data(data)
        return build_success_result(config, items, queried_at)
    except Exception as exc:
        return {
            "account_id": config.account_id,
            "template_type": config.template_type,
            "success": False,
            "data": [],
            "error": str(exc),
            "queried_at": queried_at,
            "remaining": None,
            "actual_available": None,
            "upstream_multiplier": config.upstream_multiplier,
            "unit": "",
            "plan_name": "",
            "invalid_message": "",
        }


def replace_template_vars(script: str, config: UsageQueryConfig) -> str:
    return (
        script.replace("{{apiKey}}", js_string_content(config.api_key))
        .replace("{{baseUrl}}", js_string_content(config.base_url))
        .replace("{{accessToken}}", js_string_content(config.access_token))
        .replace("{{newApiToken}}", js_string_content(config.api_key or config.access_token))
        .replace("{{userId}}", js_string_content(config.user_id))
    )


def js_string_content(value: str) -> str:
    encoded = json.dumps(str(value or ""), ensure_ascii=False)
    return encoded[1:-1]


def extract_request(script: str, timeout_seconds: int) -> dict[str, Any]:
    raw = eval_usage_script(script, "JSON.stringify(__usage_config.request);", timeout_seconds)
    try:
        request = json.loads(raw)
    except json.JSONDecodeError as exc:
        raise UsageQueryError(f"request 配置格式错误: {exc}") from exc
    if not isinstance(request, dict):
        raise UsageQueryError("request 必须是对象")
    return request


def run_extractor(script: str, response: Any, timeout_seconds: int) -> Any:
    response_json = json.dumps(response, ensure_ascii=False)
    raw = eval_usage_script(
        script,
        f"JSON.stringify(__usage_config.extractor(JSON.parse({json.dumps(response_json, ensure_ascii=False)})));",
        timeout_seconds,
    )
    try:
        return json.loads(raw)
    except json.JSONDecodeError as exc:
        raise UsageQueryError(f"extractor 返回值不是有效 JSON: {exc}") from exc


def eval_usage_script(script: str, expression: str, timeout_seconds: int) -> str:
    ctx = quickjs.Context()
    ctx.set_time_limit(max(1, min(5, int(timeout_seconds or 2))))
    ctx.set_memory_limit(8 * 1024 * 1024)
    try:
        result = ctx.eval(f"const __usage_config = {script};\n{expression}")
    except quickjs.JSException as exc:
        raise UsageQueryError(f"脚本执行失败: {exc}") from exc
    if result is None:
        raise UsageQueryError("脚本返回空结果")
    return str(result)


def validate_request(request: dict[str, Any], config: UsageQueryConfig) -> None:
    url = str(request.get("url") or "").strip()
    method = str(request.get("method") or "GET").strip().upper()
    if method not in {"GET", "POST", "PUT", "PATCH", "DELETE"}:
        raise UsageQueryError(f"不支持的 HTTP 方法: {method}")
    parsed = urlsplit(url)
    if parsed.scheme not in {"http", "https"} or not parsed.netloc:
        raise UsageQueryError("request.url 必须是 http/https 完整 URL")
    if config.template_type != "custom" and config.base_url:
        base = urlsplit(config.base_url)
        if parsed.hostname != base.hostname or parsed.port != base.port:
            raise UsageQueryError("内置模板请求 URL 必须与 Base URL 同源")
    headers = request.get("headers") or {}
    if not isinstance(headers, dict):
        raise UsageQueryError("request.headers 必须是对象")
    for key, value in headers.items():
        if not isinstance(key, str) or not isinstance(value, str):
            raise UsageQueryError("request.headers 的键和值都必须是字符串")
    request["url"] = url
    request["method"] = method
    request["headers"] = headers


def open_usage_request(request: dict[str, Any], timeout_seconds: int) -> Any:
    body = request.get("body")
    data: bytes | None
    if body is None:
        data = None
    elif isinstance(body, bytes):
        data = body
    elif isinstance(body, str):
        data = body.encode("utf-8")
    else:
        data = json.dumps(body, ensure_ascii=False).encode("utf-8")
    req = urllib.request.Request(
        str(request["url"]),
        data=data,
        headers={str(k): str(v) for k, v in (request.get("headers") or {}).items()},
        method=str(request.get("method") or "GET").upper(),
    )
    try:
        with urllib.request.urlopen(req, timeout=normalize_timeout(timeout_seconds)) as response:
            raw = response.read()
    except urllib.error.HTTPError as exc:
        preview = exc.read(200).decode("utf-8", errors="replace")
        raise UsageQueryError(f"HTTP {exc.code}: {preview}") from exc
    except urllib.error.URLError as exc:
        raise UsageQueryError(f"请求失败: {exc.reason}") from exc
    text = raw.decode("utf-8", errors="replace")
    try:
        return json.loads(text)
    except json.JSONDecodeError as exc:
        raise UsageQueryError(f"响应不是有效 JSON: {exc}") from exc


def normalize_usage_data(data: Any) -> list[dict[str, Any]]:
    items = data if isinstance(data, list) else [data]
    if not items:
        raise UsageQueryError("extractor 返回的数组不能为空")
    normalized: list[dict[str, Any]] = []
    for index, item in enumerate(items):
        if not isinstance(item, dict):
            raise UsageQueryError(f"extractor 第 {index + 1} 项必须是对象")
        normalized.append(validate_usage_item(item))
    return normalized


def validate_usage_item(item: dict[str, Any]) -> dict[str, Any]:
    checks = {
        "isValid": (bool, type(None)),
        "invalidMessage": (str, type(None)),
        "remaining": (int, float, type(None)),
        "unit": (str, type(None)),
        "total": (int, float, type(None)),
        "used": (int, float, type(None)),
        "planName": (str, type(None)),
        "extra": (str, type(None)),
    }
    for field, expected in checks.items():
        if field in item and not isinstance(item[field], expected):
            raise UsageQueryError(f"{field} 字段类型错误")
    return {key: item.get(key) for key in checks if key in item}


def build_success_result(config: UsageQueryConfig, items: list[dict[str, Any]], queried_at: str) -> dict[str, Any]:
    primary = next((item for item in items if item.get("isValid", True) is not False), items[0])
    invalid_message = str(primary.get("invalidMessage") or "")
    is_valid = primary.get("isValid", True) is not False
    remaining = numeric_or_none(primary.get("remaining"))
    available = actual_available(remaining, config.upstream_multiplier)
    return {
        "account_id": config.account_id,
        "template_type": config.template_type,
        "success": bool(is_valid),
        "data": items,
        "error": "" if is_valid else invalid_message or "查询结果无效",
        "queried_at": queried_at,
        "plan_name": str(primary.get("planName") or ""),
        "extra": str(primary.get("extra") or ""),
        "remaining": remaining,
        "used": numeric_or_none(primary.get("used")),
        "total": numeric_or_none(primary.get("total")),
        "unit": str(primary.get("unit") or ""),
        "invalid_message": invalid_message,
        "upstream_multiplier": config.upstream_multiplier,
        "actual_available": available,
    }


def should_pause_for_depleted(result: dict[str, Any]) -> bool:
    available = numeric_or_none(result.get("actual_available"))
    return bool(result.get("success")) and available is not None and available <= 0


def is_query_due(
    config: UsageQueryConfig,
    result: dict[str, Any],
    now: datetime | None = None,
    interval_seconds: object | None = None,
    interval_minutes: object | None = None,
) -> bool:
    if interval_seconds is not None:
        interval = normalize_interval_seconds(interval_seconds)
    elif interval_minutes is not None:
        interval = normalize_interval(interval_minutes) * 60
    else:
        interval = normalize_interval(config.auto_query_interval_minutes) * 60
    if interval <= 0:
        return False
    queried_at = str(result.get("queried_at") or "")
    if not queried_at:
        return True
    try:
        parsed = datetime.fromisoformat(queried_at.replace("Z", "+00:00"))
    except ValueError:
        return True
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=timezone.utc)
    current = (now or datetime.now(timezone.utc)).astimezone(timezone.utc)
    return (current - parsed.astimezone(timezone.utc)).total_seconds() >= interval


def public_config(config: UsageQueryConfig) -> dict[str, Any]:
    payload = config.to_dict()
    payload["api_key"] = ""
    payload["access_token"] = ""
    payload["api_key_saved"] = bool(config.api_key)
    payload["access_token_saved"] = bool(config.access_token)
    payload["template_label"] = TEMPLATE_LABELS[config.template_type]
    return payload
