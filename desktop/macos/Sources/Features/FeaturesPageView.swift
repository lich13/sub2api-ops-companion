import SwiftUI

/// Matches `SettingsPage(page: "features")` 4-tab container in `desktop/src/main.tsx`.
public struct FeaturesPageView: View {
    @Bindable public var store: AppStore

    public enum Tab: String, CaseIterable, Sendable {
        case recovery
        case models
        case ip
        case sub2api

        public var label: String {
            switch self {
            case .recovery: return "恢复与检测"
            case .models: return "模型配置"
            case .ip: return "IP 管理"
            case .sub2api: return "Sub2API 配置"
            }
        }
    }

    @State private var selectedTab: Tab
    @State private var visitedTabs: Set<Tab>

    public init(store: AppStore, initialTab: Tab = .recovery) {
        self.store = store
        self._selectedTab = State(initialValue: initialTab)
        self._visitedTabs = State(initialValue: [initialTab])
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(spacing: 4) {
                ForEach(Tab.allCases, id: \.rawValue) { tab in
                    Button(tab.label) {
                        selectedTab = tab
                        visitedTabs.insert(tab)
                    }
                    .buttonStyle(LinearButtonStyle(variant: selectedTab == tab ? .selectedGhost : .ghost, compact: true))
                }
                Spacer()
            }

            ZStack(alignment: .topLeading) {
                if visitedTabs.contains(.recovery) {
                    tabPane(.recovery) {
                        RecoveryDetectionTabView(store: store)
                    }
                }
                if visitedTabs.contains(.models) {
                    tabPane(.models) {
                        ModelConfigTabView(online: store.state.online, connectionKey: store.connectionKey)
                    }
                }
                if visitedTabs.contains(.ip) {
                    tabPane(.ip) {
                        ProxyManagementTabView(
                            active: selectedTab == .ip,
                            online: store.state.online,
                            connectionKey: store.connectionKey
                        )
                    }
                }
                if visitedTabs.contains(.sub2api) {
                    tabPane(.sub2api) {
                        Sub2ApiConfigTabView(
                            active: selectedTab == .sub2api && store.state.foreground != false,
                            online: store.state.online,
                            connectionKey: store.connectionKey
                        )
                    }
                }
            }
        }
        .task(id: "\(store.connectionKey):\(store.state.online)") {
            if store.state.online && store.config == nil {
                await store.reloadConfig()
            }
        }
    }

    private func tabPane<Content: View>(_ tab: Tab, @ViewBuilder content: () -> Content) -> some View {
        let isVisible = selectedTab == tab
        return content()
            .opacity(isVisible ? 1 : 0)
            .allowsHitTesting(isVisible)
            .frame(maxWidth: .infinity, maxHeight: isVisible ? nil : 0, alignment: .topLeading)
            .clipped()
            .accessibilityHidden(!isVisible)
    }
}
