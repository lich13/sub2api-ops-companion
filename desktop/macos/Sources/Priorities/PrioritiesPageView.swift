import SwiftUI

public struct PrioritiesPageView: View {
    @Bindable public var store: AppStore

    @State var view: PriorityViewEnvelope? = nil
    @State var drafts: [String: PriorityPlatformDraft] = [:]
    @State var history: [String: [PriorityPlatformDraft?]] = [:]
    @State var results: [String: String] = [:]
    @State var notices: [String: String] = [:]
    @State var query: String = ""
    @State private var selectedTab: String = ""
    @State var loading: Bool = false
    @State var submittingPlatform: String = ""
    @State var settingParallelism: String = ""
    @State var error: String = ""
    @State private var surfaceWidth: CGFloat = 1133

    static let taskLabels: [String: String] = [
        "queued": "等待执行",
        "running": "正在保存",
        "checking": "正在核对",
        "completed": "全部已确认",
        "needs_confirmation": "需要重新核对",
        "failed": "保存失败",
        "cancelled": "已停止",
    ]

    public init(store: AppStore) {
        self.store = store
    }

    var platforms: [PriorityPlatformGroup] {
        var groups = view?.platforms ?? []
        for (platform, _) in drafts where !groups.contains(where: { $0.platform == platform }) {
            groups.append(PriorityPlatformGroup(platform: platform, items: [], version: ""))
        }
        return PriorityDraftLogic.sortPlatforms(groups)
    }

    private var activeTabPlatform: String {
        if platforms.contains(where: { $0.platform == selectedTab }) { return selectedTab }
        return platforms.first?.platform ?? ""
    }

    private var useMultiColumn: Bool {
        PriorityDraftLogic.shouldUseMultiColumn(platforms: platforms, availableWidth: surfaceWidth)
    }

    private var activeTask: PriorityTask? {
        view?.active_task
    }

    private var isBusy: Bool {
        !submittingPlatform.isEmpty || (activeTask?.isPending == true)
    }

    private var totalChangesCount: Int {
        drafts.values.reduce(0) { sum, draft in
            let prios = Dictionary(uniqueKeysWithValues: draft.accounts.map { ($0.id, $0.priority) })
            let width = max(1, min(8, draft.parallelism))
            let slots = draft.slots ?? (width == 1
                ? draft.ids.enumerated().map { idx, id in PrioritySlot(priority: idx + 1, id: id) }
                : PriorityDraftLogic.prioritySlots(accounts: draft.accounts, width: width))
            return sum + slots.filter { slot in
                guard let id = slot.id, let oldP = prios[id] else { return false }
                return oldP != slot.priority
            }.count
        }
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(alignment: .center, spacing: 12) {
                Text("优先级")
                    .font(DS.fontPageTitle)
                    .foregroundStyle(DS.ink)

                Spacer()

                LinearIconButton(
                    systemName: "arrow.clockwise",
                    label: "刷新优先级",
                    size: 13,
                    boxSize: DS.controlHeightCompact,
                    bordered: true,
                    disabled: !store.state.online || loading || !submittingPlatform.isEmpty
                ) {
                    Task { await readPriorities() }
                }
            }

            HStack(spacing: 10) {
                LinearSearchField(
                    text: $query,
                    prompt: "搜索名称或账号 ID",
                    height: DS.controlHeightCompact,
                    width: 280,
                    compactFont: true
                )
                Spacer()
            }

            if !store.state.online {
                Text("当前离线，已保留草稿；连接恢复后请核对实际优先级。")
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.muted)
            }
            if !error.isEmpty {
                Text(error)
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.bad)
            }
            if loading && view == nil {
                Text("正在读取优先级…")
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.muted)
            } else if view != nil && platforms.isEmpty && error.isEmpty {
                Text("暂无账号")
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.muted)
                    .padding(.vertical, 16)
            }

            if !useMultiColumn && platforms.count > 1 {
                HStack(spacing: 6) {
                    ForEach(platforms) { grp in
                        let sel = grp.platform == activeTabPlatform
                        Button {
                            selectedTab = grp.platform
                        } label: {
                            HStack(spacing: 5) {
                                Text(PlatformMeta.priorityName(grp.platform))
                                Text("\(grp.items.count)")
                                    .monospacedDigit()
                                    .foregroundStyle(sel ? DS.accent : DS.muted)
                                if drafts[grp.platform] != nil {
                                    Circle().fill(DS.accent).frame(width: 5, height: 5)
                                }
                            }
                            .font(sel ? DS.fontCompactSemibold : DS.fontCompactMedium)
                            .foregroundStyle(sel ? DS.accent : DS.ink)
                            .padding(.horizontal, 12)
                            .frame(height: 30)
                            .background(sel ? DS.accentBg : DS.clear)
                            .clipShape(RoundedRectangle(cornerRadius: 6))
                        }
                        .buttonStyle(.linearPlain)
                    }
                }
            }

            if useMultiColumn {
                let weights = platforms.map { grp -> CGFloat in
                    let p = max(1, min(8, drafts[grp.platform]?.parallelism ?? grp.parallelism ?? 1))
                    return CGFloat(max(320, p * 220 + 72))
                }
                let totalWeight = max(1, weights.reduce(0, +))
                let gapTotal = CGFloat(max(0, platforms.count - 1)) * 12
                let availableColsW = max(320 * CGFloat(platforms.count), surfaceWidth - gapTotal)

                HStack(alignment: .top, spacing: 12) {
                    ForEach(Array(platforms.enumerated()), id: \.element.id) { idx, grp in
                        let colWidth = floor(availableColsW * weights[idx] / totalWeight)
                        platformColumn(grp)
                            .frame(width: colWidth)
                    }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            } else if let currentGrp = platforms.first(where: { $0.platform == activeTabPlatform }) {
                platformColumn(currentGrp)
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(LinearWidthObserver { surfaceWidth = $0 })
        .onChange(of: totalChangesCount) { _, count in
            store.priorityChangesCount = count
        }
        .onChange(of: store.page) { _, next in
            if next == .priorities && store.state.online && !loading && submittingPlatform.isEmpty {
                Task { await readPriorities() }
            }
        }
        .task(id: "\(store.connectionKey):\(store.state.online)") {
            if store.state.online {
                await readPriorities()
            }
        }
        .task(id: "\(store.connectionKey):\(activeTask?.id ?? ""):\(store.state.online)") {
            guard let task = activeTask, task.isPending, store.state.online else { return }
            await pollActiveTask(id: task.id)
        }
    }

    private func platformColumn(_ group: PriorityPlatformGroup) -> some View {
        let platform = group.platform
        let draft = drafts[platform]
        let accounts = draft?.accounts ?? group.items
        let byId = Dictionary(uniqueKeysWithValues: accounts.map { ($0.id, $0) })
        let liveById = store.accountById
        let width = max(1, min(8, draft?.parallelism ?? group.parallelism ?? 1))
        let slots: [PrioritySlot] = draft?.slots ?? (width == 1 && draft != nil
            ? draft!.ids.enumerated().map { idx, id in PrioritySlot(priority: idx + 1, id: id) }
            : PriorityDraftLogic.prioritySlots(accounts: accounts, width: width))
        let occupied = slots.compactMap { slot -> (id: Int, target: Int)? in
            guard let id = slot.id else { return nil }
            return (id, slot.priority)
        }
        let ids = occupied.map(\.id)
        let changes = occupied.compactMap { pair -> PriorityAccount? in
            guard let acc = byId[pair.id], acc.priority != pair.target else { return nil }
            return acc
        }
        let blocked = changes.filter { !$0.editable }
        let conflict = draft != nil && (draft!.version != group.version || !PriorityDraftLogic.samePriorityAccounts(ids: draft!.ids, accounts: group.items))
        let filtered = !query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        let editing = store.state.online && view != nil && !loading && settingParallelism.isEmpty && !filtered && !conflict && submittingPlatform != platform && !(activeTask?.isPending == true && activeTask?.platform == platform)
        let lanes = groupLanes(slots: slots)
        let visibleLanes = lanes.filter { lane in
            !filtered || lane.slots.contains(where: { entry in
                guard let id = entry.slot.id, let acc = byId[id] else { return false }
                return matchesQuery(acc)
            })
        }
        let task = (activeTask?.platform == platform) ? activeTask : group.last_task

        return VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 8) {
                Text(PlatformMeta.priorityName(platform))
                    .font(DS.fontSectionTitle)
                    .foregroundStyle(DS.ink)
                Text("\(ids.count) 个账号")
                    .font(DS.fontCompact)
                    .monospacedDigit()
                    .foregroundStyle(DS.muted)

                Spacer(minLength: 8)

                HStack(spacing: 3) {
                    Text("并列")
                        .font(DS.fontCompact)
                        .foregroundStyle(DS.muted)
                    LinearIconButton(
                        systemName: "minus",
                        label: "减少\(PlatformMeta.priorityName(platform))并列值",
                        size: 11,
                        boxSize: 22,
                        disabled: width <= 1 || draft != nil || !editing
                    ) {
                        Task { await updateParallelism(group: group, next: width - 1) }
                    }
                    Text("\(width)")
                        .font(DS.fontCompactMedium)
                        .monospacedDigit()
                        .foregroundStyle(DS.ink)
                        .frame(minWidth: 16, alignment: .center)
                    LinearIconButton(
                        systemName: "plus",
                        label: "增加\(PlatformMeta.priorityName(platform))并列值",
                        size: 11,
                        boxSize: 22,
                        disabled: width >= 8 || draft != nil || !editing
                    ) {
                        Task { await updateParallelism(group: group, next: width + 1) }
                    }
                }

                if !changes.isEmpty {
                    Text("\(changes.count) 项变更")
                        .font(DS.fontCompactMedium)
                        .monospacedDigit()
                        .foregroundStyle(DS.accent)
                        .padding(.leading, 4)
                }
            }
            .padding(.horizontal, 14)
            .frame(minHeight: 48)
            .background(DS.surface)

            Rectangle().fill(DS.line).frame(height: 1)

            if conflict {
                HStack(spacing: 8) {
                    Text("该平台账号集合或优先级已变化，草稿已保留，请重新核对。")
                        .font(DS.fontCompact)
                        .foregroundStyle(DS.bad)
                    Spacer()
                    Button("重新核对") {
                        Task { await readPriorities(repreparePlatform: platform) }
                    }
                    .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                }
                .padding(12)
                Rectangle().fill(DS.line).frame(height: 1)
            }

            if !blocked.isEmpty {
                Text("重编号会修改不可编辑账号：\(blocked.map { "\($0.name) #\($0.id)" }.joined(separator: "、"))")
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.bad)
                    .padding(12)
                Rectangle().fill(DS.line).frame(height: 1)
            }

            let lanesContent = VStack(spacing: 0) {
                ForEach(Array(visibleLanes.enumerated()), id: \.element.priority) { laneIdx, lane in
                    if width == 1, let singleEntry = lane.slots.first {
                        singleLaneRow(
                            platform: platform,
                            priority: lane.priority,
                            slotIndex: singleEntry.index,
                            slot: singleEntry.slot,
                            totalSlots: slots.count,
                            byId: byId,
                            liveById: liveById,
                            editing: editing
                        )
                    } else {
                        let gridCols = Array(repeating: GridItem(.flexible(), spacing: 8), count: min(width, 3))
                        HStack(alignment: .top, spacing: 10) {
                            Text("\(lane.priority)")
                                .font(DS.fontCompact)
                                .monospacedDigit()
                                .foregroundStyle(DS.muted)
                                .frame(width: 22, alignment: .trailing)
                                .padding(.top, 12)

                            LazyVGrid(columns: gridCols, alignment: .leading, spacing: 8) {
                                ForEach(lane.slots, id: \.index) { entry in
                                    parallelSlotCard(
                                        platform: platform,
                                        slotIndex: entry.index,
                                        slot: entry.slot,
                                        totalSlots: slots.count,
                                        byId: byId,
                                        liveById: liveById,
                                        editing: editing
                                    )
                                }
                            }
                        }
                        .padding(12)
                    }

                    if laneIdx < visibleLanes.count - 1 {
                        Rectangle().fill(DS.line).frame(height: 1)
                    }
                }
            }

            LinearBoundedScrollView(maxHeight: 540) {
                lanesContent
            }

            if filtered && visibleLanes.isEmpty {
                Text("无匹配账号")
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.muted)
                    .padding(.horizontal, 14)
                    .padding(.vertical, 20)
            }

            if !changes.isEmpty || draft != nil {
                Rectangle().fill(DS.line).frame(height: 1)
                VStack(alignment: .leading, spacing: 8) {
                    HStack(spacing: 8) {
                        Button {
                            undoDraft(platform: platform)
                        } label: {
                            Label("撤销", systemImage: "arrow.uturn.backward")
                        }
                        .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                        .disabled((history[platform] ?? []).isEmpty || !editing)

                        Button("放弃全部") {
                            drafts.removeValue(forKey: platform)
                            history[platform] = []
                        }
                        .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                        .disabled(draft == nil || !editing)

                        Spacer()

                        Button {
                            Task {
                                await applyOrder(
                                    group: group,
                                    ids: ids,
                                    targets: occupied.map(\.target),
                                    parallelism: width,
                                    expectedVersion: draft?.version ?? group.version
                                )
                            }
                        } label: {
                            Label(submittingPlatform == platform ? "正在提交…" : "应用变更", systemImage: "checkmark")
                        }
                        .buttonStyle(LinearButtonStyle(variant: .primary, compact: true))
                        .disabled(!editing || isBusy || !blocked.isEmpty || changes.isEmpty)
                    }

                    if activeTask?.isPending == true && activeTask?.platform != platform {
                        Text("其他平台排序任务进行中，草稿已保留。")
                            .font(DS.fontCompact)
                            .foregroundStyle(DS.muted)
                    }
                }
                .padding(.horizontal, 14)
                .padding(.vertical, 12)
            }

            if let notice = notices[platform], !notice.isEmpty {
                Text(notice)
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.muted)
                    .padding(.horizontal, 14)
                    .padding(.bottom, 10)
            }

            if let task, task.status != "completed" {
                Rectangle().fill(DS.line).frame(height: 1)
                HStack(spacing: 8) {
                    if task.isPending {
                        let waitingDetail: String = {
                            guard !task.waiting_reason.isEmpty else { return "" }
                            if task.waiting_account_ids.isEmpty {
                                return " · \(task.waiting_reason)"
                            }
                            let names = task.waiting_account_ids.map { wid in
                                let name = byId[wid]?.name ?? liveById[wid]?.name ?? "账号"
                                return "\(name) #\(wid)"
                            }.joined(separator: "、")
                            return " · \(task.waiting_reason)：\(names)"
                        }()
                        Text("\(Self.taskLabels[task.status] ?? task.status) · \(task.verified_count)/\(task.total) 已确认\(waitingDetail)")
                            .font(DS.fontCompact)
                            .monospacedDigit()
                            .foregroundStyle(DS.muted)
                        Spacer()
                        Button("停止剩余修改") {
                            Task { await cancelTask(task) }
                        }
                        .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                        .disabled(!store.state.online || task.cancel_requested)
                    } else {
                        Button("重新核对") {
                            Task { await readPriorities(repreparePlatform: platform) }
                        }
                        .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                        .disabled(!store.state.online || loading || isBusy || filtered)
                    }
                }
                .padding(12)
            }

            if let res = results[platform], !res.isEmpty {
                Text(res)
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.good)
                    .padding(.horizontal, 14)
                    .padding(.bottom, 10)
            }
        }
        .frame(maxWidth: .infinity, alignment: .topLeading)
        .background(DS.surface)
        .overlay(RoundedRectangle(cornerRadius: 10).strokeBorder(DS.line, lineWidth: 1))
        .clipShape(RoundedRectangle(cornerRadius: 10))
    }
}
