import SwiftUI

public struct MainShellView: View {
    @Bindable public var store: AppStore

    public init(store: AppStore) {
        self.store = store
    }

    public var body: some View {
        GeometryReader { geo in
            let compactSidebar = geo.size.width < 1080
            ZStack(alignment: .bottomTrailing) {
                HStack(spacing: 0) {
                    sidebar(width: compactSidebar ? DS.sidebarCompactWidth : DS.sidebarWidth)
                    Rectangle().fill(DS.line).frame(width: 1)
                    mainColumn
                }
                .background(DS.canvas)

                if !store.toast.isEmpty {
                    toastBanner
                        .padding(.trailing, 22)
                        .padding(.bottom, 18)
                }

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
                        .padding(22)
                        .frame(width: 460)
                        .background(DS.surface)
                        .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(DS.line, lineWidth: 1))
                        .clipShape(RoundedRectangle(cornerRadius: 12))
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

                if store.detailError != nil || store.detailBusy {
                    ErrorDetailDrawerView(
                        error: store.detailError,
                        busy: store.detailBusy,
                        onClose: { store.closeDetail() }
                    )
                }
            }
        }
        .ignoresSafeArea(.container, edges: .top)
    }

    private func sidebar(width: CGFloat) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 9) {
                ZStack {
                    RoundedRectangle(cornerRadius: 9)
                        .fill(DS.accent)
                        .frame(width: 30, height: 30)
                    Image(systemName: "waveform.path.ecg")
                        .font(DS.iconFont(15, weight: .semibold))
                        .foregroundStyle(DS.onPrimary)
                }
                Text("Sub2Ops")
                    .font(DS.fontBrand)
                    .foregroundStyle(DS.ink)
            }
            .padding(.horizontal, 6)
            .padding(.bottom, 4)

            if !store.savedConnections.isEmpty || store.state.connected {
                let sidebarPopoverOpen = Binding<Bool>(
                    get: { store.switcherAnchor == .sidebar },
                    set: { open in store.switcherAnchor = open ? .sidebar : nil }
                )
                Button {
                    store.switcherAnchor = (store.switcherAnchor == .sidebar) ? nil : .sidebar
                } label: {
                    HStack(spacing: 8) {
                        StatusDot(store.state.online ? .good : (store.state.connected ? .warning : .muted))
                        VStack(alignment: .leading, spacing: 1) {
                            Text(store.activeConnection?.name ?? "未连接实例")
                                .font(DS.fontCompactSemibold)
                                .foregroundStyle(DS.ink)
                                .lineLimit(1)
                            Text(store.activeConnection?.base_url ?? "点击选择或新增")
                                .font(DS.fontResetTime)
                                .foregroundStyle(DS.muted)
                                .lineLimit(1)
                        }
                        Spacer(minLength: 2)
                        Image(systemName: "chevron.right")
                            .font(DS.iconFont(10, weight: .semibold))
                            .foregroundStyle(DS.muted)
                    }
                    .padding(.horizontal, 9)
                    .padding(.vertical, 7)
                    .background(DS.surface)
                    .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                    .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
                }
                .buttonStyle(.plain)
                .help(store.activeConnection?.base_url ?? "切换或新增 Sub2API 实例")
                .popover(isPresented: sidebarPopoverOpen, arrowEdge: .trailing) {
                    InstanceSwitcherPopoverContent(store: store) {
                        store.switcherAnchor = nil
                    }
                }
            }

            VStack(alignment: .leading, spacing: 3) {
                ForEach(AppPage.allCases) { item in
                    let active = store.page == item
                    Button {
                        store.page = item
                    } label: {
                        HStack(spacing: 9) {
                            Image(systemName: item.symbolName)
                                .font(DS.iconFont(14, weight: active ? .semibold : .regular))
                                .frame(width: 18)
                            Text(item.label)
                                .font(active ? DS.fontBodySemibold : DS.fontBody)
                            Spacer(minLength: 0)
                        }
                        .foregroundStyle(active ? DS.accent : DS.muted)
                        .padding(.horizontal, 10)
                        .frame(height: DS.controlHeightField)
                        .background(active ? DS.accentBg : DS.clear)
                        .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
                    }
                    .buttonStyle(.plain)
                }
            }
            .padding(.top, 4)

            Spacer()
        }
        .padding(.top, 44)
        .padding(.horizontal, 12)
        .padding(.bottom, 16)
        .frame(width: width)
        .frame(maxHeight: .infinity, alignment: .topLeading)
        .background(DS.sidebar)
    }

    private var mainColumn: some View {
        VStack(spacing: 0) {
            topbar
            Rectangle().fill(DS.line).frame(height: 1)

            if store.state.connected && !store.state.online {
                offlineBanner
                Rectangle().fill(DS.line).frame(height: 1)
            }

            mainSurface
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private var topbar: some View {
        HStack(spacing: 12) {
            Text(store.page.label)
                .font(DS.fontBodySemibold)
                .foregroundStyle(DS.ink)

            Spacer()

            HStack(spacing: 8) {
                connectionStatusGroup
                OperationToolbarButton(store: store)
                LinearIconButton(systemName: "menubar.rectangle", label: "打开快捷面板", size: 13) {
                    store.onShowQuickPanel?()
                }
                LinearIconButton(systemName: "arrow.clockwise", label: "立即刷新", size: 13) {
                    store.refresh()
                }
            }
        }
        .padding(.horizontal, 24)
        .frame(height: DS.topbarHeight)
        .background(DS.topbar)
    }

    private var connectionStatusGroup: some View {
        HStack(spacing: 6) {
            HStack(spacing: 6) {
                StatusDot(store.state.online ? .good : .muted)
                Text(store.state.online ? "已连接" : (store.state.connected ? "连接中断" : "未连接"))
                    .font(DS.fontPriorityMeta)
                    .foregroundStyle(DS.muted)
            }
            .padding(.horizontal, 9)
            .padding(.vertical, 4)
            .background(DS.surface)
            .overlay(Capsule().strokeBorder(DS.line, lineWidth: 1))
            .clipShape(Capsule())

            if !store.savedConnections.isEmpty || store.state.connected {
                let topbarPopoverOpen = Binding<Bool>(
                    get: { store.switcherAnchor == .topbar },
                    set: { open in store.switcherAnchor = open ? .topbar : nil }
                )
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
                            .font(DS.fontPriorityMeta)
                            .lineLimit(1)
                    }
                    .foregroundStyle(DS.ink)
                    .padding(.horizontal, 9)
                    .padding(.vertical, 4)
                    .background(DS.surface)
                    .overlay(Capsule().strokeBorder(DS.line, lineWidth: 1))
                    .clipShape(Capsule())
                }
                .buttonStyle(.plain)
                .accessibilityLabel("切换 Sub2API 实例")
                .popover(isPresented: topbarPopoverOpen, arrowEdge: .bottom) {
                    InstanceSwitcherPopoverContent(store: store) {
                        store.switcherAnchor = nil
                    }
                }
            }
        }
    }

    private var offlineBanner: some View {
        HStack(spacing: 8) {
            Image(systemName: "wifi.slash")
                .font(DS.iconFont(12, weight: .medium))
            let baseMsg = store.state.error.isEmpty ? "正在读取云端状态" : store.state.error
            if let snap = store.state.snapshot {
                Text("\(baseMsg) · 上次更新 \(BeijingTime.fullTime(snap.observed_at))")
                    .font(DS.fontPriorityMeta)
                    .monospacedDigit()
            } else {
                Text(baseMsg)
                    .font(DS.fontPriorityMeta)
            }
            Spacer()
        }
        .foregroundStyle(DS.warn)
        .padding(.horizontal, 22)
        .padding(.vertical, 9)
        .background(DS.offlineBg)
    }

    @ViewBuilder
    private var mainSurface: some View {
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
                ConnectionOnboardingView(store: store)
                    .padding(.vertical, 42)
                    .frame(maxWidth: .infinity)
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
                        .frame(maxWidth: 760)
                        .padding(.top, 12)
                    }
                }
                .padding(32)
                .frame(maxWidth: .infinity)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            PageContentRouterView(store: store)
        }
    }

    private var toastBanner: some View {
        HStack(spacing: 10) {
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
            .buttonStyle(.plain)
            .accessibilityLabel("关闭提示")
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .background(DS.surface)
        .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(DS.lineStrong, lineWidth: 1))
        .clipShape(RoundedRectangle(cornerRadius: 8))
        .shadow(color: DS.shadowColor, radius: 10, x: 0, y: 4)
    }

    private var discardDraftDialog: some View {
        let hasGroup = store.groupChangesCount > 0
        let hasPrio = store.priorityChangesCount > 0
        let targetText = "\(hasGroup ? "分组" : "")\(hasGroup && hasPrio ? "和" : "")\(hasPrio ? "优先级" : "")"

        return VStack(alignment: .leading, spacing: 16) {
            Text("放弃\(targetText)草稿并更换连接？")
                .font(DS.fontSectionTitle)
                .foregroundStyle(DS.ink)
            HStack(spacing: 10) {
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
        }
        .padding(22)
        .frame(width: 380)
        .background(DS.surface)
        .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(DS.line, lineWidth: 1))
        .clipShape(RoundedRectangle(cornerRadius: 12))
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
