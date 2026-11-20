import Foundation

// MARK: - Beijing Time Formatting (`MM-DD HH:mm:ss`)

public enum BeijingTime {
    private static let shanghaiZone = TimeZone(identifier: "Asia/Shanghai") ?? TimeZone(secondsFromGMT: 8 * 3600)!

    public static func parseISO(_ raw: String?) -> Date? {
        guard let raw = raw?.trimmingCharacters(in: .whitespacesAndNewlines), !raw.isEmpty else {
            return nil
        }
        let iso = raw.contains("T") ? raw : raw.replacingOccurrences(of: " ", with: "T", options: [], range: raw.range(of: " "))
        return (try? Date.ISO8601FormatStyle(includingFractionalSeconds: true).parse(iso))
            ?? (try? Date.ISO8601FormatStyle().parse(iso))
    }

    public static func fullTime(_ raw: String?) -> String {
        guard let raw = raw, !raw.isEmpty else {
            return "暂无记录"
        }
        guard let date = parseISO(raw) else {
            return "时间未知"
        }
        return format(date)
    }

    public static func format(_ date: Date) -> String {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = shanghaiZone
        let comps = calendar.dateComponents([.month, .day, .hour, .minute, .second], from: date)
        return String(
            format: "%02d-%02d %02d:%02d:%02d",
            comps.month ?? 1,
            comps.day ?? 1,
            comps.hour ?? 0,
            comps.minute ?? 0,
            comps.second ?? 0
        )
    }

    public static func callTimestamp(_ raw: String?) -> Double {
        guard let date = parseISO(raw) else { return -.infinity }
        return date.timeIntervalSince1970
    }
}

// MARK: - Core Domain DTOs (`desktop/src/types.ts`)

public struct DegradationMark: Codable, Sendable, Equatable {
    public var marked: Bool?
    public var marked_at: String?
    public var version: String?
    public var error: String?

    public init(marked: Bool? = nil, marked_at: String? = nil, version: String? = nil, error: String? = nil) {
        self.marked = marked
        self.marked_at = marked_at
        self.version = version
        self.error = error
    }
}

public struct AccountKeepalive: Codable, Sendable, Equatable {
    public var account_id: Int
    public var enabled: Bool
    public var generation: Int
    public var version: String
    public var status: String
    public var reason: String
    public var paused_reason: String
    public var last_restore_at: String?
    public var restore_count: Int
    public var budget_used: Int
    public var next_allowed_at: String?
}

public struct ModelDetectionLastReport: Codable, Sendable, Equatable {
    public var prediction_name: String?
}

public struct ModelDetectionLastResult: Codable, Sendable, Equatable {
    public var status: String?
    public var report: ModelDetectionLastReport?
    public var completed_at: String?
}

public struct ModelDetectionSummary: Codable, Sendable, Equatable {
    public var schedule_mode: String?
    public var min_interval_minutes: Int?
    public var effective_interval_minutes: Int?
    public var recent_request_count: Int?
    public var last_automatic_started_at: String?
    public var enabled: Bool
    public var interval_minutes: Int
    public var model_id: String
    public var version: String
    public var next_at: String?
    public var status: String
    public var reason: String?
    public var last_result: ModelDetectionLastResult?
}

public struct AccountBlocker: Codable, Sendable, Equatable {
    public var code: String
    public var label: String
    public var until: String?

    public init(code: String, label: String, until: String? = nil) {
        self.code = code
        self.label = label
        self.until = until
    }
}

public struct SettingCapability: Codable, Sendable, Equatable {
    public var editable: Bool
    public var reason: String

    public init(editable: Bool, reason: String) {
        self.editable = editable
        self.reason = reason
    }
}

public struct RecoveryMonitorState: Codable, Sendable, Equatable {
    public var status: String
    public var label: String
    public var reason: String
    public var next_at: String?
    public var attempt_count: Int?
    public var next_retry_at: String?
}

public struct AutoResetCreditState: Codable, Sendable, Equatable {
    public var trigger_at: String?
    public var consume_deadline_at: String?
    public var verification_deadline_at: String?
    public var completed_at: String?
    public var outcome: String?
    public var notification_status: String?
    public var notification_attempts: Int?
    public var evidence_source: String?
    public var evidence_at: String?
    public var stage: String
    public var label: String
    public var error: String
    public var next_at: String?
    public var attempt_at: String?
    public var test_completed_at: String?
    public var recovered_at: String?
}

public struct UsageStats: Codable, Sendable, Equatable {
    public var requests: Int
    public var tokens: Int
    public var cost: Double
    public var standard_cost: Double
    public var user_cost: Double

    public init(requests: Int, tokens: Int, cost: Double, standard_cost: Double, user_cost: Double) {
        self.requests = requests
        self.tokens = tokens
        self.cost = cost
        self.standard_cost = standard_cost
        self.user_cost = user_cost
    }
}

public struct QuotaPlan: Codable, Sendable, Equatable {
    public var name: String
    public var remaining: Double?
    public var used: Double?
    public var total: Double?
    public var unit: String
    public var unlimited: Bool
    public var reset_at: String?
}

public struct QuotaSubscription: Codable, Sendable, Equatable {
    public var name: String?
    public var status: String?
    public var active: Bool?
    public var expires_at: String?
    public var plans: [QuotaPlan]
}

public struct KeyQuotaUsage: Codable, Sendable, Equatable {
    public var requests: Int?
    public var tokens: Int?
    public var cost: Double?
}

public struct KeyQuota: Codable, Sendable, Equatable {
    public var state: String
    public var source: String?
    public var checked_at: Double?
    public var success_at: Double?
    public var retry_at: Double?
    public var next_refresh_at: Double?
    public var stale: Bool
    public var error: String?
    public var key_status: String?
    public var plans: [QuotaPlan]
    public var subscriptions: [QuotaSubscription]
    public var expires_at: String?
    public var expires_at_unix: Double?
    public var today: KeyQuotaUsage?
    public var total_usage: KeyQuotaUsage?
}

public struct UsageBalance: Codable, Sendable, Equatable {
    public var currency: String
    public var balance: Double
    public var observed_at: String?
    public var status: String?
}

public struct ResetCreditsInfo: Codable, Sendable, Equatable {
    public var available: Int?
    public var expires_at: [String]
    public var observed_at: String?

    public init(available: Int?, expires_at: [String], observed_at: String? = nil) {
        self.available = available
        self.expires_at = expires_at
        self.observed_at = observed_at
    }
}

public struct UsageWindow: Codable, Sendable, Equatable {
    public var window_minutes: Double?
    public var estimated: Bool?
    public var key: String
    public var label: String
    public var used_percent: Double?
    public var reset_at: String?
    public var observed_at: String?
    public var status: String
    public var source: String
    public var used: Double?
    public var limit: Double?
    public var remaining: Double?
    public var color: String?
    public var stats: UsageStats?
    public var estimated_total_cost: Double?

    public init(
        key: String,
        label: String,
        used_percent: Double?,
        reset_at: String?,
        observed_at: String? = nil,
        status: String = "known",
        source: String = "upstream_snapshot"
    ) {
        self.key = key
        self.label = label
        self.used_percent = used_percent
        self.reset_at = reset_at
        self.observed_at = observed_at
        self.status = status
        self.source = source
    }
}

public struct UsageSummary: Codable, Sendable, Equatable {
    public var stats_status: String?
    public var key_quota: KeyQuota?
    public var branch: String
    public var native_read: Bool?
    public var status: String?
    public var message: String?
    public var balances: [UsageBalance]?
    public var windows: [UsageWindow]
    public var today: UsageStats?
    public var actions: [String]
    public var reset_credits: ResetCreditsInfo?
    public var prepaid_balance: Double?
    public var monthly_limit: Double?
    public var monthly_used: Double?

    public init(
        branch: String,
        windows: [UsageWindow] = [],
        today: UsageStats? = nil,
        actions: [String] = [],
        reset_credits: ResetCreditsInfo? = nil,
        key_quota: KeyQuota? = nil,
        balances: [UsageBalance]? = nil,
        native_read: Bool? = nil
    ) {
        self.branch = branch
        self.windows = windows
        self.today = today
        self.actions = actions
        self.reset_credits = reset_credits
        self.key_quota = key_quota
        self.balances = balances
        self.native_read = native_read
    }
}

public struct Account: Codable, Sendable, Equatable, Identifiable {
    public var degradation_mark: DegradationMark?
    public var recovery_selectable: Bool?
    public var model_detection: ModelDetectionSummary?
    public var id: Int
    public var parent_account_id: Int?
    public var name: String
    public var priority: Int
    public var concurrency: Int?
    public var load_factor: Int?
    public var settings_capabilities: [String: SettingCapability]?
    public var platform: String
    public var type: String
    public var status: String
    public var schedulable: Bool
    public var available: Bool
    public var group_ids: [Int]
    public var blockers: [AccountBlocker]
    public var recoverable: Bool?
    public var version: String
    public var operation_versions: [String: String]
    public var last_success_at: String?
    public var last_called_at: String?
    public var last_error_at: String?
    public var last_error_id: Int?
    public var last_error_code: String?
    public var last_error_status: Int?
    public var error_message: String
    public var success_after_error: Bool
    public var usage_windows: [UsageWindow]
    public var usage: UsageSummary?
    public var recovery_monitor: RecoveryMonitorState?
    public var auto_reset_credit: AutoResetCreditState?

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        degradation_mark = try c.decodeIfPresent(DegradationMark.self, forKey: .degradation_mark)
        recovery_selectable = try c.decodeIfPresent(Bool.self, forKey: .recovery_selectable)
        model_detection = try c.decodeIfPresent(ModelDetectionSummary.self, forKey: .model_detection)
        id = try c.decode(Int.self, forKey: .id)
        parent_account_id = try c.decodeIfPresent(Int.self, forKey: .parent_account_id)
        name = try c.decodeIfPresent(String.self, forKey: .name) ?? ""
        priority = try c.decodeIfPresent(Int.self, forKey: .priority) ?? 0
        concurrency = try c.decodeIfPresent(Int.self, forKey: .concurrency)
        load_factor = try c.decodeIfPresent(Int.self, forKey: .load_factor)
        settings_capabilities = try c.decodeIfPresent([String: SettingCapability].self, forKey: .settings_capabilities)
        platform = try c.decodeIfPresent(String.self, forKey: .platform) ?? ""
        type = try c.decodeIfPresent(String.self, forKey: .type) ?? ""
        status = try c.decodeIfPresent(String.self, forKey: .status) ?? ""
        schedulable = try c.decodeIfPresent(Bool.self, forKey: .schedulable) ?? false
        available = try c.decodeIfPresent(Bool.self, forKey: .available) ?? false
        group_ids = try c.decodeIfPresent([Int].self, forKey: .group_ids) ?? []
        blockers = try c.decodeIfPresent([AccountBlocker].self, forKey: .blockers) ?? []
        recoverable = try c.decodeIfPresent(Bool.self, forKey: .recoverable)
        version = try c.decodeIfPresent(String.self, forKey: .version) ?? ""
        operation_versions = try c.decodeIfPresent([String: String].self, forKey: .operation_versions) ?? [:]
        last_success_at = try c.decodeIfPresent(String.self, forKey: .last_success_at)
        last_called_at = try c.decodeIfPresent(String.self, forKey: .last_called_at)
        last_error_at = try c.decodeIfPresent(String.self, forKey: .last_error_at)
        last_error_id = try c.decodeIfPresent(Int.self, forKey: .last_error_id)
        last_error_code = try c.decodeIfPresent(String.self, forKey: .last_error_code)
        last_error_status = try c.decodeIfPresent(Int.self, forKey: .last_error_status)
        error_message = try c.decodeIfPresent(String.self, forKey: .error_message) ?? ""
        success_after_error = try c.decodeIfPresent(Bool.self, forKey: .success_after_error) ?? false
        usage_windows = try c.decodeIfPresent([UsageWindow].self, forKey: .usage_windows) ?? []
        usage = try c.decodeIfPresent(UsageSummary.self, forKey: .usage)
        recovery_monitor = try c.decodeIfPresent(RecoveryMonitorState.self, forKey: .recovery_monitor)
        auto_reset_credit = try c.decodeIfPresent(AutoResetCreditState.self, forKey: .auto_reset_credit)
    }

    public init(
        id: Int,
        name: String,
        priority: Int = 1,
        platform: String = "openai",
        type: String = "oauth",
        status: String = "active",
        schedulable: Bool = true,
        available: Bool = true,
        group_ids: [Int] = [],
        blockers: [AccountBlocker] = [],
        version: String = "",
        operation_versions: [String: String] = [:],
        last_success_at: String? = nil,
        last_called_at: String? = nil,
        last_error_at: String? = nil,
        last_error_id: Int? = nil,
        last_error_code: String? = nil,
        last_error_status: Int? = nil,
        error_message: String = "",
        success_after_error: Bool = false,
        usage_windows: [UsageWindow] = []
    ) {
        self.id = id
        self.name = name
        self.priority = priority
        self.platform = platform
        self.type = type
        self.status = status
        self.schedulable = schedulable
        self.available = available
        self.group_ids = group_ids
        self.blockers = blockers
        self.version = version
        self.operation_versions = operation_versions
        self.last_success_at = last_success_at
        self.last_called_at = last_called_at
        self.last_error_at = last_error_at
        self.last_error_id = last_error_id
        self.last_error_code = last_error_code
        self.last_error_status = last_error_status
        self.error_message = error_message
        self.success_after_error = success_after_error
        self.usage_windows = usage_windows
    }
}

public struct RecentAccount: Codable, Sendable, Equatable {
    public var log_id: Int
    public var account_id: Int
    public var account_name: String
    public var model: String
    public var upstream_model: String
    public var called_at: String?

    public init(log_id: Int, account_id: Int, account_name: String, model: String = "", upstream_model: String = "", called_at: String? = nil) {
        self.log_id = log_id
        self.account_id = account_id
        self.account_name = account_name
        self.model = model
        self.upstream_model = upstream_model
        self.called_at = called_at
    }
}

public struct Group: Codable, Sendable, Equatable, Identifiable {
    public var id: Int
    public var sort_order: Int?
    public var name: String
    public var platform: String
    public var account_id: Int?
    public var account_name: String
    public var model: String
    public var upstream_model: String
    public var called_at: String?
    public var recent_accounts: [RecentAccount]

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(Int.self, forKey: .id)
        sort_order = try c.decodeIfPresent(Int.self, forKey: .sort_order)
        name = try c.decodeIfPresent(String.self, forKey: .name) ?? ""
        platform = try c.decodeIfPresent(String.self, forKey: .platform) ?? ""
        account_id = try c.decodeIfPresent(Int.self, forKey: .account_id)
        account_name = try c.decodeIfPresent(String.self, forKey: .account_name) ?? ""
        model = try c.decodeIfPresent(String.self, forKey: .model) ?? ""
        upstream_model = try c.decodeIfPresent(String.self, forKey: .upstream_model) ?? ""
        called_at = try c.decodeIfPresent(String.self, forKey: .called_at)
        recent_accounts = try c.decodeIfPresent([RecentAccount].self, forKey: .recent_accounts) ?? []
    }

    public init(id: Int, name: String, platform: String = "openai", recent_accounts: [RecentAccount] = [], called_at: String? = nil) {
        self.id = id
        self.name = name
        self.platform = platform
        self.account_name = ""
        self.model = ""
        self.upstream_model = ""
        self.called_at = called_at
        self.recent_accounts = recent_accounts
    }
}

public func computeCurrentGroups(_ groups: [Group], accounts: [Account]) -> [Group] {
    let members = Dictionary(uniqueKeysWithValues: accounts.map { ($0.id, $0) })
    let mapped: [Group] = groups.map { group in
        let ordered = group.recent_accounts.filter { call in
            members[call.account_id]?.group_ids.contains(group.id) == true
        }.sorted { a, b in
            let ta = BeijingTime.callTimestamp(a.called_at)
            let tb = BeijingTime.callTimestamp(b.called_at)
            if ta != tb { return tb < ta }
            return b.log_id < a.log_id
        }
        var seen = Set<Int>()
        var recent: [RecentAccount] = []
        for call in ordered where seen.insert(call.account_id).inserted {
            recent.append(call)
            if recent.count == 3 { break }
        }
        var updated = group
        updated.recent_accounts = recent
        updated.called_at = recent.first?.called_at
        return updated
    }
    return mapped.sorted { a, b in
        let ta = BeijingTime.callTimestamp(a.called_at)
        let tb = BeijingTime.callTimestamp(b.called_at)
        if ta != tb { return tb < ta }
        return a.id < b.id
    }
}

public struct ErrorDispositionNotification: Codable, Sendable, Equatable {
    public var status: String
}

public struct ErrorClueNotification: Codable, Sendable, Equatable {
    public var status: String
    public var reason: String?
    public var at: String?
    public var attempts: Int?
    public var next_at: String?
}

public struct OpsError: Codable, Sendable, Equatable, Identifiable {
    public var id: Int
    public var account_id: Int?
    public var account_name: String
    public var group_id: Int?
    public var group_name: String
    public var created_at: String
    public var model: String
    public var requested_model: String
    public var upstream_model: String
    public var status_code: Int
    public var upstream_status_code: Int?
    public var provider_error_code: String
    public var message: String
    public var request_id: String
    public var resolved: Bool
    public var error_owner: String?
    public var error_phase: String?
    public var error_source: String?
    public var disposition_notification: ErrorDispositionNotification?
    public var content: String?
    public var content_limited: Bool?
    public var notification: ErrorClueNotification?

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(Int.self, forKey: .id)
        account_id = try c.decodeIfPresent(Int.self, forKey: .account_id)
        account_name = try c.decodeIfPresent(String.self, forKey: .account_name) ?? ""
        group_id = try c.decodeIfPresent(Int.self, forKey: .group_id)
        group_name = try c.decodeIfPresent(String.self, forKey: .group_name) ?? ""
        created_at = try c.decodeIfPresent(String.self, forKey: .created_at) ?? ""
        model = try c.decodeIfPresent(String.self, forKey: .model) ?? ""
        requested_model = try c.decodeIfPresent(String.self, forKey: .requested_model) ?? ""
        upstream_model = try c.decodeIfPresent(String.self, forKey: .upstream_model) ?? ""
        status_code = try c.decodeIfPresent(Int.self, forKey: .status_code) ?? 0
        upstream_status_code = try c.decodeIfPresent(Int.self, forKey: .upstream_status_code)
        provider_error_code = try c.decodeIfPresent(String.self, forKey: .provider_error_code) ?? ""
        message = try c.decodeIfPresent(String.self, forKey: .message) ?? ""
        request_id = try c.decodeIfPresent(String.self, forKey: .request_id) ?? ""
        resolved = try c.decodeIfPresent(Bool.self, forKey: .resolved) ?? false
        error_owner = try c.decodeIfPresent(String.self, forKey: .error_owner)
        error_phase = try c.decodeIfPresent(String.self, forKey: .error_phase)
        error_source = try c.decodeIfPresent(String.self, forKey: .error_source)
        disposition_notification = try c.decodeIfPresent(ErrorDispositionNotification.self, forKey: .disposition_notification)
        content = try c.decodeIfPresent(String.self, forKey: .content)
        content_limited = try c.decodeIfPresent(Bool.self, forKey: .content_limited)
        notification = try c.decodeIfPresent(ErrorClueNotification.self, forKey: .notification)
    }
}

public struct RecoveryResetCredit: Codable, Sendable, Equatable {
    public var consumed: Bool?
    public var completed_at: String
    public var verification_method: String
}

public struct Recovery: Codable, Sendable, Equatable, Identifiable {
    public var id: Int
    public var account_id: Int
    public var account_name: String
    public var model_id: String
    public var test_completed_at: String?
    public var recovered_at: String?
    public var legacy: Bool
    public var kind: String?
    public var result_status: String?
    public var attempt_count: Int?
    public var reset_credit: RecoveryResetCredit?
}

public struct Snapshot: Codable, Sendable, Equatable {
    public var observed_at: String
    public var accounts: [Account]
    public var groups: [Group]
    public var errors: [OpsError]
    public var recoveries: [Recovery]

    public init(
        observed_at: String,
        accounts: [Account] = [],
        groups: [Group] = [],
        errors: [OpsError] = [],
        recoveries: [Recovery] = []
    ) {
        self.observed_at = observed_at
        self.accounts = accounts
        self.groups = groups
        self.errors = errors
        self.recoveries = recoveries
    }
}

public struct SavedConnection: Codable, Sendable, Equatable, Hashable {
    public var name: String
    public var base_url: String

    public init(name: String, base_url: String) {
        self.name = name
        self.base_url = base_url
    }
}

public struct ModelTestPreference: Codable, Sendable, Equatable {
    public var engine: String
    public var sample_count: Int

    public init(engine: String, sample_count: Int) {
        self.engine = engine
        self.sample_count = sample_count
    }
}

public struct Preferences: Codable, Sendable, Equatable {
    public var base_url: String
    public var connections: [SavedConnection]
    public var pinned: Bool
    public var launch_at_login: Bool
    public var record_columns: [String]?
    public var model_test_concurrency: Int?
    public var model_test_preferences: [String: ModelTestPreference]?

    public init(
        base_url: String = "",
        connections: [SavedConnection] = [],
        pinned: Bool = false,
        launch_at_login: Bool = false,
        record_columns: [String]? = nil,
        model_test_concurrency: Int? = nil,
        model_test_preferences: [String: ModelTestPreference]? = nil
    ) {
        self.base_url = base_url
        self.connections = connections
        self.pinned = pinned
        self.launch_at_login = launch_at_login
        self.record_columns = record_columns
        self.model_test_concurrency = model_test_concurrency
        self.model_test_preferences = model_test_preferences
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        base_url = try c.decodeIfPresent(String.self, forKey: .base_url) ?? ""
        connections = try c.decodeIfPresent([SavedConnection].self, forKey: .connections) ?? []
        pinned = try c.decodeIfPresent(Bool.self, forKey: .pinned) ?? false
        launch_at_login = try c.decodeIfPresent(Bool.self, forKey: .launch_at_login) ?? false
        record_columns = try c.decodeIfPresent([String].self, forKey: .record_columns)
        model_test_concurrency = try c.decodeIfPresent(Int.self, forKey: .model_test_concurrency)
        model_test_preferences = try c.decodeIfPresent([String: ModelTestPreference].self, forKey: .model_test_preferences)
    }
}

public func defaultConnectionName(_ baseUrl: String) -> String {
    let trimmed = baseUrl.trimmingCharacters(in: .whitespacesAndNewlines).replacingOccurrences(of: "/+$", with: "", options: .regularExpression)
    guard !trimmed.isEmpty else { return "未命名实例" }
    guard let parsed = URL(string: trimmed), let host = parsed.host, !host.isEmpty else {
        return trimmed
    }
    if let port = parsed.port {
        return "\(host):\(port)"
    }
    return host
}

public func normalizedSavedConnections(_ preferences: Preferences) -> [SavedConnection] {
    var seen = Set<String>()
    var list: [SavedConnection] = []
    for item in preferences.connections {
        let url = item.base_url.trimmingCharacters(in: .whitespacesAndNewlines).replacingOccurrences(of: "/+$", with: "", options: .regularExpression)
        guard !url.isEmpty, seen.insert(url).inserted else { continue }
        let name = item.name.trimmingCharacters(in: .whitespacesAndNewlines)
        list.append(SavedConnection(name: name.isEmpty ? defaultConnectionName(url) : name, base_url: url))
    }
    let active = preferences.base_url.trimmingCharacters(in: .whitespacesAndNewlines).replacingOccurrences(of: "/+$", with: "", options: .regularExpression)
    if !active.isEmpty && seen.insert(active).inserted {
        list.append(SavedConnection(name: defaultConnectionName(active), base_url: active))
    }
    return list
}

public struct UpstreamConnectionStatus: Codable, Sendable, Equatable {
    public var state: String
    public var endpoint: String
    public var http_status: Int?
    public var message: String
    public var retryable: Bool
    public var checked_at: String?

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        state = try c.decodeIfPresent(String.self, forKey: .state) ?? ""
        endpoint = try c.decodeIfPresent(String.self, forKey: .endpoint) ?? ""
        http_status = try c.decodeIfPresent(Int.self, forKey: .http_status)
        message = try c.decodeIfPresent(String.self, forKey: .message) ?? ""
        retryable = try c.decodeIfPresent(Bool.self, forKey: .retryable) ?? false
        checked_at = try c.decodeIfPresent(String.self, forKey: .checked_at)
    }

    public init(state: String, endpoint: String = "", http_status: Int? = nil, message: String = "", retryable: Bool = false, checked_at: String? = nil) {
        self.state = state
        self.endpoint = endpoint
        self.http_status = http_status
        self.message = message
        self.retryable = retryable
        self.checked_at = checked_at
    }
}

public struct ViewState: Codable, Sendable, Equatable {
    public var platform: String?
    public var foreground: Bool?
    public var initializing: Bool?
    public var connection_revision: UInt64?
    public var connected: Bool
    public var online: Bool
    public var error: String
    public var upstream_connection: UpstreamConnectionStatus?
    public var snapshot: Snapshot?
    public var preferences: Preferences

    public init(
        platform: String? = "macos",
        foreground: Bool? = true,
        initializing: Bool? = false,
        connection_revision: UInt64? = 0,
        connected: Bool = false,
        online: Bool = false,
        error: String = "",
        upstream_connection: UpstreamConnectionStatus? = nil,
        snapshot: Snapshot? = nil,
        preferences: Preferences = Preferences()
    ) {
        self.platform = platform
        self.foreground = foreground
        self.initializing = initializing
        self.connection_revision = connection_revision
        self.connected = connected
        self.online = online
        self.error = error
        self.upstream_connection = upstream_connection
        self.snapshot = snapshot
        self.preferences = preferences
    }
}

public struct TestEvent: Codable, Sendable, Equatable {
    public var type: String
    public var text: String?
    public var model: String?
    public var error: String?
    public var success: Bool?
    public var image_url: String?
    public var audio_url: String?
    public var video_url: String?
    public var duration_ms: Double?
    public var completed_at: String?

    public init(
        type: String,
        text: String? = nil,
        model: String? = nil,
        error: String? = nil,
        success: Bool? = nil,
        image_url: String? = nil,
        audio_url: String? = nil,
        video_url: String? = nil,
        duration_ms: Double? = nil,
        completed_at: String? = nil
    ) {
        self.type = type
        self.text = text
        self.model = model
        self.error = error
        self.success = success
        self.image_url = image_url
        self.audio_url = audio_url
        self.video_url = video_url
        self.duration_ms = duration_ms
        self.completed_at = completed_at
    }
}

public struct Operation: Codable, Sendable, Equatable, Identifiable {
    public var id: String
    public var account_id: Int
    public var account_name: String
    public var action: String
    public var status: String
    public var batch_id: String?
    public var reason: String?
    public var requested: [String: JSONValue]
    public var current: [String: JSONValue]?
    public var result: [String: JSONValue]?
    public var events: [TestEvent]?
    public var next_event: Int?
    public var started_at: String?
    public var scheduled_at: String?
    public var attempt_count: Int?
    public var next_retry_at: String?
    public var repeat_mode: String?
    public var repeat_series_id: String?
    public var repeat_state: String?
    public var repeat_reason: String?
    public var next_operation_id: String?

    enum CodingKeys: String, CodingKey {
        case id, account_id, account_name, action, status, batch_id, reason
        case requested, current, result, events, next_event
        case started_at, scheduled_at, attempt_count, next_retry_at
        case repeat_mode = "repeat"
        case repeat_series_id, repeat_state, repeat_reason, next_operation_id
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        account_id = try c.decodeIfPresent(Int.self, forKey: .account_id) ?? 0
        account_name = try c.decodeIfPresent(String.self, forKey: .account_name) ?? ""
        action = try c.decodeIfPresent(String.self, forKey: .action) ?? ""
        status = try c.decodeIfPresent(String.self, forKey: .status) ?? ""
        batch_id = try c.decodeIfPresent(String.self, forKey: .batch_id)
        reason = try c.decodeIfPresent(String.self, forKey: .reason)
        requested = try c.decodeIfPresent([String: JSONValue].self, forKey: .requested) ?? [:]
        current = try c.decodeIfPresent([String: JSONValue].self, forKey: .current)
        result = try c.decodeIfPresent([String: JSONValue].self, forKey: .result)
        events = try c.decodeIfPresent([TestEvent].self, forKey: .events)
        next_event = try c.decodeIfPresent(Int.self, forKey: .next_event)
        started_at = try c.decodeIfPresent(String.self, forKey: .started_at)
        scheduled_at = try c.decodeIfPresent(String.self, forKey: .scheduled_at)
        attempt_count = try c.decodeIfPresent(Int.self, forKey: .attempt_count)
        next_retry_at = try c.decodeIfPresent(String.self, forKey: .next_retry_at)
        repeat_mode = try c.decodeIfPresent(String.self, forKey: .repeat_mode)
        repeat_series_id = try c.decodeIfPresent(String.self, forKey: .repeat_series_id)
        repeat_state = try c.decodeIfPresent(String.self, forKey: .repeat_state)
        repeat_reason = try c.decodeIfPresent(String.self, forKey: .repeat_reason)
        next_operation_id = try c.decodeIfPresent(String.self, forKey: .next_operation_id)
    }

    public init(
        id: String,
        account_id: Int,
        account_name: String,
        action: String,
        status: String,
        reason: String? = nil,
        requested: [String: JSONValue] = [:],
        current: [String: JSONValue]? = nil,
        scheduled_at: String? = nil,
        repeat_mode: String? = nil,
        repeat_state: String? = nil
    ) {
        self.id = id
        self.account_id = account_id
        self.account_name = account_name
        self.action = action
        self.status = status
        self.reason = reason
        self.requested = requested
        self.current = current
        self.scheduled_at = scheduled_at
        self.repeat_mode = repeat_mode
        self.repeat_state = repeat_state
    }
}

public struct OperationBatch: Codable, Sendable, Equatable {
    public var batch_id: String
    public var items: [Operation]
    public var pending: Int

    public init(batch_id: String, items: [Operation], pending: Int) {
        self.batch_id = batch_id
        self.items = items
        self.pending = pending
    }
}
