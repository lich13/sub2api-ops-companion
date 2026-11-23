import Foundation

public typealias UsageStats = UsageStatsRecord
public typealias QuotaPlan = QuotaPlanRecord
public typealias QuotaSubscription = QuotaSubscriptionRecord
public typealias KeyQuotaUsage = KeyQuotaUsageRecord
public typealias KeyQuota = KeyQuotaRecord
public typealias UsageBalance = UsageBalanceRecord
public typealias ResetCreditsInfo = ResetCreditsInfoRecord
public typealias UsageWindow = UsageWindowRecord
public typealias UsageSummary = UsageSummaryRecord

extension ResetCreditsInfoRecord {
    public init(available: Int?, expires_at: [String]) {
        self.init(available: available, expires_at: expires_at, observed_at: nil)
    }
}

extension UsageWindowRecord {
    public init(
        key: String,
        label: String,
        used_percent: Double?,
        reset_at: String?,
        observed_at: String? = nil,
        status: String = "known",
        source: String = "upstream_snapshot",
        window_minutes: Double? = nil,
        estimated: Bool? = nil,
        used: Double? = nil,
        limit: Double? = nil,
        remaining: Double? = nil,
        color: String? = nil,
        stats: UsageStatsRecord? = nil,
        estimated_total_cost: Double? = nil
    ) {
        self.init(
            window_minutes: window_minutes,
            estimated: estimated,
            key: key,
            label: label,
            used_percent: used_percent,
            reset_at: reset_at,
            observed_at: observed_at,
            status: status,
            source: source,
            used: used,
            limit: limit,
            remaining: remaining,
            color: color,
            stats: stats,
            estimated_total_cost: estimated_total_cost
        )
    }
}

extension UsageSummaryRecord {
    public init(
        branch: String,
        windows: [UsageWindowRecord] = [],
        today: UsageStatsRecord? = nil,
        actions: [String] = [],
        reset_credits: ResetCreditsInfoRecord? = nil,
        key_quota: KeyQuotaRecord? = nil,
        balances: [UsageBalanceRecord]? = nil,
        native_read: Bool? = nil,
        stats_status: String? = nil,
        status: String? = nil,
        message: String? = nil,
        prepaid_balance: Double? = nil,
        monthly_limit: Double? = nil,
        monthly_used: Double? = nil
    ) {
        self.init(
            stats_status: stats_status,
            key_quota: key_quota,
            branch: branch,
            native_read: native_read,
            status: status,
            message: message,
            balances: balances,
            windows: windows,
            today: today,
            actions: actions,
            reset_credits: reset_credits,
            prepaid_balance: prepaid_balance,
            monthly_limit: monthly_limit,
            monthly_used: monthly_used
        )
    }
}
