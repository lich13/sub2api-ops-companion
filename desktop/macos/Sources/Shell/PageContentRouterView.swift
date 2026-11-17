import SwiftUI

public struct PageContentRouterView: View {
    @Bindable public var store: AppStore

    public init(store: AppStore) {
        self.store = store
    }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                HStack(alignment: .lastTextBaseline) {
                    Text(store.page.label)
                        .font(DS.fontPageTitle)
                        .foregroundStyle(DS.ink)
                    Spacer()
                    if store.page != .records, let observedAt = store.state.snapshot?.observed_at {
                        HStack(spacing: 4) {
                            Image(systemName: "clock")
                                .font(DS.iconFont(11))
                            Text("更新于 \(BeijingTime.fullTime(observedAt))")
                                .monospacedDigit()
                        }
                        .font(DS.fontPriorityMeta)
                        .foregroundStyle(DS.muted)
                    }
                }

                switch store.page {
                case .settings:
                    VStack(alignment: .leading, spacing: 16) {
                        ConnectionStatusCardView(connectionKey: store.connectionKey)
                        CardSection {
                            ConnectionOnboardingView(store: store, embeddedInSettings: true)
                        }
                    }
                    .frame(maxWidth: 820)
                default:
                    CardSection {
                        HStack(spacing: 8) {
                            StatusDot(store.state.online ? .good : .warning)
                            Text("已连接 \(store.accounts.count) 个账号 · \(store.groups.count) 个分组")
                                .font(DS.fontBody)
                                .monospacedDigit()
                                .foregroundStyle(DS.ink)
                        }
                    }
                }
            }
            .padding(.horizontal, 24)
            .padding(.vertical, 22)
            .frame(maxWidth: .infinity, alignment: .topLeading)
        }
    }
}
