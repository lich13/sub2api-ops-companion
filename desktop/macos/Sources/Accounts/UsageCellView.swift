import SwiftUI

/// Matches `UsageCell.tsx` & `.usage-cell` in `style.css` (`32px` badge, `32×6px` track, `32px` right-aligned percent, `9px` stat chips).
public struct UsageCellView: View {
    public let account: Account
    @Bindable public var store: AppStore
    public var nativeUsage: NativeUsageReader
    public let onOpenKeyQuotaSettings: (Account) -> Void

    @State private var confirmReset: Bool = false
    @State private var actionBusy: String = ""
    @State private var creditExpanded: Bool = false

    public init(
        account: Account,
        store: AppStore,
        nativeUsage: NativeUsageReader,
        onOpenKeyQuotaSettings: @escaping (Account) -> Void
    ) {
        self.account = account
        self.store = store
        self.nativeUsage = nativeUsage
        self.onOpenKeyQuotaSettings = onOpenKeyQuotaSettings
    }

    private var summary: UsageSummary? {
        nativeUsage.resolvedUsage(for: account)
    }

    private var windows: [UsageWindow] {
        summary?.windows ?? account.usage_windows
    }

    private var actions: [String] {
        summary?.actions ?? []
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            if let balances = summary?.balances, !balances.isEmpty {
                HStack(spacing: 6) {
                    ForEach(balances, id: \.currency) { bal in
                        Text("\(bal.currency) \(String(format: "%.2f", bal.balance))")
                            .font(DS.fontResetTimeSemibold)
                            .monospacedDigit()
                            .foregroundStyle(DS.windowEmerald)
                            .padding(.horizontal, 6)
                            .padding(.vertical, 2)
                            .background(DS.windowEmeraldBg)
                            .clipShape(RoundedRectangle(cornerRadius: 4))
                    }
                }
            }

            if let keyQuota = summary?.key_quota {
                KeyQuotaView(quota: keyQuota) {
                    onOpenKeyQuotaSettings(account)
                }
            }

            if summary?.stats_status == "error" {
                Text("统计暂不可用，请重试")
                    .font(DS.fontResetTime)
                    .foregroundStyle(DS.muted)
            } else if summary?.branch == "apikey" || summary?.today != nil {
                if let today = summary?.today {
                    statsChipLine(today)
                } else {
                    Text("今日统计未知")
                        .font(DS.fontResetTime)
                        .foregroundStyle(DS.muted)
                }
            }

            ForEach(windows, id: \.key) { w in
                windowBlock(w)
            }

            if let msg = summary?.message, !msg.isEmpty {
                Text(msg)
                    .font(DS.fontResetTime)
                    .foregroundStyle(DS.muted)
                    .lineLimit(2)
            }

            if windows.isEmpty && summary?.key_quota == nil && (summary?.balances ?? []).isEmpty
                && summary?.today == nil && summary?.branch != "apikey" && (summary?.message ?? "").isEmpty
            {
                Text(nativeUsage.loadingIds.contains(account.id) ? "正在读取额度…" : "未知")
                    .font(DS.fontResetTime)
                    .foregroundStyle(DS.muted)
            }

            if let nativeErr = nativeUsage.errors[account.id], !nativeErr.isEmpty {
                Text(nativeErr)
                    .font(DS.fontResetTime)
                    .foregroundStyle(DS.bad)
                    .lineLimit(2)
            }

            if !actions.isEmpty {
                actionBar
            }

            if let expList = summary?.reset_credits?.expires_at, !expList.isEmpty {
                HStack(spacing: 4) {
                    ForEach(expList.prefix(2), id: \.self) { exp in
                        statChip("到期 \(BeijingTime.fullTime(exp))")
                    }
                }
            }

            if let monitor = account.recovery_monitor {
                HStack(spacing: 4) {
                    StatusDot(monitor.status == "failed" ? .bad : .warning, size: 5)
                    Text(monitor.label)
                        .font(DS.fontResetTime)
                        .foregroundStyle(DS.muted)
                    if let next = monitor.next_retry_at ?? monitor.next_at, !next.isEmpty {
                        Text(BeijingTime.fullTime(next))
                            .font(DS.fontResetTime)
                            .monospacedDigit()
                            .foregroundStyle(DS.muted)
                    }
                }
            }

            if let autoCredit = account.auto_reset_credit {
                autoResetCreditBlock(autoCredit)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func windowBlock(_ w: UsageWindow) -> some View {
        let isDimmed = w.status == "stale" || w.status == "error"
        return VStack(alignment: .leading, spacing: 2) {
            if let st = w.stats, st.requests > 0 || st.tokens > 0 {
                statsChipLine(st, estimated: w.estimated_total_cost)
            }

            HStack(spacing: 4) {
                Text(w.label)
                    .font(DS.fontResetTimeMedium)
                    .lineLimit(1)
                    .foregroundStyle(badgeForeground(w))
                    .frame(width: 32, alignment: .center)
                    .padding(.vertical, 2)
                    .background(badgeBackground(w))
                    .clipShape(RoundedRectangle(cornerRadius: 4))

                ZStack(alignment: .leading) {
                    Capsule()
                        .fill(DS.usageTrackBg)
                        .frame(width: 32, height: 6)
                    if let pct = w.used_percent {
                        let clamped = max(0, min(100, pct))
                        Capsule()
                            .fill(trackFillColor(pct: clamped))
                            .frame(width: 32 * CGFloat(clamped / 100.0), height: 6)
                    }
                }
                .opacity(isDimmed ? 0.45 : 1.0)

                if let pct = w.used_percent {
                    Text(pct > 999 ? ">999%" : "\(Int(pct.rounded()))%")
                        .font(DS.fontResetTimeMedium)
                        .monospacedDigit()
                        .foregroundStyle(pct >= 90 ? DS.usageFillRed : DS.usagePercentInk)
                        .frame(width: 32, alignment: .trailing)
                } else {
                    Text("—")
                        .font(DS.fontResetTime)
                        .foregroundStyle(DS.muted)
                        .frame(width: 32, alignment: .trailing)
                }

                if let reset = w.reset_at, !reset.isEmpty {
                    Text(BeijingTime.fullTime(reset))
                        .font(DS.fontResetTime)
                        .monospacedDigit()
                        .foregroundStyle(DS.usageTimeInk)
                        .lineLimit(1)
                }

                if w.status == "stale" {
                    Text("历史")
                        .font(DS.fontResetTime)
                        .foregroundStyle(DS.warn)
                } else if w.status == "error" {
                    Text("异常")
                        .font(DS.fontResetTime)
                        .foregroundStyle(DS.warn)
                } else if w.status == "unknown" {
                    Text("未知")
                        .font(DS.fontResetTime)
                        .foregroundStyle(DS.warn)
                } else if w.estimated == true {
                    Text("估算")
                        .font(DS.fontResetTime)
                        .foregroundStyle(DS.muted)
                }
            }
            .help("采集 \(BeijingTime.fullTime(w.observed_at)) · 重置 \(BeijingTime.fullTime(w.reset_at))")
        }
    }

    private func badgeForeground(_ w: UsageWindow) -> Color {
        switch w.color {
        case "emerald": return DS.windowEmerald
        case "purple": return DS.windowPurple
        default: return DS.windowIndigo
        }
    }

    private func badgeBackground(_ w: UsageWindow) -> Color {
        switch w.color {
        case "emerald": return DS.windowEmeraldBg
        case "purple": return DS.windowPurpleBg
        default: return DS.windowIndigoBg
        }
    }

    private func trackFillColor(pct: Double) -> Color {
        if pct >= 90 { return DS.usageFillRed }
        if pct >= 75 { return DS.usageFillAmber }
        return DS.usageFillGreen
    }

    private func statsChipLine(_ st: UsageStats, estimated: Double? = nil) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 4) {
                statChip("\(UsageFormatting.compactTokens(st.requests)) req")
                statChip(UsageFormatting.compactTokens(st.tokens))
                statChip("A \(UsageFormatting.money(st.cost))")
                statChip("U \(UsageFormatting.money(st.user_cost))")
            }
            if let est = estimated {
                statChip("预计总费用 \(UsageFormatting.money(est))")
            }
        }
    }

    private func statChip(_ text: String) -> some View {
        Text(text)
            .font(DS.fontUsageStat)
            .monospacedDigit()
            .foregroundStyle(DS.usageStatInk)
            .lineLimit(1)
            .padding(.horizontal, 5)
            .padding(.vertical, 2)
            .background(DS.usageStatBg)
            .clipShape(RoundedRectangle(cornerRadius: 4))
    }

    private var actionBar: some View {
        HStack(spacing: 12) {
            if actions.contains("query_usage") {
                inlineActionButton(title: "查询", icon: "arrow.clockwise", color: DS.usageActionBlue, key: "usage") {
                    if summary?.native_read == true {
                        nativeUsage.refresh(accountId: account.id)
                    } else {
                        await runUsageOperation(action: "usage")
                    }
                }
            }
            if actions.contains("query_reset_credits") {
                let countText = summary?.reset_credits?.available.map { "次数 \($0)" } ?? "次数"
                inlineActionButton(title: countText, icon: "arrow.clockwise", color: DS.usageActionBlue, key: "credits") {
                    await runUsageOperation(action: "usage", payload: ["include_reset_credits": .bool(true)])
                }
            }
            if actions.contains("reset_quota") {
                if confirmReset {
                    HStack(spacing: 6) {
                        Button("确认用卡") {
                            confirmReset = false
                            Task { await runUsageOperation(action: "reset_quota", payload: ["action": .string("reset_quota"), "confirmed": .bool(true)]) }
                        }
                        .font(DS.fontResetTimeSemibold)
                        .foregroundStyle(DS.usageActionOrange)
                        .buttonStyle(.plain)

                        Button("取消") {
                            confirmReset = false
                        }
                        .font(DS.fontResetTime)
                        .foregroundStyle(DS.muted)
                        .buttonStyle(.plain)
                    }
                } else {
                    inlineActionButton(title: "重置", icon: "arrow.counterclockwise", color: DS.usageActionOrange, key: "reset_quota") {
                        confirmReset = true
                    }
                }
            }
            if actions.contains("probe_quota") {
                inlineActionButton(title: "探测", icon: "arrow.clockwise", color: DS.usageActionCyan, key: "probe") {
                    await runUsageOperation(action: "usage", payload: ["probe": .bool(true)])
                }
            }
            if account.type == "apikey" && summary?.key_quota != nil {
                Button {
                    onOpenKeyQuotaSettings(account)
                } label: {
                    Image(systemName: "key")
                        .font(DS.iconFont(10))
                        .foregroundStyle(DS.muted)
                }
                .buttonStyle(.plain)
                .help("配置余额查询访问令牌")
            }
        }
        .padding(.vertical, 3)
    }

    private func inlineActionButton(
        title: String,
        icon: String,
        color: Color,
        key: String,
        action: @escaping @MainActor () async -> Void
    ) -> some View {
        let isBusy = actionBusy == key
        return Button {
            Task { await action() }
        } label: {
            HStack(spacing: 3) {
                Image(systemName: icon)
                    .font(DS.iconFont(9, weight: .semibold))
                Text(title)
                    .font(DS.fontResetTime)
                    .monospacedDigit()
            }
            .foregroundStyle(color)
        }
        .buttonStyle(.plain)
        .disabled(!store.state.online || !actionBusy.isEmpty)
        .opacity(!store.state.online || (!actionBusy.isEmpty && !isBusy) ? 0.45 : 1.0)
    }

    private func autoResetCreditBlock(_ state: AutoResetCreditState) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            Button {
                creditExpanded.toggle()
            } label: {
                HStack(spacing: 4) {
                    StatusDot(state.error.isEmpty ? .good : .warning, size: 5)
                    Text(state.label)
                        .font(DS.fontResetTimeMedium)
                        .foregroundStyle(DS.muted)
                    Image(systemName: creditExpanded ? "chevron.up" : "chevron.down")
                        .font(DS.iconFont(8, weight: .semibold))
                        .foregroundStyle(DS.muted)
                }
            }
            .buttonStyle(.plain)

            if creditExpanded {
                VStack(alignment: .leading, spacing: 2) {
                    if let source = state.evidence_source, !source.isEmpty {
                        Text("依据：\(source == "upstream_error" ? "上游 7d 429 日志" : "账号限流状态")")
                            .font(DS.fontUsageStat)
                            .foregroundStyle(DS.muted)
                    }
                    if !state.error.isEmpty {
                        Text(state.error)
                            .font(DS.fontUsageStat)
                            .foregroundStyle(DS.bad)
                    }
                    if let next = state.next_at, !next.isEmpty {
                        Text("下次检查：\(BeijingTime.fullTime(next))")
                            .font(DS.fontUsageStat)
                            .monospacedDigit()
                            .foregroundStyle(DS.muted)
                    }
                }
                .padding(.leading, 9)
            }
        }
    }

    private func runUsageOperation(action: String, payload: [String: JSONValue] = [:]) async {
        guard store.state.online, actionBusy.isEmpty else { return }
        actionBusy = action
        defer { actionBusy = "" }
        do {
            _ = try await store.operations.accountOperation(account: account, action: action, payload: payload)
        } catch {
            store.report(error)
        }
    }
}
