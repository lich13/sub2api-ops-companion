import SwiftUI

/// Matches `accountRestriction` in `desktop/src/QuickAccountStatus.tsx`.
public enum QuickAccountStatusHelper {
    public static func restriction(for account: Account?) -> String {
        guard let account else { return "" }
        if account.available && account.blockers.isEmpty && account.recoverable != true {
            return ""
        }
        var reasons = account.blockers.map(\.label).filter { !$0.isEmpty }
        if reasons.isEmpty {
            reasons.append(account.recoverable == true ? "存在可恢复的状态限制" : "暂不可调度")
        }
        return reasons.joined(separator: " · ")
    }
}

/// Matches `.compact .recent-account` (`minmax(0, 1fr) 148px 32px`, `height: 48px`) in `desktop/src/main.tsx` & `style.css`.
public struct QuickAccountRowView: View {
    public let account: Account?
    public let displayName: String
    public let calledAt: String?
    public let online: Bool
    public let scheduleBusy: Bool
    public let recoverBusy: Bool
    public let onToggleSchedulable: () -> Void
    public let onRecover: () -> Void
    public let onOpenError: (Int) -> Void

    @State private var errorHovered: Bool = false

    public init(
        account: Account?,
        displayName: String? = nil,
        callModel: String = "",
        calledAt: String?,
        online: Bool,
        scheduleBusy: Bool,
        recoverBusy: Bool = false,
        onToggleSchedulable: @escaping () -> Void,
        onRecover: @escaping () -> Void = {},
        onOpenError: @escaping (Int) -> Void
    ) {
        self.account = account
        self.displayName = displayName ?? account?.name ?? ""
        _ = callModel
        self.calledAt = calledAt
        self.online = online
        self.scheduleBusy = scheduleBusy
        self.recoverBusy = recoverBusy
        self.onToggleSchedulable = onToggleSchedulable
        self.onRecover = onRecover
        self.onOpenError = onOpenError
    }

    public var body: some View {
        let restriction = QuickAccountStatusHelper.restriction(for: account)
        let isRestricted = !restriction.isEmpty
        let windows = account?.usage?.windows ?? account?.usage_windows ?? []
        let canRecover = account?.recoverable == true

        HStack(alignment: .center, spacing: 8) {
            // Column 1: Name + Mini Usage (flex 1)
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 4) {
                    Text(displayName)
                        .font(DS.fontCompactSemibold)
                        .foregroundStyle(isRestricted ? DS.warn : DS.ink)
                        .lineLimit(1)
                    if isRestricted {
                        Image(systemName: "exclamationmark.circle")
                            .font(DS.iconFont(10, weight: .medium))
                            .foregroundStyle(DS.warn)
                            .accessibilityLabel("调度受限：\(restriction)")
                    }
                    if let account {
                        DegradationBadgeView(account: account, compact: true)
                    }
                }
                .help([displayName, restriction].filter { !$0.isEmpty }.joined(separator: " · "))

                if let account {
                    if account.type == "oauth" {
                        if !windows.isEmpty {
                            MiniUsageView(windows: windows)
                        } else if isRestricted {
                            Text(restriction)
                                .font(DS.fontUsageStat)
                                .foregroundStyle(DS.muted)
                                .lineLimit(1)
                        }
                    } else if let summary = compactApiKeySummary(account) {
                        Text(summary)
                            .font(DS.fontResetTime)
                            .monospacedDigit()
                            .foregroundStyle(DS.muted)
                            .lineLimit(1)
                    } else if isRestricted {
                        Text(restriction)
                            .font(DS.fontUsageStat)
                            .foregroundStyle(DS.muted)
                            .lineLimit(1)
                    }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)

            // Column 2: Two-line Call / Error Timestamps (148pt fixed)
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 5) {
                    Text("最近调用")
                        .foregroundStyle(DS.muted)
                    Spacer(minLength: 4)
                    Text(BeijingTime.fullTime(calledAt))
                        .monospacedDigit()
                        .foregroundStyle(DS.muted)
                }
                .font(DS.fontResetTime)

                if let errId = account?.last_error_id {
                    Button {
                        onOpenError(errId)
                    } label: {
                        HStack(spacing: 5) {
                            Text("上次错误")
                                .foregroundStyle(DS.warn)
                            Spacer(minLength: 4)
                            Text(BeijingTime.fullTime(account?.last_error_at))
                                .monospacedDigit()
                                .foregroundStyle(errorHovered ? DS.accent : DS.muted)
                        }
                        .font(DS.fontResetTime)
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .onHover { errorHovered = $0 }
                } else {
                    HStack(spacing: 5) {
                        Text("上次错误")
                            .foregroundStyle(DS.muted)
                        Spacer(minLength: 4)
                        Text(BeijingTime.fullTime(account?.last_error_at))
                            .monospacedDigit()
                            .foregroundStyle(DS.muted)
                    }
                    .font(DS.fontResetTime)
                }
            }
            .frame(width: 148)

            // Column 3: Optional Recover Button + Schedule Switch (32pt fixed)
            if let account {
                VStack(spacing: 2) {
                    if canRecover {
                        Button(action: onRecover) {
                            Image(systemName: "arrow.counterclockwise")
                                .font(DS.iconFont(10, weight: .semibold))
                                .foregroundStyle(DS.warn)
                                .frame(width: 24, height: 18)
                                .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .disabled(!online || recoverBusy)
                        .opacity(!online || recoverBusy ? 0.45 : 1.0)
                        .help("恢复状态：\(account.name)")
                        .accessibilityLabel("恢复状态：\(account.name)")
                    }
                    LinearSwitch(
                        isOn: account.schedulable,
                        disabled: !online || scheduleBusy,
                        compact: false,
                        label: "\(account.name) 调度开关",
                        onToggle: onToggleSchedulable
                    )
                }
                .frame(width: 32, alignment: .center)
            } else {
                Spacer().frame(width: 32)
            }
        }
        .padding(.vertical, 4)
        .frame(height: 48)
    }

    private func compactApiKeySummary(_ account: Account) -> String? {
        if let bal = account.usage?.balances?.first {
            return "剩余 \(String(format: "%.2f", bal.balance)) \(bal.currency)"
        }
        if let plan = account.usage?.key_quota?.plans.first {
            if plan.unlimited { return "\(plan.name) 不限额" }
            if let rem = plan.remaining {
                return "剩余 \(UsageFormatting.quotaAmount(rem, unit: plan.unit))"
            }
        }
        if let cost = account.usage?.today?.cost ?? account.usage?.key_quota?.today?.cost, cost > 0 {
            return "今日 \(UsageFormatting.money(cost))"
        }
        return nil
    }
}
