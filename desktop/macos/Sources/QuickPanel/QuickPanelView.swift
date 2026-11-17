import SwiftUI

/// Stage M2 full QuickPanel surface matching `main.tsx` Quick mode & `QuickAccountStatus.tsx`.
public struct QuickPanelRootView: View {
    @Bindable var store: AppStore
    let onContentHeightChange: @MainActor (CGFloat) -> Void

    public init(store: AppStore, onContentHeightChange: @escaping @MainActor (CGFloat) -> Void = { _ in }) {
        self.store = store
        self.onContentHeightChange = onContentHeightChange
    }

    private var activeGroups: [Group] {
        computeCurrentGroups(store.groups, accounts: store.accounts)
    }

    private var accountById: [Int: Account] {
        Dictionary(uniqueKeysWithValues: store.accounts.map { ($0.id, $0) })
    }

    public var body: some View {
        ZStack {
            VStack(spacing: 0) {
                headerBar
                Divider().overlay(DS.line)

                if store.state.connected && !store.state.online {
                    HStack(spacing: 6) {
                        Image(systemName: "wifi.slash")
                            .font(DS.iconFont(11, weight: .medium))
                        Text(store.state.error.isEmpty ? "连接中断，显示上次状态" : store.state.error)
                            .font(DS.fontResetTime)
                            .lineLimit(1)
                        Spacer()
                    }
                    .foregroundStyle(DS.warn)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 5)
                    .background(DS.offlineBg)
                    Divider().overlay(DS.line)
                }

                ScrollView {
                    VStack(alignment: .leading, spacing: 8) {
                        if !store.state.connected {
                            Text("尚未连接运维服务，请打开主窗口配置连接")
                                .font(DS.fontCompact)
                                .foregroundStyle(DS.muted)
                                .frame(maxWidth: .infinity, alignment: .center)
                                .padding(.vertical, 22)
                        } else if store.quickTab == .groups {
                            groupsBody
                        } else {
                            errorsBody
                        }
                    }
                    .padding(10)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(
                        GeometryReader { geo in
                            DS.clear.preference(key: QuickPanelContentHeightKey.self, value: geo.size.height)
                        }
                    )
                }
                .frame(maxHeight: DS.quickPanelMaxHeight - 66)

                Divider().overlay(DS.line)
                footerBar
            }
            .frame(width: DS.quickPanelWidth)
            .background(DS.surface)
            .overlay(
                RoundedRectangle(cornerRadius: DS.cardRadius)
                    .strokeBorder(DS.line, lineWidth: 1)
            )
            .clipShape(RoundedRectangle(cornerRadius: DS.cardRadius))

            if let detail = store.detailError {
                quickErrorOverlay(detail)
            }
        }
        .onPreferenceChange(QuickPanelContentHeightKey.self) { bodyHeight in
            let clamped = min(max(bodyHeight + 66, DS.quickPanelMinHeight), DS.quickPanelMaxHeight)
            onContentHeightChange(clamped)
        }
        .onExitCommand {
            if store.detailError != nil || store.detailBusy {
                store.closeDetail()
            } else {
                store.onHideQuickPanel?()
            }
        }
    }

    private var headerBar: some View {
        HStack(spacing: 8) {
            HStack(spacing: 4) {
                tabButton(title: "分组", count: activeGroups.isEmpty ? nil : activeGroups.count, selected: store.quickTab == .groups) {
                    store.quickTab = .groups
                }
                tabButton(
                    title: "异常",
                    count: store.errors.isEmpty ? nil : store.errors.count,
                    selected: store.quickTab == .errors
                ) {
                    store.quickTab = .errors
                }
            }
            Spacer()
            HStack(spacing: 5) {
                StatusDot(store.state.online ? .good : (store.state.connected ? .warning : .muted), size: 6)
                Text(store.state.online ? "已连接" : (store.state.connected ? "连接中断" : "未连接"))
                    .font(DS.fontPriorityMeta)
                    .foregroundStyle(DS.muted)
            }
            .padding(.horizontal, 8)
            .padding(.vertical, 3)
            .background(DS.surface)
            .overlay(Capsule().strokeBorder(DS.line, lineWidth: 1))
            .clipShape(Capsule())

            LinearIconButton(systemName: "arrow.clockwise", label: "立即刷新", size: 12) {
                store.refresh()
            }
            LinearIconButton(
                systemName: store.state.preferences.pinned ? "pin.fill" : "pin",
                label: store.state.preferences.pinned ? "取消固定" : "固定面板",
                size: 12,
                selected: store.state.preferences.pinned
            ) {
                Task { await store.updatePreferences(pinned: !store.state.preferences.pinned) }
            }
            LinearIconButton(systemName: "xmark", label: "关闭快捷面板", size: 12) {
                store.onHideQuickPanel?()
            }
        }
        .padding(.horizontal, 12)
        .frame(height: 36)
        .background(DS.topbar)
    }

    @ViewBuilder
    private var groupsBody: some View {
        if activeGroups.isEmpty {
            Text("暂无分组动态")
                .font(DS.fontCompact)
                .foregroundStyle(DS.muted)
                .frame(maxWidth: .infinity, alignment: .center)
                .padding(.vertical, 18)
        } else {
            ForEach(activeGroups) { group in
                VStack(alignment: .leading, spacing: 6) {
                    HStack(spacing: 6) {
                        Text(group.name)
                            .font(DS.fontCompactSemibold)
                            .foregroundStyle(DS.ink)
                        Text(PlatformMeta.displayName(group.platform))
                            .font(DS.fontUsageStatMedium)
                            .foregroundStyle(DS.muted)
                            .padding(.horizontal, 5)
                            .padding(.vertical, 1)
                            .background(DS.surface)
                            .overlay(RoundedRectangle(cornerRadius: 4).strokeBorder(DS.line, lineWidth: 1))
                            .clipShape(RoundedRectangle(cornerRadius: 4))
                        Spacer()
                        if !group.model.isEmpty {
                            Text(verbatim: group.model)
                                .font(DS.fontMono11)
                                .foregroundStyle(DS.muted)
                                .lineLimit(1)
                        }
                    }

                    if group.recent_accounts.isEmpty {
                        Text("近期无成员调用")
                            .font(DS.fontResetTime)
                            .foregroundStyle(DS.muted)
                            .padding(.vertical, 2)
                    } else {
                        ForEach(group.recent_accounts, id: \.log_id) { call in
                            if let acc = accountById[call.account_id] {
                                QuickAccountRowView(
                                    account: acc,
                                    callModel: call.model,
                                    calledAt: call.called_at,
                                    online: store.state.online,
                                    scheduleBusy: store.busyScheduleAccountId == acc.id,
                                    onToggleSchedulable: {
                                        Task { await store.toggleSchedulable(acc) }
                                    },
                                    onOpenError: { errId in
                                        store.openErrorDetail(id: errId)
                                    }
                                )
                            }
                        }
                    }
                }
                .padding(9)
                .background(DS.surfaceSubtle)
                .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(DS.line, lineWidth: 1))
                .clipShape(RoundedRectangle(cornerRadius: 8))
            }
        }
    }

    @ViewBuilder
    private var errorsBody: some View {
        if store.errors.isEmpty {
            Text("当前无未恢复异常")
                .font(DS.fontCompact)
                .foregroundStyle(DS.muted)
                .frame(maxWidth: .infinity, alignment: .center)
                .padding(.vertical, 18)
        } else {
            ForEach(store.errors.prefix(12)) { err in
                Button {
                    store.openErrorDetail(id: err.id)
                } label: {
                    VStack(alignment: .leading, spacing: 4) {
                        HStack(spacing: 6) {
                            Text("\(err.status_code)")
                                .font(DS.fontResetTimeSemibold)
                                .monospacedDigit()
                                .foregroundStyle(DS.bad)
                                .padding(.horizontal, 5)
                                .padding(.vertical, 1)
                                .background(DS.badBg)
                                .clipShape(RoundedRectangle(cornerRadius: 4))
                            Text(err.account_name.isEmpty ? "#\(err.account_id ?? 0)" : err.account_name)
                                .font(DS.fontCompactSemibold)
                                .foregroundStyle(DS.ink)
                                .lineLimit(1)
                            Spacer()
                            Text(BeijingTime.fullTime(err.created_at))
                                .font(DS.fontResetTime)
                                .monospacedDigit()
                                .foregroundStyle(DS.muted)
                        }
                        Text(err.message.isEmpty ? err.provider_error_code : err.message)
                            .font(DS.fontResetTime)
                            .foregroundStyle(DS.muted)
                            .lineLimit(2)
                            .multilineTextAlignment(.leading)
                    }
                    .padding(9)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(DS.surfaceSubtle)
                    .overlay(RoundedRectangle(cornerRadius: 7).strokeBorder(DS.line, lineWidth: 1))
                    .clipShape(RoundedRectangle(cornerRadius: 7))
                }
                .buttonStyle(.plain)
            }
        }
    }

    private var footerBar: some View {
        HStack {
            Text(BeijingTime.fullTime(store.state.snapshot?.observed_at))
                .font(DS.fontResetTime)
                .monospacedDigit()
                .foregroundStyle(DS.muted)
            Spacer()
            Button {
                store.onReopenMainWindow?()
            } label: {
                HStack(spacing: 3) {
                    Text("打开主窗口")
                    Image(systemName: "arrow.up.right")
                        .font(DS.iconFont(10, weight: .semibold))
                }
                .font(DS.fontPriorityMeta)
                .foregroundStyle(DS.accent)
            }
            .buttonStyle(.plain)
        }
        .padding(.horizontal, 12)
        .frame(height: 28)
        .background(DS.surfaceSubtle)
    }

    private func quickErrorOverlay(_ detail: OpsError) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text(verbatim: "错误 #\(detail.id)")
                    .font(DS.fontSectionTitle)
                    .foregroundStyle(DS.ink)
                Spacer()
                LinearIconButton(systemName: "xmark", label: "关闭详情", size: 12) {
                    store.closeDetail()
                }
            }
            Text("\(detail.account_name) · HTTP \(detail.status_code) · \(BeijingTime.fullTime(detail.created_at))")
                .font(DS.fontResetTime)
                .monospacedDigit()
                .foregroundStyle(DS.muted)
            ScrollView {
                VStack(alignment: .leading, spacing: 8) {
                    if !detail.message.isEmpty {
                        DiagnosticTextView(text: detail.message)
                    }
                    if let content = detail.content, !content.isEmpty {
                        DiagnosticTextView(text: content, monospaced: true)
                    }
                }
            }
        }
        .padding(12)
        .frame(width: DS.quickPanelWidth, height: 340)
        .background(DS.surface)
        .overlay(RoundedRectangle(cornerRadius: DS.cardRadius).strokeBorder(DS.lineStrong, lineWidth: 1))
        .clipShape(RoundedRectangle(cornerRadius: DS.cardRadius))
    }

    private func tabButton(title: String, count: Int?, selected: Bool, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            HStack(spacing: 4) {
                Text(title)
                if let count {
                    Text("\(count)")
                        .monospacedDigit()
                        .padding(.horizontal, 5)
                        .padding(.vertical, 1)
                        .background(selected ? DS.accentBg : DS.surfaceSubtle)
                        .clipShape(Capsule())
                }
            }
            .font(selected ? DS.fontCompactSemibold : DS.fontCompact)
            .foregroundStyle(selected ? DS.accent : DS.muted)
            .padding(.horizontal, 8)
            .padding(.vertical, 4)
            .background(selected ? DS.accentBg : DS.clear)
            .clipShape(RoundedRectangle(cornerRadius: 6))
        }
        .buttonStyle(.plain)
    }
}

struct QuickPanelContentHeightKey: PreferenceKey {
    static let defaultValue: CGFloat = 120
    static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) {
        value = max(value, nextValue())
    }
}
