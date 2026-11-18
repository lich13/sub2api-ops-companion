import SwiftUI

/// Matches `SettingsPage(page: "features")` 3-tab container in `desktop/src/main.tsx`.
public struct FeaturesPageView: View {
    @Bindable public var store: AppStore

    public enum Tab: String, CaseIterable, Sendable {
        case recovery
        case ip
        case sub2api

        public var label: String {
            switch self {
            case .recovery: return "恢复与检测"
            case .ip: return "IP 管理"
            case .sub2api: return "Sub2API 配置"
            }
        }
    }

    public var padContainer: Bool
    public var horizontalPadding: CGFloat
    @State private var selectedTab: Tab
    @State private var visitedTabs: Set<Tab>

    public init(store: AppStore, initialTab: Tab = .recovery, padContainer: Bool = false, horizontalPadding: CGFloat = 24) {
        self.store = store
        self.padContainer = padContainer
        self.horizontalPadding = horizontalPadding
        self._selectedTab = State(initialValue: initialTab)
        self._visitedTabs = State(initialValue: [initialTab])
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: selectedTab == .sub2api ? 12 : 16) {
            if selectedTab != .sub2api {
                HStack(alignment: .firstTextBaseline, spacing: 12) {
                    Text("功能")
                        .font(DS.fontPageTitle)
                        .foregroundStyle(DS.ink)

                    Spacer()

                    if let observed = store.state.snapshot?.observed_at, !observed.isEmpty {
                        HStack(spacing: 5) {
                            Image(systemName: "clock")
                                .font(DS.iconFont(11))
                            Text("更新于 \(BeijingTime.fullTime(observed))")
                                .font(DS.fontPriorityMeta)
                                .monospacedDigit()
                        }
                        .foregroundStyle(DS.muted)
                    }
                }
                .padding(.horizontal, padContainer ? horizontalPadding : 0)
                .padding(.top, padContainer ? 18 : 0)
            }

            tabBar
                .padding(.horizontal, padContainer ? horizontalPadding : 0)
                .padding(.top, (selectedTab == .sub2api && padContainer) ? 10 : 0)

            ZStack(alignment: .topLeading) {
                if visitedTabs.contains(.recovery) {
                    LinearRetainedTabSlot(isActive: selectedTab == .recovery) {
                        scrollableTabBody {
                            RecoveryDetectionTabView(store: store)
                        }
                    }
                }
                if visitedTabs.contains(.ip) {
                    LinearRetainedTabSlot(isActive: selectedTab == .ip) {
                        scrollableTabBody {
                            ProxyManagementTabView(
                                store: store,
                                active: selectedTab == .ip,
                                online: store.state.online,
                                connectionKey: store.connectionKey
                            )
                        }
                    }
                }
                if visitedTabs.contains(.sub2api) {
                    LinearRetainedTabSlot(isActive: selectedTab == .sub2api) {
                        Sub2ApiConfigTabView(
                            store: store,
                            active: selectedTab == .sub2api && store.state.foreground != false,
                            online: store.state.online,
                            connectionKey: store.connectionKey
                        )
                        .padding(.horizontal, padContainer ? horizontalPadding : 0)
                        .padding(.bottom, padContainer ? 16 : 0)
                        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
                    }
                }
            }
            .frame(maxWidth: .infinity, maxHeight: padContainer ? .infinity : nil, alignment: .topLeading)
        }
        .frame(maxWidth: .infinity, maxHeight: padContainer ? .infinity : nil, alignment: .topLeading)
        .task(id: "\(store.connectionKey):\(store.state.online)") {
            if store.state.online && store.config == nil {
                await store.reloadConfig()
            }
        }
    }

    @ViewBuilder
    private func scrollableTabBody<Content: View>(@ViewBuilder _ content: () -> Content) -> some View {
        let inner = content()
            .padding(.horizontal, padContainer ? horizontalPadding : 0)
            .padding(.bottom, padContainer ? 28 : 0)
            .frame(maxWidth: .infinity, alignment: .leading)
        if padContainer {
            ScrollView { inner }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            inner
        }
    }

    private var tabBar: some View {
        LinearUnderlineTabs(
            items: Tab.allCases.map { LinearTabItem(id: $0, label: $0.label) },
            selection: selectedTab,
            onSelect: { next in
                store.dismissWindowModal()
                visitedTabs.insert(next)
                selectedTab = next
            }
        )
    }
}
