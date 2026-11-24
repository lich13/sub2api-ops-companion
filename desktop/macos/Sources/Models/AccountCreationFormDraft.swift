import Foundation

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

    static let modelIdPattern = "^([A-Za-z0-9_.:/\\-]+\\*?|\\*)$"
    static let headerNamePattern = "^[!#$%&'*+\\-.^_`|~0-9A-Za-z]+$"
    static let imageTiers = ["1K", "2K", "4K"]

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
        if path == "upstream_billing_group_sync_enabled" && value == .bool(true) {
            values["upstream_billing_probe_enabled"] = .bool(true)
        } else if path == "upstream_billing_probe_enabled" && value == .bool(false) {
            values["upstream_billing_group_sync_enabled"] = .bool(false)
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
            && values["extra.openai_passthrough_model_filter"]?.boolValue != true
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

    private static let expiryFormatterLock = NSLock()
    private static let sharedExpiryFormatter: DateFormatter = {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.dateFormat = "yyyy-MM-dd HH:mm"
        return formatter
    }()

    /// Parses `YYYY-MM-DD HH:mm` in the local time zone into epoch seconds.
    public static func parseExpiry(_ text: String) -> Int64? {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines).replacingOccurrences(of: "T", with: " ")
        guard !trimmed.isEmpty else { return nil }
        expiryFormatterLock.lock()
        sharedExpiryFormatter.timeZone = .current
        let parsed = sharedExpiryFormatter.date(from: trimmed)
        expiryFormatterLock.unlock()
        guard let date = parsed else { return nil }
        let seconds = Int64(date.timeIntervalSince1970.rounded(.down))
        return seconds > 0 ? seconds : nil
    }

    public static func formatExpiry(_ seconds: Int64) -> String {
        expiryFormatterLock.lock()
        sharedExpiryFormatter.timeZone = .current
        let text = sharedExpiryFormatter.string(from: Date(timeIntervalSince1970: TimeInterval(seconds)))
        expiryFormatterLock.unlock()
        return text
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

    static func matches(_ text: String, _ pattern: String) -> Bool {
        text.range(of: pattern, options: .regularExpression) != nil
    }

    enum URLProblem { case none, format, scheme }

    static func httpURLProblem(_ raw: String) -> URLProblem {
        let text = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let comps = URLComponents(string: text), let scheme = comps.scheme?.lowercased(),
              let host = comps.host, !host.isEmpty
        else { return .format }
        if !["http", "https"].contains(scheme) || comps.user != nil || comps.password != nil {
            return .scheme
        }
        return .none
    }

    func rows(for path: String) -> [KeyValuePairDraft] {
        path == "credentials.compact_model_mapping" ? compactMappings : mappings
    }

    static func trimmed(_ text: String) -> String {
        text.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    func serviceAccountIdentity() throws -> (email: JSONValue, project: JSONValue) {
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
}
