import SwiftUI

public struct AccountStatusView: View {
    public let account: Account
    public let onOpenError: (Int) -> Void

    public init(account: Account, onOpenError: @escaping (Int) -> Void) {
        self.account = account
        self.onOpenError = onOpenError
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            if account.available {
                HStack(spacing: 6) {
                    StatusDot(.good, size: 6)
                    Text("可调度")
                        .font(DS.fontCompactMedium)
                        .foregroundStyle(DS.good)
                }
            } else if let rateBlocker = account.blockers.first(where: { $0.code == "rate_limit_reset_at" }) {
                HStack(spacing: 5) {
                    Text(rateBlocker.label ?? "限流中")
                        .font(DS.fontBadge)
                        .foregroundStyle(DS.warn)
                        .padding(.horizontal, 6)
                        .padding(.vertical, 2)
                        .background(DS.warnBg)
                        .clipShape(RoundedRectangle(cornerRadius: 4))
                    Image(systemName: "questionmark.circle")
                        .font(DS.iconFont(11))
                        .foregroundStyle(DS.muted)
                        .help("上游返回限流，到达重置时间后自动恢复参与调度")
                }

                if let status = account.last_error_status, status == 429 || account.last_error_id != nil {
                    Button {
                        if let errId = account.last_error_id {
                            onOpenError(errId)
                        }
                    } label: {
                        HStack(spacing: 3) {
                            Image(systemName: "exclamationmark.triangle")
                                .font(DS.iconFont(10, weight: .semibold))
                            Text("\(status)")
                                .font(DS.fontBadge)
                                .monospacedDigit()
                        }
                        .foregroundStyle(DS.warn)
                        .padding(.horizontal, 6)
                        .padding(.vertical, 2)
                        .background(DS.warnBg)
                        .clipShape(RoundedRectangle(cornerRadius: 4))
                    }
                    .buttonStyle(.linearPlain)
                    .disabled(account.last_error_id == nil)
                    .help(account.last_error_id != nil ? "查看最近报错原文" : "暂无关联报错记录")
                }

                if let countdown = UsageFormatting.countdownString(until: rateBlocker.until) {
                    Text(countdown)
                        .font(DS.fontResetTime)
                        .monospacedDigit()
                        .foregroundStyle(DS.muted)
                }
            } else if let primaryBlocker = account.blockers.first {
                HStack(spacing: 6) {
                    StatusDot(primaryBlocker.code == "disabled" ? .muted : .warning, size: 6)
                    Text(primaryBlocker.label ?? "不可调度")
                        .font(DS.fontCompactMedium)
                        .foregroundStyle(primaryBlocker.code == "disabled" ? DS.muted : DS.ink)
                }
                if let until = primaryBlocker.until, let countdown = UsageFormatting.countdownString(until: until) {
                    Text(countdown)
                        .font(DS.fontResetTime)
                        .monospacedDigit()
                        .foregroundStyle(DS.muted)
                }
            } else {
                HStack(spacing: 6) {
                    StatusDot(account.status == "error" ? .bad : .muted, size: 6)
                    Text(account.status == "error" ? "状态异常" : "不可调度")
                        .font(DS.fontCompactMedium)
                        .foregroundStyle(account.status == "error" ? DS.bad : DS.muted)
                }
            }
        }
    }
}
