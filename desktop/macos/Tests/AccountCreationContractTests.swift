import Foundation
import os
import Testing
@testable import Sub2OpsMacOS

/// Mirrors `server/src/account_creation.rs::validate`, which answers anything outside this
/// contract with 422 before the request reaches Sub2API.
private enum CreationContract {
    static let common: Set<String> = [
        "name", "notes", "proxy_id", "concurrency", "load_factor", "priority",
        "rate_multiplier", "group_ids", "expires_at", "auto_pause_on_expired",
    ]
    static let createOnly: Set<String> = [
        "platform", "type", "credentials", "extra",
        "upstream_billing_probe_enabled", "upstream_billing_group_sync_enabled",
        "confirm_mixed_channel_risk",
    ]
    static let identity: Set<String> = [
        "api_key", "access_token", "refresh_token", "id_token", "token_type", "expires_at",
        "expires_in", "scope", "email", "email_address", "org_uuid", "account_uuid",
        "chatgpt_account_id", "chatgpt_user_id", "organization_id", "plan_type",
        "subscription_expires_at", "client_id", "project_id", "client_email", "tier_id",
        "oauth_type", "sub", "team_id", "subscription_tier", "entitlement_status",
        "aws_access_key_id", "aws_secret_access_key", "aws_session_token", "service_account_json",
    ]
    static let authExtra: Set<String> = [
        "email", "email_address", "name", "privacy_mode", "org_uuid", "account_uuid",
        "subscription_type", "subscription_tier", "entitlement_status",
        "drive_storage_limit", "drive_storage_usage", "drive_tier_updated_at",
    ]

    static func valueProblem(_ field: CreationFieldSpec, _ value: JSONValue) -> Bool {
        if value == .null { return false }
        let choices = (field.choices ?? []).map(JSONValue.string)
        switch field.kind {
        case "boolean": return value.boolValue == nil
        case "stringbool": return !["true", "false"].contains(value.stringValue ?? "")
        case "number":
            guard let number = value.doubleValue else { return true }
            return (field.min.map { number < $0 } ?? false) || (field.max.map { number > $0 } ?? false)
        case "tls": return (value.intValue ?? 0) <= 0
        case "select": return !choices.contains(value)
        case "tier", "text", "url": return value.stringValue == nil
        case "strings":
            guard let items = value.arrayValue, !items.isEmpty else { return true }
            return !items.allSatisfy(choices.contains)
        case "numbers":
            guard let items = value.arrayValue else { return true }
            let low = Int64(field.min ?? 0), high = field.max.map { Int64($0) } ?? .max
            return !items.allSatisfy { item in
                if case .int(let number) = item { return number >= low && number <= high }
                return false
            }
        case "mapping", "headers", "protocols":
            guard let object = value.objectValue else { return true }
            return !object.values.allSatisfy { $0.stringValue != nil }
        case "pause-rules", "protocol-rules":
            guard let items = value.arrayValue else { return true }
            return !items.allSatisfy { $0.objectValue != nil }
        default: return true
        }
    }

    static func configurationProblems(_ body: [String: JSONValue], platform: String, type: String, auth: Bool) -> [String] {
        var problems: [String] = []
        for section in ["credentials", "extra"] {
            guard let object = body[section]?.objectValue else { continue }
            for (key, value) in object {
                let path = "\(section).\(key)"
                let field = AccountCreationCatalog.shared.fields.first { field in
                    field.path == path
                        && (field.platforms?.contains(platform) ?? true)
                        && (field.types?.contains(type) ?? true)
                }
                if let field {
                    if valueProblem(field, value) { problems.append("\(path) 格式无效") }
                    if field.kind == "headers", let map = value.objectValue {
                        let invalidVar = map.values.contains { val in
                            !AccountCreationFormDraft.isValidHeaderOverrideValue(
                                val.stringValue ?? "",
                                allowVariables: type == "apikey"
                            )
                        }
                        if invalidVar { problems.append("\(path) 模板变量无效") }
                    }
                } else if section == "extra" && platform == "openai"
                    && key == "openai_\(type)_responses_websockets_v2_enabled"
                {
                    if value.boolValue == nil { problems.append("\(path) 需为布尔值") }
                } else if auth && (section == "credentials" ? identity.contains(key) : authExtra.contains(key)) {
                    if value.objectValue != nil || value.arrayValue != nil { problems.append("\(path) 需为标量") }
                } else {
                    problems.append("\(path) 不适用于 \(platform)/\(type)")
                }
            }
        }
        return problems
    }

    static func commonProblems(_ body: [String: JSONValue]) -> [String] {
        var problems: [String] = []
        for key in ["proxy_id", "load_factor", "expires_at"] {
            if let value = body[key], value != .null, (value.intValue ?? 0) < 1 { problems.append("\(key) 无效") }
        }
        if let value = body["concurrency"], (value.intValue ?? 0) < 1 { problems.append("concurrency 无效") }
        if let value = body["priority"], (value.intValue ?? -1) < 0 { problems.append("priority 无效") }
        if let value = body["rate_multiplier"], (value.doubleValue ?? -1) < 0 { problems.append("rate_multiplier 无效") }
        if let value = body["auto_pause_on_expired"], value.boolValue == nil { problems.append("auto_pause_on_expired 无效") }
        if let value = body["group_ids"], value.arrayValue == nil { problems.append("group_ids 无效") }
        return problems
    }

    static func createProblems(_ body: [String: JSONValue]) -> [String] {
        var problems = body.keys.filter { !common.union(createOnly).contains($0) }.map { "未知字段 \($0)" }
        for key in ["name", "platform", "type", "credentials"] where body[key] == nil {
            problems.append("缺少 \(key)")
        }
        if (body["name"]?.stringValue ?? "").trimmingCharacters(in: .whitespaces).isEmpty { problems.append("name 为空") }
        problems += configurationProblems(
            body,
            platform: body["platform"]?.stringValue ?? "",
            type: body["type"]?.stringValue ?? "",
            auth: true
        )
        return (problems + commonProblems(body)).sorted()
    }

    static func importProblems(_ body: [String: JSONValue], platform: String, secretKey: String) -> [String] {
        let allowed = common.union(["extra", "credential_extras", "credentials", secretKey])
            .union(secretKey == "content" ? ["update_existing"] : [])
        var problems = body.keys.filter { !allowed.contains($0) }.map { "未知字段 \($0)" }
        if body[secretKey] == nil { problems.append("缺少 \(secretKey)") }
        var config = body
        if let extras = body["credential_extras"] { config["credentials"] = extras }
        problems += configurationProblems(config, platform: platform, type: "oauth", auth: false)
        return (problems + commonProblems(body)).sorted()
    }
}

private let serviceAccountFixture =
    #"{"type":"service_account","project_id":"demo-project","private_key":"fixture-private-key","client_email":"ops@example.invalid"}"#

extension Sub2OpsMacOSTestRoot {
@Suite("Account creation request contract", .serialized)
struct AccountCreationContractTests {
    private func filledDraft(platform: String, type: String) -> AccountCreationFormDraft {
        var draft = AccountCreationFormDraft()
        draft.applyPlatformDefaults(for: platform, authType: type)
        draft.name = "契约测试账号"
        draft.apiKey = "sk-example-invalid"
        draft.awsAccessKeyId = "AKIAEXAMPLEINVALID"
        draft.awsSecretAccessKey = "secret-example-invalid"
        draft.serviceAccountJSON = serviceAccountFixture
        if platform == "antigravity" && type == "apikey" {
            draft.setValue("credentials.base_url", .string("https://upstream.example.invalid/v1"))
        }
        return draft
    }

    @Test("Every platform and type builds a body the server accepts")
    func everyPlatformTypeMatchesServerContract() throws {
        let identity: [String: JSONValue] = ["access_token": .string("tok-example"), "email": .string("ops@example.invalid")]
        for platform in AccountCreationCatalog.shared.platforms {
            for type in platform.types {
                let draft = filledDraft(platform: platform.id, type: type)
                let payload = try draft.buildPayload(
                    identityCredentials: draft.isOAuthFlow ? identity : nil,
                    identityExtra: draft.isOAuthFlow ? ["email": .string("ops@example.invalid")] : nil
                )
                #expect(CreationContract.createProblems(payload) == [], "\(platform.id)/\(type)")
                #expect(payload["schedulable"] == nil)
            }
        }
    }

    @Test("Advanced options stay inside the contract once enabled")
    func advancedOptionsMatchServerContract() throws {
        for platform in AccountCreationCatalog.shared.platforms {
            for type in platform.types {
                var draft = filledDraft(platform: platform.id, type: type)
                // Turn on every boolean so each conditional field is exercised.
                for _ in 0..<2 {
                    for field in draft.visibleFields where field.kind == "boolean" {
                        draft.setValue(field.path, .bool(true))
                    }
                }
                for field in draft.visibleFields {
                    switch field.kind {
                    case "number" where draft.values[field.path] == nil:
                        draft.setValue(field.path, .int(Int64(field.min ?? 1)))
                    case "numbers":
                        draft.setValue(field.path, .string("500, 502，503"))
                    case "url" where draft.values[field.path] == nil:
                        draft.setValue(field.path, .string("https://relay.example.invalid"))
                    default:
                        break
                    }
                }
                draft.headers = [KeyValuePairDraft(key: "X-Trace", value: "fixture")]
                draft.pauseRules = [PauseRuleDraft(keywords: "rate limit， overloaded", description: "限流")]
                draft.expiresAtText = "2030-01-02 03:04"
                let payload = try draft.buildPayload(
                    identityCredentials: draft.isOAuthFlow ? ["access_token": .string("tok-example")] : nil
                )
                #expect(CreationContract.createProblems(payload) == [], "\(platform.id)/\(type)")
                #expect(payload["expires_at"] == .int(AccountCreationFormDraft.parseExpiry("2030-01-02 03:04") ?? 0))
                if let rules = payload["credentials"]?["temp_unschedulable_rules"]?.arrayValue {
                    #expect(rules[0]["keywords"] == .array([.string("rate limit"), .string("overloaded")]))
                    #expect(rules[0]["error_code"] == .int(429))
                }
            }
        }
    }

    @Test("Native imports send configuration as credential_extras without identity keys")
    func importParametersMatchServerContract() throws {
        var codex = filledDraft(platform: "openai", type: "oauth")
        codex.name = ""
        var session = try codex.importParameters()
        #expect(session["platform"] == nil && session["type"] == nil && session["credentials"] == nil)
        session["content"] = .string("{}")
        session["update_existing"] = .bool(true)
        #expect(CreationContract.importProblems(session, platform: "openai", secretKey: "content") == [])

        var pat = try codex.importParameters()
        pat["access_token"] = .string("pat-example")
        #expect(CreationContract.importProblems(pat, platform: "openai", secretKey: "access_token") == [])

        let grok = filledDraft(platform: "grok", type: "oauth")
        var sso = try grok.importParameters()
        sso["credentials"] = sso.removeValue(forKey: "credential_extras") ?? .object([:])
        sso["sso_tokens"] = .array([.string("sso-example")])
        #expect(CreationContract.importProblems(sso, platform: "grok", secretKey: "sso_tokens") == [])
    }

    @Test("Protocol defaults follow the account mode unless the user edited them")
    func protocolDefaultsFollowMode() throws {
        var draft = filledDraft(platform: "kimi", type: "apikey")
        #expect(draft.values["credentials.api_protocol"] == .string("adaptive"))
        #expect(draft.protocolURLs["chat_completions"] == "https://api.moonshot.cn/v1")
        #expect(draft.visibleFields.contains { $0.path == "credentials.api_base_urls" })
        #expect(!draft.visibleFields.contains { $0.path == "credentials.base_url" })

        draft.setValue("credentials.account_mode", .string("coding"))
        #expect(draft.protocolURLs["chat_completions"] == "https://api.kimi.com/coding/v1")
        #expect(draft.protocolURLs["anthropic"] == "https://api.kimi.com/coding")

        draft.protocolURLs["anthropic"] = "https://relay.example.invalid/anthropic"
        draft.setValue("credentials.account_mode", .string("payg"))
        #expect(draft.protocolURLs["anthropic"] == "https://relay.example.invalid/anthropic")
        #expect(draft.protocolURLs["chat_completions"] == "https://api.moonshot.cn/v1")

        let adaptive = try draft.buildPayload()
        #expect(adaptive["credentials"]?["account_mode"] == .string("payg"))
        #expect(adaptive["credentials"]?["base_url"] == .string("https://api.moonshot.cn/v1"))

        draft.setValue("credentials.api_protocol", .string("anthropic"))
        #expect(draft.values["credentials.base_url"] == .string("https://api.moonshot.cn/anthropic"))
        let single = try draft.buildPayload()
        #expect(single["credentials"]?["api_base_urls"] == nil)
        #expect(single["credentials"]?["base_url"] == .string("https://api.moonshot.cn/anthropic"))

        draft.setValue("credentials.api_protocol", .string("adaptive"))
        draft.protocolURLs["responses"] = "ftp://relay.example.invalid"
        #expect(draft.validate() == "协议地址需为 HTTP 或 HTTPS 地址")
        draft.protocolURLs["responses"] = ""
        #expect(draft.validate() == "请填写各协议的有效 API 地址")
    }

    @Test("Model routing rules are normalised and replaced with the mode defaults")
    func protocolRulesFollowMode() throws {
        var draft = filledDraft(platform: "opencode_go", type: "apikey")
        #expect(draft.values["credentials.account_mode"] == .string("zen"))
        #expect(draft.protocolRules.contains { $0.pattern == "claude-*" && $0.preferred == "anthropic" })
        draft.setValue("credentials.account_mode", .string("go"))
        #expect(!draft.protocolRules.contains { $0.pattern == "claude-*" })
        #expect(draft.protocolURLs["chat_completions"] == "https://opencode.ai/zen/go/v1")

        draft.protocolRules = [ProtocolRuleDraft(pattern: " GPT-* ", preferred: "responses", also: ["chat_completions"])]
        draft.setValue("credentials.account_mode", .string("zen"))
        #expect(draft.protocolRules.count == 1)
        let payload = try draft.buildPayload()
        #expect(payload["credentials"]?["protocol_rules"] == .array([.object([
            "pattern": .string("gpt-*"),
            "protocol": .string("responses"),
            "protocols": .array([.string("responses"), .string("chat_completions")]),
        ])]))
        #expect(CreationContract.createProblems(payload) == [])

        draft.protocolRules = [ProtocolRuleDraft(pattern: "", preferred: "responses")]
        #expect(draft.validate() == "请填写模型匹配与有效协议")
    }

    @Test("Field validation reports the same problems as the web client")
    func fieldValidationProblems() throws {
        var draft = filledDraft(platform: "anthropic", type: "apikey")
        #expect(draft.validate() == "")

        draft.mappings = [KeyValuePairDraft(key: "claude-*", value: "claude-opus-4-6")]
        #expect(draft.validate() == "")
        draft.mappings = [KeyValuePairDraft(key: "claude-opus", value: "claude-*")]
        #expect(draft.validate().hasSuffix("通配白名单请将请求和转发模型填为相同值"))
        draft.mappings = [KeyValuePairDraft(key: "claude opus", value: "claude-opus-4-6")]
        #expect(draft.validate().contains("需使用有效模型 ID"))
        draft.mappings = [KeyValuePairDraft(key: "a", value: "a"), KeyValuePairDraft(key: "a", value: "b")]
        #expect(draft.validate().hasSuffix("存在重复名称"))
        draft.mappings = [KeyValuePairDraft(key: "a", value: "")]
        #expect(draft.validate().hasSuffix("存在未填项目"))
        draft.mappings = []

        draft.setValue("credentials.header_override_enabled", .bool(true))
        draft.headers = [KeyValuePairDraft(key: "Authorization", value: "Bearer fixture")]
        #expect(draft.validate() == "请求头名称、值或重复项无效，认证头不能覆写")
        draft.headers = [KeyValuePairDraft(key: "X-Trace", value: "ok")]
        #expect(draft.validate() == "")

        draft.setValue("credentials.temp_unschedulable_enabled", .bool(true))
        draft.pauseRules = [PauseRuleDraft(errorCode: "99", keywords: "limit")]
        #expect(draft.validate() == "临时不可调度规则需包含状态码、关键词和正整数分钟")
        draft.pauseRules = [PauseRuleDraft(keywords: " , ")]
        #expect(draft.validate() == "临时不可调度规则需包含状态码、关键词和正整数分钟")
        draft.pauseRules = [PauseRuleDraft(keywords: "limit")]
        #expect(draft.validate() == "")

        draft.setValue("credentials.custom_error_codes_enabled", .bool(true))
        draft.setValue("credentials.custom_error_codes", .string("429, abc"))
        #expect(draft.validate().hasSuffix("需填写有效整数"))
        draft.setValue("credentials.custom_error_codes", .string("429 529,500"))
        #expect(draft.validate() == "")
        #expect(draft.customErrorRisks == [429, 529])
        let payload = try draft.buildPayload()
        #expect(payload["credentials"]?["custom_error_codes"] == .array([.int(429), .int(529), .int(500)]))

        draft.setValue("credentials.base_url", .string("ftp://relay.example.invalid"))
        #expect(draft.validate().hasSuffix("需为 HTTP 或 HTTPS 地址"))
        draft.setValue("credentials.base_url", .string("https://api.anthropic.com"))
        draft.expiresAtText = "明天"
        #expect(draft.validate() == "到期时间格式应为 YYYY-MM-DD HH:mm")
        draft.expiresAtText = ""
        draft.apiKey = "  "
        #expect(draft.validate() == "请输入 API Key")
        #expect(draft.validate(requireCredentials: false) == "")
    }

    @Test("Platform specific credentials are normalised like the web client")
    func platformSpecificCredentials() throws {
        let gemini = try filledDraft(platform: "gemini", type: "apikey").buildPayload()
        #expect(gemini["credentials"]?["tier_id"] == .string("aistudio_free"))
        #expect(gemini["credentials"]?["explicit_cache_upstream"] == nil)

        var relay = filledDraft(platform: "gemini", type: "apikey")
        relay.setValue("credentials.base_url", .string("https://relay.example.invalid"))
        #expect(relay.visibleFields.contains { $0.path == "credentials.explicit_cache_upstream" })
        #expect(try relay.buildPayload()["credentials"]?["tier_id"] == nil)

        let vertex = try filledDraft(platform: "gemini", type: "service_account").buildPayload()
        #expect(vertex["credentials"]?["tier_id"] == .string("vertex"))
        #expect(vertex["credentials"]?["client_email"] == .string("ops@example.invalid"))
        #expect(vertex["credentials"]?["project_id"] == .string("demo-project"))
        #expect(vertex["credentials"]?["auth_mode"] == nil && vertex["credentials"]?["api_key"] == nil)

        var vertexKey = filledDraft(platform: "gemini", type: "service_account")
        vertexKey.setValue("credentials.auth_mode", .string("apikey"))
        #expect(vertexKey.validate() == "请输入 API Key")
        vertexKey.apiKey = "vertex-key-example"
        let vertexKeyPayload = try vertexKey.buildPayload()
        #expect(vertexKeyPayload["credentials"]?["service_account_json"] == nil)
        #expect(vertexKeyPayload["credentials"]?["api_key"] == .string("vertex-key-example"))
        #expect(vertexKeyPayload["credentials"]?["location"] == .string("global"))

        var bedrock = filledDraft(platform: "anthropic", type: "bedrock")
        #expect(try bedrock.buildPayload()["credentials"]?["api_key"] == nil)
        bedrock.setValue("credentials.auth_mode", .string("apikey"))
        let bedrockKey = try bedrock.buildPayload()
        #expect(bedrockKey["credentials"]?["api_key"] == .string("sk-example-invalid"))
        #expect(bedrockKey["credentials"]?["aws_access_key_id"] == nil)

        var openai = filledDraft(platform: "openai", type: "apikey")
        #expect(try openai.buildPayload()["extra"]?["openai_image_size_tiers"] == nil)
        openai.setValue("extra.openai_image_size_tiers", .array([.string("4K"), .string("1K")]))
        let tiers = try openai.buildPayload()
        #expect(tiers["extra"]?["openai_image_size_tiers"] == .array([.string("1K"), .string("4K")]))
        #expect(tiers["extra"]?["openai_apikey_responses_websockets_v2_enabled"]?.boolValue != nil)

        var antigravity = filledDraft(platform: "antigravity", type: "apikey")
        antigravity.setValue("credentials.base_url", nil)
        #expect(antigravity.validate() == "请输入上游 API 地址")
    }

    @Test("Reusing a source replaces configuration and clears what the source lacks")
    func reuseReplacesConfiguration() throws {
        var draft = filledDraft(platform: "anthropic", type: "apikey")
        draft.setValue("credentials.pool_mode", .bool(true))
        draft.headers = [KeyValuePairDraft(key: "X-Old", value: "1")]
        try draft.applyReusedSource([
            "platform": .string("anthropic"),
            "type": .string("apikey"),
            "notes": .null,
            "proxy_id": .null,
            "load_factor": .int(4),
            "expires_at": .int(1_899_999_960),
            "auto_pause_on_expired": .bool(false),
            "credentials": .object([
                "base_url": .string("https://relay.example.invalid"),
                "temp_unschedulable_enabled": .bool(true),
                "temp_unschedulable_rules": .array([.object([
                    "error_code": .int(503),
                    "keywords": .array([.string("busy")]),
                    "duration_minutes": .int(5),
                ])]),
            ]),
        ])
        #expect(draft.name == "契约测试账号")
        #expect(draft.apiKey == "sk-example-invalid")
        #expect(draft.notes == "" && draft.proxyId == nil && draft.loadFactor == "4")
        #expect(AccountCreationFormDraft.parseExpiry(draft.expiresAtText) == 1_899_999_960)
        #expect(draft.values["credentials.pool_mode"] == nil)
        #expect(draft.values["auto_pause_on_expired"] == .bool(false))
        #expect(draft.headers.isEmpty && draft.mappings.isEmpty)
        #expect(draft.pauseRules.count == 1 && draft.pauseRules[0].scope == "account" && draft.pauseRules[0].errorCode == "503")
        let payload = try draft.buildPayload()
        #expect(CreationContract.createProblems(payload) == [])
        #expect(payload["auto_pause_on_expired"] == .bool(false))
    }

    @Test("Model directory requests carry the purpose the client allowlist requires")
    func modelDirectoryUsesModelTestPurpose() async throws {
        let seen = OSAllocatedUnfairLock(initialState: [String]())
        RustBridge.shared.setMockHandler { _, payload in
            seen.withLock { $0.append(payload) }
            return #"[{"id":"gpt-5.4","display_name":"GPT-5.4"}]"#
        }
        defer { RustBridge.shared.setMockHandler(nil) }
        let directory = await ModelDirectoryStore()
        let models = try await directory.readModels(accountId: 7)
        _ = try await directory.readModels(accountId: 7, refresh: true)
        #expect(models.map(\.id) == ["gpt-5.4"])
        let requests = seen.withLock { $0 }
        #expect(requests.count == 2)
        #expect(requests[0].contains("/accounts/7/models?purpose=model_test\""))
        #expect(requests[1].contains("/accounts/7/models?purpose=model_test&refresh=true\""))
    }

    @Test("Sub2API 0.2.25 passthrough model filter, group sync linkage and header override variables")
    func sub2api0225AccountCreationFeatures() throws {
        var openai = filledDraft(platform: "openai", type: "apikey")
        openai.setValue("extra.openai_passthrough", .bool(true))
        let plainPassthrough = try openai.buildPayload()
        #expect(plainPassthrough["credentials"]?["model_mapping"] == nil)
        #expect(plainPassthrough["extra"]?["openai_passthrough_model_filter"] == nil)

        openai.setValue("extra.openai_passthrough_model_filter", .bool(true))
        let filteredPassthrough = try openai.buildPayload()
        #expect(filteredPassthrough["extra"]?["openai_passthrough_model_filter"] == .bool(true))
        #expect(filteredPassthrough["credentials"]?["model_mapping"]?["gpt-6.2-sol"] == .string("gpt-6.2-sol"))

        openai.setValue("upstream_billing_probe_enabled", .bool(false))
        openai.setValue("upstream_billing_group_sync_enabled", .bool(true))
        #expect(openai.values["upstream_billing_probe_enabled"] == .bool(true))
        #expect(try openai.buildPayload()["upstream_billing_group_sync_enabled"] == .bool(true))
        openai.setValue("upstream_billing_probe_enabled", .bool(false))
        #expect(openai.values["upstream_billing_group_sync_enabled"] == .bool(false))
        #expect(try openai.buildPayload()["upstream_billing_group_sync_enabled"] == nil)

        var reused = filledDraft(platform: "openai", type: "apikey")
        try reused.applyReusedSource([
            "platform": .string("openai"),
            "type": .string("apikey"),
            "upstream_billing_group_sync_enabled": .bool(true),
        ])
        #expect(reused.values["upstream_billing_group_sync_enabled"] == nil)

        openai.setValue("credentials.header_override_enabled", .bool(true))
        openai.headers = [KeyValuePairDraft(key: "X-Sub2Api-Identity", value: "{{user_id}}:{{ request_id }}")]
        #expect(openai.validate() == "")
        #expect(CreationContract.createProblems(try openai.buildPayload()) == [])

        openai.headers = [KeyValuePairDraft(key: "X-Sub2Api-Identity", value: "{{unknown}}")]
        #expect(openai.validate().contains("模板变量"))

        var grokOAuth = filledDraft(platform: "grok", type: "oauth")
        grokOAuth.setValue("credentials.header_override_enabled", .bool(true))
        grokOAuth.headers = [KeyValuePairDraft(key: "X-Trace", value: "{{user_id}}")]
        #expect(grokOAuth.validate(requireCredentials: false).contains("模板变量"))
    }
}
}
