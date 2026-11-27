import Foundation
import Observation

public enum AccountSortKey: String, CaseIterable, Codable, Sendable {
    case recent
    case priority
    case name
    case usage

    public var label: String {
        switch self {
        case .recent: return "最近调用"
        case .priority: return "优先级"
        case .name: return "账号名称"
        case .usage: return "最高用量"
        }
    }
}

public struct AccountSortOption: Codable, Sendable, Equatable {
    public var key: AccountSortKey
    public var ascending: Bool

    public init(key: AccountSortKey = .recent, ascending: Bool = false) {
        self.key = key
        self.ascending = ascending
    }
}

@MainActor
@Observable
public final class AccountSortStore {
    private static let storageKey = "sub2ops-account-sort"
    public var option: AccountSortOption {
        didSet { persist() }
    }

    public init() {
        if let raw = UserDefaults.standard.data(forKey: Self.storageKey),
           let decoded = try? JSONDecoder().decode(AccountSortOption.self, from: raw)
        {
            self.option = decoded
        } else {
            self.option = AccountSortOption(key: .recent, ascending: false)
        }
    }

    public func selectKey(_ key: AccountSortKey) {
        if option.key == key {
            option.ascending.toggle()
        } else {
            let defaultAsc = (key == .priority || key == .name)
            option = AccountSortOption(key: key, ascending: defaultAsc)
        }
    }

    public static func effectiveUsagePeak(
        for account: Account,
        resolvedUsage: UsageSummary? = nil
    ) -> Double {
        let windows = (resolvedUsage ?? account.usage)?.windows ?? account.usage_windows
        return windows.compactMap(\.used_percent).max() ?? -1
    }

    public func sort(
        _ accounts: [Account],
        effectiveUsage: ((Account) -> UsageSummary?)? = nil
    ) -> [Account] {
        switch option.key {
        case .recent:
            return AccountFilterSort.sortRecentCall(accounts, ascending: option.ascending)
        case .priority:
            return AccountFilterSort.sortPriority(accounts, ascending: option.ascending)
        case .name:
            return accounts.sorted { a, b in
                let cmp = a.name.localizedStandardCompare(b.name)
                if cmp != .orderedSame {
                    return option.ascending ? (cmp == .orderedAscending) : (cmp == .orderedDescending)
                }
                return a.id < b.id
            }
        case .usage:
            guard accounts.count > 1 else { return accounts }
            let keyed: [(account: Account, peak: Double)] = accounts.map { account in
                let peak = Self.effectiveUsagePeak(
                    for: account,
                    resolvedUsage: effectiveUsage?(account)
                )
                return (account, peak)
            }
            return keyed.sorted { lhs, rhs in
                let pa = lhs.peak
                let pb = rhs.peak
                if pa != pb {
                    if pa < 0 { return false }
                    if pb < 0 { return true }
                    return option.ascending ? pa < pb : pa > pb
                }
                return lhs.account.id < rhs.account.id
            }.map(\.account)
        }
    }

    private func persist() {
        if let encoded = try? JSONEncoder().encode(option) {
            UserDefaults.standard.set(encoded, forKey: Self.storageKey)
        }
    }
}

public enum PlatformMeta {
    public static let preferredOrder: [String] = [
        "openai", "grok", "anthropic", "gemini", "antigravity",
        "deepseek", "kimi", "zhipu", "minimax", "opencode_go",
        "typesafe", "command_code", "cline",
    ]

    public static func displayName(_ platform: String) -> String {
        switch platform {
        case "openai": return "OpenAI"
        case "grok": return "Grok"
        case "anthropic": return "Claude"
        case "gemini": return "Gemini"
        case "antigravity": return "Antigravity"
        case "deepseek": return "DeepSeek"
        case "kimi": return "Kimi"
        case "zhipu": return "智谱"
        case "minimax": return "MiniMax"
        case "opencode_go": return "OpenCode"
        case "typesafe": return "TypeSafe / Jev"
        case "command_code": return "Command Code"
        case "cline": return "Cline"
        default: return platform
        }
    }

    public static func priorityName(_ platform: String) -> String {
        switch platform {
        case "openai": return "Codex"
        case "opencode_go": return "OpenCode Go"
        default: return displayName(platform)
        }
    }

    public static func typeLabel(_ type: String) -> String {
        switch type {
        case "oauth": return "OAuth"
        case "apikey": return "Key"
        case "setup-token": return "Setup Token"
        case "bedrock": return "Bedrock"
        case "service_account": return "Service Account"
        default: return type
        }
    }

    public static func sortPlatforms(_ platforms: [String]) -> [String] {
        let unique = Array(Set(platforms))
        return unique.sorted { a, b in
            let ia = preferredOrder.firstIndex(of: a) ?? preferredOrder.count
            let ib = preferredOrder.firstIndex(of: b) ?? preferredOrder.count
            if ia != ib { return ia < ib }
            return a < b
        }
    }
}
