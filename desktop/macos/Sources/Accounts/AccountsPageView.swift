import SwiftUI

public struct AccountsPageView: View {
    @Bindable public var store: AppStore

    @State private var sortStore = AccountSortStore()
    @State private var nativeUsage = NativeUsageReader()
    @State private var query: String = ""
    @State private var groupFilter: String = ""
    @State private var platformFilter: String = ""
    @State private var typeFilter: String = ""
    @State private var statusFilter: String = ""
    @State private var selectedIds: Set<Int> = []
    @State private var collapsedAccountName: Bool = false
    @State private var quotaJob: QuotaRefreshJob? = nil
    @State private var connectionBatch: OperationBatch? = nil
    @State private var batchBusy: Bool = false

    public init(store: AppStore) {
        self.store = store
    }

    private var filteredSortedAccounts: [Account] {
        let filtered = AccountFilterSort.filter(
            store.accounts,
            query: query,
            group: groupFilter,
            platform: platformFilter,
            status: statusFilter,
            type: typeFilter
        )
        return sortStore.sort(filtered)
    }

    private var availablePlatforms: [String] {
        PlatformMeta.sortPlatforms(store.accounts.map(\.platform))
    }

    private var availableTypes: [String] {
        Array(Set(store.accounts.map(\.type))).sorted()
    }

    private var selectedAccounts: [Account] {
        store.accounts.filter { selectedIds.contains($0.id) }
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
                    onBatchCredits: { store.accountModal = .batchCredits(selectedAccounts.filter { $0.platform == "openai" && $0.type == "oauth" }) },
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
        .task(id: "\(store.connectionKey):\(store.accounts.count):\(store.state.online)") {
            nativeUsage.bindConnection(store.connectionKey)
            nativeUsage.enqueueIfNeeded(accounts: store.accounts, online: store.state.online)
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

            Spacer()

            HStack(spacing: 6) {
                Text("排序")
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.muted)
                Menu {
                    ForEach(AccountSortKey.allCases, id: \.rawValue) { key in
                        Button {
                            sortStore.selectKey(key)
                        } label: {
                            HStack {
                                Text(key.label)
                                if sortStore.option.key == key {
                                    Image(systemName: sortStore.option.ascending ? "arrow.up" : "arrow.down")
                                }
                            }
                        }
                    }
                } label: {
                    HStack(spacing: 5) {
                        Text(sortStore.option.key.label)
                            .font(DS.fontCompactMedium)
                        Image(systemName: sortStore.option.ascending ? "arrow.up" : "arrow.down")
                            .font(DS.iconFont(10, weight: .semibold))
                            .foregroundStyle(DS.muted)
                    }
                    .foregroundStyle(DS.ink)
                    .padding(.horizontal, 10)
                    .frame(height: DS.controlHeightCompact)
                    .background(DS.surface)
                    .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                    .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
                }
                .menuStyle(.borderlessButton)
                .menuIndicator(.hidden)
                .fixedSize()
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
            HStack(spacing: 8) {
                Image(systemName: "magnifyingglass")
                    .font(DS.iconFont(13))
                    .foregroundStyle(DS.muted)
                TextField(text: $query, prompt: Text(verbatim: "搜索名称或账号 ID")) { EmptyView() }
                    .textFieldStyle(.plain)
                    .font(DS.fontCompact)
                if !query.isEmpty {
                    Button {
                        query = ""
                    } label: {
                        Image(systemName: "xmark.circle.fill")
                            .font(DS.iconFont(12))
                            .foregroundStyle(DS.muted)
                    }
                    .buttonStyle(.plain)
                }
            }
            .padding(.horizontal, 10)
            .frame(height: DS.controlHeightCompact)
            .background(DS.surface)
            .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
            .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))

            filterMenu(
                label: groupFilter.isEmpty ? "全部分组" : (store.groups.first(where: { "\($0.id)" == groupFilter })?.name ?? "分组 #\(groupFilter)"),
                options: [("", "全部分组")] + store.groups.map { ("\($0.id)", $0.name) },
                selection: $groupFilter
            )

            filterMenu(
                label: platformFilter.isEmpty ? "全部平台" : PlatformMeta.displayName(platformFilter),
                options: [("", "全部平台")] + availablePlatforms.map { ($0, PlatformMeta.displayName($0)) },
                selection: $platformFilter
            )

            filterMenu(
                label: typeFilter.isEmpty ? "全部类型" : PlatformMeta.typeLabel(typeFilter),
                options: [("", "全部类型")] + availableTypes.map { ($0, PlatformMeta.typeLabel($0)) },
                selection: $typeFilter
            )

            filterMenu(
                label: statusLabel(statusFilter),
                options: [
                    ("", "全部状态"),
                    ("ready", "可调度"),
                    ("error", "有报错"),
                    ("disabled", "已停调度"),
                ],
                selection: $statusFilter
            )
        }
    }

    private func filterMenu(label: String, options: [(String, String)], selection: Binding<String>) -> some View {
        Menu {
            ForEach(options, id: \.0) { value, text in
                Button(text) { selection.wrappedValue = value }
            }
        } label: {
            HStack(spacing: 5) {
                Text(label)
                    .font(DS.fontCompact)
                    .lineLimit(1)
                Image(systemName: "chevron.down")
                    .font(DS.iconFont(10, weight: .semibold))
                    .foregroundStyle(DS.muted)
            }
            .foregroundStyle(DS.ink)
            .padding(.horizontal, 10)
            .frame(height: DS.controlHeightCompact)
            .background(DS.surface)
            .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
            .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
    }

    private func statusLabel(_ key: String) -> String {
        switch key {
        case "ready": return "可调度"
        case "error": return "有报错"
        case "disabled": return "已停调度"
        default: return "全部状态"
        }
    }

    private var accountsTable: some View {
        let rows = filteredSortedAccounts
        let allSelected = !rows.isEmpty && rows.allSatisfy { selectedIds.contains($0.id) }
        let nameWidth: CGFloat = collapsedAccountName ? 86 : 164

        return VStack(spacing: 0) {
            HStack(spacing: 12) {
                Button {
                    if allSelected {
                        for r in rows { selectedIds.remove(r.id) }
                    } else {
                        for r in rows { selectedIds.insert(r.id) }
                    }
                } label: {
                    Image(systemName: allSelected ? "checkmark.square.fill" : "square")
                        .font(DS.iconFont(13))
                        .foregroundStyle(allSelected ? DS.accent : DS.muted)
                }
                .buttonStyle(.plain)
                .frame(width: 20)
                .accessibilityLabel("全选账号")

                Button {
                    collapsedAccountName.toggle()
                } label: {
                    HStack(spacing: 4) {
                        Text("账号")
                            .font(DS.fontTableHeader)
                            .foregroundStyle(DS.muted)
                        Image(systemName: collapsedAccountName ? "chevron.left" : "chevron.right")
                            .font(DS.iconFont(9, weight: .semibold))
                            .foregroundStyle(DS.muted)
                    }
                }
                .buttonStyle(.plain)
                .frame(width: nameWidth, alignment: .leading)

                Text("调度参数")
                    .font(DS.fontTableHeader)
                    .foregroundStyle(DS.muted)
                    .frame(width: 96, alignment: .leading)

                Text("当前状态")
                    .font(DS.fontTableHeader)
                    .foregroundStyle(DS.muted)
                    .frame(width: 118, alignment: .leading)

                Text("用量窗口")
                    .font(DS.fontTableHeader)
                    .foregroundStyle(DS.muted)
                    .frame(maxWidth: .infinity, alignment: .leading)

                Text("最近报错")
                    .font(DS.fontTableHeader)
                    .foregroundStyle(DS.muted)
                    .frame(width: 126, alignment: .leading)

                Text("允许调度")
                    .font(DS.fontTableHeader)
                    .foregroundStyle(DS.muted)
                    .frame(width: 56, alignment: .center)

                Text("操作")
                    .font(DS.fontTableHeader)
                    .foregroundStyle(DS.muted)
                    .frame(width: 214, alignment: .leading)
            }
            .padding(.horizontal, 12)
            .frame(height: 36)
            .background(DS.surfaceSubtle)

            Rectangle().fill(DS.line).frame(height: 1)

            if rows.isEmpty {
                Text(store.accounts.isEmpty ? "当前实例暂无账号" : "无匹配当前筛选条件的账号")
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.muted)
                    .frame(maxWidth: .infinity, alignment: .center)
                    .padding(.vertical, 32)
            } else {
                LazyVStack(spacing: 0) {
                    ForEach(Array(rows.enumerated()), id: \.element.id) { idx, account in
                        accountRow(account, nameWidth: nameWidth)
                        if idx < rows.count - 1 {
                            Rectangle().fill(DS.line).frame(height: 1)
                        }
                    }
                }
            }
        }
        .background(DS.surface)
        .overlay(RoundedRectangle(cornerRadius: DS.cardRadius).strokeBorder(DS.line, lineWidth: 1))
        .clipShape(RoundedRectangle(cornerRadius: DS.cardRadius))
    }

    private func accountRow(_ account: Account, nameWidth: CGFloat) -> some View {
        let checked = selectedIds.contains(account.id)
        let pendingOps = store.operations.pendingForAccount(account.id)

        return HStack(alignment: .center, spacing: 12) {
            Button {
                if checked { selectedIds.remove(account.id) } else { selectedIds.insert(account.id) }
            } label: {
                Image(systemName: checked ? "checkmark.square.fill" : "square")
                    .font(DS.iconFont(13))
                    .foregroundStyle(checked ? DS.accent : DS.muted)
            }
            .buttonStyle(.plain)
            .frame(width: 20)
            .accessibilityLabel("选择 \(account.name)")

            VStack(alignment: .leading, spacing: 3) {
                if !collapsedAccountName {
                    Text(account.name)
                        .font(DS.fontBodySemibold)
                        .foregroundStyle(DS.ink)
                        .lineLimit(2)
                        .help(account.name)
                }
                Text("#\(account.id)")
                    .font(DS.fontMono11)
                    .foregroundStyle(DS.muted)
                Text("\(PlatformMeta.displayName(account.platform)) · \(PlatformMeta.typeLabel(account.type))")
                    .font(DS.fontResetTime)
                    .foregroundStyle(DS.muted)
                    .lineLimit(1)
                DegradationBadgeView(account: account)
                if !pendingOps.isEmpty {
                    Button {
                        store.operations.isOpen = true
                    } label: {
                        Text("\(pendingOps.count) 项任务进行中")
                            .font(DS.fontUsageStatMedium)
                            .monospacedDigit()
                            .foregroundStyle(DS.accent)
                    }
                    .buttonStyle(.plain)
                }
            }
            .frame(width: nameWidth, alignment: .leading)

            SchedulingParametersView(account: account, store: store)

            AccountStatusView(account: account) { errId in
                store.openErrorDetail(id: errId)
            }
            .frame(width: 118, alignment: .leading)

            UsageCellView(
                account: account,
                store: store,
                nativeUsage: nativeUsage,
                onOpenKeyQuotaSettings: { target in
                    store.accountModal = .keyQuotaSettings(target)
                }
            )

            errorCell(account)
                .frame(width: 126, alignment: .leading)

            LinearSwitch(
                isOn: account.schedulable,
                disabled: !store.state.online || store.busyScheduleAccountId == account.id,
                label: "\(account.name) 允许调度"
            ) {
                Task { await store.toggleSchedulable(account) }
            }
            .frame(width: 56, alignment: .center)

            AccountActionsView(account: account, disabled: !store.state.online) { action in
                store.triggerAccountAction(action)
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 12)
        .background(checked ? DS.accentBg.opacity(0.35) : DS.surface)
    }

    @ViewBuilder
    private func errorCell(_ account: Account) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            if let errId = account.last_error_id {
                Button {
                    store.openErrorDetail(id: errId)
                } label: {
                    Text(verbatim: account.last_error_code?.isEmpty == false ? account.last_error_code! : "HTTP \(account.last_error_status ?? 500)")
                        .font(DS.fontMono11)
                        .foregroundStyle(DS.bad)
                        .lineLimit(1)
                }
                .buttonStyle(.plain)

                Text(account.last_error_at == nil ? "错误时间未知" : BeijingTime.fullTime(account.last_error_at))
                    .font(DS.fontResetTime)
                    .monospacedDigit()
                    .foregroundStyle(DS.muted)
            } else if !account.error_message.isEmpty || account.last_error_status != nil {
                Text(account.last_error_at == nil ? "错误时间未知" : BeijingTime.fullTime(account.last_error_at))
                    .font(DS.fontCompactMedium)
                    .foregroundStyle(DS.ink)
            } else {
                Text("暂无记录")
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.muted)
            }

            if let okAt = account.last_success_at, !okAt.isEmpty {
                Text("最后成功")
                    .font(DS.fontUsageStat)
                    .foregroundStyle(DS.muted)
                Text(BeijingTime.fullTime(okAt))
                    .font(DS.fontResetTime)
                    .monospacedDigit()
                    .foregroundStyle(DS.muted)
            }
        }
    }

    private func triggerQuotaRefresh() async {
        guard store.state.online else { return }
        nativeUsage.refreshAll(accounts: store.accounts)
        do {
            let started: QuotaRefreshJob = try await RustBridge.shared.api("POST", "/quota-refresh", body: .object([:]))
            quotaJob = started
            while quotaJob?.status == "running" || quotaJob?.status == "queued" {
                try? await Task.sleep(for: .milliseconds(800))
                guard !Task.isCancelled, store.state.online else { return }
                let polled: QuotaRefreshJob = try await RustBridge.shared.api("GET", "/quota-refresh")
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
