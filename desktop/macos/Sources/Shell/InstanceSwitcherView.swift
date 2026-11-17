import SwiftUI

/// Multi-instance switcher popover & sidebar button (`renderSwitcherMenu` in `desktop/src/main.tsx`).
public struct InstanceSwitcherPopoverContent: View {
    @Bindable public var store: AppStore
    public let dismiss: () -> Void

    public init(store: AppStore, dismiss: @escaping () -> Void) {
        self.store = store
        self.dismiss = dismiss
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Text("Sub2API 实例 (\(store.savedConnections.count))")
                    .font(DS.fontPriorityMeta)
                    .monospacedDigit()
                    .foregroundStyle(DS.muted)
                Spacer()
                Button("管理实例") {
                    dismiss()
                    store.page = .settings
                }
                .buttonStyle(LinearButtonStyle(variant: .ghost, compact: true))
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)

            Divider().overlay(DS.line)

            ScrollView {
                VStack(spacing: 3) {
                    ForEach(store.savedConnections, id: \.base_url) { item in
                        let isCurrent = store.state.connected && item.base_url == store.activeUrl
                        let isSwitching = store.switchingUrl == item.base_url
                        Button {
                            dismiss()
                            Task { await store.switchConnection(item) }
                        } label: {
                            HStack(spacing: 9) {
                                StatusDot(isCurrent ? (store.state.online ? .good : .warning) : .muted)
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(item.name)
                                        .font(DS.fontCompactSemibold)
                                        .foregroundStyle(DS.ink)
                                        .lineLimit(1)
                                    Text(item.base_url)
                                        .font(DS.fontMono11)
                                        .foregroundStyle(DS.muted)
                                        .lineLimit(1)
                                }
                                Spacer()
                                if isSwitching {
                                    ProgressView().controlSize(.small)
                                } else if isCurrent {
                                    Text("当前")
                                        .font(DS.fontResetTimeSemibold)
                                        .foregroundStyle(DS.accent)
                                        .padding(.horizontal, 6)
                                        .padding(.vertical, 2)
                                        .background(DS.accentBg)
                                        .clipShape(RoundedRectangle(cornerRadius: 4))
                                }
                            }
                            .padding(.horizontal, 10)
                            .padding(.vertical, 7)
                            .background(isCurrent ? DS.accentBg.opacity(0.55) : DS.clear)
                            .clipShape(RoundedRectangle(cornerRadius: 6))
                        }
                        .buttonStyle(.plain)
                        .disabled(store.switchingUrl != nil)
                    }
                }
                .padding(8)
            }
            .frame(maxHeight: 260)

            Divider().overlay(DS.line)

            Button {
                dismiss()
                store.connectionModal = ConnectionModalTarget(baseUrl: "", name: "", requireKey: true)
            } label: {
                HStack(spacing: 6) {
                    Image(systemName: "plus")
                        .font(DS.iconFont(12, weight: .semibold))
                    Text("新增实例")
                        .font(DS.fontCompactMedium)
                    Spacer()
                }
                .foregroundStyle(DS.accent)
                .padding(.horizontal, 12)
                .padding(.vertical, 9)
            }
            .buttonStyle(.plain)
        }
        .frame(width: 290)
        .background(DS.surface)
    }
}
