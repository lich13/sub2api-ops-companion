import Foundation
import Observation

public struct QuotaRefreshItem: Codable, Sendable, Equatable {
    public var account_id: Int?
    public var status: String?
    public var error: String?
}

public struct QuotaRefreshJob: Codable, Sendable, Equatable {
    public var id: String?
    public var status: String
    public var total: Int
    public var completed: Int
    public var items: [QuotaRefreshItem]
    public var started_at: String?
    public var completed_at: String?

    public init(status: String = "idle", total: Int = 0, completed: Int = 0, items: [QuotaRefreshItem] = []) {
        self.status = status
        self.total = total
        self.completed = completed
        self.items = items
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decodeIfPresent(String.self, forKey: .id)
        status = try c.decodeIfPresent(String.self, forKey: .status) ?? "idle"
        total = try c.decodeIfPresent(Int.self, forKey: .total) ?? 0
        completed = try c.decodeIfPresent(Int.self, forKey: .completed) ?? 0
        items = try c.decodeIfPresent([QuotaRefreshItem].self, forKey: .items) ?? []
        started_at = try c.decodeIfPresent(String.self, forKey: .started_at)
        completed_at = try c.decodeIfPresent(String.self, forKey: .completed_at)
    }
}

public typealias KeyQuotaGlobalConfig = GlobalQuotaReply
public typealias AccountKeyQuotaSettings = AccountQuotaReply
public typealias AccountKeyQuotaTokenResponse = AccountTokenReply

/// 4-slot concurrent native usage reader matching `desktop/src/nativeUsage.ts`.
@MainActor
@Observable
public final class NativeUsageReader {
    public private(set) var overrides: [Int: UsageSummary] = [:]
    public private(set) var loadingIds: Set<Int> = []
    public private(set) var errors: [Int: String] = [:]

    private var connectionKey: String = ""
    private var generation: UInt64 = 0
    private var activeSlots: Int = 0
    private var queue: [Int] = []
    private let maxConcurrency = 4

    public init() {}

    public func bindConnection(_ key: String) {
        guard connectionKey != key else { return }
        connectionKey = key
        generation &+= 1
        overrides.removeAll()
        loadingIds.removeAll()
        errors.removeAll()
        queue.removeAll()
        activeSlots = 0
    }

    public func resolvedUsage(for account: Account) -> UsageSummary? {
        overrides[account.id] ?? account.usage
    }

    public func enqueueIfNeeded(accounts: [Account], online: Bool) {
        guard online else { return }
        for account in accounts where account.usage?.native_read == true {
            if overrides[account.id] == nil && !loadingIds.contains(account.id) && !queue.contains(account.id) && account.usage?.windows.isEmpty == true && account.usage?.key_quota == nil && account.usage?.balances == nil {
                queue.append(account.id)
            }
        }
        pumpQueue()
    }

    public func refresh(accountId: Int) {
        errors[accountId] = nil
        if !loadingIds.contains(accountId) && !queue.contains(accountId) {
            queue.insert(accountId, at: 0)
        }
        pumpQueue()
    }

    public func refreshAll(accounts: [Account]) {
        for account in accounts where account.usage?.native_read == true {
            if !loadingIds.contains(account.id) && !queue.contains(account.id) {
                queue.append(account.id)
            }
        }
        pumpQueue()
    }

    private func pumpQueue() {
        let capturedGen = generation
        let capturedKey = connectionKey
        while activeSlots < maxConcurrency, !queue.isEmpty {
            let accountId = queue.removeFirst()
            activeSlots += 1
            loadingIds.insert(accountId)
            Task { @MainActor [weak self] in
                guard let self else { return }
                defer {
                    if self.generation == capturedGen {
                        self.activeSlots = max(0, self.activeSlots - 1)
                        self.loadingIds.remove(accountId)
                        self.pumpQueue()
                    }
                }
                do {
                    let summary: UsageSummary = try await RustBridge.shared.api("GET", "/accounts/\(accountId)/usage")
                    if self.generation == capturedGen && self.connectionKey == capturedKey {
                        self.overrides[accountId] = summary
                        self.errors[accountId] = nil
                    }
                } catch {
                    if self.generation == capturedGen && self.connectionKey == capturedKey {
                        self.errors[accountId] = error.localizedDescription
                    }
                }
            }
        }
    }
}

public enum UsageFormatting {
    public static func compactTokens(_ tokens: Int) -> String {
        if tokens >= 1_000_000 {
            return String(format: "%.1fM", Double(tokens) / 1_000_000.0)
        }
        if tokens >= 1_000 {
            return String(format: "%.1fK", Double(tokens) / 1_000.0)
        }
        return "\(tokens)"
    }

    public static func money(_ amount: Double) -> String {
        String(format: "$%.2f", amount)
    }

    public static func quotaAmount(_ value: Double?, unit: String) -> String {
        guard let value else { return "—" }
        if unit.uppercased() == "USD" {
            return String(format: "$%.2f", value)
        }
        if unit.uppercased() == "CNY" {
            return String(format: "¥%.2f", value)
        }
        return String(format: "%.2f %@", value, unit)
    }

    public static func countdownString(until raw: String?, now: Date = Date()) -> String? {
        guard let target = BeijingTime.parseISO(raw) else { return nil }
        let seconds = Int(target.timeIntervalSince(now).rounded())
        if seconds <= 0 { return "即将解除" }
        let days = seconds / 86400
        let hours = (seconds % 86400) / 3600
        let minutes = (seconds % 3600) / 60
        let secs = seconds % 60
        if days > 0 {
            return "\(days)天\(hours)小时后自动解除"
        }
        if hours > 0 {
            return "\(hours)小时\(minutes)分后自动解除"
        }
        if minutes > 0 {
            return "\(minutes)分\(secs)秒后自动解除"
        }
        return "\(secs)秒后自动解除"
    }
}
