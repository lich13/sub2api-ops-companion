import SwiftUI

public struct GroupsPageView: View {
    @Bindable public var store: AppStore

    @State private var selectedPlatform: String = "openai"
    @State private var leftGroupOverride: Int? = nil
    @State private var rightGroupOverride: Int? = nil
    @State private var query: String = ""
    @State private var drafts: [Int: GroupDraftEntry] = [:]
    @State private var history: [[Int: GroupDraftEntry]] = []
    @State private var saving: Bool = false
    @State private var expandedDraftList: Bool = false
    @State private var statusMessage: String = ""
    @State private var hoveredZone: GroupMembershipZone? = nil

    public init(store: AppStore) {
        self.store = store
    }

    private func computePlatforms() -> [String] {
        let all = store.accounts.map(\.platform) + store.groups.compactMap(\.platform)
        return PlatformMeta.sortPlatforms(all)
    }

    private func computePair(availableGroups: [Group]) -> [Int] {
        guard !availableGroups.isEmpty else { return [] }
        if availableGroups.count == 1 {
            return [availableGroups[0].id]
        }
        let first = leftGroupOverride.flatMap { id in availableGroups.contains(where: { $0.id == id }) ? id : nil }
            ?? availableGroups[0].id
        let second = rightGroupOverride.flatMap { id in (id != first && availableGroups.contains(where: { $0.id == id })) ? id : nil }
            ?? availableGroups.first(where: { $0.id != first })?.id
        if let second {
            return [first, second]
        }
        return [first]
    }

    private func computeCandidates(platform: String) -> [Account] {
        store.accounts
            .filter { $0.platform == platform }
            .sorted { a, b in
                let cmp = a.name.localizedStandardCompare(b.name)
                if cmp != .orderedSame { return cmp == .orderedAscending }
                return a.id < b.id
            }
    }

    private func buildGroupById() -> [Int: Group] {
        Dictionary(store.groups.map { ($0.id, $0) }, uniquingKeysWith: { _, last in last })
    }

    private func sortedPendingDrafts() -> [GroupDraftEntry] {
        drafts.values.sorted { $0.id < $1.id }
    }

    private func conflictDrafts(pending: [GroupDraftEntry], groupById: [Int: Group]) -> [GroupDraftEntry] {
        pending.filter {
            GroupDraftLogic.hasConflict(draft: $0, accountById: store.accountById, groupById: groupById)
        }
    }

    private func memberships(for account: Account) -> [Int] {
        drafts[account.id]?.target ?? account.group_ids
    }

    private func matchesQuery(_ account: Account) -> Bool {
        let q = query.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard !q.isEmpty else { return true }
        return "\(account.name) #\(account.id)".lowercased().contains(q)
    }

    private func groupName(_ id: Int, groupById: [Int: Group]) -> String {
        groupById[id]?.name ?? "#\(id)"
    }

    private struct PartitionedCandidates {
        var leftItems: [Account] = []
        var bothItems: [Account] = []
        var rightItems: [Account] = []
        var unassigned: [Account] = []
        var otherGroups: [Account] = []
        var leftTotal: Int = 0
        var rightTotal: Int = 0
    }

    private func partitionCandidates(candidates: [Account], pair: [Int], validGroupIds: Set<Int>) -> PartitionedCandidates {
        var out = PartitionedCandidates()
        for acc in candidates {
            let mem = memberships(for: acc)
            let zone = GroupDraftLogic.membershipZone(memberships: mem, pair: pair)
            switch zone {
            case .left:
                out.leftItems.append(acc)
                out.leftTotal += 1
            case .both:
                out.bothItems.append(acc)
                out.leftTotal += 1
                out.rightTotal += 1
            case .right:
                out.rightItems.append(acc)
                out.rightTotal += 1
            case .none:
                if mem.contains(where: { validGroupIds.contains($0) }) {
                    out.otherGroups.append(acc)
                } else {
                    out.unassigned.append(acc)
                }
            }
        }
        return out
    }

    public var body: some View {
        let allPlatforms = computePlatforms()
        let activePlatform = allPlatforms.contains(selectedPlatform) ? selectedPlatform : (allPlatforms.first ?? "openai")
        let availableGroups = GroupDraftLogic.platformGroups(store.groups, platform: activePlatform)
        let activePair = computePair(availableGroups: availableGroups)
        let gMap = buildGroupById()
        let partitioned = partitionCandidates(
            candidates: computeCandidates(platform: activePlatform),
            pair: activePair,
            validGroupIds: Set(gMap.keys)
        )
        let pending = drafts.isEmpty ? [] : sortedPendingDrafts()
        return VStack(alignment: .leading, spacing: 18) {
            toolbar(platforms: allPlatforms, activePlatform: activePlatform)

            if availableGroups.isEmpty {
                HStack(spacing: 10) {
                    Image(systemName: "square.3.layers.3d")
                        .font(DS.iconFont(18))
                    Text("此平台没有分组")
                        .font(DS.fontBody)
                }
                .foregroundStyle(DS.muted)
                .padding(.vertical, 28)
            } else {
                vennHeading(availableGroups: availableGroups, pair: activePair, partitioned: partitioned, groupById: gMap)
                vennMap(pair: activePair, partitioned: partitioned, groupById: gMap)
            }

            poolSection(partitioned: partitioned, pair: activePair, groupById: gMap)

            if !pending.isEmpty {
                draftFooterPanel(pending: pending, groupById: gMap)
            }

            if !statusMessage.isEmpty {
                HStack {
                    Text(statusMessage)
                        .font(DS.fontCompact)
                        .foregroundStyle(DS.accent)
                    Spacer()
                    LinearIconButton(systemName: "xmark", label: "关闭分组提示", size: 11) {
                        statusMessage = ""
                    }
                }
            }
        }
        .onChange(of: drafts) { _, next in
            store.groupChangesCount = next.count
        }
        .onChange(of: saving) { _, next in
            store.groupSavingBusy = next
        }
        .onChange(of: store.connectionKey) { _, _ in
            drafts = [:]
            history = []
            statusMessage = ""
            expandedDraftList = false
            leftGroupOverride = nil
            rightGroupOverride = nil
        }
        .onChange(of: store.accounts) { _, live in
            let liveIds = Set(live.map(\.id))
            if drafts.keys.contains(where: { !liveIds.contains($0) }) {
                drafts = drafts.filter { liveIds.contains($0.key) }
                history = history.map { snap in snap.filter { liveIds.contains($0.key) } }
                statusMessage = "已删除账号的分组草稿已失效"
            }
        }
    }

    private func toolbar(platforms: [String], activePlatform: String) -> some View {
        HStack(spacing: 10) {
            LinearSegmentedTabs(
                items: platforms.map { pid in
                    LinearTabItem(id: pid, label: PlatformMeta.priorityName(pid))
                },
                selection: activePlatform,
                disabled: saving,
                onSelect: { pid in
                    selectedPlatform = pid
                    leftGroupOverride = nil
                    rightGroupOverride = nil
                    query = ""
                }
            )

            Spacer(minLength: 12)

            LinearSearchField(
                text: $query,
                prompt: "搜索名称或账号 ID",
                height: DS.controlHeight,
                width: 256,
                compactFont: true
            )

            LinearIconButton(
                systemName: "arrow.clockwise",
                label: "刷新分组",
                size: 13,
                boxSize: DS.controlHeight,
                bordered: true,
                disabled: !store.state.online || saving
            ) {
                store.refresh()
            }
        }
    }

    private func vennHeading(availableGroups: [Group], pair: [Int], partitioned: PartitionedCandidates, groupById: [Int: Group]) -> some View {
        HStack(alignment: .center, spacing: 16) {
            if let first = pair.first {
                groupHeaderColumn(id: first, count: partitioned.leftTotal, isRight: false, groupById: groupById)
            }

            if pair.count > 1 {
                HStack(spacing: 6) {
                    if availableGroups.count > 2 {
                        LinearSelect(
                            label: groupName(pair[0], groupById: groupById),
                            accessibilityTitle: "左侧分组",
                            height: 28,
                            compactFont: true,
                            disabled: saving,
                            entries: availableGroups.map { g in
                                LinearMenuEntry(id: "\(g.id)", title: "\(g.name) #\(g.id)", checked: g.id == pair[0]) {
                                    if g.id == pair[1] {
                                        rightGroupOverride = pair[0]
                                    }
                                    leftGroupOverride = g.id
                                }
                            }
                        )
                        Text("×")
                            .font(DS.fontCompact)
                            .foregroundStyle(DS.muted)
                        LinearSelect(
                            label: groupName(pair[1], groupById: groupById),
                            accessibilityTitle: "右侧分组",
                            height: 28,
                            compactFont: true,
                            disabled: saving,
                            entries: availableGroups.map { g in
                                LinearMenuEntry(id: "\(g.id)", title: "\(g.name) #\(g.id)", checked: g.id == pair[1]) {
                                    if g.id == pair[0] {
                                        leftGroupOverride = pair[1]
                                    }
                                    rightGroupOverride = g.id
                                }
                            }
                        )
                    }
                }
                .frame(maxWidth: .infinity)

                groupHeaderColumn(id: pair[1], count: partitioned.rightTotal, isRight: true, groupById: groupById)
            }
        }
        .padding(.horizontal, 4)
    }

    private func groupHeaderColumn(id: Int, count: Int, isRight: Bool, groupById: [Int: Group]) -> some View {
        HStack(spacing: 10) {
            RoundedRectangle(cornerRadius: 4)
                .fill(isRight ? DS.groupTeal : DS.accent)
                .frame(width: 8, height: 28)
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Text(groupName(id, groupById: groupById))
                    .font(DS.fontDialogTitle)
                    .foregroundStyle(DS.ink)
                    .lineLimit(1)
                Text("#\(id)")
                    .font(DS.fontPriorityMeta).monospacedDigit()
                    .foregroundStyle(DS.muted)
            }
            Spacer(minLength: 8)
            Text("\(count)")
                .font(DS.fontStatValue)
                .monospacedDigit()
                .foregroundStyle(DS.muted)
        }
        .frame(maxWidth: .infinity)
    }

    private func vennMap(pair: [Int], partitioned: PartitionedCandidates, groupById: [Int: Group]) -> some View {
        ZStack {
            GeometryReader { geo in
                let outlineW = pair.count > 1 ? geo.size.width * (2.0 / 3.0) : geo.size.width
                ZStack(alignment: .leading) {
                    RoundedRectangle(cornerRadius: 28)
                        .fill(DS.groupLeftFill)
                        .overlay(RoundedRectangle(cornerRadius: 28).strokeBorder(DS.groupLeftBorder, lineWidth: 1))
                        .frame(width: outlineW)
                    if pair.count > 1 {
                        HStack {
                            Spacer()
                            RoundedRectangle(cornerRadius: 28)
                                .fill(DS.groupRightFill)
                                .overlay(RoundedRectangle(cornerRadius: 28).strokeBorder(DS.groupRightBorder, lineWidth: 1))
                                .frame(width: outlineW)
                        }
                    }
                }
            }

            if pair.count == 1 {
                zoneColumn(zone: .left, title: groupName(pair[0], groupById: groupById), items: partitioned.leftItems, accentTitle: false, pair: pair, groupById: groupById)
                    .padding(20)
            } else {
                HStack(alignment: .top, spacing: 16) {
                    zoneColumn(zone: .left, title: "仅 \(groupName(pair[0], groupById: groupById))", items: partitioned.leftItems, accentTitle: false, pair: pair, groupById: groupById)
                    zoneColumn(zone: .both, title: "两组共有", items: partitioned.bothItems, accentTitle: true, pair: pair, groupById: groupById)
                    zoneColumn(zone: .right, title: "仅 \(groupName(pair[1], groupById: groupById))", items: partitioned.rightItems, accentTitle: false, pair: pair, groupById: groupById)
                }
                .padding(20)
            }
        }
        .frame(minHeight: 240)
    }

    private func poolSection(partitioned: PartitionedCandidates, pair: [Int], groupById: [Int: Group]) -> some View {
        VStack(alignment: .leading, spacing: 14) {
            Rectangle().fill(DS.line).frame(height: 1)
            poolGrid(title: "待分组", items: partitioned.unassigned, pair: pair, groupById: groupById)
            if !partitioned.otherGroups.isEmpty {
                poolGrid(title: "其他分组", items: partitioned.otherGroups, pair: pair, groupById: groupById)
            }
        }
    }

    private func zoneColumn(zone: GroupMembershipZone, title: String, items: [Account], accentTitle: Bool, pair: [Int], groupById: [Int: Group]) -> some View {
        let visible = items.filter(matchesQuery)
        let isHover = hoveredZone == zone
        return VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text(title)
                    .font(accentTitle ? DS.fontCompactSemibold : DS.fontCompactMedium)
                    .foregroundStyle(accentTitle ? DS.accent : DS.muted)
                Spacer()
                Text("\(items.count)")
                    .font(DS.fontCompact)
                    .monospacedDigit()
                    .foregroundStyle(DS.muted)
            }

            if visible.isEmpty {
                Text(query.isEmpty ? "暂无账号" : "无匹配账号")
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.muted)
                    .frame(maxWidth: .infinity, alignment: .center)
                    .padding(.vertical, 36)
            } else {
                ForEach(visible) { acc in
                    accountCard(acc, pair: pair, groupById: groupById)
                }
            }
            Spacer(minLength: 0)
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .topLeading)
        .background(isHover ? DS.accentBg.opacity(0.55) : DS.clear)
        .overlay(
            RoundedRectangle(cornerRadius: 14)
                .strokeBorder(isHover ? DS.accent : DS.clear, style: StrokeStyle(lineWidth: 1.5, dash: [5, 4]))
        )
        .clipShape(RoundedRectangle(cornerRadius: 14))
        .dropDestination(for: String.self) { dropped, _ in
            guard let first = dropped.first, let id = Int(first) else { return false }
            move(accountId: id, to: zone)
            return true
        } isTargeted: { targeted in
            hoveredZone = targeted ? zone : (hoveredZone == zone ? nil : hoveredZone)
        }
    }

    private func poolGrid(title: String, items: [Account], pair: [Int], groupById: [Int: Group]) -> some View {
        let visible = items.filter(matchesQuery)
        let cols = [
            GridItem(.flexible(minimum: 220), spacing: 10),
            GridItem(.flexible(minimum: 220), spacing: 10),
            GridItem(.flexible(minimum: 220), spacing: 10),
        ]
        return VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text(title)
                    .font(DS.fontSectionTitle)
                    .foregroundStyle(DS.ink)
                Spacer()
                Text("\(items.count)")
                    .font(DS.fontCompact)
                    .monospacedDigit()
                    .foregroundStyle(DS.muted)
            }
            if visible.isEmpty {
                Text(query.isEmpty ? "暂无账号" : "无匹配账号")
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.muted)
                    .padding(.top, 2)
            } else {
                LazyVGrid(columns: cols, spacing: 10) {
                    ForEach(visible) { acc in
                        accountCard(acc, pair: pair, groupById: groupById)
                    }
                }
            }
        }
        .padding(.vertical, 6)
        .dropDestination(for: String.self) { dropped, _ in
            guard let first = dropped.first, let id = Int(first) else { return false }
            move(accountId: id, to: .none)
            return true
        }
    }

    private func accountCard(_ account: Account, pair: [Int], groupById: [Int: Group]) -> some View {
        let dirty = drafts[account.id]
        let conflict = dirty.map {
            GroupDraftLogic.hasConflict(draft: $0, accountById: store.accountById, groupById: groupById)
        } ?? false

        var moveEntries: [LinearMenuEntry] = []
        if let first = pair.first {
            let firstName = groupName(first, groupById: groupById)
            moveEntries.append(
                LinearMenuEntry(
                    id: "left",
                    title: pair.count > 1 ? "移入 仅 \(firstName)" : "移入 \(firstName)"
                ) {
                    move(accountId: account.id, to: .left)
                }
            )
        }
        if pair.count > 1 {
            moveEntries.append(LinearMenuEntry(id: "both", title: "移入 两组共有") {
                move(accountId: account.id, to: .both)
            })
            moveEntries.append(LinearMenuEntry(id: "right", title: "移入 仅 \(groupName(pair[1], groupById: groupById))") {
                move(accountId: account.id, to: .right)
            })
        }
        moveEntries.append(.separator())
        moveEntries.append(LinearMenuEntry(id: "none", title: "移出当前组（待分组）") {
            move(accountId: account.id, to: .none)
        })

        let statusHint: String? = {
            if !account.schedulable {
                return "调度已关闭"
            }
            if !account.available {
                return account.blockers.first?.label ?? "不可调度"
            }
            return nil
        }()

        return HStack(alignment: .center, spacing: 8) {
            GroupMoveHandle(
                label: "移动 \(account.name)",
                disabled: !store.state.online || saving || pair.isEmpty,
                entries: moveEntries
            )

            VStack(alignment: .leading, spacing: 4) {
                Text(account.name)
                    .font(DS.fontBodySemibold)
                    .foregroundStyle(DS.ink)
                    .lineLimit(2)
                    .fixedSize(horizontal: false, vertical: true)

                HStack(spacing: 6) {
                    Text("#\(account.id)")
                        .font(DS.fontPriorityMeta).monospacedDigit()
                        .foregroundStyle(DS.muted)
                    Text(PlatformMeta.typeLabel(account.type))
                        .font(DS.fontResetTime)
                        .foregroundStyle(DS.muted)
                    DegradationBadgeView(account: account, compact: false)
                    if let statusHint {
                        Text(statusHint)
                            .font(DS.fontResetTime)
                            .foregroundStyle(DS.warn)
                            .lineLimit(1)
                    }
                    if dirty != nil {
                        Text(conflict ? "冲突" : "待应用")
                            .font(DS.fontResetTimeSemibold)
                            .foregroundStyle(conflict ? DS.bad : DS.accent)
                    }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)

            HStack(alignment: .center, spacing: 6) {
                LinearSwitch(
                    isOn: account.schedulable,
                    disabled: !store.state.online || saving || store.busyScheduleAccountId == account.id,
                    label: "\(account.name) 调度"
                ) {
                    Task { await store.toggleSchedulable(account) }
                }

                AccountActionMenuView(account: account, disabled: !store.state.online || saving) { action in
                    store.triggerAccountAction(action)
                }
            }
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 10)
        .background(DS.surface)
        .overlay(
            RoundedRectangle(cornerRadius: 10)
                .strokeBorder(dirty != nil ? (conflict ? DS.bad : DS.accent) : DS.line, lineWidth: 1)
        )
        .clipShape(RoundedRectangle(cornerRadius: 10))
        .draggable("\(account.id)")
    }

    private func draftFooterPanel(pending: [GroupDraftEntry], groupById: [Int: Group]) -> some View {
        let conflicts = conflictDrafts(pending: pending, groupById: groupById)
        return VStack(alignment: .leading, spacing: 10) {
            HStack {
                Button {
                    expandedDraftList.toggle()
                } label: {
                    HStack(spacing: 5) {
                        Text("\(pending.count) 个账号待应用")
                            .font(DS.fontCompactSemibold)
                            .monospacedDigit()
                            .foregroundStyle(DS.ink)
                        Image(systemName: expandedDraftList ? "chevron.down" : "chevron.right")
                            .font(DS.iconFont(10, weight: .semibold))
                            .foregroundStyle(DS.muted)
                    }
                }
                .buttonStyle(.linearPlain)

                Spacer()

                Button("撤销") {
                    if let prev = history.last {
                        drafts = prev
                        history.removeLast()
                        statusMessage = ""
                    }
                }
                .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                .disabled(saving || history.isEmpty)

                Button("放弃全部") {
                    drafts = [:]
                    history = []
                    statusMessage = ""
                }
                .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                .disabled(saving)

                Button(saving ? "正在保存…" : "应用变更 \(pending.count)") {
                    Task { await applyDrafts() }
                }
                .buttonStyle(LinearButtonStyle(variant: .primary, compact: true))
                .disabled(!store.state.online || saving || !conflicts.isEmpty)
            }

            if expandedDraftList {
                Rectangle().fill(DS.line).frame(height: 1)
                ForEach(pending) { d in
                    let isConflict = GroupDraftLogic.hasConflict(draft: d, accountById: store.accountById, groupById: groupById)
                    let origNames = d.original.map { groupName($0, groupById: groupById) }.joined(separator: "、")
                    let targetNames = d.target.map { groupName($0, groupById: groupById) }.joined(separator: "、")
                    HStack(spacing: 8) {
                        Text("\(d.name) #\(d.id)")
                            .font(DS.fontCompactSemibold)
                            .foregroundStyle(DS.ink)
                        Text(origNames.isEmpty ? "待分组" : origNames)
                            .font(DS.fontCompact)
                            .foregroundStyle(DS.muted)
                        Image(systemName: "arrow.right")
                            .font(DS.iconFont(10))
                            .foregroundStyle(DS.muted)
                        Text(targetNames.isEmpty ? "待分组" : targetNames)
                            .font(DS.fontCompactMedium)
                            .foregroundStyle(DS.accent)
                        Spacer()
                        if isConflict {
                            Button("放弃冲突项") {
                                drafts.removeValue(forKey: d.id)
                                history.removeAll()
                            }
                            .buttonStyle(LinearButtonStyle(variant: .danger, compact: true))
                        }
                    }
                }
            }
        }
        .padding(14)
        .background(DS.surface)
        .overlay(RoundedRectangle(cornerRadius: 10).strokeBorder(DS.lineStrong, lineWidth: 1))
        .clipShape(RoundedRectangle(cornerRadius: 10))
    }

    private func move(accountId: Int, to zone: GroupMembershipZone) {
        guard store.state.online,
              !saving,
              let account = store.accountById[accountId]
        else { return }
        let allPlatforms = computePlatforms()
        let activePlatform = allPlatforms.contains(selectedPlatform) ? selectedPlatform : (allPlatforms.first ?? "openai")
        let activePair = computePair(availableGroups: GroupDraftLogic.platformGroups(store.groups, platform: activePlatform))
        let next = GroupDraftLogic.moveAccount(drafts: drafts, account: account, pair: activePair, zone: zone)
        if next != drafts {
            history.append(drafts)
            drafts = next
            statusMessage = ""
        }
    }

    private func applyDrafts() async {
        guard store.state.online, !saving, !drafts.isEmpty else { return }
        let snapshotPending = sortedPendingDrafts()
        if !conflictDrafts(pending: snapshotPending, groupById: buildGroupById()).isEmpty {
            statusMessage = "账号或分组已变化，请先处理冲突"
            expandedDraftList = true
            return
        }
        saving = true
        statusMessage = ""
        defer { saving = false }

        let targets: [[String: JSONValue]] = snapshotPending.map { d in
            [
                "account_id": .int(Int64(d.id)),
                "expected_version": .string(d.version),
                "payload": .object([
                    "scope_group_ids": .array(d.scope.map { .int(Int64($0)) }),
                    "group_ids": .array(d.target.filter { d.scope.contains($0) }.map { .int(Int64($0)) }),
                ]),
            ]
        }
        do {
            let initial = try await store.operations.submitBatch(
                action: "groups",
                accounts: targets,
                requestId: UUID().uuidString.lowercased()
            )
            let final = try await store.operations.followBatch(initial: initial)
            let completed = final.items.filter { $0.status == "completed" && $0.result?.verified != false }
            for job in completed {
                drafts.removeValue(forKey: job.account_id)
            }
            if !completed.isEmpty {
                history.removeAll()
            }
            let failed = final.items.filter { OperationStore.isTerminal($0.status) && $0.status != "completed" }
            if failed.isEmpty {
                statusMessage = "已保存 \(completed.count) 个账号"
                expandedDraftList = false
            } else {
                statusMessage = "已保存 \(completed.count) 个账号；\(failed.map { "\($0.accountNameText)：\($0.reason ?? $0.status)" }.joined(separator: "；"))"
            }
            store.refresh()
        } catch {
            statusMessage = error.localizedDescription
        }
    }
}

private struct GroupMoveHandle: View {
    let label: String
    let disabled: Bool
    let entries: [LinearMenuEntry]

    @State private var open: Bool = false
    @State private var hovered: Bool = false

    var body: some View {
        Button {
            guard !disabled else { return }
            open.toggle()
        } label: {
            SixDotGripIcon(color: hovered && !disabled ? DS.ink : DS.muted)
                .frame(width: 20, height: 26)
                .background(hovered && !disabled ? DS.hover : DS.clear)
                .clipShape(RoundedRectangle(cornerRadius: 5))
        }
        .buttonStyle(.linearPlain)
        .disabled(disabled)
        .onHover { hovered = $0 }
        .help("拖拽或点击调整分组")
        .accessibilityLabel(label)
        .popover(isPresented: $open, arrowEdge: .bottom) {
            VStack(alignment: .leading, spacing: 2) {
                ForEach(entries) { entry in
                    if entry.isSeparator {
                        Rectangle().fill(DS.line).frame(height: 1).padding(.vertical, 3)
                    } else {
                        Button {
                            open = false
                            entry.action()
                        } label: {
                            Text(entry.title)
                                .font(DS.fontCompact)
                                .foregroundStyle(DS.ink)
                                .padding(.horizontal, 10)
                                .padding(.vertical, 6)
                                .frame(width: 170, alignment: .leading)
                                .contentShape(Rectangle())
                        }
                        .buttonStyle(.linearPlain)
                    }
                }
            }
            .padding(6)
            .background(DS.surface)
        }
    }
}
