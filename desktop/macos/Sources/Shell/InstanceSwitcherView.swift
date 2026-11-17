import SwiftUI

/// Multi-instance switcher popover (`renderSwitcherMenu` in `desktop/src/main.tsx`).
public struct InstanceSwitcherPopoverContent: View {
    @Bindable public var store: AppStore
    public var isQuick: Bool
    public let dismiss: () -> Void

    public init(store: AppStore, isQuick: Bool = false, dismiss: @escaping () -> Void) {
        self.store = store
        self.isQuick = isQuick
        self.dismiss = dismiss
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text("Sub2API 实例 (\(store.savedConnections.count))")
                    .font(DS.fontBadge)
                    .monospacedDigit()
                    .foregroundStyle(DS.muted)
                Spacer()
                if !isQuick {
                    Button("管理实例") {
                        dismiss()
                        store.page = .settings
                    }
                    .font(DS.fontPriorityMeta)
                    .foregroundStyle(DS.muted)
                    .buttonStyle(.plain)
                }
            }
            .padding(.horizontal, 8)
            .padding(.top, 4)

            LinearBoundedScrollView(maxHeight: 220) {
                VStack(spacing: 4) {
                    ForEach(store.savedConnections, id: \.base_url) { item in
                        SwitcherRow(
                            item: item,
                            isCurrent: store.state.connected && item.base_url == store.activeUrl,
                            online: store.state.online,
                            isSwitching: store.switchingUrl == item.base_url,
                            disabled: store.switchingUrl != nil
                        ) {
                            dismiss()
                            Task { await store.switchConnection(item) }
                        }
                    }
                }
            }

            Rectangle().fill(DS.line).frame(height: 1)

            Button {
                dismiss()
                if isQuick {
                    store.onReopenMainWindow?()
                }
                store.connectionModal = ConnectionModalTarget(baseUrl: "", name: "", requireKey: true)
            } label: {
                HStack(spacing: 6) {
                    Image(systemName: "plus")
                        .font(DS.iconFont(12, weight: .semibold))
                    Text("新增实例")
                        .font(DS.fontCompactMedium)
                }
                .frame(maxWidth: .infinity)
            }
            .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
        }
        .padding(8)
        .frame(width: isQuick ? 248 : 292)
        .background(DS.surface)
    }

    private struct SwitcherRow: View {
        let item: SavedConnection
        let isCurrent: Bool
        let online: Bool
        let isSwitching: Bool
        let disabled: Bool
        let action: () -> Void
        @State private var hovered: Bool = false

        var body: some View {
            Button(action: action) {
                HStack(spacing: 8) {
                    StatusDot(isCurrent ? (online ? .good : .warning) : .muted)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(item.name)
                            .font(DS.fontCompactSemibold)
                            .foregroundStyle(DS.ink)
                            .lineLimit(1)
                        Text(verbatim: item.base_url)
                            .font(DS.fontPriorityMeta)
                            .foregroundStyle(DS.muted)
                            .lineLimit(1)
                            .truncationMode(.middle)
                    }
                    Spacer(minLength: 4)
                    if isSwitching {
                        ProgressView().controlSize(.small)
                    } else if isCurrent {
                        Text("当前")
                            .font(DS.fontBadge)
                            .foregroundStyle(DS.accent)
                            .padding(.horizontal, 7)
                            .padding(.vertical, 2)
                            .background(DS.accentBg)
                            .overlay(Capsule().strokeBorder(DS.accentBorder, lineWidth: 1))
                            .clipShape(Capsule())
                    }
                }
                .padding(.horizontal, 9)
                .padding(.vertical, 7)
                .background(isCurrent ? DS.accentBg : (hovered && !disabled ? DS.hover : DS.clear))
                .overlay(
                    RoundedRectangle(cornerRadius: 7)
                        .strokeBorder(isCurrent ? DS.accentBorder : (hovered && !disabled ? DS.line : DS.clear), lineWidth: 1)
                )
                .clipShape(RoundedRectangle(cornerRadius: 7))
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .disabled(disabled)
            .onHover { hovered = $0 }
        }
    }
}
