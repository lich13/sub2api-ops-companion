import Foundation
import Observation

public typealias KeyQuotaGlobalConfig = GlobalQuotaReply
public typealias AccountKeyQuotaSettings = AccountQuotaReply

@MainActor
@Observable
public final class AccountUsageSlot {
    public var cachedIdentity: String = ""
    public var overrideSummary: UsageSummary?
    public var isLoading: Bool = false
    public var errorIdentity: String = ""
    public var errorMessage: String?
    @ObservationIgnored fileprivate var lastIdentity: String = ""
    @ObservationIgnored fileprivate var cooldownIdentity: String = ""
    @ObservationIgnored fileprivate var cooldownUntil: Date?

    public init() {}
}

/// 4-slot concurrent native usage reader matching `desktop/src/nativeUsage.ts` & `UsageCell.tsx`.
@MainActor
public final class NativeUsageReader {
    private var slots: [Int: AccountUsageSlot] = [:]
    private var connectionKey: String = ""
    private var generation: UInt64 = 0
    private var activeSlots: Int = 0
    private var queue: [(accountId: Int, identity: String)] = []
    private let maxConcurrency = 4
    private static let failureCooldown: TimeInterval = 60

    public init() {}

    public func slot(for accountId: Int) -> AccountUsageSlot {
        if let existing = slots[accountId] {
            return existing
        }
        let created = AccountUsageSlot()
        slots[accountId] = created
        return created
    }

    public func usageIdentity(for account: Account, accountById: [Int: Account]) -> String {
        let parentVer: String
        if account.type == "apikey", let pid = account.parent_account_id, let parent = accountById[pid] {
            parentVer = parent.operation_versions["usage"] ?? ""
        } else {
            parentVer = ""
        }
        return "\(connectionKey):\(account.id):\(account.operation_versions["usage"] ?? ""):\(parentVer)"
    }

    public func bindConnection(_ key: String) {
        guard connectionKey != key else { return }
        connectionKey = key
        generation &+= 1
        for slot in slots.values {
            slot.cachedIdentity = ""
            slot.overrideSummary = nil
            slot.isLoading = false
            slot.errorIdentity = ""
            slot.errorMessage = nil
            slot.lastIdentity = ""
            slot.cooldownIdentity = ""
            slot.cooldownUntil = nil
        }
        queue.removeAll()
        activeSlots = 0
    }

    public func resolvedUsage(for account: Account, accountById: [Int: Account] = [:]) -> UsageSummary? {
        let identity = usageIdentity(for: account, accountById: accountById)
        let s = slot(for: account.id)
        s.lastIdentity = identity
        guard s.cachedIdentity == identity, let cached = s.overrideSummary else {
            return account.usage
        }
        let mergedToday: UsageStats? = account.usage?.stats_status == "error"
            ? nil
            : (account.usage?.today ?? cached.today)
        let mergedStatsStatus: String? = cached.stats_status == "error"
            ? "error"
            : (account.usage?.stats_status ?? cached.stats_status)
        return UsageSummary(
            stats_status: mergedStatsStatus,
            key_quota: cached.key_quota,
            branch: cached.branch,
            native_read: cached.native_read,
            status: cached.status,
            message: cached.message,
            balances: cached.balances,
            windows: cached.windows,
            today: mergedToday,
            actions: cached.actions,
            reset_credits: cached.reset_credits,
            prepaid_balance: cached.prepaid_balance,
            monthly_limit: cached.monthly_limit,
            monthly_used: cached.monthly_used
        )
    }

    public func isLoading(accountId: Int) -> Bool {
        slot(for: accountId).isLoading
    }

    public func error(for account: Account, accountById: [Int: Account] = [:]) -> String? {
        let identity = usageIdentity(for: account, accountById: accountById)
        let s = slot(for: account.id)
        return s.errorIdentity == identity ? s.errorMessage : nil
    }

    public func enqueueIfNeeded(
        accounts: [Account],
        accountById: [Int: Account] = [:],
        online: Bool,
        now: Date = Date()
    ) {
        guard online else { return }
        let lookup = accountById.isEmpty
            ? Dictionary(accounts.map { ($0.id, $0) }, uniquingKeysWith: { _, last in last })
            : accountById
        let liveIds = Set(accounts.map(\.id))
        slots = slots.filter { liveIds.contains($0.key) }
        for account in accounts where account.usage?.native_read == true {
            let identity = usageIdentity(for: account, accountById: lookup)
            let s = slot(for: account.id)
            s.lastIdentity = identity
            if s.cachedIdentity == identity && s.overrideSummary != nil {
                continue
            }
            if s.cooldownIdentity == identity, let until = s.cooldownUntil, now < until {
                continue
            }
            let needsInitialRead = account.usage?.windows.isEmpty == true
                && account.usage?.key_quota == nil
                && account.usage?.balances == nil
            let invalidatedOverride = !s.cachedIdentity.isEmpty && s.cachedIdentity != identity
            if (needsInitialRead || invalidatedOverride)
                && !s.isLoading
                && !queue.contains(where: { $0.accountId == account.id })
            {
                queue.append((account.id, identity))
            }
        }
        pumpQueue()
    }

    public func refresh(account: Account, accountById: [Int: Account] = [:]) {
        let identity = usageIdentity(for: account, accountById: accountById)
        let s = slot(for: account.id)
        s.lastIdentity = identity
        s.errorMessage = nil
        s.errorIdentity = ""
        s.cooldownUntil = nil
        s.cooldownIdentity = ""
        if !s.isLoading && !queue.contains(where: { $0.accountId == account.id }) {
            queue.insert((account.id, identity), at: 0)
        }
        pumpQueue()
    }

    public func refreshAll(accounts: [Account], accountById: [Int: Account] = [:]) {
        let lookup = accountById.isEmpty
            ? Dictionary(accounts.map { ($0.id, $0) }, uniquingKeysWith: { _, last in last })
            : accountById
        for account in accounts where account.usage?.native_read == true {
            let identity = usageIdentity(for: account, accountById: lookup)
            let s = slot(for: account.id)
            s.lastIdentity = identity
            s.errorMessage = nil
            s.errorIdentity = ""
            s.cooldownUntil = nil
            s.cooldownIdentity = ""
            if !s.isLoading && !queue.contains(where: { $0.accountId == account.id }) {
                queue.append((account.id, identity))
            }
        }
        pumpQueue()
    }

    private func pumpQueue() {
        let capturedGen = generation
        let capturedKey = connectionKey
        while activeSlots < maxConcurrency, !queue.isEmpty {
            let item = queue.removeFirst()
            let accountId = item.accountId
            let identity = item.identity
            let s = slot(for: accountId)
            activeSlots += 1
            s.isLoading = true
            Task { @MainActor [weak self] in
                guard let self else { return }
                defer {
                    if self.generation == capturedGen {
                        self.activeSlots = max(0, self.activeSlots - 1)
                        self.slot(for: accountId).isLoading = false
                        self.pumpQueue()
                    }
                }
                do {
                    let summary: UsageSummary = try await RustBridge.shared.api("GET", "/accounts/\(accountId)/usage")
                    if self.generation == capturedGen && self.connectionKey == capturedKey {
                        let target = self.slot(for: accountId)
                        if target.lastIdentity.isEmpty || target.lastIdentity == identity {
                            target.cachedIdentity = identity
                            target.overrideSummary = summary
                            target.errorIdentity = ""
                            target.errorMessage = nil
                            target.cooldownIdentity = ""
                            target.cooldownUntil = nil
                        }
                    }
                } catch {
                    if self.generation == capturedGen && self.connectionKey == capturedKey {
                        let target = self.slot(for: accountId)
                        if target.lastIdentity.isEmpty || target.lastIdentity == identity {
                            target.errorIdentity = identity
                            target.errorMessage = error.localizedDescription
                            target.cooldownIdentity = identity
                            target.cooldownUntil = Date().addingTimeInterval(Self.failureCooldown)
                        }
                    }
                }
            }
        }
    }
}

public enum UsageFormatting {
    public static func compactTokens(_ tokens: Int) -> String {
        compactTokens(Double(tokens))
    }

    public static func compactTokens(_ tokens: Double) -> String {
        guard tokens.isFinite else { return "0" }
        let clamped = max(0, tokens)
        let rounded = clamped.rounded()
        if rounded < 1_000 {
            return "\(Int(rounded))"
        }
        let tenthsK = max(10, (clamped / 100.0).rounded())
        if tenthsK < 10_000 {
            return String(format: "%.1fK", tenthsK / 10.0)
        }
        let tenthsM = max(10, (clamped / 100_000.0).rounded())
        if tenthsM < 10_000 {
            return String(format: "%.1fM", tenthsM / 10.0)
        }
        let tenthsB = max(10, (clamped / 100_000_000.0).rounded())
        return String(format: "%.1fB", tenthsB / 10.0)
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
