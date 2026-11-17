import SwiftUI

public struct QuickAccountRowView: View {
    public let account: Account
    public let callModel: String
    public let calledAt: String?
    public let online: Bool
    public let scheduleBusy: Bool
    public let onToggleSchedulable: () -> Void
    public let onOpenError: (Int) -> Void

    public init(
        account: Account,
        callModel: String,
        calledAt: String?,
        online: Bool,
        scheduleBusy: Bool,
        onToggleSchedulable: @escaping () -> Void,
        onOpenError: @escaping (Int) -> Void
    ) {
        self.account = account
        self.callModel = callModel
        self.calledAt = calledAt
        self.online = online
        self.scheduleBusy = scheduleBusy
        self.onToggleSchedulable = onToggleSchedulable
        self.onOpenError = onOpenError
    }

    public var body: some View {
        let windows = account.usage?.windows ?? account.usage_windows
        VStack(alignment: .leading, spacing: 5) {
            HStack(spacing: 6) {
                StatusDot(account.available ? .good : (account.schedulable ? .warning : .muted), size: 6)
                Text(account.name)
                    .font(DS.fontCompactMedium)
                    .foregroundStyle(DS.ink)
                    .lineLimit(1)
                Text("#\(account.id)")
                    .font(DS.fontMono11)
                    .foregroundStyle(DS.muted)
                DegradationBadgeView(account: account, compact: true)
                Spacer(minLength: 4)
                LinearSwitch(
                    isOn: account.schedulable,
                    disabled: !online || scheduleBusy,
                    label: "\(account.name) 调度开关",
                    onToggle: onToggleSchedulable
                )
            }

            HStack(spacing: 8) {
                if !windows.isEmpty {
                    MiniUsageView(windows: windows)
                }
                if !account.available, let blocker = account.blockers.first {
                    Text(blocker.label)
                        .font(DS.fontResetTime)
                        .foregroundStyle(DS.warn)
                        .lineLimit(1)
                }
                if let errId = account.last_error_id, let status = account.last_error_status {
                    Button {
                        onOpenError(errId)
                    } label: {
                        Text("\(status)")
                            .font(DS.fontResetTimeSemibold)
                            .monospacedDigit()
                            .foregroundStyle(DS.bad)
                            .padding(.horizontal, 4)
                            .padding(.vertical, 1)
                            .background(DS.badBg)
                            .clipShape(RoundedRectangle(cornerRadius: 3))
                    }
                    .buttonStyle(.plain)
                }
                Spacer(minLength: 2)
                if let calledAt, !calledAt.isEmpty {
                    Text(BeijingTime.fullTime(calledAt))
                        .font(DS.fontResetTime)
                        .monospacedDigit()
                        .foregroundStyle(DS.muted)
                }
            }
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 7)
        .background(DS.surface)
        .overlay(RoundedRectangle(cornerRadius: 7).strokeBorder(DS.line, lineWidth: 1))
        .clipShape(RoundedRectangle(cornerRadius: 7))
    }
}
