import Foundation

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
