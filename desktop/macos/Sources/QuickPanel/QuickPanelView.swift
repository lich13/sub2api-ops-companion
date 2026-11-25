import SwiftUI

/// Stage M2 full QuickPanel surface matching `main.tsx` Quick mode & `QuickAccountStatus.tsx` + `DESIGN.md` lines 9, 12, 13.
public struct QuickPanelRootView: View {
    @Bindable var store: AppStore
    let onContentHeightChange: @MainActor (CGFloat) -> Void

    public init(store: AppStore, onContentHeightChange: @escaping @MainActor (CGFloat) -> Void = { _ in }) {
        self.store = store
        self.onContentHeightChange = onContentHeightChange
    }

    private var activeGroups: [Group] {
        store.activeGroups
    }

    private var accountById: [Int: Account] {
        store.accountById
    }

    public var body: some View {
        ZStack {
            VStack(spacing: 0) {
                headerBar
                Rectangle().fill(DS.line).frame(height: 1)

                if store.state.connected && !store.state.online {
                    HStack(spacing: 6) {
                        Image(systemName: "wifi.slash")
                            .font(DS.iconFont(10, weight: .medium))
                        Text(store.state.error.isEmpty ? "连接中断，显示上次状态" : store.state.error)
                            .font(DS.fontResetTime)
                            .lineLimit(1)
                        Spacer()
                    }
                    .foregroundStyle(DS.warn)
                    .padding(.horizontal, 10)
                    .padding(.vertical, 5)
                    .background(DS.offlineBg)
                    Rectangle().fill(DS.line).frame(height: 1)
                }

                let bodyMaxHeight = DS.quickPanelMaxHeight - ((store.state.connected && !store.state.online) ? 90 : 66)
                LinearBoundedScrollView(maxHeight: bodyMaxHeight) {
                    VStack(alignment: .leading, spacing: 6) {
                        if !store.state.connected {
                            ConnectionOnboardingView(store: store, compactQuick: true)
                                .padding(.vertical, 8)
                        } else if store.quickTab == .groups {
                            groupsBody
                        } else {
                            errorsBody
                        }
                    }
                    .padding(.horizontal, 10)
                    .padding(.top, 4)
                    .padding(.bottom, 6)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(
                        GeometryReader { geo in
                            DS.clear.preference(key: QuickPanelContentHeightKey.self, value: geo.size.height)
                        }
                    )
                }

                Rectangle().fill(DS.line).frame(height: 1)
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
            let offlineExtra: CGFloat = (store.state.connected && !store.state.online) ? 24 : 0
            let clamped = min(max(bodyHeight + 66 + offlineExtra, DS.quickPanelMinHeight), DS.quickPanelMaxHeight)
            onContentHeightChange(clamped)
        }
        .onExitCommand {
            guard !ResponderRestorer.shouldSuppressActiveEscape() else { return }
            if store.switcherAnchor == .quick {
                store.switcherAnchor = nil
            } else if store.detailError != nil || store.detailBusy {
                store.closeDetail()
            } else {
                store.onHideQuickPanel?()
            }
        }
    }

    private var headerBar: some View {
        HStack(spacing: 6) {
            HStack(spacing: 3) {
                tabButton(title: "分组", count: nil, selected: store.quickTab == .groups) {
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

            Spacer(minLength: 4)

            HStack(spacing: 4) {
                StatusDot(store.state.online ? .good : (store.state.connected ? .warning : .muted), size: 6)
                Text(store.state.online ? "已连接" : (store.state.connected ? "连接中断" : "未连接"))
                    .font(DS.fontResetTime)
                    .foregroundStyle(DS.muted)
                    .lineLimit(1)
            }
            .padding(.trailing, 2)

            if !store.savedConnections.isEmpty || store.state.connected {
                Button {
                    store.switcherAnchor = (store.switcherAnchor == .quick) ? nil : .quick
                } label: {
                    HStack(spacing: 4) {
                        if store.switchingUrl != nil {
                            ProgressView().controlSize(.mini)
                        } else {
                            Image(systemName: "powerplug")
                                .font(DS.iconFont(10, weight: .medium))
                        }
                        Text(store.activeConnection?.name ?? "选择实例")
                            .font(DS.fontPriorityMeta)
                            .lineLimit(1)
                    }
                    .foregroundStyle(DS.ink)
                    .padding(.horizontal, 7)
                    .frame(maxWidth: 124, minHeight: 24)
                    .background(DS.surfaceSubtle)
                    .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(DS.line, lineWidth: 1))
                    .clipShape(RoundedRectangle(cornerRadius: 6))
                }
                .buttonStyle(.linearPlain)
                .accessibilityLabel("切换 Sub2API 实例")
                .sub2opsSwitcherPopover(anchor: .quick, store: store, arrowEdge: .bottom) {
                    InstanceSwitcherPopoverContent(store: store, isQuick: true) {
                        store.switcherAnchor = nil
                    }
                }
            }

            HStack(spacing: 2) {
                LinearIconButton(systemName: "arrow.clockwise", label: "立即刷新", size: 12, boxSize: 27) {
                    store.refresh()
                }
                LinearIconButton(
                    systemName: store.state.preferences.pinned ? "pin.fill" : "pin",
                    label: store.state.preferences.pinned ? "取消固定" : "固定面板",
                    size: 12,
                    boxSize: 27,
                    selected: store.state.preferences.pinned
                ) {
                    Task { await store.updatePreferences(pinned: !store.state.preferences.pinned) }
                }
                LinearIconButton(systemName: "xmark", label: "关闭快捷面板", size: 12, boxSize: 27) {
                    store.onHideQuickPanel?()
                }
            }
        }
        .padding(.horizontal, 10)
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
            VStack(alignment: .leading, spacing: 6) {
                ForEach(activeGroups) { group in
                    VStack(alignment: .leading, spacing: 0) {
                        HStack(spacing: 6) {
                            Text(group.name)
                                .font(DS.fontBadge)
                                .foregroundStyle(DS.ink)
                                .lineLimit(1)
                            Text(verbatim: group.platform)
                                .font(DS.fontResetTime)
                                .foregroundStyle(DS.muted)
                                .lineLimit(1)
                            Spacer(minLength: 0)
                        }
                        .frame(height: 24)

                        if group.recent_accounts.isEmpty {
                            Rectangle().fill(DS.line).frame(height: 1)
                            Text("暂无成功调用")
                                .font(DS.fontPriorityMeta)
                                .foregroundStyle(DS.muted)
                                .padding(.vertical, 10)
                        } else {
                            ForEach(Array(group.recent_accounts.enumerated()), id: \.offset) { _, call in
                                let aid = call.account_id
                                let acc = accountById[aid]
                                let cname = call.account_name
                                Rectangle().fill(DS.line).frame(height: 1)
                                QuickAccountRowView(
                                    account: acc,
                                    displayName: cname.isEmpty ? (acc?.name ?? "账号 #\(aid)") : cname,
                                    calledAt: call.called_at,
                                    online: store.state.online,
                                    scheduleBusy: store.busyScheduleAccountId == aid,
                                    recoverBusy: store.busyRecoverAccountIds.contains(aid),
                                    onToggleSchedulable: {
                                        if let acc {
                                            Task { await store.toggleSchedulable(acc) }
                                        }
                                    },
                                    onRecover: {
                                        if let acc {
                                            Task { await store.recoverAccount(acc) }
                                        }
                                    },
                                    onOpenError: { errId in
                                        store.openErrorDetail(id: errId)
                                    }
                                )
                            }
                        }
                    }
                }
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
            let visibleErrors = Array(store.errors.prefix(12))
            VStack(spacing: 0) {
                ForEach(Array(visibleErrors.enumerated()), id: \.element.recordId) { idx, err in
                    let acc = err.account_id.flatMap { accountById[$0] }
                    let restriction = QuickAccountStatusHelper.restriction(for: acc)
                    let codeVal = err.upstream_status_code ?? err.statusCodeValue
                    let isWarnCode = codeVal == 429
                    let isCurrent = err.id != nil && acc?.last_error_id == err.id

                    HStack(alignment: .center, spacing: 8) {
                        Button {
                            if let errId = err.id {
                                store.openErrorDetail(id: errId)
                            }
                        } label: {
                            HStack(alignment: .top, spacing: 10) {
                                Text(codeVal > 0 ? "\(codeVal)" : "ERR")
                                    .font(DS.fontResetTimeSemibold)
                                    .monospacedDigit()
                                    .foregroundStyle(isWarnCode ? DS.warn : DS.bad)
                                    .frame(minWidth: 34)
                                    .padding(.vertical, 5)
                                    .background(isWarnCode ? DS.warnBg : DS.badBg)
                                    .clipShape(RoundedRectangle(cornerRadius: 5))

                                VStack(alignment: .leading, spacing: 4) {
                                    HStack(spacing: 4) {
                                        Text(err.accountNameText.isEmpty ? (err.account_id.map { "账号 #\($0)" } ?? "未关联账号") : err.accountNameText)
                                            .font(DS.fontCompactSemibold)
                                            .foregroundStyle(restriction.isEmpty ? DS.ink : DS.warn)
                                            .lineLimit(1)
                                        if !restriction.isEmpty {
                                            Image(systemName: "exclamationmark.circle")
                                                .font(DS.iconFont(10, weight: .medium))
                                                .foregroundStyle(DS.warn)
                                        }
                                        if let acc {
                                            DegradationBadgeView(account: acc, compact: true)
                                        }
                                    }

                                    HStack(spacing: 6) {
                                        Text(isCurrent ? "当前" : "历史")
                                            .font(DS.fontResetTimeSemibold)
                                            .foregroundStyle(isCurrent ? DS.warn : DS.muted)
                                        Text(err.providerErrorCodeText.isEmpty ? (err.messageText.isEmpty ? "查看错误详情" : err.messageText) : err.providerErrorCodeText)
                                            .font(DS.fontPriorityMeta)
                                            .foregroundStyle(DS.muted)
                                            .lineLimit(1)
                                    }

                                    Text(BeijingTime.fullTime(err.created_at))
                                        .font(DS.fontPriorityMeta)
                                        .monospacedDigit()
                                        .foregroundStyle(DS.muted)
                                }
                                Spacer(minLength: 0)
                            }
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.linearPlain)

                        if let acc, acc.recoverable == true {
                            Button {
                                Task { await store.recoverAccount(acc) }
                            } label: {
                                Image(systemName: "arrow.counterclockwise")
                                    .font(DS.iconFont(12, weight: .semibold))
                                    .foregroundStyle(DS.warn)
                                    .frame(width: 24, height: 24)
                            }
                            .buttonStyle(.linearPlain)
                            .disabled(!store.state.online || store.busyRecoverAccountIds.contains(acc.id))
                            .help("恢复状态：\(acc.name)")
                        } else {
                            Image(systemName: "chevron.right")
                                .font(DS.iconFont(11, weight: .medium))
                                .foregroundStyle(DS.muted)
                                .frame(width: 20)
                        }
                    }
                    .padding(.horizontal, 10)
                    .padding(.vertical, 9)

                    if idx < visibleErrors.count - 1 {
                        Rectangle().fill(DS.line).frame(height: 1)
                    }
                }
            }
            .background(DS.surface)
            .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(DS.line, lineWidth: 1))
            .clipShape(RoundedRectangle(cornerRadius: 8))
        }
    }

    private var footerBar: some View {
        HStack {
            QuickObservedFooterText(clock: store.observedClock)
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
                .frame(height: 28)
            }
            .buttonStyle(.linearPlain)
        }
        .padding(.horizontal, 10)
        .frame(height: 28)
        .background(DS.surface)
    }

    private func quickErrorOverlay(_ detail: OpsError) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text(verbatim: "错误 #\(detail.recordId)")
                    .font(DS.fontSectionTitle)
                    .foregroundStyle(DS.ink)
                Spacer()
                LinearIconButton(systemName: "xmark", label: "关闭详情", size: 12, boxSize: 26) {
                    store.closeDetail()
                }
            }
            Text("\(detail.accountNameText) · HTTP \(detail.upstream_status_code ?? detail.statusCodeValue) · \(BeijingTime.fullTime(detail.created_at))")
                .font(DS.fontResetTime)
                .monospacedDigit()
                .foregroundStyle(DS.muted)
            ScrollView {
                VStack(alignment: .leading, spacing: 8) {
                    if !detail.messageText.isEmpty {
                        DiagnosticTextView(text: detail.messageText)
                    }
                    if let content = detail.content, !content.isEmpty {
                        DiagnosticTextView(text: content, monospaced: true)
                    }
                }
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(DS.surface)
        .clipShape(RoundedRectangle(cornerRadius: DS.cardRadius))
    }

    private func tabButton(title: String, count: Int?, selected: Bool, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            HStack(spacing: 4) {
                Text(title)
                    .font(selected ? DS.fontCompactSemibold : DS.fontCompact)
                    .foregroundStyle(selected ? DS.accent : DS.muted)
                if let count {
                    Text("\(count)")
                        .font(DS.fontPriorityMeta)
                        .monospacedDigit()
                        .foregroundStyle(selected ? DS.accent : DS.muted)
                }
            }
            .padding(.horizontal, 8)
            .frame(height: 26)
            .background(selected ? DS.accentBg : DS.clear)
            .clipShape(RoundedRectangle(cornerRadius: 5))
        }
        .buttonStyle(.linearPlain)
    }
}

private struct QuickPanelContentHeightKey: PreferenceKey {
    static let defaultValue: CGFloat = 0
    static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) {
        value = max(value, nextValue())
    }
}
