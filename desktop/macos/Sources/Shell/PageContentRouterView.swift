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
                case .accounts:
                    AccountsPageView(store: store)
                case .groups:
                    GroupsPageView(store: store)
                case .priorities:
                    PrioritiesPageView(store: store)
                case .records:
                    RecordsPageView(store: store)
                case .events:
                    EventsPageView(store: store)
                case .features:
                    FeaturesPageView(store: store)
                case .settings:
                    SettingsPageView(store: store)
                }
            }
            .padding(.horizontal, 24)
            .padding(.vertical, 22)
            .frame(maxWidth: .infinity, alignment: .topLeading)
        }
    }
}
