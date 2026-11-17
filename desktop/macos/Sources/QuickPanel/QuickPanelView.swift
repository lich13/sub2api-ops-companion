import SwiftUI

/// Stage M1 QuickPanel surface; expanded with full group & error rows in M2.
public struct QuickPanelRootView: View {
    @Bindable var store: AppStore
    let onContentHeightChange: @MainActor (CGFloat) -> Void

    public init(store: AppStore, onContentHeightChange: @escaping @MainActor (CGFloat) -> Void = { _ in }) {
        self.store = store
        self.onContentHeightChange = onContentHeightChange
    }

    public var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                HStack(spacing: 4) {
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

            Divider().overlay(DS.line)

            VStack(spacing: 8) {
                if !store.state.connected {
                    Text("尚未连接运维服务")
                        .font(DS.fontCompact)
                        .foregroundStyle(DS.muted)
                        .padding(.vertical, 20)
                } else {
                    Text(store.quickTab == .groups ? "分组与最近调用" : "异常状态")
                        .font(DS.fontCompact)
                        .foregroundStyle(DS.muted)
                        .padding(.vertical, 20)
                }
            }
            .frame(maxWidth: .infinity)
            .background(
                GeometryReader { geo in
                    DS.clear.preference(key: QuickPanelContentHeightKey.self, value: geo.size.height)
                }
            )

            Divider().overlay(DS.line)

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
        .frame(width: DS.quickPanelWidth)
        .background(DS.surface)
        .overlay(
            RoundedRectangle(cornerRadius: DS.cardRadius)
                .strokeBorder(DS.line, lineWidth: 1)
        )
        .clipShape(RoundedRectangle(cornerRadius: DS.cardRadius))
        .onPreferenceChange(QuickPanelContentHeightKey.self) { bodyHeight in
            onContentHeightChange(bodyHeight + 66)
        }
        .onExitCommand {
            if store.detailError != nil || store.detailBusy {
                store.closeDetail()
            } else {
                store.onHideQuickPanel?()
            }
        }
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
