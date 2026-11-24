import Foundation

public typealias Account = AccountRecord
public typealias RecentAccount = RecentAccountRecord
public typealias Group = GroupRecord

extension AccountRecord: Identifiable {
    public var statusText: String { status ?? "" }
    public var errorMessageText: String { error_message ?? "" }
    public var isSuccessAfterError: Bool { success_after_error ?? false }

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
        blockers: [AccountBlockerRecord] = [],
        recoverable: Bool? = nil,
        recovery_selectable: Bool? = nil,
        parent_account_id: Int? = nil,
        concurrency: Int? = nil,
        load_factor: Int? = nil,
        settings_capabilities: [String: SettingCapabilityRecord]? = nil,
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
        usage_windows: [UsageWindowRecord] = [],
        usage: UsageSummaryRecord? = nil,
        degradation_mark: DegradationMarkRecord? = nil,
        model_detection: ModelDetectionSummaryRecord? = nil,
        recovery_monitor: RecoveryMonitorStateRecord? = nil,
        auto_reset_credit: AutoResetCreditStateRecord? = nil
    ) {
        self.init(
            degradation_mark: degradation_mark,
            recovery_selectable: recovery_selectable,
            model_detection: model_detection,
            id: id,
            parent_account_id: parent_account_id,
            name: name,
            priority: priority,
            concurrency: concurrency,
            load_factor: load_factor,
            settings_capabilities: settings_capabilities,
            platform: platform,
            type: type,
            status: status,
            schedulable: schedulable,
            available: available,
            group_ids: group_ids,
            blockers: blockers,
            recoverable: recoverable,
            version: version,
            operation_versions: operation_versions,
            last_success_at: last_success_at,
            last_called_at: last_called_at,
            last_error_at: last_error_at,
            last_error_id: last_error_id,
            last_error_code: last_error_code,
            last_error_status: last_error_status,
            error_message: error_message,
            success_after_error: success_after_error,
            usage_windows: usage_windows,
            usage: usage,
            recovery_monitor: recovery_monitor,
            auto_reset_credit: auto_reset_credit
        )
    }
}

extension RecentAccountRecord {
    public init(
        log_id: Int,
        account_id: Int,
        account_name: String,
        model: String = "",
        called_at: String? = nil
    ) {
        self.init(
            log_id: log_id,
            account_id: account_id,
            account_name: account_name,
            model: model,
            upstream_model: "",
            called_at: called_at
        )
    }
}

extension GroupRecord: Identifiable {
    public init(
        id: Int,
        name: String,
        platform: String = "openai",
        recent_accounts: [RecentAccountRecord] = [],
        called_at: String? = nil
    ) {
        self.init(
            id: id,
            sort_order: nil,
            name: name,
            platform: platform,
            account_id: nil,
            account_name: "",
            model: "",
            upstream_model: "",
            called_at: called_at,
            recent_accounts: recent_accounts
        )
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
        for call in ordered {
            guard seen.insert(call.account_id).inserted else { continue }
            recent.append(call)
            if recent.count == 3 { break }
        }
        return GroupRecord(
            id: group.id,
            sort_order: group.sort_order,
            name: group.name,
            platform: group.platform,
            account_id: group.account_id,
            account_name: group.account_name,
            model: group.model,
            upstream_model: group.upstream_model,
            called_at: recent.first?.called_at,
            recent_accounts: recent
        )
    }
    return mapped.sorted { a, b in
        let ta = BeijingTime.callTimestamp(a.called_at)
        let tb = BeijingTime.callTimestamp(b.called_at)
        if ta != tb { return tb < ta }
        return a.id < b.id
    }
}
