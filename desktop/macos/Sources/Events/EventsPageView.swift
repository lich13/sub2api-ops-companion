import SwiftUI

public struct EventsPageView: View {
    @Bindable public var store: AppStore

    public enum Tab: String, CaseIterable, Sendable {
        case degradation
        case other
        case recoveries
        case logs

        public var label: String {
            switch self {
            case .degradation: return "降智错误"
            case .other: return "其他错误"
            case .recoveries: return "恢复历史"
            case .logs: return "日志"
            }
        }
    }

    public var padContainer: Bool
    public var horizontalPadding: CGFloat
    @State private var selectedTab: Tab
    @State private var visitedTabs: Set<Tab>
    @State private var loadedErrorCategories: Set<Tab> = []
    @State private var lastLoadedScope: String = ""
    @State private var degradationFeed = ErrorFeedStore(category: .degradation)
    @State private var otherFeed = ErrorFeedStore(category: .other)

    public init(store: AppStore, initialTab: Tab = .degradation, padContainer: Bool = false, horizontalPadding: CGFloat = 24) {
        self.store = store
        self.padContainer = padContainer
        self.horizontalPadding = horizontalPadding
        self._selectedTab = State(initialValue: initialTab)
        self._visitedTabs = State(initialValue: [initialTab])
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            VStack(alignment: .leading, spacing: 14) {
                HStack(alignment: .firstTextBaseline, spacing: 12) {
                    Text("事件")
                        .font(DS.fontPageTitle)
                        .foregroundStyle(DS.ink)

                    Spacer()

                    ObservedHeaderLabel(clock: store.observedClock)
                }

                LinearUnderlineTabs(
                    items: Tab.allCases.map { LinearTabItem(id: $0, label: $0.label) },
                    selection: selectedTab,
                    onSelect: { next in
                        visitedTabs.insert(next)
                        selectedTab = next
                    }
                )
            }
            .padding(.horizontal, padContainer ? horizontalPadding : 0)
            .padding(.top, padContainer ? 18 : 0)

            ZStack(alignment: .topLeading) {
                if visitedTabs.contains(.degradation) {
                    LinearRetainedTabSlot(isActive: selectedTab == .degradation) {
                        scrollablePanel {
                            errorCategoryPanel(feed: degradationFeed, category: "degradation", emptyLabel: "暂无降智相关错误")
                        }
                    }
                }
                if visitedTabs.contains(.other) {
                    LinearRetainedTabSlot(isActive: selectedTab == .other) {
                        scrollablePanel {
                            errorCategoryPanel(feed: otherFeed, category: "other", emptyLabel: "暂无其他上游错误")
                        }
                    }
                }
                if visitedTabs.contains(.recoveries) {
                    LinearRetainedTabSlot(isActive: selectedTab == .recoveries) {
                        scrollablePanel {
                            RecoveryHistoryView(
                                online: store.state.online,
                                connectionKey: store.connectionKey,
                                initialItems: store.state.snapshot?.recoveries ?? []
                            )
                        }
                    }
                }
                if visitedTabs.contains(.logs) {
                    LinearRetainedTabSlot(isActive: selectedTab == .logs) {
                        scrollablePanel {
                            DetectionLogsTableView(
                                online: store.state.online,
                                connectionKey: store.connectionKey,
                                onSelectLog: { store.selectedDetectionLog = $0 }
                            )
                        }
                    }
                }
            }
            .frame(maxWidth: .infinity, maxHeight: padContainer ? .infinity : nil, alignment: .topLeading)
        }
        .frame(maxWidth: .infinity, maxHeight: padContainer ? .infinity : nil, alignment: .topLeading)
        .task(id: "\(store.connectionKey):\(selectedTab.rawValue):\(store.state.online)") {
            let scope = "\(store.connectionKey):\(store.state.online)"
            if scope != lastLoadedScope {
                lastLoadedScope = scope
                loadedErrorCategories.removeAll()
            }
            degradationFeed.bind(connectionKey: store.connectionKey)
            otherFeed.bind(connectionKey: store.connectionKey)
            switch selectedTab {
            case .degradation:
                guard !loadedErrorCategories.contains(.degradation) else { return }
                loadedErrorCategories.insert(.degradation)
                await degradationFeed.load(replace: true, online: store.state.online)
            case .other:
                guard !loadedErrorCategories.contains(.other) else { return }
                loadedErrorCategories.insert(.other)
                await otherFeed.load(replace: true, online: store.state.online)
            case .recoveries, .logs:
                break
            }
        }
    }

    @ViewBuilder
    private func scrollablePanel<Content: View>(@ViewBuilder _ content: () -> Content) -> some View {
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

    private func errorCategoryPanel(feed: ErrorFeedStore, category: String, emptyLabel: String) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text("\(feed.items.count) 条记录")
                    .font(DS.fontCompact)
                    .monospacedDigit()
                    .foregroundStyle(DS.muted)
                Spacer()
                Button("刷新记录") {
                    Task { await feed.load(replace: true, online: store.state.online) }
                }
                .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                .disabled(!store.state.online || feed.loading)
            }

            if !feed.error.isEmpty {
                HStack(spacing: 8) {
                    Text(feed.error)
                        .font(DS.fontCompact)
                        .foregroundStyle(DS.bad)
                    Button("重试") {
                        Task { await feed.load(replace: true, online: store.state.online) }
                    }
                    .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                }
            }

            if feed.items.isEmpty {
                Text(feed.loading ? "正在读取错误列表…" : emptyLabel)
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.muted)
                    .frame(maxWidth: .infinity, alignment: .center)
                    .padding(.vertical, 28)
                    .background(DS.surface)
                    .overlay(RoundedRectangle(cornerRadius: DS.cardRadius).strokeBorder(DS.line, lineWidth: 1))
                    .clipShape(RoundedRectangle(cornerRadius: DS.cardRadius))
            } else {
                VStack(spacing: 0) {
                    ForEach(Array(feed.items.enumerated()), id: \.element.recordId) { idx, err in
                        errorRow(err, category: category)
                        if idx < feed.items.count - 1 {
                            Rectangle().fill(DS.line).frame(height: 1)
                        }
                    }
                }
                .background(DS.surface)
                .overlay(RoundedRectangle(cornerRadius: DS.cardRadius).strokeBorder(DS.line, lineWidth: 1))
                .clipShape(RoundedRectangle(cornerRadius: DS.cardRadius))
            }

            if feed.cursor != nil {
                Button(feed.loading ? "加载中…" : "加载更早错误") {
                    Task { await feed.load(replace: false, online: store.state.online) }
                }
                .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                .disabled(!store.state.online || feed.loading)
            }
        }
    }

    private func errorRow(_ err: OpsError, category: String) -> some View {
        let isWarnCode = err.statusCodeValue == 429 || err.statusCodeValue == 529
        let detailSummary: String = {
            var parts: [String] = []
            if !err.modelText.isEmpty { parts.append(err.modelText) }
            let msg = err.providerErrorCodeText.isEmpty ? err.messageText : err.providerErrorCodeText
            if !msg.isEmpty { parts.append(msg) }
            return parts.isEmpty ? err.messageText : parts.joined(separator: " · ")
        }()

        return Button {
            if let errId = err.id {
                store.openErrorDetail(id: errId, category: category)
            }
        } label: {
            HStack(alignment: .center, spacing: 14) {
                Text("\(err.statusCodeValue)")
                    .font(DS.fontCompactSemibold)
                    .monospacedDigit()
                    .foregroundStyle(isWarnCode ? DS.warn : DS.bad)
                    .frame(minWidth: 38, minHeight: 30)
                    .padding(.horizontal, 4)
                    .background(isWarnCode ? DS.warnBg : DS.badBg)
                    .clipShape(RoundedRectangle(cornerRadius: 6))

                VStack(alignment: .leading, spacing: 4) {
                    Text(err.accountNameText.isEmpty ? "账号 #\(err.account_id ?? 0)" : err.accountNameText)
                        .font(DS.fontBodySemibold)
                        .foregroundStyle(DS.ink)
                        .lineLimit(1)

                    HStack(spacing: 6) {
                        Text(err.isResolved ? "历史" : "当前")
                            .font(DS.fontResetTimeSemibold)
                            .foregroundStyle(err.isResolved ? DS.muted : DS.warn)

                        Text(verbatim: detailSummary)
                            .font(DS.fontCompact)
                            .foregroundStyle(DS.muted)
                            .lineLimit(1)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)

                HStack(spacing: 8) {
                    Text(BeijingTime.fullTime(err.created_at))
                        .font(DS.fontCompact)
                        .monospacedDigit()
                        .foregroundStyle(DS.muted)
                    Image(systemName: "chevron.right")
                        .font(DS.iconFont(11, weight: .semibold))
                        .foregroundStyle(DS.muted)
                }
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 14)
            .contentShape(Rectangle())
        }
        .buttonStyle(.linearPlain)
    }
}
