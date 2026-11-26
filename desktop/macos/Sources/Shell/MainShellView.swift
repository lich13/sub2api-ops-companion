import SwiftUI

public struct MainShellView: View {
    @Bindable public var store: AppStore

    public init(store: AppStore) {
        self.store = store
    }

    private var detailAccount: Account? {
        guard let aid = store.detailError?.account_id else { return nil }
        return store.accountById[aid]
    }

    public var body: some View {
        GeometryReader { geo in
            // Mirrors responsive.css: sidebar 176 → 144 and content padding shrink at ≤1360.
            let compactContent = geo.size.width <= 1360
            let compactSidebar = compactContent
            let sidebarW = compactSidebar ? DS.sidebarCompactWidth : DS.sidebarWidth
            let mainW = max(320, geo.size.width - sidebarW - 1)
            ZStack {
                HStack(spacing: 0) {
                    sidebar(width: sidebarW, compact: compactSidebar)
                    Rectangle().fill(DS.line).frame(width: 1)
                    mainColumn(compact: compactContent, mainWidth: mainW)
                }
                .background(DS.canvas)

                if let modalTarget = store.connectionModal {
                    modalOverlay {
                        VStack {
                            ConnectionOnboardingView(
                                store: store,
                                initialUrl: modalTarget.baseUrl,
                                initialName: modalTarget.name,
                                requireKey: modalTarget.requireKey,
                                showNameField: true,
                                onConnected: { store.connectionModal = nil },
                                onCancel: { store.connectionModal = nil }
                            )
                        }
                        .padding(24)
                        .frame(width: 460)
                        .background(DS.surface)
                        .overlay(RoundedRectangle(cornerRadius: DS.modalRadius).strokeBorder(DS.line, lineWidth: 1))
                        .clipShape(RoundedRectangle(cornerRadius: DS.modalRadius))
                        .shadow(color: DS.modalShadowColor, radius: 24, x: 0, y: 12)
                        .sub2opsDialogFocus {
                            store.connectionModal = nil
                        }
                    } onDismiss: {
                        store.connectionModal = nil
                    }
                }

                if store.pendingDiscardContinuation != nil {
                    modalOverlay {
                        discardDraftDialog
                    } onDismiss: {
                        store.resolveDiscardPrompt(proceed: false)
                    }
                }

                if let modal = store.accountModal {
                    AccountModalHostView(
                        store: store,
                        destination: modal,
                        onClose: { store.accountModal = nil }
                    )
                }

                if let customModal = store.windowModalView {
                    modalOverlay {
                        customModal
                    } onDismiss: {
                        if let onBackdrop = store.windowModalOnBackdrop {
                            onBackdrop()
                        }
                    }
                }

                if let logRow = store.selectedDetectionLog {
                    DetectionLogDetailModal(row: logRow, online: store.state.online) {
                        store.selectedDetectionLog = nil
                    }
                }

                if store.selectedRecordId != nil {
                    RecordDetailDrawerView(
                        row: store.detailRecord,
                        loading: store.recordDetailLoading,
                        error: store.recordDetailError,
                        onClose: { store.closeRecordDetail() }
                    )
                }

                if store.detailError != nil || store.detailBusy {
                    ErrorDetailDrawerView(
                        error: store.detailError,
                        busy: store.detailBusy,
                        account: detailAccount,
                        store: store,
                        onClose: { store.closeDetail() }
                    )
                }
            }
            .overlay(alignment: .bottom) {
                if !store.toast.isEmpty {
                    toastBanner
                        .padding(.bottom, 22)
                }
            }
        }
        .ignoresSafeArea(.container, edges: .top)
    }

    private func sidebar(width: CGFloat, compact: Bool) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: compact ? 8 : 9) {
                ZStack {
                    RoundedRectangle(cornerRadius: 9)
                        .fill(DS.accent)
                        .frame(width: 32, height: 32)
                    Image(systemName: "waveform.path.ecg")
                        .font(DS.iconFont(15, weight: .semibold))
                        .foregroundStyle(DS.onPrimary)
                }
                Text("Sub2Ops")
                    .font(compact ? DS.fontBrandCompact : DS.fontBrand)
                    .foregroundStyle(DS.ink)
                    .lineLimit(1)
            }
            .padding(.horizontal, 6)
            .padding(.bottom, 6)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(LinearWindowDragHandle())

            if !store.savedConnections.isEmpty || store.state.connected {
                SidebarInstanceButton(store: store, compact: compact)
                    .padding(.bottom, 2)
            }

            VStack(alignment: .leading, spacing: 3) {
                ForEach(AppPage.allCases) { item in
                    SidebarNavButton(
                        item: item,
                        active: store.page == item,
                        action: { store.page = item }
                    )
                }
            }

            Spacer()
        }
        .padding(.top, 48)
        .padding(.horizontal, compact ? 10 : 14)
        .padding(.bottom, 16)
        .frame(width: width)
        .frame(maxHeight: .infinity, alignment: .topLeading)
        .background(DS.sidebar.overlay(LinearWindowDragHandle()))
    }

    private struct SidebarInstanceButton: View {
        @Bindable var store: AppStore
        var compact: Bool = false
        @State private var hovered: Bool = false

        var body: some View {
            let isOpen = store.switcherAnchor == .sidebar
            Button {
                store.switcherAnchor = isOpen ? nil : .sidebar
            } label: {
                HStack(spacing: compact ? 5 : 7) {
                    StatusDot(store.state.online ? .good : (store.state.connected ? .warning : .muted))
                    VStack(alignment: .leading, spacing: 1) {
                        Text(store.activeConnection?.name ?? "未连接实例")
                            .font(DS.fontCompactSemibold)
                            .foregroundStyle(DS.ink)
                            .lineLimit(1)
                        Text(verbatim: store.activeConnection?.base_url ?? "点击选择或新增")
                            .font(DS.fontPriorityMeta)
                            .foregroundStyle(DS.muted)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    Image(systemName: isOpen ? "chevron.down" : "chevron.right")
                        .font(DS.iconFont(10, weight: .semibold))
                        .foregroundStyle(isOpen ? DS.accent : DS.muted)
                }
                .padding(.horizontal, compact ? 7 : 9)
                .padding(.vertical, 7)
                .background(isOpen ? DS.surface : (hovered ? DS.hover : DS.surfaceSubtle))
                .overlay(
                    RoundedRectangle(cornerRadius: 8)
                        .strokeBorder(isOpen ? DS.accentBorder : (hovered ? DS.lineStrong : DS.line), lineWidth: 1)
                )
                .clipShape(RoundedRectangle(cornerRadius: 8))
            }
            .buttonStyle(.linearPlain)
            .onHover { hovered = $0 }
            .help(store.activeConnection?.base_url ?? "切换或新增 Sub2API 实例")
            .sub2opsSwitcherPopover(anchor: .sidebar, store: store, arrowEdge: .trailing) {
                InstanceSwitcherPopoverContent(store: store) {
                    store.switcherAnchor = nil
                }
            }
        }
    }

    private struct SidebarNavButton: View {
        let item: AppPage
        let active: Bool
        let action: () -> Void
        @State private var hovered: Bool = false

        var body: some View {
            Button(action: action) {
                HStack(spacing: 9) {
                    Image(systemName: item.symbolName)
                        .font(DS.iconFont(14, weight: active ? .semibold : .regular))
                        .frame(width: 18)
                    Text(item.label)
                        .font(active ? DS.fontBodySemibold : DS.fontBody)
                    Spacer(minLength: 0)
                }
                .foregroundStyle(active ? DS.accent : (hovered ? DS.ink : DS.muted))
                .padding(.horizontal, 11)
                .frame(height: DS.controlHeightField)
                .background(active ? DS.accentBg : (hovered ? DS.sidebarHover : DS.clear))
                .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
                .contentShape(Rectangle())
            }
            .buttonStyle(.linearPlain)
            .onHover { hovered = $0 }
        }
    }

    private func mainColumn(compact: Bool, mainWidth: CGFloat) -> some View {
        VStack(spacing: 0) {
            topbar(compact: compact)
            Rectangle().fill(DS.line).frame(height: 1)

            if store.state.connected && !store.state.online {
                offlineBanner
                Rectangle().fill(DS.line).frame(height: 1)
            }

            mainSurface(compact: compact, mainWidth: mainWidth)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private func topbar(compact: Bool) -> some View {
        HStack(spacing: 16) {
            Text(store.page.label)
                .font(DS.fontBodySemibold)
                .foregroundStyle(DS.ink)
                .allowsHitTesting(false)

            Spacer()
                .allowsHitTesting(false)

            HStack(spacing: 8) {
                connectionStatusGroup
                OperationToolbarButton(store: store)
                LinearIconButton(systemName: "rectangle.grid.2x2", label: "打开快捷面板", size: 13, boxSize: 30) {
                    store.onShowQuickPanel?()
                }
                LinearIconButton(systemName: "arrow.clockwise", label: "立即刷新", size: 13, boxSize: 30) {
                    store.refresh()
                }
            }
        }
        .padding(.horizontal, compact ? 18 : 26)
        .frame(height: DS.topbarHeight)
        .background(DS.topbar.overlay(LinearWindowDragHandle()))
    }

    private var connectionStatusGroup: some View {
        HStack(spacing: 6) {
            HStack(spacing: 6) {
                StatusDot(store.state.online ? .good : (store.state.connected ? .warning : .muted))
                Text(store.state.online ? "已连接" : (store.state.connected ? "连接中断" : "未连接"))
                    .font(DS.fontPriorityMeta)
                    .foregroundStyle(DS.muted)
            }
            .padding(.horizontal, 9)
            .frame(height: 26)
            .background(DS.surface)
            .overlay(Capsule().strokeBorder(DS.line, lineWidth: 1))
            .clipShape(Capsule())

            if !store.savedConnections.isEmpty || store.state.connected {
                TopbarInstanceTrigger(store: store)
            }
        }
    }

    private struct TopbarInstanceTrigger: View {
        @Bindable var store: AppStore
        @State private var hovered: Bool = false

        var body: some View {
            Button {
                store.switcherAnchor = (store.switcherAnchor == .topbar) ? nil : .topbar
            } label: {
                HStack(spacing: 5) {
                    if store.switchingUrl != nil {
                        ProgressView().controlSize(.small)
                    } else {
                        Image(systemName: "powerplug")
                            .font(DS.iconFont(11, weight: .medium))
                    }
                    Text(store.activeConnection?.name ?? "选择实例")
                        .font(DS.fontCompactMedium)
                        .lineLimit(1)
                }
                .foregroundStyle(DS.ink)
                .padding(.horizontal, 9)
                .frame(height: 28)
                .background(hovered ? DS.hover : DS.surfaceSubtle)
                .overlay(
                    RoundedRectangle(cornerRadius: DS.cornerRadius)
                        .strokeBorder(hovered ? DS.lineStrong : DS.line, lineWidth: 1)
                )
                .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
            }
            .buttonStyle(.linearPlain)
            .onHover { hovered = $0 }
            .accessibilityLabel("切换 Sub2API 实例")
            .sub2opsSwitcherPopover(anchor: .topbar, store: store, arrowEdge: .bottom) {
                InstanceSwitcherPopoverContent(store: store) {
                    store.switcherAnchor = nil
                }
            }
        }
    }

    private var offlineBanner: some View {
        HStack(spacing: 8) {
            Image(systemName: "wifi.slash")
                .font(DS.iconFont(12, weight: .medium))
            let baseMsg = store.state.error.isEmpty ? "正在读取云端状态" : store.state.error
            OfflineObservedBannerText(
                baseMessage: baseMsg,
                hasSnapshot: store.state.snapshot != nil,
                clock: store.observedClock
            )
            Spacer()
        }
        .foregroundStyle(DS.warn)
        .padding(.horizontal, 24)
        .padding(.vertical, 9)
        .background(DS.offlineBg)
    }

    @ViewBuilder
    private func mainSurface(compact: Bool, mainWidth: CGFloat) -> some View {
        if !store.ready || store.state.initializing == true {
            VStack(spacing: 12) {
                ProgressView()
                Text("正在加载客户端")
                    .font(DS.fontBody)
                    .foregroundStyle(DS.muted)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else if !store.state.connected {
            ScrollView {
                VStack {
                    ConnectionOnboardingView(store: store)
                        .frame(width: 420)
                }
                .padding(.vertical, 42)
                .frame(maxWidth: .infinity, alignment: .center)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else if store.state.snapshot == nil {
            ScrollView {
                VStack(spacing: 16) {
                    Image(systemName: "powerplug")
                        .font(DS.iconFont(26))
                        .foregroundStyle(DS.muted)
                    Text("等待云端数据")
                        .font(DS.fontSectionTitle)
                        .foregroundStyle(DS.ink)
                    Text(store.state.error.isEmpty ? "正在连接运维服务" : store.state.error)
                        .font(DS.fontCompact)
                        .foregroundStyle(DS.muted)
                    Button("连接设置") {
                        store.page = .settings
                    }
                    .buttonStyle(LinearButtonStyle(variant: .secondary))

                    if store.page == .settings {
                        VStack(spacing: 16) {
                            ConnectionStatusCardView(connectionKey: store.connectionKey)
                            CardSection {
                                ConnectionOnboardingView(store: store, embeddedInSettings: true)
                            }
                        }
                        .frame(maxWidth: 820)
                        .padding(.top, 12)
                    }
                }
                .padding(32)
                .frame(maxWidth: .infinity)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            PageContentRouterView(store: store, compact: compact, mainWidth: mainWidth)
        }
    }

    private var toastBanner: some View {
        HStack(spacing: 12) {
            Text(store.toast)
                .font(DS.fontCompact)
                .foregroundStyle(DS.ink)
                .lineLimit(3)
            Button {
                store.toast = ""
            } label: {
                Image(systemName: "xmark")
                    .font(DS.iconFont(11, weight: .semibold))
                    .foregroundStyle(DS.muted)
            }
            .buttonStyle(.linearPlain)
            .accessibilityLabel("关闭提示")
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .background(DS.surface)
        .overlay(RoundedRectangle(cornerRadius: 9).strokeBorder(DS.lineStrong, lineWidth: 1))
        .clipShape(RoundedRectangle(cornerRadius: 9))
        .shadow(color: DS.modalShadowColor, radius: 16, x: 0, y: 6)
    }

    private var discardDraftDialog: some View {
        let hasGroup = store.groupChangesCount > 0
        let hasPrio = store.priorityChangesCount > 0
        let targetText = "\(hasGroup ? "分组" : "")\(hasGroup && hasPrio ? "和" : "")\(hasPrio ? "优先级" : "")"

        return LinearModalCard(
            title: "放弃\(targetText)草稿并更换连接？",
            width: 390,
            onClose: { store.resolveDiscardPrompt(proceed: false) }
        ) {
            Text("切换连接后，当前尚未提交的\(targetText)修改将被丢弃。")
                .font(DS.fontCompact)
                .foregroundStyle(DS.muted)
        } footer: {
            Spacer()
            Button("保留草稿") {
                store.resolveDiscardPrompt(proceed: false)
            }
            .buttonStyle(LinearButtonStyle(variant: .secondary))
            Button("放弃并继续") {
                store.resolveDiscardPrompt(proceed: true)
            }
            .buttonStyle(LinearButtonStyle(variant: .primary))
        }
        .sub2opsDialogFocus {
            store.resolveDiscardPrompt(proceed: false)
        }
    }

    private func modalOverlay<DialogContent: View>(
        @ViewBuilder content: () -> DialogContent,
        onDismiss: @escaping () -> Void
    ) -> some View {
        ZStack {
            DS.backdrop
                .ignoresSafeArea()
                .onTapGesture(perform: onDismiss)
            content()
        }
    }
}
