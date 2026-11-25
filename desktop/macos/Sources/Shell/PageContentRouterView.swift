import SwiftUI

public struct PageContentRouterView: View {
    @Bindable public var store: AppStore
    public var compact: Bool
    public var mainWidth: CGFloat?
    @State private var visitedPages: Set<AppPage>
    @State private var accountsScrollWidth: CGFloat? = nil

    public init(store: AppStore, compact: Bool = false, mainWidth: CGFloat? = nil) {
        self.store = store
        self.compact = compact
        self.mainWidth = mainWidth
        self._visitedPages = State(initialValue: [store.page])
    }

    /// Pure helper for retained-tab lifecycle tests (`visitedPages` reset on connection switch).
    public static func updatedVisitedPages(
        current: Set<AppPage>,
        activePage: AppPage,
        connectionChanged: Bool = false
    ) -> Set<AppPage> {
        if connectionChanged {
            return [activePage]
        }
        var next = current
        next.insert(activePage)
        return next
    }

    public var body: some View {
        let hPad: CGFloat = compact ? 12 : 20
        let contentWidth: CGFloat? = mainWidth.map { max(320, $0 - hPad * 2) }
        let retainGroups = store.page == .groups || visitedPages.contains(.groups)
        let retainPriorities = store.page == .priorities || visitedPages.contains(.priorities)

        ZStack(alignment: .topLeading) {
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
                let effectiveScrollWidth = accountsScrollWidth.map {
                    if let mainWidth { return min($0, mainWidth) }
                    return $0
                }
                let effectiveAccountsWidth = effectiveScrollWidth.map { max(320, $0 - hPad * 2) } ?? contentWidth
                ScrollView {
                    VStack(alignment: .leading, spacing: 16) {
                        accountsHeading
                        AccountsPageView(store: store, viewportWidth: effectiveAccountsWidth)
                    }
                    .padding(.horizontal, hPad)
                    .padding(.vertical, 18)
                    .frame(width: effectiveScrollWidth, alignment: .topLeading)
                    .frame(maxWidth: effectiveScrollWidth == nil ? .infinity : nil, alignment: .topLeading)
                    .background(
                        LinearWidthObserver(preferScrollContentWidth: true) { width in
                            if abs(width - (accountsScrollWidth ?? -1)) >= 0.5 {
                                accountsScrollWidth = width
                            }
                        }
                    )
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)

            case .groups, .priorities:
                EmptyView()

            case .settings:
                ScrollView {
                    SettingsPageView(store: store)
                        .padding(.horizontal, hPad)
                        .padding(.vertical, 18)
                        .frame(maxWidth: .infinity, alignment: .topLeading)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            }

            if retainGroups {
                LinearRetainedTabSlot(isActive: store.page == .groups) {
                    ScrollView {
                        GroupsPageView(store: store)
                            .padding(.horizontal, hPad)
                            .padding(.vertical, 18)
                            .frame(maxWidth: .infinity, alignment: .topLeading)
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
                }
                .id("groups:\(store.connectionKey)")
            }

            if retainPriorities {
                LinearRetainedTabSlot(isActive: store.page == .priorities) {
                    PrioritiesPageView(store: store)
                        .padding(.horizontal, hPad)
                        .padding(.top, 18)
                        .padding(.bottom, 16)
                        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
                }
                .id("priorities:\(store.connectionKey)")
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .onChange(of: store.page) { _, next in
            visitedPages = Self.updatedVisitedPages(current: visitedPages, activePage: next)
        }
        .onChange(of: store.connectionKey) { _, _ in
            visitedPages = Self.updatedVisitedPages(
                current: visitedPages,
                activePage: store.page,
                connectionChanged: true
            )
        }
    }

    private var accountsHeading: some View {
        HStack(alignment: .lastTextBaseline) {
            Text(store.page.label)
                .font(DS.fontPageTitle)
                .foregroundStyle(DS.ink)
            Spacer()
            ObservedHeaderLabel(clock: store.observedClock, spacing: 4)
        }
    }
}
