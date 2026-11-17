import Foundation

public struct CreationPlatformSpec: Codable, Sendable, Equatable, Identifiable {
    public var id: String
    public var label: String
    public var types: [String]
    public var base_url: String
}

public struct CreationModelPreset: Codable, Sendable, Equatable, Identifiable {
    public var label: String
    public var from: String
    public var to: String

    public var id: String { "\(label):\(from):\(to)" }
}

public struct CreationProtocolRule: Codable, Sendable, Equatable, Identifiable {
    public var id: UUID = UUID()
    public var pattern: String
    public var protocolName: String
    public var protocols: [String]?

    enum CodingKeys: String, CodingKey {
        case pattern
        case protocolName = "protocol"
        case protocols
    }

    public init(pattern: String = "", protocolName: String = "responses", protocols: [String]? = nil) {
        self.pattern = pattern
        self.protocolName = protocolName
        self.protocols = protocols
    }
}

public struct CreationProviderModeProfile: Codable, Sendable, Equatable {
    public var mode: String
    public var base_urls: [String: String]
    public var protocol_rules: [CreationProtocolRule]?
}

public struct CreationProviderProfile: Codable, Sendable, Equatable {
    public var default_mode: String
    public var routing: String
    public var responses_path: String?
    public var model_catalog: Bool?
    public var modes: [CreationProviderModeProfile]
}

public struct CreationFieldSpec: Codable, Sendable, Equatable, Identifiable {
    public var path: String
    public var label: String
    public var kind: String
    public var group: String
    public var platforms: [String]?
    public var types: [String]?
    public var defaultValue: JSONValue?
    public var choices: [String]?
    public var when: [String: JSONValue]?
    public var min: Double?
    public var max: Double?

    public var id: String {
        "\(path):\(platforms?.joined(separator: ",") ?? "*")"
    }

    enum CodingKeys: String, CodingKey {
        case path, label, kind, group, platforms, types, choices, when, min, max
        case defaultValue = "default"
    }
}

public struct AccountCreationCatalog: Codable, Sendable {
    public var upstream: String
    public var platforms: [CreationPlatformSpec]
    public var fields: [CreationFieldSpec]
    public var models: [String: [String]]?
    public var blocked_headers: [String]?
    public var mapping_presets: [String: [CreationModelPreset]]
    public var provider_profiles: [String: CreationProviderProfile]

    public static let choiceLabels: [String: String] = [
        "service_account": "Service Account JSON",
        "model": "模型",
        "account": "账号",
        "sigv4": "AWS Signature V4",
        "apikey": "API Key",
        "payg": "按量付费",
        "coding": "Coding Plan",
        "zen": "Zen",
        "go": "Go",
        "adaptive": "自动选择协议",
        "chat_completions": "Chat Completions",
        "anthropic": "Anthropic",
        "responses": "Responses",
        "code_assist": "GCP Code Assist",
        "google_one": "Google One",
        "ai_studio": "AI Studio",
        "google_one_free": "Google One 免费",
        "google_ai_pro": "Google AI Pro",
        "google_ai_ultra": "Google AI Ultra",
        "gcp_standard": "GCP Standard",
        "gcp_enterprise": "GCP Enterprise",
        "aistudio_free": "AI Studio 免费",
        "aistudio_paid": "AI Studio 付费",
        "rolling": "滚动窗口",
        "fixed": "固定值",
        "percentage": "百分比",
        "off": "关闭",
        "device": "设备",
        "session": "会话",
        "full": "完整",
        "auto": "自动",
        "force_on": "强制开启",
        "force_off": "强制关闭",
        "force_responses": "强制 Responses",
        "force_chat_completions": "强制 Chat Completions",
        "ctx_pool": "上下文池",
        "passthrough": "透传",
        "http_bridge": "HTTP Bridge",
        "enabled": "开启",
        "disabled": "关闭",
        "default": "跟随全局",
        "x_api_key": "X-API-Key",
        "authorization_bearer": "Authorization Bearer",
        "tiered": "分层",
        "sticky_exempt": "粘性豁免",
        "throttle": "节流",
        "serialize": "串行",
        "embeddings": "Embeddings",
        "images": "Images",
        "seedance": "Seedance (Ark)",
    ]

    public static func choiceLabel(_ raw: String) -> String {
        if raw.isEmpty { return "关闭" }
        return choiceLabels[raw] ?? raw
    }

    public static let shared: AccountCreationCatalog = {
        let data: Data
        #if SWIFT_PACKAGE
        let url = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .appendingPathComponent("server/src/account_creation_fields.json")
        data = (try? Data(contentsOf: url)) ?? Data("{}".utf8)
        #else
        data = Data(AccountCreationCatalogGenerated.json.utf8)
        #endif
        if let decoded = try? JSONDecoder().decode(AccountCreationCatalog.self, from: data) {
            return decoded
        }
        return AccountCreationCatalog(
            upstream: "",
            platforms: [],
            fields: [],
            models: nil,
            blocked_headers: nil,
            mapping_presets: [:],
            provider_profiles: [:]
        )
    }()
}

public struct OAuthAuthorizationSession: Codable, Sendable, Equatable {
    public var auth_url: String
    public var session_id: String
    public var state: String?
}

public struct CreationOptionsResponse: Codable, Sendable, Equatable {
    public struct GroupOption: Codable, Sendable, Equatable, Identifiable {
        public var id: Int
        public var name: String
        public var platform: String
        public var status: String?
        public var long_context_pricing_enabled: Bool?
    }

    public struct ProxyOption: Codable, Sendable, Equatable, Identifiable {
        public var id: Int
        public var name: String
        public var status: String?
    }

    public var groups: [GroupOption]?
    public var proxies: [ProxyOption]?
}

public struct CreationCreatedResponse: Codable, Sendable, Equatable {
    public struct CreatedAccount: Codable, Sendable, Equatable {
        public var id: Int
        public var name: String
        public var platform: String
        public var type: String
    }

    public var account: CreatedAccount
    public var warnings: [String]?
}

public struct CreationTLSProfileOption: Codable, Sendable, Equatable, Identifiable {
    public var id: Int
    public var name: String
}

public struct KeyValuePairDraft: Sendable, Equatable, Identifiable {
    public var id: UUID = UUID()
    public var key: String
    public var value: String

    public init(key: String = "", value: String = "") {
        self.key = key
        self.value = value
    }
}

public struct AccountCreationFormDraft: Sendable, Equatable {
    public var name: String = ""
    public var notes: String = ""
    public var platform: String = "anthropic"
    public var type: String = "oauth"
    public var apiKey: String = ""
    public var serviceAccountJSON: String = ""
    public var awsAccessKeyId: String = ""
    public var awsSecretAccessKey: String = ""
    public var awsSessionToken: String = ""
    public var proxyId: Int? = nil
    public var concurrency: String = "10"
    public var loadFactor: String = ""
    public var priority: String = "1"
    public var rateMultiplier: String = "1"
    public var expiresAtText: String = ""
    public var schedulable: Bool = true
    public var groupIds: Set<Int> = []
    public var values: [String: JSONValue] = [:]
    public var mappings: [KeyValuePairDraft] = []
    public var compactMappings: [KeyValuePairDraft] = []
    public var headers: [KeyValuePairDraft] = []
    public var oauthCredentials: [String: JSONValue]? = nil
    public var oauthExtra: [String: JSONValue]? = nil

    public init() {
        let initialPlatform = AccountCreationCatalog.shared.platforms.first?.id ?? "anthropic"
        let initialType = AccountCreationCatalog.shared.platforms.first?.types.first ?? "oauth"
        applyPlatformDefaults(for: initialPlatform, authType: initialType)
    }

    public var isOAuthFlow: Bool {
        type == "oauth" || type == "setup-token"
    }

    public var authProvider: String {
        type == "setup-token" ? "setup-token" : platform
    }

    public var authMethods: [(id: String, label: String)] {
        var list: [(String, String)] = [("manual", "授权码")]
        if platform == "anthropic" {
            list.append(("cookie", "Cookie / Session Key"))
        }
        if ["openai", "antigravity", "grok"].contains(platform) {
            list.append(("refresh", "Refresh Token"))
        }
        if platform == "openai" {
            list.append(contentsOf: [
                ("mobile-refresh", "Mobile Refresh Token"),
                ("codex-session", "Codex Session"),
                ("agent-identity", "Agent Identity"),
                ("codex-pat", "Codex PAT"),
            ])
        }
        if platform == "grok" {
            list.append(("grok-sso", "SSO Cookie"))
        }
        return list
    }

    public var authConfigBody: [String: JSONValue] {
        var body: [String: JSONValue] = [:]
        if let proxyId {
            body["proxy_id"] = .int(Int64(proxyId))
        }
        if platform == "gemini" {
            for key in ["project_id", "oauth_type", "tier_id"] {
                if let val = values["credentials.\(key)"], val != .null {
                    body[key] = val
                }
            }
        }
        return body
    }

    public var tierChoices: [String] {
        let kind = type == "apikey" ? "ai_studio" : (values["credentials.oauth_type"]?.stringValue ?? "")
        if kind == "google_one" {
            return ["google_one_free", "google_ai_pro", "google_ai_ultra"]
        }
        if kind == "code_assist" {
            return ["gcp_standard", "gcp_enterprise"]
        }
        return ["aistudio_free", "aistudio_paid"]
    }

    public mutating func selectPlatform(_ nextPlatform: String) {
        let catalog = AccountCreationCatalog.shared
        let spec = catalog.platforms.first(where: { $0.id == nextPlatform })
        let nextType = spec?.types.contains(type) == true ? type : (spec?.types.first ?? "apikey")
        applyPlatformDefaults(for: nextPlatform, authType: nextType)
    }

    public mutating func selectType(_ nextType: String) {
        applyPlatformDefaults(for: platform, authType: nextType)
    }

    public mutating func applyPlatformDefaults(for nextPlatform: String, authType: String) {
        platform = nextPlatform
        type = authType
        concurrency = nextPlatform == "grok" ? "1" : "10"
        loadFactor = ""
        priority = "1"
        rateMultiplier = "1"
        groupIds = []
        oauthCredentials = nil
        oauthExtra = nil
        mappings = []
        compactMappings = []
        headers = []

        var seeded: [String: JSONValue] = [:]
        let catalog = AccountCreationCatalog.shared
        if let platSpec = catalog.platforms.first(where: { $0.id == nextPlatform }),
           authType == "apikey",
           !platSpec.base_url.isEmpty
        {
            seeded["credentials.base_url"] = .string(platSpec.base_url)
        }
        for field in catalog.fields where matchesScope(field) {
            if let defVal = field.defaultValue {
                seeded[field.path] = defVal
            }
        }
        if nextPlatform == "gemini" && authType == "oauth" {
            values = seeded
            seeded["credentials.tier_id"] = .string(tierChoices[0])
        }
        if let profile = catalog.provider_profiles[nextPlatform], authType == "apikey" {
            seeded["credentials.account_mode"] = .string(profile.default_mode)
            if let modeSpec = profile.modes.first(where: { $0.mode == profile.default_mode }) {
                seeded["credentials.api_base_urls"] = .object(modeSpec.base_urls.mapValues(JSONValue.string))
                let proto = seeded["credentials.api_protocol"]?.stringValue ?? ""
                if let url = modeSpec.base_urls[proto] ?? modeSpec.base_urls["chat_completions"] {
                    seeded["credentials.base_url"] = .string(url)
                }
            }
        }
        if catalog.fields.contains(where: { $0.path == "credentials.model_mapping" && matchesScope($0) }),
           !["grok", "antigravity"].contains(nextPlatform),
           let presetModels = catalog.models?[nextPlatform]
        {
            mappings = presetModels.map { KeyValuePairDraft(key: $0, value: $0) }
        }
        values = seeded
    }

    public mutating func applyReusedSource(_ source: [String: JSONValue]) throws {
        guard source["platform"]?.stringValue == platform,
              source["type"]?.stringValue == type
        else {
            throw RustBridgeError("来源账号平台或类型已变化，请重新选择")
        }
        if let n = source["notes"]?.stringValue { notes = n }
        proxyId = source["proxy_id"]?.intValue.map(Int.init)
        if let c = source["concurrency"]?.intValue { concurrency = String(c) }
        loadFactor = source["load_factor"]?.intValue.map(String.init) ?? ""
        if let p = source["priority"]?.intValue { priority = String(p) }
        if let r = source["rate_multiplier"]?.doubleValue {
            rateMultiplier = r == floor(r) ? String(Int(r)) : String(r)
        } else if let ri = source["rate_multiplier"]?.intValue {
            rateMultiplier = String(ri)
        }
        if let gArr = source["group_ids"]?.arrayValue {
            groupIds = Set(gArr.compactMap { $0.intValue.map(Int.init) })
        }
        let creds = source["credentials"]?.objectValue ?? [:]
        let extra = source["extra"]?.objectValue ?? [:]
        for field in AccountCreationCatalog.shared.fields where matchesScope(field) {
            if field.path == "credentials.model_mapping" {
                let mapObj = creds["model_mapping"]?.objectValue ?? [:]
                mappings = mapObj.keys.sorted().compactMap { k in
                    creds["model_mapping"]?[k]?.stringValue.map { KeyValuePairDraft(key: k, value: $0) }
                }
            } else if field.path == "credentials.header_overrides" {
                let hObj = creds["header_overrides"]?.objectValue ?? [:]
                headers = hObj.keys.sorted().compactMap { k in
                    hObj[k]?.stringValue.map { KeyValuePairDraft(key: k, value: $0) }
                }
            } else if field.path.hasPrefix("credentials.") {
                let k = String(field.path.dropFirst("credentials.".count))
                if let val = creds[k] { values[field.path] = val }
            } else if field.path.hasPrefix("extra.") {
                let k = String(field.path.dropFirst("extra.".count))
                if let val = extra[k] { values[field.path] = val }
            } else if let val = source[field.path] {
                values[field.path] = val
            }
        }
    }

    public func matchesScope(_ field: CreationFieldSpec) -> Bool {
        if let platforms = field.platforms, !platforms.contains(platform) { return false }
        if let types = field.types, !types.contains(type) { return false }
        return true
    }

    public func isVisible(_ field: CreationFieldSpec) -> Bool {
        guard matchesScope(field) else { return false }
        if field.path == "credentials.model_mapping" && platform == "openai" && values["extra.openai_passthrough"]?.boolValue == true {
            return false
        }
        if field.path == "credentials.account_mode",
           let prof = AccountCreationCatalog.shared.provider_profiles[platform],
           prof.modes.count == 1
        {
            return false
        }
        if field.path == "credentials.base_url",
           AccountCreationCatalog.shared.provider_profiles[platform] != nil,
           values["credentials.api_protocol"]?.stringValue == "adaptive"
        {
            return false
        }
        if field.path == "extra.quota_reset_timezone",
           values["extra.quota_daily_reset_mode"]?.stringValue != "fixed",
           values["extra.quota_weekly_reset_mode"]?.stringValue != "fixed"
        {
            return false
        }
        guard let conditions = field.when else { return true }
        for (depPath, expected) in conditions {
            let actual = values[depPath] ?? .null
            if actual != expected { return false }
        }
        return true
    }

    public var visibleFields: [CreationFieldSpec] {
        var seen = Set<String>()
        return AccountCreationCatalog.shared.fields.filter { field in
            guard isVisible(field) else { return false }
            return seen.insert(field.path).inserted
        }
    }

    public func validateStepOne(requireName: Bool = true) -> String {
        if requireName && name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            return "请输入账号名称"
        }
        guard let conc = Int(concurrency.trimmingCharacters(in: .whitespacesAndNewlines)), conc >= 1 else {
            return "并发数需为正整数"
        }
        let lfTrim = loadFactor.trimmingCharacters(in: .whitespacesAndNewlines)
        if !lfTrim.isEmpty {
            guard let lf = Int(lfTrim), lf >= 1 else {
                return "负载因子需为正整数"
            }
        }
        guard let prio = Int(priority.trimmingCharacters(in: .whitespacesAndNewlines)), prio >= 0 else {
            return "优先级需为非负整数"
        }
        guard let rate = Double(rateMultiplier.trimmingCharacters(in: .whitespacesAndNewlines)), rate >= 0 else {
            return "倍率需为非负数"
        }
        return ""
    }

    public func buildPayload(
        identityCredentials: [String: JSONValue]? = nil,
        identityExtra: [String: JSONValue]? = nil,
        requireCredentials: Bool = true,
        requireName: Bool = true
    ) throws -> [String: JSONValue] {
        let stepProblem = validateStepOne(requireName: requireName)
        if !stepProblem.isEmpty {
            throw RustBridgeError(stepProblem)
        }
        let trimmedName = name.trimmingCharacters(in: .whitespacesAndNewlines)
        let conc = Int(concurrency.trimmingCharacters(in: .whitespacesAndNewlines)) ?? 1
        let prio = Int(priority.trimmingCharacters(in: .whitespacesAndNewlines)) ?? 1
        let rate = Double(rateMultiplier.trimmingCharacters(in: .whitespacesAndNewlines)) ?? 1

        var credentials: [String: JSONValue] = identityCredentials ?? oauthCredentials ?? [:]
        var extra: [String: JSONValue] = identityExtra ?? oauthExtra ?? [:]
        var root: [String: JSONValue] = [
            "name": .string(trimmedName),
            "notes": notes.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? .string("") : .string(notes.trimmingCharacters(in: .whitespacesAndNewlines)),
            "platform": .string(platform),
            "type": .string(type),
            "proxy_id": proxyId.map { .int(Int64($0)) } ?? .null,
            "concurrency": .int(Int64(conc)),
            "load_factor": Int(loadFactor.trimmingCharacters(in: .whitespacesAndNewlines)).map { .int(Int64($0)) } ?? .null,
            "priority": .int(Int64(prio)),
            "rate_multiplier": .double(rate),
            "schedulable": .bool(schedulable),
            "group_ids": .array(groupIds.sorted().map { .int(Int64($0)) }),
            "expires_at": .null,
        ]

        for field in visibleFields {
            guard let val = values[field.path], val != .null else { continue }
            if case .string(let s) = val, s.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty, field.kind != "select" {
                continue
            }
            let normalizedVal: JSONValue
            if field.kind == "stringbool", case .bool(let b) = val {
                normalizedVal = .string(b ? "true" : "false")
            } else {
                normalizedVal = val
            }
            if field.path.hasPrefix("credentials.") {
                let sub = String(field.path.dropFirst("credentials.".count))
                credentials[sub] = normalizedVal
            } else if field.path.hasPrefix("extra.") {
                let sub = String(field.path.dropFirst("extra.".count))
                extra[sub] = normalizedVal
            } else {
                root[field.path] = normalizedVal
            }
        }

        if requireCredentials {
            if type == "apikey" {
                let trimmedKey = apiKey.trimmingCharacters(in: .whitespacesAndNewlines)
                guard !trimmedKey.isEmpty else {
                    throw RustBridgeError("请输入 API Key")
                }
                credentials["api_key"] = .string(trimmedKey)
            } else if isOAuthFlow && credentials["access_token"] == nil && credentials["setup_token"] == nil {
                if type == "setup-token", !apiKey.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                    credentials["setup_token"] = .string(apiKey.trimmingCharacters(in: .whitespacesAndNewlines))
                } else if credentials.isEmpty {
                    throw RustBridgeError("请先完成 OAuth 授权交换")
                }
            } else if type == "service_account" {
                let isVertexKey = platform == "gemini" && values["credentials.auth_mode"]?.stringValue == "apikey"
                if isVertexKey {
                    let k = apiKey.trimmingCharacters(in: .whitespacesAndNewlines)
                    guard !k.isEmpty else { throw RustBridgeError("请输入 API Key") }
                    credentials["api_key"] = .string(k)
                    credentials["tier_id"] = .string("vertex")
                } else {
                    let trimmedSA = serviceAccountJSON.trimmingCharacters(in: .whitespacesAndNewlines)
                    guard !trimmedSA.isEmpty,
                          let saData = trimmedSA.data(using: .utf8),
                          let saObj = (try? JSONDecoder().decode([String: JSONValue].self, from: saData)),
                          saObj["private_key"]?.stringValue?.isEmpty == false,
                          let email = saObj["client_email"]?.stringValue,
                          let proj = saObj["project_id"]?.stringValue
                    else {
                        throw RustBridgeError("Service Account 文件缺少私钥、邮箱或项目 ID")
                    }
                    credentials["service_account_json"] = .string(trimmedSA)
                    credentials["client_email"] = .string(email)
                    credentials["project_id"] = .string(proj)
                    credentials["tier_id"] = .string("vertex")
                    credentials.removeValue(forKey: "auth_mode")
                    credentials.removeValue(forKey: "api_key")
                }
            } else if type == "bedrock" {
                let mode = values["credentials.auth_mode"]?.stringValue ?? "sigv4"
                if mode == "apikey" {
                    let k = apiKey.trimmingCharacters(in: .whitespacesAndNewlines)
                    guard !k.isEmpty else { throw RustBridgeError("请填写 Bedrock API Key") }
                    credentials["api_key"] = .string(k)
                } else {
                    let ak = awsAccessKeyId.trimmingCharacters(in: .whitespacesAndNewlines)
                    let sk = awsSecretAccessKey.trimmingCharacters(in: .whitespacesAndNewlines)
                    guard !ak.isEmpty && !sk.isEmpty else {
                        throw RustBridgeError("请填写 AWS Access Key ID 和 Secret Access Key")
                    }
                    credentials["aws_access_key_id"] = .string(ak)
                    credentials["aws_secret_access_key"] = .string(sk)
                    if !awsSessionToken.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                        credentials["aws_session_token"] = .string(awsSessionToken.trimmingCharacters(in: .whitespacesAndNewlines))
                    }
                }
            }
        }

        let mapDict = Dictionary(
            mappings.compactMap { pair -> (String, JSONValue)? in
                let k = pair.key.trimmingCharacters(in: .whitespacesAndNewlines)
                let v = pair.value.trimmingCharacters(in: .whitespacesAndNewlines)
                return (!k.isEmpty && !v.isEmpty) ? (k, .string(v)) : nil
            },
            uniquingKeysWith: { _, last in last }
        )
        if !mapDict.isEmpty { credentials["model_mapping"] = .object(mapDict) }

        let compactDict = Dictionary(
            compactMappings.compactMap { pair -> (String, JSONValue)? in
                let k = pair.key.trimmingCharacters(in: .whitespacesAndNewlines)
                let v = pair.value.trimmingCharacters(in: .whitespacesAndNewlines)
                return (!k.isEmpty && !v.isEmpty) ? (k, .string(v)) : nil
            },
            uniquingKeysWith: { _, last in last }
        )
        if !compactDict.isEmpty { credentials["compact_model_mapping"] = .object(compactDict) }

        let headerDict = Dictionary(
            headers.compactMap { pair -> (String, JSONValue)? in
                let k = pair.key.trimmingCharacters(in: .whitespacesAndNewlines)
                return !k.isEmpty ? (k, .string(pair.value)) : nil
            },
            uniquingKeysWith: { _, last in last }
        )
        if !headerDict.isEmpty { credentials["header_overrides"] = .object(headerDict) }

        if platform == "openai" {
            let wsKey = "openai_\(type)_responses_websockets_v2_mode"
            let wsEnabled = extra[wsKey].map { $0 != .null && $0.stringValue != "off" } ?? false
            extra["openai_\(type)_responses_websockets_v2_enabled"] = .bool(wsEnabled)
        }

        root["credentials"] = .object(credentials)
        root["extra"] = .object(extra)
        return root
    }

    public static func extractTokenIdentity(
        platform: String,
        token: [String: JSONValue],
        fallbackRefresh: String? = nil
    ) -> (credentials: [String: JSONValue], extra: [String: JSONValue]) {
        let keys = [
            "access_token", "refresh_token", "id_token", "token_type", "expires_at",
            "scope", "email", "email_address", "org_uuid", "account_uuid",
            "chatgpt_account_id", "chatgpt_user_id", "organization_id", "plan_type",
            "subscription_expires_at", "client_id", "project_id", "oauth_type",
            "tier_id", "sub", "team_id", "subscription_tier", "entitlement_status",
        ]
        var credentials: [String: JSONValue] = [:]
        for key in keys {
            guard let val = token[key], val != .null else { continue }
            if case .string(let s) = val, s.isEmpty { continue }
            credentials[key] = val
        }
        if let fallbackRefresh, !fallbackRefresh.isEmpty, credentials["refresh_token"] == nil {
            credentials["refresh_token"] = .string(fallbackRefresh)
        }
        if ["gemini", "antigravity"].contains(platform), let exp = credentials["expires_at"] {
            credentials["expires_at"] = .string(exp.displayText)
        }
        var extra: [String: JSONValue] = [:]
        if platform == "gemini", let nestedExtra = token["extra"]?.objectValue {
            for k in ["drive_storage_limit", "drive_storage_usage", "drive_tier_updated_at"] {
                if let val = nestedExtra[k] { extra[k] = val }
            }
        }
        for k in ["email", "email_address", "org_uuid", "account_uuid", "subscription_tier", "entitlement_status", "name", "privacy_mode"] {
            if let val = token[k], val != .null, val.displayText != "" {
                extra[k] = val
            }
        }
        return (credentials, extra)
    }
}
