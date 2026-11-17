import SwiftUI

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
        VStack(alignment: .leading, spacing: 6) {
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

            ForEach(windows, id: \.key) { w in
                windowBlock(w)
            }

            if windows.isEmpty && summary?.key_quota == nil && (summary?.balances ?? []).isEmpty {
                if let today = summary?.today {
                    statsChipLine(today)
                } else {
                    Text(nativeUsage.loadingIds.contains(account.id) ? "正在读取额度…" : "暂无窗口数据")
                        .font(DS.fontResetTime)
                        .foregroundStyle(DS.muted)
                }
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

            if let exp = summary?.reset_credits?.expires_at.first, !exp.isEmpty {
                Text("到期 \(BeijingTime.fullTime(exp))")
                    .font(DS.fontUsageStat)
                    .monospacedDigit()
                    .foregroundStyle(DS.muted)
                    .padding(.horizontal, 5)
                    .padding(.vertical, 1)
                    .background(DS.surfaceSubtle)
                    .clipShape(RoundedRectangle(cornerRadius: 3))
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
        VStack(alignment: .leading, spacing: 3) {
            if let st = w.stats {
                statsChipLine(st)
            }
            if let est = w.estimated_total_cost {
                Text("预计总费用 \(UsageFormatting.money(est))")
                    .font(DS.fontUsageStat)
                    .monospacedDigit()
                    .foregroundStyle(DS.muted)
                    .padding(.horizontal, 5)
                    .padding(.vertical, 1)
                    .background(DS.surfaceSubtle)
                    .clipShape(RoundedRectangle(cornerRadius: 3))
            }

            HStack(spacing: 6) {
                Text(w.label)
                    .font(DS.fontResetTimeSemibold)
                    .lineLimit(1)
                    .fixedSize(horizontal: true, vertical: false)
                    .foregroundStyle(w.color == "emerald" ? DS.windowEmerald : DS.windowIndigo)
                    .padding(.horizontal, 6)
                    .padding(.vertical, 1)
                    .background(w.color == "emerald" ? DS.windowEmeraldBg : DS.windowIndigoBg)
                    .clipShape(RoundedRectangle(cornerRadius: 4))

                GeometryReader { geo in
                    ZStack(alignment: .leading) {
                        Capsule().fill(DS.line)
                        if let pct = w.used_percent {
                            Capsule()
                                .fill(barColor(pct: pct, window: w))
                                .frame(width: max(0, min(geo.size.width, geo.size.width * CGFloat(pct / 100.0))))
                        }
                    }
                }
                .frame(width: 42, height: 5)

                if let pct = w.used_percent {
                    Text("\(Int(pct.rounded()))%")
                        .font(DS.fontResetTimeSemibold)
                        .monospacedDigit()
                        .foregroundStyle(pct >= 90 ? DS.bad : DS.ink)
                        .frame(minWidth: 28, alignment: .trailing)
                } else {
                    Text("—")
                        .font(DS.fontResetTime)
                        .foregroundStyle(DS.muted)
                }

                if let reset = w.reset_at, !reset.isEmpty {
                    Text(BeijingTime.fullTime(reset))
                        .font(DS.fontResetTime)
                        .monospacedDigit()
                        .foregroundStyle(DS.muted)
                        .lineLimit(1)
                        .fixedSize(horizontal: true, vertical: false)
                }

                if w.status == "stale" {
                    Text("历史")
                        .font(DS.fontResetTime)
                        .foregroundStyle(DS.warn)
                } else if w.status == "error" {
                    Text("异常")
                        .font(DS.fontResetTime)
                        .foregroundStyle(DS.warn)
                } else if w.estimated == true {
                    Text("估算")
                        .font(DS.fontResetTime)
                        .foregroundStyle(DS.muted)
                }
            }
        }
    }

    private func statsChipLine(_ st: UsageStats) -> some View {
        HStack(spacing: 4) {
            statChip("\(st.requests) req")
            statChip(UsageFormatting.compactTokens(st.tokens))
            statChip("A \(UsageFormatting.money(st.cost))")
            statChip("U \(UsageFormatting.money(st.user_cost))")
        }
    }

    private func statChip(_ text: String) -> some View {
        Text(text)
            .font(DS.fontUsageStat)
            .monospacedDigit()
            .foregroundStyle(DS.muted)
            .lineLimit(1)
            .fixedSize(horizontal: true, vertical: false)
            .padding(.horizontal, 4)
            .padding(.vertical, 1)
            .background(DS.surfaceSubtle)
            .clipShape(RoundedRectangle(cornerRadius: 3))
    }

    private var actionBar: some View {
        HStack(spacing: 8) {
            if actions.contains("query_usage") {
                inlineActionButton(title: "查询", icon: "arrow.clockwise", color: DS.accent, key: "usage") {
                    if summary?.native_read == true {
                        nativeUsage.refresh(accountId: account.id)
                    } else {
                        await runUsageOperation(action: "usage")
                    }
                }
            }
            if actions.contains("query_reset_credits") {
                let countText = summary?.reset_credits?.available.map { "次数 \($0)" } ?? "次数"
                inlineActionButton(title: countText, icon: "arrow.clockwise", color: DS.accent, key: "credits") {
                    await runUsageOperation(action: "usage", payload: ["include_reset_credits": .bool(true)])
                }
            }
            if actions.contains("reset_quota") {
                if confirmReset {
                    HStack(spacing: 4) {
                        Button("确认用卡") {
                            confirmReset = false
                            Task { await runUsageOperation(action: "reset_quota", payload: ["confirmed": .bool(true)]) }
                        }
                        .font(DS.fontResetTimeSemibold)
                        .foregroundStyle(DS.warn)
                        .buttonStyle(.plain)

                        Button("取消") {
                            confirmReset = false
                        }
                        .font(DS.fontResetTime)
                        .foregroundStyle(DS.muted)
                        .buttonStyle(.plain)
                    }
                } else {
                    inlineActionButton(title: "重置", icon: "arrow.counterclockwise", color: DS.costOrange, key: "reset_quota") {
                        confirmReset = true
                    }
                }
            }
            if actions.contains("probe_quota") {
                inlineActionButton(title: "探测", icon: "arrow.clockwise", color: DS.windowEmerald, key: "probe") {
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
    }

    private func inlineActionButton(
        title: String,
        icon: String,
        color: Color,
        key: String,
        action: @escaping @MainActor () async -> Void
    ) -> some View {
        Button {
            Task { await action() }
        } label: {
            HStack(spacing: 3) {
                Image(systemName: icon)
                    .font(DS.iconFont(9, weight: .semibold))
                Text(title)
                    .font(DS.fontResetTimeMediumOrRegular)
                    .monospacedDigit()
            }
            .foregroundStyle(color)
        }
        .buttonStyle(.plain)
        .disabled(!store.state.online || !actionBusy.isEmpty)
    }

    private func autoResetCreditBlock(_ state: AutoResetCreditState) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            Button {
                creditExpanded.toggle()
            } label: {
                HStack(spacing: 4) {
                    StatusDot(state.error.isEmpty ? .good : .warning, size: 5)
                    Text(state.label)
                        .font(DS.fontResetTimeSemibold)
                        .foregroundStyle(DS.ink)
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

    private func barColor(pct: Double, window: UsageWindow) -> Color {
        if pct >= 90 { return DS.bad }
        if pct >= 75 { return DS.warn }
        return window.color == "emerald" ? DS.windowEmerald : DS.windowIndigo
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

private extension DS {
    static var fontResetTimeMediumOrRegular: Font { fontResetTime }
}
