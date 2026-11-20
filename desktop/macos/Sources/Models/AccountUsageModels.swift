import Foundation

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
