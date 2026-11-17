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

    @State private var selectedTab: Tab
    @State private var degradationFeed = ErrorFeedStore(category: .degradation)
    @State private var otherFeed = ErrorFeedStore(category: .other)

    public init(store: AppStore, initialTab: Tab = .degradation) {
        self.store = store
        self._selectedTab = State(initialValue: initialTab)
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(spacing: 4) {
                ForEach(Tab.allCases, id: \.rawValue) { tab in
                    Button(tab.label) {
                        selectedTab = tab
                    }
                    .buttonStyle(LinearButtonStyle(variant: selectedTab == tab ? .selectedGhost : .ghost, compact: true))
                }
                Spacer()
            }

            switch selectedTab {
            case .degradation:
                errorCategoryPanel(feed: degradationFeed, category: "degradation", emptyLabel: "暂无降智相关错误")
            case .other:
                errorCategoryPanel(feed: otherFeed, category: "other", emptyLabel: "暂无其他上游错误")
            case .recoveries:
                RecoveryHistoryView(
                    online: store.state.online,
                    connectionKey: store.connectionKey,
                    initialItems: store.state.snapshot?.recoveries ?? []
                )
            case .logs:
                DetectionLogsTableView(
                    online: store.state.online,
                    connectionKey: store.connectionKey
                )
            }
        }
        .task(id: "\(store.connectionKey):\(selectedTab.rawValue):\(store.state.online)") {
            degradationFeed.bind(connectionKey: store.connectionKey)
            otherFeed.bind(connectionKey: store.connectionKey)
            switch selectedTab {
            case .degradation:
                await degradationFeed.load(replace: true, online: store.state.online)
            case .other:
                await otherFeed.load(replace: true, online: store.state.online)
            case .recoveries, .logs:
                break
            }
        }
    }

    private func errorCategoryPanel(feed: ErrorFeedStore, category: String, emptyLabel: String) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text("已读取 \(feed.items.count) 条错误记录")
                    .font(DS.fontCompactMedium)
                    .monospacedDigit()
                    .foregroundStyle(DS.muted)
                Spacer()
                Button("刷新列表") {
                    Task { await feed.load(replace: true, online: store.state.online) }
                }
                .buttonStyle(LinearButtonStyle(variant: .ghost, compact: true))
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
                    .padding(.vertical, 18)
            } else {
                VStack(spacing: 8) {
                    ForEach(feed.items) { err in
                        Button {
                            store.openErrorDetail(id: err.id, category: category)
                        } label: {
                            HStack(alignment: .top, spacing: 10) {
                                Text("\(err.status_code)")
                                    .font(DS.fontBadge)
                                    .monospacedDigit()
                                    .foregroundStyle(DS.bad)
                                    .padding(.horizontal, 6)
                                    .padding(.vertical, 2)
                                    .background(DS.badBg)
                                    .clipShape(RoundedRectangle(cornerRadius: 4))

                                VStack(alignment: .leading, spacing: 3) {
                                    HStack(spacing: 6) {
                                        Text(err.account_name.isEmpty ? "账号 #\(err.account_id ?? 0)" : err.account_name)
                                            .font(DS.fontBodySemibold)
                                            .foregroundStyle(DS.ink)
                                        if let aid = err.account_id {
                                            Text("#\(aid)")
                                                .font(DS.fontMono11)
                                                .foregroundStyle(DS.muted)
                                        }
                                        if !err.model.isEmpty {
                                            Text(verbatim: err.model)
                                                .font(DS.fontMono11)
                                                .foregroundStyle(DS.muted)
                                        }
                                        Spacer()
                                        Text(BeijingTime.fullTime(err.created_at))
                                            .font(DS.fontResetTime)
                                            .monospacedDigit()
                                            .foregroundStyle(DS.muted)
                                    }
                                    Text(err.message.isEmpty ? err.provider_error_code : err.message)
                                        .font(DS.fontCompact)
                                        .foregroundStyle(DS.muted)
                                        .lineLimit(2)
                                        .multilineTextAlignment(.leading)
                                }
                            }
                            .padding(12)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .background(DS.surface)
                            .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                            .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
                        }
                        .buttonStyle(.plain)
                    }
                }
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
}
