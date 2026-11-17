import SwiftUI

public struct PrioritiesPageView: View {
    @Bindable public var store: AppStore

    @State private var view: PriorityViewEnvelope? = nil
    @State private var drafts: [String: PriorityPlatformDraft] = [:]
    @State private var history: [String: [PriorityPlatformDraft?]] = [:]
    @State private var results: [String: String] = [:]
    @State private var notices: [String: String] = [:]
    @State private var query: String = ""
    @State private var selectedTab: String = ""
    @State private var loading: Bool = false
    @State private var submittingPlatform: String = ""
    @State private var settingParallelism: String = ""
    @State private var error: String = ""
    @State private var surfaceWidth: CGFloat = 1133

    private static let taskLabels: [String: String] = [
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

    private var platforms: [PriorityPlatformGroup] {
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
        .task(id: "\(store.connectionKey):\(store.state.online)") {
            if store.state.online {
                await readPriorities()
            }
        }
        .task(id: activeTask?.id ?? "") {
            guard let task = activeTask, task.isPending, store.state.online else { return }
            await pollActiveTask(id: task.id)
        }
    }

    private func platformColumn(_ group: PriorityPlatformGroup) -> some View {
        let platform = group.platform
        let draft = drafts[platform]
        let accounts = draft?.accounts ?? group.items
        let byId = Dictionary(uniqueKeysWithValues: accounts.map { ($0.id, $0) })
        let liveById = Dictionary(uniqueKeysWithValues: store.accounts.map { ($0.id, $0) })
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

    @ViewBuilder
    private func singleLaneRow(
        platform: String,
        priority: Int,
        slotIndex: Int,
        slot: PrioritySlot,
        totalSlots: Int,
        byId: [Int: PriorityAccount],
        liveById: [Int: Account],
        editing: Bool
    ) -> some View {
        if let accId = slot.id, let acc = byId[accId] {
            let live = liveById[acc.id]
            let isSchedulable = live?.schedulable ?? acc.schedulable
            let changed = acc.priority != slot.priority
            let warningText: String? = {
                if !acc.editable { return acc.reason.isEmpty ? "不可编辑" : acc.reason }
                if acc.status != "active" { return acc.status }
                if !isSchedulable { return "不可调度" }
                return nil
            }()

            HStack(alignment: .center, spacing: 10) {
                Text("\(priority)")
                    .font(DS.fontCompact)
                    .monospacedDigit()
                    .foregroundStyle(DS.muted)
                    .frame(width: 20, alignment: .trailing)

                SixDotGripIcon()

                VStack(alignment: .leading, spacing: 4) {
                    Text(acc.name)
                        .font(DS.fontBodySemibold)
                        .foregroundStyle(DS.ink)
                        .lineLimit(2)

                    HStack(spacing: 8) {
                        Text("#\(acc.id)")
                            .font(DS.fontPriorityMeta).monospacedDigit()
                        Text(acc.type)
                            .font(DS.fontPriorityMeta)
                    }
                    .foregroundStyle(DS.muted)

                    if let warningText {
                        Text(warningText)
                            .font(DS.fontPriorityMeta)
                            .foregroundStyle(DS.warn)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)

                VStack(alignment: .trailing, spacing: 3) {
                    HStack(spacing: 8) {
                        Text("当前")
                            .foregroundStyle(DS.muted)
                        Text("\(acc.priority)")
                            .monospacedDigit()
                            .foregroundStyle(DS.ink)
                            .frame(minWidth: 20, alignment: .trailing)
                    }
                    HStack(spacing: 8) {
                        Text("目标")
                            .foregroundStyle(changed ? DS.accent : DS.muted)
                        Text("\(slot.priority)")
                            .font(changed ? DS.fontCompactSemibold : DS.fontCompact)
                            .monospacedDigit()
                            .foregroundStyle(changed ? DS.accent : DS.muted)
                            .frame(minWidth: 20, alignment: .trailing)
                    }
                }
                .font(DS.fontCompact)

                VStack(spacing: 2) {
                    LinearIconButton(
                        systemName: "arrow.up",
                        label: "上移 \(acc.name)",
                        size: 11,
                        boxSize: 22,
                        disabled: !editing || !acc.editable || slotIndex == 0
                    ) {
                        moveItem(platform: platform, id: acc.id, targetIndex: slotIndex - 1)
                    }
                    LinearIconButton(
                        systemName: "arrow.down",
                        label: "下移 \(acc.name)",
                        size: 11,
                        boxSize: 22,
                        disabled: !editing || !acc.editable || slotIndex >= totalSlots - 1
                    ) {
                        moveItem(platform: platform, id: acc.id, targetIndex: slotIndex + 1)
                    }
                }

                LinearSwitch(
                    isOn: isSchedulable,
                    disabled: !store.state.online || live == nil || store.busyScheduleAccountId == acc.id,
                    label: "\(acc.name) 调度"
                ) {
                    if let live {
                        Task { await store.toggleSchedulable(live) }
                    }
                }
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 12)
            .frame(minHeight: 62)
            .contentShape(Rectangle())
            .draggable("\(platform):\(acc.id)")
            .dropDestination(for: String.self) { items, _ in
                guard let raw = items.first, raw.hasPrefix("\(platform):"),
                      let draggedId = Int(raw.dropFirst(platform.count + 1))
                else { return false }
                moveItem(platform: platform, id: draggedId, targetIndex: slotIndex)
                return true
            }
        }
    }

    @ViewBuilder
    private func parallelSlotCard(
        platform: String,
        slotIndex: Int,
        slot: PrioritySlot,
        totalSlots: Int,
        byId: [Int: PriorityAccount],
        liveById: [Int: Account],
        editing: Bool
    ) -> some View {
        if let accId = slot.id, let acc = byId[accId] {
            if !query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && !matchesQuery(acc) {
                DS.clear.frame(height: 0)
            } else {
                let live = liveById[acc.id]
                let isSchedulable = live?.schedulable ?? acc.schedulable
                let changed = acc.priority != slot.priority
                let warningText: String? = {
                    if !acc.editable { return acc.reason.isEmpty ? "不可编辑" : acc.reason }
                    if acc.status != "active" { return acc.status }
                    if !isSchedulable { return "不可调度" }
                    return nil
                }()

                VStack(alignment: .leading, spacing: 6) {
                    HStack(alignment: .top, spacing: 8) {
                        SixDotGripIcon()
                            .padding(.top, 1)

                        VStack(alignment: .leading, spacing: 3) {
                            Text(acc.name)
                                .font(DS.fontBodySemibold)
                                .foregroundStyle(DS.ink)
                                .lineLimit(1)
                            HStack(spacing: 6) {
                                Text("#\(acc.id)").font(DS.fontPriorityMeta).monospacedDigit()
                                Text(acc.type).font(DS.fontPriorityMeta)
                                if let warningText {
                                    Text(warningText)
                                        .font(DS.fontPriorityMeta)
                                        .foregroundStyle(DS.warn)
                                }
                            }
                            .foregroundStyle(DS.muted)
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)

                        VStack(alignment: .trailing, spacing: 2) {
                            HStack(spacing: 4) {
                                Text("当前").foregroundStyle(DS.muted)
                                Text("\(acc.priority)")
                                    .monospacedDigit()
                                    .foregroundStyle(DS.ink)
                                    .frame(minWidth: 16, alignment: .trailing)
                            }
                            HStack(spacing: 4) {
                                Text("目标").foregroundStyle(changed ? DS.accent : DS.muted)
                                Text("\(slot.priority)")
                                    .font(changed ? DS.fontCompactSemibold : DS.fontCompact)
                                    .monospacedDigit()
                                    .foregroundStyle(changed ? DS.accent : DS.muted)
                                    .frame(minWidth: 16, alignment: .trailing)
                            }
                        }
                        .font(DS.fontPriorityMeta)
                    }

                    HStack(spacing: 8) {
                        if let live {
                            LinearSwitch(
                                isOn: live.schedulable,
                                disabled: !store.state.online || store.busyScheduleAccountId == live.id,
                                compact: true,
                                label: "\(acc.name) 调度"
                            ) {
                                Task { await store.toggleSchedulable(live) }
                            }
                        }
                        Spacer(minLength: 4)
                        HStack(spacing: 2) {
                            LinearIconButton(systemName: "arrow.up", label: "上移 \(acc.name)", size: 10, boxSize: 22, disabled: !editing || !acc.editable || slotIndex == 0) {
                                moveItem(platform: platform, id: acc.id, targetIndex: slotIndex - 1)
                            }
                            LinearIconButton(systemName: "arrow.down", label: "下移 \(acc.name)", size: 10, boxSize: 22, disabled: !editing || !acc.editable || slotIndex >= totalSlots - 1) {
                                moveItem(platform: platform, id: acc.id, targetIndex: slotIndex + 1)
                            }
                        }
                    }
                    .padding(.leading, 22)
                }
                .padding(8)
                .frame(minHeight: 64)
                .background(DS.surface)
                .overlay(RoundedRectangle(cornerRadius: 7).strokeBorder(changed ? DS.accent : DS.line, lineWidth: 1))
                .clipShape(RoundedRectangle(cornerRadius: 7))
                .draggable("\(platform):\(acc.id)")
                .dropDestination(for: String.self) { items, _ in
                    guard let raw = items.first, raw.hasPrefix("\(platform):"),
                          let draggedId = Int(raw.dropFirst(platform.count + 1))
                    else { return false }
                    moveItem(platform: platform, id: draggedId, targetIndex: slotIndex)
                    return true
                }
            }
        } else {
            HStack {
                Spacer()
                Image(systemName: "plus")
                    .font(DS.iconFont(12))
                    .foregroundStyle(DS.muted)
                Spacer()
            }
            .frame(minHeight: 64)
            .background(DS.surfaceSubtle)
            .overlay(RoundedRectangle(cornerRadius: 7).strokeBorder(DS.line, style: StrokeStyle(lineWidth: 1, dash: [4, 3])))
            .clipShape(RoundedRectangle(cornerRadius: 7))
            .dropDestination(for: String.self) { items, _ in
                guard let raw = items.first, raw.hasPrefix("\(platform):"),
                      let draggedId = Int(raw.dropFirst(platform.count + 1))
                else { return false }
                moveItem(platform: platform, id: draggedId, targetIndex: slotIndex)
                return true
            }
        }
    }

    private func groupLanes(slots: [PrioritySlot]) -> [(priority: Int, slots: [(index: Int, slot: PrioritySlot)])] {
        var order: [Int] = []
        var map: [Int: [(index: Int, slot: PrioritySlot)]] = [:]
        for (idx, s) in slots.enumerated() {
            if map[s.priority] == nil { order.append(s.priority) }
            map[s.priority, default: []].append((idx, s))
        }
        return order.map { ($0, map[$0] ?? []) }
    }

    private func matchesQuery(_ acc: PriorityAccount) -> Bool {
        let q = query.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        return q.isEmpty || "\(acc.name) #\(acc.id)".lowercased().contains(q)
    }

    private func moveItem(platform: String, id: Int, targetIndex: Int) {
        guard let grp = platforms.first(where: { $0.platform == platform }) else { return }
        let existing = drafts[platform]
        let accounts = existing?.accounts ?? grp.items
        guard accounts.first(where: { $0.id == id })?.editable == true else { return }
        let width = max(1, min(8, existing?.parallelism ?? grp.parallelism ?? 1))
        let currentSlots = existing?.slots ?? (width == 1 && existing != nil
            ? existing!.ids.enumerated().map { idx, item in PrioritySlot(priority: idx + 1, id: item) }
            : PriorityDraftLogic.prioritySlots(accounts: accounts, width: width))
        let currentIds = currentSlots.compactMap(\.id)

        if width > 1 {
            let nextSlots = PriorityDraftLogic.movePrioritySlot(slots: currentSlots, id: id, target: targetIndex, width: width)
            guard nextSlots != currentSlots else { return }
            history[platform, default: []].append(existing)
            drafts[platform] = PriorityPlatformDraft(
                ids: nextSlots.compactMap(\.id),
                version: existing?.version ?? grp.version,
                accounts: accounts,
                parallelism: width,
                slots: nextSlots
            )
        } else {
            let nextIds = PriorityDraftLogic.movePriority(ids: currentIds, id: id, target: targetIndex)
            guard nextIds != currentIds else { return }
            history[platform, default: []].append(existing)
            drafts[platform] = PriorityPlatformDraft(
                ids: nextIds,
                version: existing?.version ?? grp.version,
                accounts: accounts,
                parallelism: 1
            )
        }
        notices[platform] = ""
    }

    private func undoDraft(platform: String) {
        guard var stack = history[platform], !stack.isEmpty else { return }
        let prev = stack.removeLast()
        history[platform] = stack
        if let prev {
            drafts[platform] = prev
        } else {
            drafts.removeValue(forKey: platform)
        }
    }

    private func showTransientResult(platform: String, message: String) {
        results[platform] = message
        Task { @MainActor in
            try? await Task.sleep(for: .seconds(3))
            guard !Task.isCancelled else { return }
            if results[platform] == message {
                results.removeValue(forKey: platform)
            }
        }
    }

    private func readPriorities(repreparePlatform: String? = nil) async {
        guard store.state.online else { return }
        loading = true
        error = ""
        defer { loading = false }
        do {
            let loaded: PriorityViewEnvelope = try await RustBridge.shared.api("GET", "/account-priorities?schema=2")
            view = loaded
            if let rep = repreparePlatform {
                drafts.removeValue(forKey: rep)
                history[rep] = []
                notices[rep] = "已重新读取实际优先级，请核对后应用"
            }
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func updateParallelism(group: PriorityPlatformGroup, next: Int) async {
        let clamped = max(1, min(8, next))
        settingParallelism = group.platform
        error = ""
        defer { settingParallelism = "" }
        do {
            let updated: PriorityViewEnvelope = try await RustBridge.shared.api(
                "PUT",
                "/account-priorities?schema=2",
                body: .object([
                    "platform": .string(group.platform),
                    "parallelism": .int(Int64(clamped)),
                    "expected_version": .string(group.version),
                ])
            )
            view = updated
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func applyOrder(group: PriorityPlatformGroup, ids: [Int], targets: [Int], parallelism: Int, expectedVersion: String) async {
        submittingPlatform = group.platform
        error = ""
        defer { submittingPlatform = "" }
        var body: [String: JSONValue] = [
            "platform": .string(group.platform),
            "ordered_account_ids": .array(ids.map { .int(Int64($0)) }),
            "parallelism": .int(Int64(parallelism)),
            "expected_version": .string(expectedVersion),
            "request_id": .string(UUID().uuidString.lowercased()),
            "client_id": .string(store.operations.clientId()),
        ]
        if parallelism > 1 {
            body["target_priorities"] = .array(targets.map { .int(Int64($0)) })
        }
        do {
            let task: PriorityTask = try await RustBridge.shared.api("POST", "/account-priority-orders?schema=2", body: .object(body))
            drafts.removeValue(forKey: group.platform)
            history[group.platform] = []
            if task.isPending {
                view?.active_task = task
            } else {
                await readPriorities()
                let label = Self.taskLabels[task.status] ?? "已完成"
                showTransientResult(platform: group.platform, message: "\(label) · \(task.verified_count)/\(task.total) 已确认")
                store.refresh()
            }
        } catch {
            showTransientResult(platform: group.platform, message: error.localizedDescription)
        }
    }

    private func pollActiveTask(id: String) async {
        while !Task.isCancelled && store.state.online {
            try? await Task.sleep(for: .seconds(1))
            guard !Task.isCancelled, store.state.online else { return }
            do {
                let next: PriorityTask = try await RustBridge.shared.api("GET", "/account-priority-orders/\(id)?schema=2&details=0")
                if !next.isPending {
                    await readPriorities()
                    let label = Self.taskLabels[next.status] ?? "已完成"
                    showTransientResult(platform: next.platform, message: "\(label) · \(next.verified_count)/\(next.total) 已确认")
                    store.refresh()
                    return
                }
                view?.active_task = next
            } catch {
                self.error = error.localizedDescription
                return
            }
        }
    }

    private func cancelTask(_ task: PriorityTask) async {
        do {
            let updated: PriorityTask = try await RustBridge.shared.api("POST", "/account-priority-orders/\(task.id)/cancel?schema=2", body: .object([:]))
            view?.active_task = updated.isPending ? updated : nil
            if !updated.isPending {
                await readPriorities()
            }
        } catch {
            self.error = error.localizedDescription
        }
    }
}
