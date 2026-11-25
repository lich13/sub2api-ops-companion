import SwiftUI

public struct AccountsPageView: View {
    @Bindable public var store: AppStore
    public var viewportWidth: CGFloat?

    @State private var sortStore = AccountSortStore()
    @State private var nativeUsage = NativeUsageReader()
    @State private var query: String = ""
    @State private var groupFilter: String = ""
    @State private var platformFilter: String = ""
    @State private var typeFilter: String = ""
    @State private var statusFilter: String = ""
    @State private var selectedIds: Set<Int> = []
    @State private var nameExpanded: Bool = UserDefaults.standard.bool(forKey: "sub2ops-account-name-expanded")
    @State private var quotaJob: QuotaRefreshHeader? = nil
    @State private var connectionBatch: OperationBatch? = nil
    @State private var batchBusy: Bool = false
    @State private var measuredWidth: CGFloat = 1085
    @State private var filterCache = AccountFilterCache()

    public init(store: AppStore, viewportWidth: CGFloat? = nil) {
        self.store = store
        self.viewportWidth = viewportWidth
    }

    private var availableWidth: CGFloat {
        viewportWidth ?? measuredWidth
    }

    private var filteredSortedAccounts: [Account] {
        filterCache.resolve(
            accounts: store.accounts,
            query: query,
            group: groupFilter,
            platform: platformFilter,
            status: statusFilter,
            type: typeFilter,
            sortStore: sortStore
        )
    }

    private var availablePlatforms: [String] {
        PlatformMeta.sortPlatforms(store.accounts.map(\.platform))
    }

    private var availableTypes: [String] {
        Array(Set(store.accounts.map(\.type))).sorted()
    }

    private var selectedAccounts: [Account] {
        filteredSortedAccounts.filter { selectedIds.contains($0.id) }
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            primaryActionBar
            filterToolbar

            if !selectedAccounts.isEmpty {
                BatchToolbarView(
                    selectedAccounts: selectedAccounts,
                    online: store.state.online,
                    busy: batchBusy,
                    onBatchTest: { Task { await startBatchConnectionTest(selectedAccounts) } },
                    onBatchRecover: { Task { await runBatchRecover(selectedAccounts.filter { $0.recoverable == true }) } },
                    onBatchCredits: { store.accountModal = .batchCredits(selectedAccounts) },
                    onBatchProxy: { store.accountModal = .batchProxy(selectedAccounts) },
                    onBatchDelete: { store.accountModal = .deleteAccounts(selectedAccounts) },
                    onClear: { selectedIds.removeAll() }
                )
            }

            ConnectionBatchBanner(
                batch: connectionBatch,
                onOpenBatchDetails: { batchId in
                    store.operations.batchId = batchId
                    store.operations.isOpen = true
                    Task { await store.operations.pollOnce() }
                },
                onDismiss: { connectionBatch = nil }
            )

            QuotaRefreshBarView(job: quotaJob)

            accountsTable
        }
        .background(LinearWidthObserver { if viewportWidth == nil { measuredWidth = $0 } })
        .onChange(of: "\(query)|\(groupFilter)|\(platformFilter)|\(typeFilter)|\(statusFilter)") { _, _ in
            selectedIds.removeAll()
        }
        .task(id: "\(store.connectionKey):\(store.state.snapshot_revision ?? 0):\(store.state.online)") {
            nativeUsage.bindConnection(store.connectionKey)
            nativeUsage.enqueueIfNeeded(
                accounts: store.accounts,
                accountById: store.accountById,
                online: store.state.online
            )
            let liveIds = Set(store.accounts.map(\.id))
            selectedIds = selectedIds.intersection(liveIds)
        }
    }

    private var primaryActionBar: some View {
        HStack(spacing: 8) {
            Button {
                store.accountModal = .addAccount
            } label: {
                HStack(spacing: 5) {
                    Image(systemName: "plus")
                        .font(DS.iconFont(12, weight: .semibold))
                    Text("添加账号")
                }
            }
            .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
            .disabled(!store.state.online)

            if store.accounts.contains(where: { $0.type == "apikey" }) {
                Button {
                    store.accountModal = .keyQuotaSettings(nil)
                } label: {
                    HStack(spacing: 5) {
                        Image(systemName: "slider.horizontal.3")
                            .font(DS.iconFont(12))
                        Text("余额刷新")
                    }
                }
                .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                .disabled(!store.state.online)
            }

            Button {
                Task { await triggerQuotaRefresh() }
            } label: {
                HStack(spacing: 5) {
                    Image(systemName: "arrow.clockwise")
                        .font(DS.iconFont(12))
                    Text("刷新全部用量")
                }
            }
            .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
            .disabled(!store.state.online || quotaJob?.status == "running")

            Spacer(minLength: 12)

            HStack(spacing: 8) {
                Text("排序")
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.muted)

                let sortLabel: String = {
                    switch (sortStore.option.key, sortStore.option.ascending) {
                    case (.recent, false): return "最近调用"
                    case (.recent, true): return "最早调用"
                    case (.priority, true): return "优先级 ↑"
                    case (.priority, false): return "优先级 ↓"
                    default: return "\(sortStore.option.key.label) \(sortStore.option.ascending ? "↑" : "↓")"
                    }
                }()

                LinearSelect(
                    label: sortLabel,
                    accessibilityTitle: "账号排序",
                    height: DS.controlHeightCompact,
                    compactFont: true,
                    entries: [
                        LinearMenuEntry(
                            id: "recent:desc",
                            title: "最近调用",
                            checked: sortStore.option == AccountSortOption(key: .recent, ascending: false)
                        ) {
                            sortStore.option = AccountSortOption(key: .recent, ascending: false)
                        },
                        LinearMenuEntry(
                            id: "recent:asc",
                            title: "最早调用",
                            checked: sortStore.option == AccountSortOption(key: .recent, ascending: true)
                        ) {
                            sortStore.option = AccountSortOption(key: .recent, ascending: true)
                        },
                        LinearMenuEntry(
                            id: "priority:asc",
                            title: "优先级 ↑",
                            checked: sortStore.option == AccountSortOption(key: .priority, ascending: true)
                        ) {
                            sortStore.option = AccountSortOption(key: .priority, ascending: true)
                        },
                        LinearMenuEntry(
                            id: "priority:desc",
                            title: "优先级 ↓",
                            checked: sortStore.option == AccountSortOption(key: .priority, ascending: false)
                        ) {
                            sortStore.option = AccountSortOption(key: .priority, ascending: false)
                        },
                    ]
                )
            }

            Button("账号模板") {
                store.accountModal = .templates(nil)
            }
            .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
            .disabled(!store.state.online)
        }
    }

    private var filterToolbar: some View {
        HStack(spacing: 8) {
            LinearSearchField(
                text: $query,
                prompt: "搜索名称或账号 ID",
                height: DS.controlHeightCompact,
                compactFont: true
            )

            LinearSelect(
                selection: $groupFilter,
                options: [("", "全部分组")] + store.groups.map { ("\($0.id)", $0.name) },
                accessibilityTitle: "分组筛选",
                height: DS.controlHeightCompact,
                compactFont: true
            )

            LinearSelect(
                selection: $platformFilter,
                options: [("", "全部平台")] + availablePlatforms.map { ($0, PlatformMeta.displayName($0)) },
                accessibilityTitle: "平台筛选",
                height: DS.controlHeightCompact,
                compactFont: true
            )

            LinearSelect(
                selection: $typeFilter,
                options: [("", "全部类型")] + availableTypes.map { ($0, PlatformMeta.typeLabel($0)) },
                accessibilityTitle: "认证类型筛选",
                height: DS.controlHeightCompact,
                compactFont: true
            )

            LinearSelect(
                selection: $statusFilter,
                options: [
                    ("", "全部状态"),
                    ("ready", "可调度"),
                    ("off", "调度关闭"),
                    ("error", "曾有错误"),
                ],
                accessibilityTitle: "状态筛选",
                height: DS.controlHeightCompact,
                compactFont: true
            )
        }
    }

    public struct TableColumnMetrics {
        // Mirrors responsive.css `.accounts-table` col widths (border-box, 7px cell padding):
        // select 30, name 128/240, priority 120, status 128, usage 262, error 122, schedule 72, actions auto (≥110).
        public let selectW: CGFloat = 16
        public let nameW: CGFloat
        public let paramsW: CGFloat = 106
        public let statusW: CGFloat = 114
        public let usageW: CGFloat = 248
        public let errorW: CGFloat = 108
        public let scheduleW: CGFloat = 58
        public let actionsW: CGFloat
        public let gap: CGFloat = 14
        public let horizontalPad: CGFloat = 7
        public var separatorInset: CGFloat { gap / 2 }
        public let totalWidth: CGFloat

        public init(containerWidth: CGFloat, nameExpanded: Bool) {
            let nameBase: CGFloat = nameExpanded ? 226 : 114
            let minActions: CGFloat = 96
            let fixedCols = selectW + nameBase + paramsW + statusW + usageW + errorW + scheduleW + (gap * 7) + (horizontalPad * 2)
            let minTotal = fixedCols + minActions
            self.nameW = nameBase
            if containerWidth >= minTotal {
                self.actionsW = minActions + (containerWidth - minTotal)
                self.totalWidth = containerWidth
            } else {
                self.actionsW = minActions
                self.totalWidth = minTotal
            }
        }
    }

    private var accountsTable: some View {
        let rows = filteredSortedAccounts
        let allSelected = !rows.isEmpty && rows.allSatisfy { selectedIds.contains($0.id) }
        let pendingByAccount = store.operations.pendingByAccount
        let metrics = TableColumnMetrics(containerWidth: max(320, availableWidth), nameExpanded: nameExpanded)
        let needsHorizontalScroll = metrics.totalWidth > availableWidth + 0.5

        let tableContent = VStack(spacing: 0) {
            HStack(spacing: metrics.gap) {
                LinearCheckbox(
                    isChecked: allSelected,
                    accessibilityTitle: "全选当前筛选结果",
                    disabled: rows.isEmpty
                ) {
                    if allSelected {
                        for r in rows { selectedIds.remove(r.id) }
                    } else {
                        for r in rows { selectedIds.insert(r.id) }
                    }
                }
                .frame(width: metrics.selectW, alignment: .center)

                Button {
                    nameExpanded.toggle()
                    UserDefaults.standard.set(nameExpanded, forKey: "sub2ops-account-name-expanded")
                } label: {
                    HStack(spacing: 4) {
                        Text("账号")
                            .font(DS.fontTableHeader)
                            .foregroundStyle(DS.muted)
                        Image(systemName: nameExpanded ? "chevron.left" : "chevron.right")
                            .font(DS.iconFont(9, weight: .semibold))
                            .foregroundStyle(DS.muted)
                    }
                    .frame(width: metrics.nameW, alignment: .leading)
                }
                .buttonStyle(.linearPlain)
                .help(nameExpanded ? "收起长账号名" : "展开完整账号名")

                Button {
                    sortStore.selectKey(.priority)
                } label: {
                    HStack(spacing: 3) {
                        Text("调度参数")
                        if sortStore.option.key == .priority {
                            Text(sortStore.option.ascending ? "↑" : "↓")
                        }
                    }
                    .font(DS.fontTableHeader)
                    .foregroundStyle(DS.muted)
                    .frame(width: metrics.paramsW, alignment: .leading)
                }
                .buttonStyle(.linearPlain)

                Text("当前状态")
                    .font(DS.fontTableHeader)
                    .foregroundStyle(DS.muted)
                    .frame(width: metrics.statusW, alignment: .leading)

                Text("用量窗口")
                    .font(DS.fontTableHeader)
                    .foregroundStyle(DS.muted)
                    .frame(width: metrics.usageW, alignment: .leading)

                Text("最近报错")
                    .font(DS.fontTableHeader)
                    .foregroundStyle(DS.muted)
                    .frame(width: metrics.errorW, alignment: .leading)

                Text("允许调度")
                    .font(DS.fontTableHeader)
                    .foregroundStyle(DS.muted)
                    .frame(width: metrics.scheduleW, alignment: .center)

                Text("操作")
                    .font(DS.fontTableHeader)
                    .foregroundStyle(DS.muted)
                    .frame(width: metrics.actionsW, height: 35, alignment: .leading)
            }
            .padding(.horizontal, metrics.horizontalPad)
            .frame(width: metrics.totalWidth, height: 35, alignment: .leading)
            .background(DS.surfaceSubtle)
            .overlay(alignment: .trailing) {
                HStack(spacing: 0) {
                    Rectangle().fill(DS.line).frame(width: 1)
                    DS.clear.frame(width: metrics.actionsW + metrics.horizontalPad + metrics.separatorInset - 1)
                }
            }

            Rectangle().fill(DS.line).frame(height: 1)

            if rows.isEmpty {
                Text(store.accounts.isEmpty ? "当前实例暂无账号" : "无匹配当前筛选条件的账号")
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.muted)
                    .frame(maxWidth: .infinity, alignment: .center)
                    .padding(.vertical, 36)
            } else {
                LazyVStack(spacing: 0) {
                    ForEach(Array(rows.enumerated()), id: \.element.id) { idx, account in
                        AccountTableRowView(
                            account: account,
                            checked: selectedIds.contains(account.id),
                            pendingCount: pendingByAccount[account.id]?.count ?? 0,
                            nameExpanded: nameExpanded,
                            metrics: metrics,
                            store: store,
                            nativeUsage: nativeUsage
                        ) {
                            if selectedIds.contains(account.id) {
                                selectedIds.remove(account.id)
                            } else {
                                selectedIds.insert(account.id)
                            }
                        }
                        if idx < rows.count - 1 {
                            Rectangle().fill(DS.line).frame(height: 1)
                        }
                    }
                }
            }
        }
        .frame(width: metrics.totalWidth)

        return SwiftUI.Group {
            if needsHorizontalScroll {
                ScrollView(.horizontal, showsIndicators: true) {
                    tableContent
                }
            } else {
                tableContent
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(DS.surface)
        .overlay(RoundedRectangle(cornerRadius: DS.cardRadius).strokeBorder(DS.line, lineWidth: 1))
        .clipShape(RoundedRectangle(cornerRadius: DS.cardRadius))
        .shadow(color: DS.shadowColor, radius: 2, x: 0, y: 1)
    }

    private func triggerQuotaRefresh() async {
        guard store.state.online else { return }
        nativeUsage.refreshAll(accounts: store.accounts, accountById: store.accountById)
        do {
            let started: QuotaRefreshHeader = try await RustBridge.shared.api("POST", "/quota-refresh", body: .object([:]))
            quotaJob = started
            while quotaJob?.status == "running" || quotaJob?.status == "queued" {
                try? await Task.sleep(for: .milliseconds(800))
                guard !Task.isCancelled, store.state.online else { return }
                let polled: QuotaRefreshHeader = try await RustBridge.shared.api("GET", "/quota-refresh")
                quotaJob = polled
            }
            store.refresh()
        } catch {
            store.report(error)
        }
    }

    private func startBatchConnectionTest(_ targets: [Account]) async {
        guard store.state.online, !targets.isEmpty, !batchBusy else { return }
        batchBusy = true
        defer { batchBusy = false }
        let items: [[String: JSONValue]] = targets.map { acc in
            [
                "account_id": .int(Int64(acc.id)),
                "expected_version": .string(acc.operation_versions["test"] ?? acc.version),
            ]
        }
        do {
            let initial = try await store.operations.submitBatch(
                action: "test",
                accounts: items,
                requestId: UUID().uuidString.lowercased()
            )
            connectionBatch = initial
            _ = try await store.operations.followBatch(initial: initial) { updated in
                connectionBatch = updated
            }
            store.refresh()
        } catch {
            store.report(error)
        }
    }

    private func runBatchRecover(_ targets: [Account]) async {
        guard store.state.online, !targets.isEmpty, !batchBusy else { return }
        batchBusy = true
        defer { batchBusy = false }
        let items: [[String: JSONValue]] = targets.map { acc in
            [
                "account_id": .int(Int64(acc.id)),
                "expected_version": .string(acc.operation_versions["recover"] ?? acc.version),
            ]
        }
        do {
            let initial = try await store.operations.submitBatch(
                action: "recover",
                accounts: items,
                requestId: UUID().uuidString.lowercased()
            )
            let final = try await store.operations.followBatch(initial: initial)
            let ok = final.items.filter { $0.status == "completed" }.count
            store.refresh()
            store.showToast("已恢复 \(ok)/\(final.items.count) 个账号状态")
        } catch {
            store.report(error)
        }
    }
}


@MainActor
private final class AccountFilterCache {
    private var lastAccounts: [Account] = []
    private var lastQuery: String = ""
    private var lastGroup: String = ""
    private var lastPlatform: String = ""
    private var lastStatus: String = ""
    private var lastType: String = ""
    private var lastSortOption: AccountSortOption?
    private var cachedResult: [Account] = []

    func resolve(
        accounts: [Account],
        query: String,
        group: String,
        platform: String,
        status: String,
        type: String,
        sortStore: AccountSortStore
    ) -> [Account] {
        let opt = sortStore.option
        if lastSortOption == opt,
           lastQuery == query,
           lastGroup == group,
           lastPlatform == platform,
           lastStatus == status,
           lastType == type,
           lastAccounts == accounts
        {
            return cachedResult
        }
        let filtered = AccountFilterSort.filter(
            accounts,
            query: query,
            group: group,
            platform: platform,
            status: status,
            type: type
        )
        let sorted = sortStore.sort(filtered)
        lastAccounts = accounts
        lastQuery = query
        lastGroup = group
        lastPlatform = platform
        lastStatus = status
        lastType = type
        lastSortOption = opt
        cachedResult = sorted
        return sorted
    }
}

private struct AccountTableRowView: View {
    let account: Account
    let checked: Bool
    let pendingCount: Int
    let nameExpanded: Bool
    let metrics: AccountsPageView.TableColumnMetrics
    @Bindable var store: AppStore
    let nativeUsage: NativeUsageReader
    let onToggleSelected: () -> Void

    @State private var isHovered: Bool = false

    var body: some View {
        HStack(alignment: .center, spacing: metrics.gap) {
            LinearCheckbox(
                isChecked: checked,
                accessibilityTitle: "选择 \(account.name)",
                onToggle: onToggleSelected
            )
            .frame(width: metrics.selectW, alignment: .center)

            VStack(alignment: .leading, spacing: 3) {
                Text(account.name)
                    .font(DS.fontCompactSemibold)
                    .foregroundStyle(DS.ink)
                    .lineLimit(nameExpanded ? 4 : 1)
                    .truncationMode(.tail)
                    .help(account.name)
                Text("#\(account.id)")
                    .font(DS.fontResetTime)
                    .monospacedDigit()
                    .foregroundStyle(DS.muted)
                Text("\(PlatformMeta.displayName(account.platform)) · \(PlatformMeta.typeLabel(account.type))")
                    .font(DS.fontResetTime)
                    .foregroundStyle(DS.muted)
                    .lineLimit(1)
                DegradationBadgeView(account: account)
                if pendingCount > 0 {
                    Button {
                        store.operations.isOpen = true
                    } label: {
                        Text("\(pendingCount) 项任务进行中")
                            .font(DS.fontResetTimeMedium)
                            .monospacedDigit()
                            .foregroundStyle(DS.accent)
                    }
                    .buttonStyle(.linearPlain)
                }
            }
            .frame(width: metrics.nameW, alignment: .leading)

            SchedulingParametersView(account: account, store: store)
                .frame(width: metrics.paramsW, alignment: .leading)

            AccountStatusView(account: account) { errId in
                store.openErrorDetail(id: errId)
            }
            .frame(width: metrics.statusW, alignment: .leading)

            UsageCellView(
                account: account,
                store: store,
                nativeUsage: nativeUsage,
                onOpenKeyQuotaSettings: { target in
                    store.accountModal = .keyQuotaSettings(target)
                }
            )
            .frame(width: metrics.usageW, alignment: .leading)

            errorCell
                .frame(width: metrics.errorW, alignment: .leading)

            LinearSwitch(
                isOn: account.schedulable,
                disabled: !store.state.online || store.busyScheduleAccountId == account.id,
                label: "\(account.name) 允许调度"
            ) {
                Task { await store.toggleSchedulable(account) }
            }
            .frame(width: metrics.scheduleW, alignment: .center)

            AccountActionsView(
                account: account,
                totalWidth: metrics.actionsW,
                disabled: !store.state.online
            ) { action in
                store.triggerAccountAction(action)
            }
            .frame(width: metrics.actionsW, alignment: .leading)
        }
        .padding(.horizontal, metrics.horizontalPad)
        .padding(.vertical, 10)
        .frame(width: metrics.totalWidth, alignment: .leading)
        .background(checked ? DS.accentBg.opacity(0.32) : (isHovered ? DS.rowHover : DS.surface))
        .overlay(alignment: .trailing) {
            HStack(spacing: 0) {
                Rectangle().fill(DS.line).frame(width: 1)
                DS.clear.frame(width: metrics.actionsW + metrics.horizontalPad + metrics.separatorInset - 1)
            }
        }
        .onHover { isHovered = $0 }
    }

    @ViewBuilder
    private var errorCell: some View {
        VStack(alignment: .leading, spacing: 4) {
            if let errId = account.last_error_id {
                Button {
                    store.openErrorDetail(id: errId)
                } label: {
                    VStack(alignment: .leading, spacing: 3) {
                        Text(verbatim: account.last_error_code?.isEmpty == false ? account.last_error_code! : "HTTP \(account.last_error_status ?? 500)")
                            .font(DS.fontResetTimeMedium)
                            .foregroundStyle(DS.bad)
                            .lineLimit(2)
                            .multilineTextAlignment(.leading)
                        Text(account.last_error_at == nil ? "错误时间未知" : BeijingTime.fullTime(account.last_error_at))
                            .font(DS.fontResetTime)
                            .monospacedDigit()
                            .foregroundStyle(DS.muted)
                    }
                }
                .buttonStyle(.linearPlain)
            } else if !account.errorMessageText.isEmpty || account.last_error_status != nil {
                Text(account.last_error_at == nil ? "错误时间未知" : BeijingTime.fullTime(account.last_error_at))
                    .font(DS.fontCompactMedium)
                    .monospacedDigit()
                    .foregroundStyle(DS.ink)
            } else {
                Text("暂无记录")
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.muted)
            }

            if let okAt = account.last_success_at, !okAt.isEmpty {
                let recoveredAfterErr = account.isSuccessAfterError
                VStack(alignment: .leading, spacing: 2) {
                    Text("最后成功")
                        .font(DS.fontResetTime)
                        .foregroundStyle(recoveredAfterErr ? DS.good : DS.muted)
                    Text(BeijingTime.fullTime(okAt))
                        .font(DS.fontResetTime)
                        .monospacedDigit()
                        .foregroundStyle(recoveredAfterErr ? DS.good : DS.muted)
                }
                .padding(.top, 2)
                .help(recoveredAfterErr ? "最近报错后已有成功调用" : "最后成功调用时间")
            }
        }
    }
}
