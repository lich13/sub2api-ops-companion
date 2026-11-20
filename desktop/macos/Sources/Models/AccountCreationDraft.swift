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

public struct CreationURLPreset: Codable, Sendable, Equatable {
    public var mode: String?
    public var protocolName: String?
    public var label: String
    public var url: String

    enum CodingKeys: String, CodingKey {
        case mode, label, url
        case protocolName = "protocol"
    }
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
    public var url_presets: [String: [CreationURLPreset]]? = nil

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
        let data = Data(AccountCreationCatalogGenerated.json.utf8)
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

public struct PauseRuleDraft: Sendable, Equatable, Identifiable {
    public var id: UUID = UUID()
    public var scope: String
    public var errorCode: String
    public var durationMinutes: String
    public var keywords: String
    public var description: String

    public init(
        scope: String = "model",
        errorCode: String = "429",
        durationMinutes: String = "30",
        keywords: String = "",
        description: String = ""
    ) {
        self.scope = scope
        self.errorCode = errorCode
        self.durationMinutes = durationMinutes
        self.keywords = keywords
        self.description = description
    }

    public var keywordList: [String] {
        keywords
            .components(separatedBy: CharacterSet(charactersIn: ",，"))
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty }
    }
}

public struct ProtocolRuleDraft: Sendable, Equatable, Identifiable {
    public var id: UUID = UUID()
    public var pattern: String
    public var preferred: String
    /// Protocols accepted in addition to `preferred`.
    public var also: [String]

    public init(pattern: String = "", preferred: String = "", also: [String] = []) {
        self.pattern = pattern
        self.preferred = preferred
        self.also = also
    }

    public var accepted: [String] {
        var out = [preferred]
        for item in also where !out.contains(item) { out.append(item) }
        return out
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
    public var groupIds: Set<Int> = []
    /// Scalar catalog fields keyed by catalog path. Unset fields are absent.
    public var values: [String: JSONValue] = [:]
    public var mappings: [KeyValuePairDraft] = []
    public var compactMappings: [KeyValuePairDraft] = []
    public var headers: [KeyValuePairDraft] = []
    public var pauseRules: [PauseRuleDraft] = []
    public var protocolURLs: [String: String] = [:]
    public var protocolRules: [ProtocolRuleDraft] = []

    private static let modelIdPattern = "^([A-Za-z0-9_.:/\\-]+\\*?|\\*)$"
    private static let headerNamePattern = "^[!#$%&'*+\\-.^_`|~0-9A-Za-z]+$"
    private static let imageTiers = ["1K", "2K", "4K"]

    public init() {
        let first = AccountCreationCatalog.shared.platforms.first
        applyPlatformDefaults(for: first?.id ?? "anthropic", authType: first?.types.first ?? "oauth")
    }

    // MARK: Identity and provider profile

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
                if let text = values["credentials.\(key)"]?.stringValue, !text.isEmpty {
                    body[key] = .string(text)
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

    public var isMultiProtocol: Bool {
        AccountCreationCatalog.shared.provider_profiles[platform] != nil
    }

    public var isVertexApiKey: Bool {
        platform == "gemini" && type == "service_account" && values["credentials.auth_mode"]?.stringValue == "apikey"
    }

    private static func providerMode(_ platform: String, _ mode: String?) -> CreationProviderModeProfile? {
        guard let profile = AccountCreationCatalog.shared.provider_profiles[platform] else { return nil }
        return profile.modes.first(where: { $0.mode == mode })
            ?? profile.modes.first(where: { $0.mode == profile.default_mode })
    }

    private static func defaultURL(_ platform: String, _ mode: String?, _ protocolName: String) -> String {
        let urls = providerMode(platform, mode)?.base_urls
        return urls?[protocolName]
            ?? urls?["chat_completions"]
            ?? AccountCreationCatalog.shared.platforms.first(where: { $0.id == platform })?.base_url
            ?? ""
    }

    private static func defaultRules(_ platform: String, _ mode: String?) -> [ProtocolRuleDraft] {
        (providerMode(platform, mode)?.protocol_rules ?? []).map(ruleDraft)
    }

    private static func ruleDraft(_ rule: CreationProtocolRule) -> ProtocolRuleDraft {
        let preferred = rule.protocolName.isEmpty ? (rule.protocols?.first ?? "") : rule.protocolName
        return ProtocolRuleDraft(
            pattern: rule.pattern,
            preferred: preferred,
            also: (rule.protocols ?? []).filter { $0 != preferred }
        )
    }

    private static func sameRules(_ a: [ProtocolRuleDraft], _ b: [ProtocolRuleDraft]) -> Bool {
        a.count == b.count && zip(a, b).allSatisfy {
            $0.pattern == $1.pattern && $0.accepted == $1.accepted
        }
    }

    /// Protocols offered by the selected account mode, in catalog order.
    public var protocolNames: [String] {
        (Self.providerMode(platform, values["credentials.account_mode"]?.stringValue)?.base_urls.keys).map { $0.sorted() } ?? []
    }

    public var urlPresets: [CreationURLPreset] {
        let mode = values["credentials.account_mode"]?.stringValue
        let proto = values["credentials.api_protocol"]?.stringValue
        return (AccountCreationCatalog.shared.url_presets?[platform] ?? []).filter { preset in
            platform == "grok" || (preset.mode == mode && (proto == "adaptive" || preset.protocolName == proto))
        }
    }

    public var mappingPresets: [CreationModelPreset] {
        AccountCreationCatalog.shared.mapping_presets[type == "bedrock" ? "bedrock" : platform] ?? []
    }

    /// Custom error codes that override native rate-limit / overload handling.
    public var customErrorRisks: [Int] {
        guard type == "apikey", values["credentials.custom_error_codes_enabled"]?.boolValue == true,
              let codes = Self.listNumbers(values["credentials.custom_error_codes"])
        else { return [] }
        return [429, 529].filter { codes.contains(Double($0)) }
    }

    // MARK: Defaults

    public mutating func selectPlatform(_ nextPlatform: String) {
        let spec = AccountCreationCatalog.shared.platforms.first(where: { $0.id == nextPlatform })
        let nextType = spec?.types.contains(type) == true ? type : (spec?.types.first ?? "apikey")
        applyPlatformDefaults(for: nextPlatform, authType: nextType)
    }

    public mutating func selectType(_ nextType: String) {
        applyPlatformDefaults(for: platform, authType: nextType)
    }

    /// Resets everything except name and notes to the catalog defaults of a platform/type.
    public mutating func applyPlatformDefaults(for nextPlatform: String, authType: String) {
        let catalog = AccountCreationCatalog.shared
        platform = nextPlatform
        type = authType
        apiKey = ""
        serviceAccountJSON = ""
        awsAccessKeyId = ""
        awsSecretAccessKey = ""
        awsSessionToken = ""
        proxyId = nil
        concurrency = nextPlatform == "grok" ? "1" : "10"
        loadFactor = ""
        priority = "1"
        rateMultiplier = "1"
        expiresAtText = ""
        groupIds = []
        mappings = []
        compactMappings = []
        headers = []
        pauseRules = []
        protocolURLs = [:]
        protocolRules = []
        values = [:]

        for field in catalog.fields where matchesScope(field) {
            if let defVal = field.defaultValue, defVal != .null {
                values[field.path] = defVal
            }
        }
        if authType == "apikey",
           let base = catalog.platforms.first(where: { $0.id == nextPlatform })?.base_url,
           !base.isEmpty
        {
            values["credentials.base_url"] = .string(base)
        }
        if nextPlatform == "gemini" && authType == "oauth" {
            values["credentials.tier_id"] = .string(tierChoices[0])
        }
        if catalog.fields.contains(where: { $0.path == "credentials.model_mapping" && matchesScope($0) }),
           !["grok", "antigravity"].contains(nextPlatform)
        {
            mappings = (catalog.models?[nextPlatform] ?? []).map { KeyValuePairDraft(key: $0, value: $0) }
        }
        applyProtocolDefaults()
    }

    /// Fills protocol URLs and routing rules for the selected mode; user edits survive a mode
    /// or protocol switch, untouched defaults follow it.
    public mutating func applyProtocolDefaults(previous: AccountCreationFormDraft? = nil) {
        guard let profile = AccountCreationCatalog.shared.provider_profiles[platform] else { return }
        let mode = values["credentials.account_mode"]?.stringValue
        let proto = values["credentials.api_protocol"]?.stringValue ?? ""
        let prevMode = previous?.values["credentials.account_mode"]?.stringValue
        let prevProto = previous?.values["credentials.api_protocol"]?.stringValue ?? ""

        let baseUntouched = previous.map {
            ($0.values["credentials.base_url"]?.stringValue ?? "") == Self.defaultURL(platform, prevMode, prevProto)
        } ?? true
        if baseUntouched {
            values["credentials.base_url"] = .string(Self.defaultURL(platform, mode, proto))
        }

        let prevURLs = Self.providerMode(platform, prevMode)?.base_urls ?? [:]
        var nextURLs: [String: String] = [:]
        for (key, value) in Self.providerMode(platform, mode)?.base_urls ?? [:] {
            let untouched = previous.map { $0.protocolURLs[key] == prevURLs[key] } ?? true
            nextURLs[key] = untouched ? value : (protocolURLs[key] ?? value)
        }
        protocolURLs = nextURLs

        if profile.routing == "by_model" {
            let untouched = previous.map { Self.sameRules($0.protocolRules, Self.defaultRules(platform, prevMode)) } ?? true
            if untouched {
                protocolRules = Self.defaultRules(platform, mode)
            }
        }
    }

    /// Sets or clears a scalar catalog field and applies the dependent defaults.
    public mutating func setValue(_ path: String, _ value: JSONValue?) {
        let previous = self
        if let value, value != .null {
            values[path] = value
        } else {
            values.removeValue(forKey: path)
        }
        if path == "credentials.account_mode" || path == "credentials.api_protocol" {
            applyProtocolDefaults(previous: previous)
        }
        if path == "credentials.auth_mode" && type == "service_account" {
            apiKey = ""
            serviceAccountJSON = ""
            values.removeValue(forKey: "credentials.project_id")
        }
        if path == "credentials.oauth_type" {
            values["credentials.tier_id"] = .string(tierChoices[0])
        }
    }

    // MARK: Visibility

    public func matchesScope(_ field: CreationFieldSpec) -> Bool {
        if let platforms = field.platforms, !platforms.contains(platform) { return false }
        if let types = field.types, !types.contains(type) { return false }
        return true
    }

    public static func isOfficialGemini(_ raw: String?) -> Bool {
        let text = (raw ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        if text.isEmpty { return true }
        guard let comps = URLComponents(string: text) else { return false }
        let path = comps.path.replacingOccurrences(of: "/+$", with: "", options: .regularExpression)
        return comps.scheme?.lowercased() == "https"
            && comps.host?.lowercased() == "generativelanguage.googleapis.com"
            && comps.port == nil
            && path.isEmpty
            && (comps.query ?? "").isEmpty
    }

    public func isVisible(_ field: CreationFieldSpec) -> Bool {
        guard matchesScope(field) else { return false }
        if field.path.hasPrefix("credentials.explicit_cache_upstream"),
           Self.isOfficialGemini(values["credentials.base_url"]?.stringValue)
        {
            return false
        }
        if field.path == "credentials.account_mode",
           let profile = AccountCreationCatalog.shared.provider_profiles[platform],
           profile.modes.count == 1
        {
            return false
        }
        if field.path == "credentials.model_mapping" && platform == "openai"
            && values["extra.openai_passthrough"]?.boolValue == true
        {
            return false
        }
        if field.path == "credentials.base_url", isMultiProtocol,
           values["credentials.api_protocol"]?.stringValue == "adaptive"
        {
            return false
        }
        if field.path == "extra.openai_responses_mode",
           let capabilities = values["credentials.openai_capabilities"]?.arrayValue,
           !capabilities.contains(.string("chat_completions"))
        {
            return false
        }
        if field.path == "extra.quota_reset_timezone",
           values["extra.quota_daily_reset_mode"]?.stringValue != "fixed",
           values["extra.quota_weekly_reset_mode"]?.stringValue != "fixed"
        {
            return false
        }
        for (depPath, expected) in field.when ?? [:] where values[depPath] != expected {
            return false
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

    // MARK: Parsing helpers

    private static func expiryFormatter() -> DateFormatter {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.timeZone = .current
        formatter.dateFormat = "yyyy-MM-dd HH:mm"
        return formatter
    }

    /// Parses `YYYY-MM-DD HH:mm` in the local time zone into epoch seconds.
    public static func parseExpiry(_ text: String) -> Int64? {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines).replacingOccurrences(of: "T", with: " ")
        guard let date = expiryFormatter().date(from: trimmed) else { return nil }
        let seconds = Int64(date.timeIntervalSince1970.rounded(.down))
        return seconds > 0 ? seconds : nil
    }

    public static func formatExpiry(_ seconds: Int64) -> String {
        expiryFormatter().string(from: Date(timeIntervalSince1970: TimeInterval(seconds)))
    }

    /// Mirrors the comma / whitespace separated number list input; `nil` means not a list.
    static func listNumbers(_ value: JSONValue?) -> [Double]? {
        switch value {
        case .string(let raw):
            let parts = raw
                .components(separatedBy: CharacterSet(charactersIn: ",，").union(.whitespacesAndNewlines))
                .filter { !$0.isEmpty }
            return parts.map { Double($0) ?? .nan }
        case .array(let items):
            return items.map { $0.doubleValue ?? .nan }
        default:
            return nil
        }
    }

    private static func matches(_ text: String, _ pattern: String) -> Bool {
        text.range(of: pattern, options: .regularExpression) != nil
    }

    private enum URLProblem { case none, format, scheme }

    private static func httpURLProblem(_ raw: String) -> URLProblem {
        let text = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let comps = URLComponents(string: text), let scheme = comps.scheme?.lowercased(),
              let host = comps.host, !host.isEmpty
        else { return .format }
        if !["http", "https"].contains(scheme) || comps.user != nil || comps.password != nil {
            return .scheme
        }
        return .none
    }

    private func rows(for path: String) -> [KeyValuePairDraft] {
        path == "credentials.compact_model_mapping" ? compactMappings : mappings
    }

    private static func trimmed(_ text: String) -> String {
        text.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private func serviceAccountIdentity() throws -> (email: JSONValue, project: JSONValue) {
        let text = Self.trimmed(serviceAccountJSON)
        guard let object = try? JSONDecoder().decode([String: JSONValue].self, from: Data((text.isEmpty ? "{}" : text).utf8)),
              let key = object["private_key"]?.stringValue, !key.isEmpty,
              let email = object["client_email"], email.stringValue?.isEmpty == false,
              let project = object["project_id"], project.stringValue?.isEmpty == false
        else {
            throw RustBridgeError("Service Account 文件缺少私钥、邮箱或项目 ID")
        }
        return (email, project)
    }

    // MARK: Validation

    public func validateStepOne(requireName: Bool = true) -> String {
        if requireName && Self.trimmed(name).isEmpty {
            return "请输入账号名称"
        }
        guard let conc = Int(Self.trimmed(concurrency)), conc >= 1 else {
            return "并发数需为正整数"
        }
        guard let prio = Int(Self.trimmed(priority)), prio >= 0 else {
            return "优先级需为非负整数"
        }
        guard let rate = Double(Self.trimmed(rateMultiplier)), rate.isFinite, rate >= 0 else {
            return "倍率需为非负数"
        }
        let lfTrim = Self.trimmed(loadFactor)
        if !lfTrim.isEmpty {
            guard let lf = Int(lfTrim), lf >= 1 else {
                return "负载因子需为正整数"
            }
        }
        if !Self.trimmed(expiresAtText).isEmpty && Self.parseExpiry(expiresAtText) == nil {
            return "到期时间格式应为 YYYY-MM-DD HH:mm"
        }
        return ""
    }

    private func pairProblem(_ field: CreationFieldSpec, _ rows: [KeyValuePairDraft]) -> String {
        let isHeaders = field.kind == "headers"
        if rows.contains(where: { Self.trimmed($0.key).isEmpty || (!isHeaders && Self.trimmed($0.value).isEmpty) }) {
            return "\(field.label)存在未填项目"
        }
        let names = rows.map { isHeaders ? Self.trimmed($0.key).lowercased() : Self.trimmed($0.key) }
        if Set(names).count != names.count {
            return "\(field.label)存在重复名称"
        }
        if !isHeaders {
            let invalid = rows.count > 300 || rows.contains { row in
                let from = Self.trimmed(row.key), to = Self.trimmed(row.value)
                return !Self.matches(from, Self.modelIdPattern) || !Self.matches(to, Self.modelIdPattern)
                    || (to.contains("*") && from != to)
            }
            return invalid ? "\(field.label)需使用有效模型 ID；通配白名单请将请求和转发模型填为相同值" : ""
        }
        let blocked = Set(AccountCreationCatalog.shared.blocked_headers ?? [])
        for row in rows {
            let key = Self.trimmed(row.key)
            let controls = row.value.unicodeScalars.contains { ($0.value < 0x20 && $0.value != 0x09) || $0.value == 0x7f }
            if !Self.matches(key, Self.headerNamePattern) || key.count > 200 || blocked.contains(key.lowercased())
                || controls || row.value.utf8.count > 8192
            {
                return "请求头名称、值或重复项无效，认证头不能覆写"
            }
        }
        return rows.count > 64 ? "请求头最多 64 项" : ""
    }

    private func fieldProblem(_ field: CreationFieldSpec) -> String {
        switch field.kind {
        case "mapping":
            return pairProblem(field, rows(for: field.path))
        case "headers":
            return pairProblem(field, headers)
        case "pause-rules":
            let invalid = pauseRules.contains { rule in
                guard let code = Int(Self.trimmed(rule.errorCode)), (100...599).contains(code),
                      let minutes = Int(Self.trimmed(rule.durationMinutes)), minutes > 0
                else { return true }
                return rule.keywordList.isEmpty || !["model", "account"].contains(rule.scope)
            }
            return invalid ? "临时不可调度规则需包含状态码、关键词和正整数分钟" : ""
        case "protocols":
            for name in protocolNames {
                switch Self.httpURLProblem(protocolURLs[name] ?? "") {
                case .format: return "请填写各协议的有效 API 地址"
                case .scheme: return "协议地址需为 HTTP 或 HTTPS 地址"
                case .none: continue
                }
            }
            return ""
        case "protocol-rules":
            let available = protocolNames
            let invalid = protocolRules.contains { rule in
                Self.trimmed(rule.pattern).isEmpty || !rule.accepted.allSatisfy(available.contains)
            }
            return invalid ? "请填写模型匹配与有效协议" : ""
        default:
            break
        }
        guard let value = values[field.path], value != .string("") else { return "" }
        switch field.kind {
        case "number":
            guard let number = value.doubleValue, number.isFinite,
                  field.min.map({ number >= $0 }) ?? true,
                  field.max.map({ number <= $0 }) ?? true
            else { return "\(field.label)超出有效范围" }
        case "numbers":
            let low = field.min ?? 0, high = field.max ?? .infinity
            guard let list = Self.listNumbers(value),
                  list.allSatisfy({ $0.isFinite && $0.rounded() == $0 && $0 >= low && $0 <= high })
            else { return "\(field.label)需填写有效整数" }
        case "url":
            switch Self.httpURLProblem(value.stringValue ?? "") {
            case .format: return "\(field.label)格式无效"
            case .scheme: return "\(field.label)需为 HTTP 或 HTTPS 地址"
            case .none: break
            }
        default:
            break
        }
        return ""
    }

    /// Full form validation; returns the first problem or an empty string.
    public func validate(requireCredentials: Bool = true, requireName: Bool = true) -> String {
        let stepProblem = validateStepOne(requireName: requireName)
        if !stepProblem.isEmpty { return stepProblem }
        for field in visibleFields {
            let problem = fieldProblem(field)
            if !problem.isEmpty { return problem }
        }
        guard requireCredentials, !isOAuthFlow else { return "" }
        if isVertexApiKey {
            if Self.trimmed(apiKey).isEmpty { return "请输入 API Key" }
            if !(values["credentials.project_id"]?.stringValue ?? "").isEmpty,
               Self.trimmed(values["credentials.location"]?.stringValue ?? "").isEmpty
            {
                return "请输入 Vertex 区域"
            }
        } else if type == "service_account" {
            guard (try? serviceAccountIdentity()) != nil else { return "请选择有效的 Service Account JSON 文件" }
            if (values["credentials.location"]?.stringValue ?? "").isEmpty { return "请输入 Vertex 区域" }
        } else if type == "bedrock" && values["credentials.auth_mode"]?.stringValue == "sigv4" {
            if Self.trimmed(awsAccessKeyId).isEmpty || Self.trimmed(awsSecretAccessKey).isEmpty {
                return "请填写 AWS Access Key ID 和 Secret Access Key"
            }
        } else if Self.trimmed(apiKey).isEmpty {
            return "请输入 API Key"
        }
        if platform == "antigravity" && (values["credentials.base_url"]?.stringValue ?? "").isEmpty {
            return "请输入上游 API 地址"
        }
        return ""
    }

    // MARK: Payload

    private func payloadValue(_ field: CreationFieldSpec) -> JSONValue? {
        switch field.kind {
        case "mapping":
            let pairs = rows(for: field.path).map { (Self.trimmed($0.key), JSONValue.string(Self.trimmed($0.value))) }
            return pairs.isEmpty ? nil : .object(Dictionary(pairs, uniquingKeysWith: { _, last in last }))
        case "headers":
            let pairs = headers.map { (Self.trimmed($0.key), JSONValue.string($0.value)) }
            return pairs.isEmpty ? nil : .object(Dictionary(pairs, uniquingKeysWith: { _, last in last }))
        case "pause-rules":
            if pauseRules.isEmpty { return nil }
            return .array(pauseRules.map { rule in
                .object([
                    "error_code": .int(Int64(Self.trimmed(rule.errorCode)) ?? 0),
                    "keywords": .array(rule.keywordList.map(JSONValue.string)),
                    "duration_minutes": .int(Int64(Self.trimmed(rule.durationMinutes)) ?? 0),
                    "description": .string(rule.description),
                    "scope": .string(rule.scope),
                ])
            })
        case "protocols":
            return .object(Dictionary(uniqueKeysWithValues: protocolNames.map {
                ($0, JSONValue.string(Self.trimmed(protocolURLs[$0] ?? "")))
            }))
        case "protocol-rules":
            return .array(protocolRules.map { rule in
                var object: [String: JSONValue] = [
                    "pattern": .string(Self.trimmed(rule.pattern).lowercased()),
                    "protocol": .string(rule.preferred),
                ]
                if rule.accepted.count > 1 {
                    object["protocols"] = .array(rule.accepted.map(JSONValue.string))
                }
                return .object(object)
            })
        default:
            break
        }
        guard let value = values[field.path], value != .null else { return nil }
        if case .string(let text) = value, text.isEmpty, field.kind != "select" { return nil }
        switch field.kind {
        case "numbers":
            return .array((Self.listNumbers(value) ?? []).compactMap { Int64(exactly: $0).map(JSONValue.int) })
        case "stringbool":
            if case .bool(let flag) = value { return .string(flag ? "true" : "false") }
            return value
        default:
            return value
        }
    }

    /// Builds the `/account-creation` request body. Throws the first validation problem.
    public func buildPayload(
        identityCredentials: [String: JSONValue]? = nil,
        identityExtra: [String: JSONValue]? = nil,
        requireCredentials: Bool = true,
        requireName: Bool = true
    ) throws -> [String: JSONValue] {
        let problem = validate(
            requireCredentials: requireCredentials && identityCredentials == nil,
            requireName: requireName
        )
        if !problem.isEmpty {
            throw RustBridgeError(problem)
        }

        var root: [String: JSONValue] = [
            "name": .string(Self.trimmed(name)),
            "notes": .string(Self.trimmed(notes)),
            "platform": .string(platform),
            "type": .string(type),
            "proxy_id": proxyId.map { .int(Int64($0)) } ?? .null,
            "concurrency": .int(Int64(Self.trimmed(concurrency)) ?? 1),
            "load_factor": Int64(Self.trimmed(loadFactor)).map(JSONValue.int) ?? .null,
            "priority": .int(Int64(Self.trimmed(priority)) ?? 1),
            "rate_multiplier": .double(Double(Self.trimmed(rateMultiplier)) ?? 1),
            "group_ids": .array(groupIds.sorted().map { .int(Int64($0)) }),
            "expires_at": Self.parseExpiry(expiresAtText).map(JSONValue.int) ?? .null,
        ]
        var credentials: [String: JSONValue] = [:]
        var extra: [String: JSONValue] = identityExtra ?? [:]

        for field in visibleFields {
            guard let value = payloadValue(field) else { continue }
            if field.path.hasPrefix("credentials.") {
                credentials[String(field.path.dropFirst("credentials.".count))] = value
            } else if field.path.hasPrefix("extra.") {
                extra[String(field.path.dropFirst("extra.".count))] = value
            } else {
                root[field.path] = value
            }
        }

        if !isOAuthFlow {
            let secrets = [
                ("api_key", apiKey),
                ("aws_access_key_id", awsAccessKeyId),
                ("aws_secret_access_key", awsSecretAccessKey),
                ("aws_session_token", awsSessionToken),
                ("service_account_json", serviceAccountJSON),
            ]
            for (key, raw) in secrets where !Self.trimmed(raw).isEmpty {
                credentials[key] = .string(Self.trimmed(raw))
            }
        }

        if type == "service_account" {
            credentials["tier_id"] = .string("vertex")
            if isVertexApiKey {
                credentials.removeValue(forKey: "service_account_json")
                let location = Self.trimmed(values["credentials.location"]?.stringValue ?? "")
                credentials["location"] = .string(location.isEmpty ? "global" : location)
            } else {
                let identity = try serviceAccountIdentity()
                credentials["client_email"] = identity.email
                credentials["project_id"] = identity.project
                credentials.removeValue(forKey: "auth_mode")
                credentials.removeValue(forKey: "api_key")
            }
        }
        if platform == "gemini" && type == "apikey" {
            if Self.isOfficialGemini(values["credentials.base_url"]?.stringValue) {
                credentials["tier_id"] = .string("aistudio_free")
            }
            if credentials["explicit_cache_upstream"]?.boolValue != true {
                credentials.removeValue(forKey: "explicit_cache_upstream")
                credentials.removeValue(forKey: "explicit_cache_upstream_max_ttl_seconds")
            } else if let ttl = credentials["explicit_cache_upstream_max_ttl_seconds"]?.doubleValue {
                credentials["explicit_cache_upstream_max_ttl_seconds"] = .int(Int64(ttl.rounded(.down)))
            }
        }
        if platform == "openai" {
            let selected = values["extra.openai_image_size_tiers"]?.arrayValue ?? []
            let tiers = Self.imageTiers.filter { selected.contains(.string($0)) }
            if tiers.isEmpty || tiers.count == Self.imageTiers.count {
                extra.removeValue(forKey: "openai_image_size_tiers")
            } else {
                extra["openai_image_size_tiers"] = .array(tiers.map(JSONValue.string))
            }
        }
        if type == "bedrock" {
            if credentials["auth_mode"]?.stringValue == "sigv4" {
                credentials.removeValue(forKey: "api_key")
            } else if credentials["auth_mode"]?.stringValue == "apikey" {
                for key in ["aws_access_key_id", "aws_secret_access_key", "aws_session_token"] {
                    credentials.removeValue(forKey: key)
                }
            }
        }
        if isMultiProtocol {
            if let mode = values["credentials.account_mode"] {
                credentials["account_mode"] = mode
            }
            if credentials["api_protocol"]?.stringValue == "adaptive",
               let chat = credentials["api_base_urls"]?["chat_completions"]
            {
                credentials["base_url"] = chat
            }
        }
        for (key, value) in identityCredentials ?? [:] {
            credentials[key] = value
        }
        if platform == "openai" {
            let mode = extra["openai_\(type)_responses_websockets_v2_mode"]?.stringValue ?? ""
            extra["openai_\(type)_responses_websockets_v2_enabled"] = .bool(!mode.isEmpty && mode != "off")
        }

        root["credentials"] = .object(credentials)
        root["extra"] = .object(extra)
        return root
    }

    /// Shared parameters of the native import endpoints: the creation body without
    /// platform/type, with the configuration credentials moved to `credential_extras`.
    public func importParameters() throws -> [String: JSONValue] {
        var body = try buildPayload(requireCredentials: false, requireName: false)
        body.removeValue(forKey: "platform")
        body.removeValue(forKey: "type")
        body["credential_extras"] = body.removeValue(forKey: "credentials") ?? .object([:])
        return body
    }

    // MARK: Reuse

    /// Replaces the configuration with a source account's, keeping name and newly typed secrets.
    public mutating func applyReusedSource(_ source: [String: JSONValue]) throws {
        guard source["platform"]?.stringValue == platform,
              source["type"]?.stringValue == type
        else {
            throw RustBridgeError("来源账号平台或类型已变化，请重新选择")
        }
        if let value = source["notes"] { notes = value.stringValue ?? "" }
        if let value = source["proxy_id"] { proxyId = value.intValue.map(Int.init) }
        if let value = source["concurrency"]?.intValue { concurrency = String(value) }
        if let value = source["load_factor"] { loadFactor = value.intValue.map { String($0) } ?? "" }
        if let value = source["priority"]?.intValue { priority = String(value) }
        if let value = source["rate_multiplier"]?.doubleValue {
            rateMultiplier = value == value.rounded() ? String(Int64(value)) : String(value)
        }
        if let value = source["group_ids"]?.arrayValue {
            groupIds = Set(value.compactMap { $0.intValue.map(Int.init) })
        }
        if let value = source["expires_at"] {
            expiresAtText = value.intValue.map(Self.formatExpiry) ?? ""
        }

        let credentials = source["credentials"]?.objectValue ?? [:]
        let extra = source["extra"]?.objectValue ?? [:]
        func pairs(_ value: JSONValue?) -> [KeyValuePairDraft] {
            let object = value?.objectValue ?? [:]
            return object.keys.sorted().map { KeyValuePairDraft(key: $0, value: object[$0]?.stringValue ?? "") }
        }
        for field in AccountCreationCatalog.shared.fields where matchesScope(field) {
            let value: JSONValue?
            if field.path.hasPrefix("credentials.") {
                value = credentials[String(field.path.dropFirst("credentials.".count))]
            } else if field.path.hasPrefix("extra.") {
                value = extra[String(field.path.dropFirst("extra.".count))]
            } else {
                value = source[field.path]
            }
            switch field.kind {
            case "mapping":
                if field.path == "credentials.compact_model_mapping" {
                    compactMappings = pairs(value)
                } else {
                    mappings = pairs(value)
                }
            case "headers":
                headers = pairs(value)
            case "pause-rules":
                pauseRules = (value?.arrayValue ?? []).map { rule in
                    PauseRuleDraft(
                        scope: rule["scope"]?.stringValue ?? "account",
                        errorCode: rule["error_code"]?.intValue.map { String($0) } ?? "",
                        durationMinutes: rule["duration_minutes"]?.intValue.map { String($0) } ?? "",
                        keywords: (rule["keywords"]?.arrayValue ?? []).compactMap(\.stringValue).joined(separator: ","),
                        description: rule["description"]?.stringValue ?? ""
                    )
                }
            case "protocols":
                protocolURLs = (value?.objectValue ?? [:]).compactMapValues(\.stringValue)
            case "protocol-rules":
                protocolRules = (value?.arrayValue ?? []).map { rule in
                    let accepted = (rule["protocols"]?.arrayValue ?? []).compactMap(\.stringValue)
                    let preferred = rule["protocol"]?.stringValue ?? accepted.first ?? ""
                    return ProtocolRuleDraft(
                        pattern: rule["pattern"]?.stringValue ?? "",
                        preferred: preferred,
                        also: accepted.filter { $0 != preferred }
                    )
                }
            default:
                if let value, value != .null {
                    values[field.path] = value
                } else {
                    values.removeValue(forKey: field.path)
                }
            }
        }
        if platform == "openai", (values["extra.openai_image_size_tiers"]?.arrayValue ?? []).isEmpty {
            values["extra.openai_image_size_tiers"] = .array(Self.imageTiers.map(JSONValue.string))
        }
    }

    // MARK: OAuth identity

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
            if let val = token[k], val != .null, val != .bool(false), val != .string(""), val != .int(0) {
                extra[k] = val
            }
        }
        return (credentials, extra)
    }
}
