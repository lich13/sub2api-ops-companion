import SwiftUI

public struct PageContentRouterView: View {
    @Bindable public var store: AppStore
    public var compact: Bool
    public var mainWidth: CGFloat?

    public init(store: AppStore, compact: Bool = false, mainWidth: CGFloat? = nil) {
        self.store = store
        self.compact = compact
        self.mainWidth = mainWidth
    }

    public var body: some View {
        let hPad: CGFloat = compact ? 12 : 20
        let contentWidth: CGFloat? = mainWidth.map { max(320, $0 - hPad * 2) }
        switch store.page {
        case .records:
            RecordsPageView(store: store)
                .padding(.horizontal, hPad)
                .padding(.top, 18)
                .padding(.bottom, 16)
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)

        case .events:
            EventsPageView(store: store, padContainer: true, horizontalPadding: hPad)
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)

        case .features:
            FeaturesPageView(store: store, padContainer: true, horizontalPadding: hPad)
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)

        case .accounts:
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    accountsHeading
                    AccountsPageView(store: store, viewportWidth: contentWidth)
                }
                .padding(.horizontal, hPad)
                .padding(.vertical, 18)
                .frame(maxWidth: .infinity, alignment: .topLeading)
            }

        case .groups:
            ScrollView {
                GroupsPageView(store: store)
                    .padding(.horizontal, hPad)
                    .padding(.vertical, 18)
                    .frame(maxWidth: .infinity, alignment: .topLeading)
            }

        case .priorities:
            PrioritiesPageView(store: store)
                .padding(.horizontal, hPad)
                .padding(.top, 18)
                .padding(.bottom, 16)
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)

        case .settings:
            ScrollView {
                SettingsPageView(store: store)
                    .padding(.horizontal, hPad)
                    .padding(.vertical, 18)
                    .frame(maxWidth: .infinity, alignment: .topLeading)
            }
        }
    }

    private var accountsHeading: some View {
        HStack(alignment: .lastTextBaseline) {
            Text(store.page.label)
                .font(DS.fontPageTitle)
                .foregroundStyle(DS.ink)
            Spacer()
            if let observedAt = store.state.snapshot?.observed_at, !observedAt.isEmpty {
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
    }
}
