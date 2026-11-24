import SwiftUI

public struct KeyQuotaView: View {
    public let quota: KeyQuota
    public let onOpenSettings: () -> Void

    public init(quota: KeyQuota, onOpenSettings: @escaping () -> Void) {
        self.quota = quota
        self.onOpenSettings = onOpenSettings
    }

    static func todayRequestsLabel(_ requests: Double?) -> String {
        "今日 \(UsageFormatting.compactTokens(requests ?? 0)) req"
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            ForEach(Array(quota.plans.enumerated()), id: \.offset) { _, plan in
                planRow(plan)
            }

            ForEach(Array(quota.subscriptions.enumerated()), id: \.offset) { _, sub in
                VStack(alignment: .leading, spacing: 4) {
                    HStack(spacing: 6) {
                        Text(sub.name ?? "套餐")
                            .font(DS.fontUsageStatMedium)
                            .foregroundStyle(DS.ink)
                        if let exp = sub.expires_at, !exp.isEmpty {
                            Text("到期 \(BeijingTime.fullTime(exp))")
                                .font(DS.fontUsageStat)
                                .monospacedDigit()
                                .foregroundStyle(DS.muted)
                        }
                    }
                    ForEach(Array(sub.plans.enumerated()), id: \.offset) { _, plan in
                        planRow(plan)
                    }
                }
            }

            if let today = quota.today,
               (today.requests ?? 0) > 0 || (today.tokens ?? 0) > 0 || (today.cost ?? 0) > 0
            {
                HStack(spacing: 6) {
                    Text(verbatim: Self.todayRequestsLabel(today.requests))
                    if let tok = today.tokens {
                        Text(UsageFormatting.compactTokens(tok))
                    }
                    if let c = today.cost {
                        Text(UsageFormatting.money(c))
                    }
                }
                .font(DS.fontUsageStat)
                .monospacedDigit()
                .foregroundStyle(DS.muted)
            }

            if let err = quota.error, !err.isEmpty {
                HStack(spacing: 6) {
                    Text(err)
                        .font(DS.fontResetTime)
                        .foregroundStyle(DS.bad)
                        .lineLimit(2)
                    if err.contains("令牌") || err.contains("401") || err.contains("403") {
                        Button("配置令牌", action: onOpenSettings)
                            .buttonStyle(LinearButtonStyle(variant: .ghost, compact: true))
                    }
                }
            } else if quota.stale {
                Text("余额快照待刷新")
                    .font(DS.fontUsageStat)
                    .foregroundStyle(DS.warn)
            }
        }
    }

    private func planRow(_ plan: QuotaPlan) -> some View {
        HStack(spacing: 6) {
            Text(plan.name)
                .font(DS.fontResetTimeSemibold)
                .foregroundStyle(DS.windowIndigo)
                .padding(.horizontal, 5)
                .padding(.vertical, 1)
                .background(DS.windowIndigoBg)
                .clipShape(RoundedRectangle(cornerRadius: 4))

            if plan.unlimited {
                Text("不限额")
                    .font(DS.fontResetTime)
                    .foregroundStyle(DS.good)
            } else if let total = plan.total, total > 0, let used = plan.used {
                let pct = min(100, max(0, (used / total) * 100.0))
                GeometryReader { geo in
                    ZStack(alignment: .leading) {
                        Capsule().fill(DS.line)
                        Capsule()
                            .fill(pct >= 90 ? DS.bad : (pct >= 75 ? DS.warn : DS.windowIndigo))
                            .frame(width: geo.size.width * CGFloat(pct / 100.0))
                    }
                }
                .frame(width: 34, height: 5)

                Text("\(UsageFormatting.quotaAmount(plan.remaining, unit: plan.unit)) 剩余")
                    .font(DS.fontResetTime)
                    .monospacedDigit()
                    .foregroundStyle(DS.ink)
            } else {
                Text(UsageFormatting.quotaAmount(plan.remaining, unit: plan.unit))
                    .font(DS.fontResetTimeSemibold)
                    .monospacedDigit()
                    .foregroundStyle(DS.ink)
            }

            if let reset = plan.reset_at, !reset.isEmpty {
                Text(BeijingTime.fullTime(reset))
                    .font(DS.fontResetTime)
                    .monospacedDigit()
                    .foregroundStyle(DS.muted)
            }
        }
    }
}
