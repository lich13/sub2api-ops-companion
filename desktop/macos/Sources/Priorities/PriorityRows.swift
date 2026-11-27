import SwiftUI

private struct ConditionalPriorityDragModifier: ViewModifier {
    let enabled: Bool
    let payload: String

    @ViewBuilder
    func body(content: Content) -> some View {
        if enabled {
            content.draggable(payload)
        } else {
            content
        }
    }
}

extension PrioritiesPageView {
    @ViewBuilder
    func singleLaneRow(
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
            .modifier(ConditionalPriorityDragModifier(enabled: editing && acc.editable, payload: "\(platform):\(acc.id)"))
            .dropDestination(for: String.self) { items, _ in
                guard editing,
                      let raw = items.first, raw.hasPrefix("\(platform):"),
                      let draggedId = Int(raw.dropFirst(platform.count + 1))
                else { return false }
                return moveItem(platform: platform, id: draggedId, targetIndex: slotIndex)
            }
        }
    }

    @ViewBuilder
    func parallelSlotCard(
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
                .modifier(ConditionalPriorityDragModifier(enabled: editing && acc.editable, payload: "\(platform):\(acc.id)"))
                .dropDestination(for: String.self) { items, _ in
                    guard editing,
                          let raw = items.first, raw.hasPrefix("\(platform):"),
                          let draggedId = Int(raw.dropFirst(platform.count + 1))
                    else { return false }
                    return moveItem(platform: platform, id: draggedId, targetIndex: slotIndex)
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
                guard editing,
                      let raw = items.first, raw.hasPrefix("\(platform):"),
                      let draggedId = Int(raw.dropFirst(platform.count + 1))
                else { return false }
                return moveItem(platform: platform, id: draggedId, targetIndex: slotIndex)
            }
        }
    }

    func groupLanes(slots: [PrioritySlot]) -> [(priority: Int, slots: [(index: Int, slot: PrioritySlot)])] {
        var order: [Int] = []
        var map: [Int: [(index: Int, slot: PrioritySlot)]] = [:]
        for (idx, s) in slots.enumerated() {
            if map[s.priority] == nil { order.append(s.priority) }
            map[s.priority, default: []].append((idx, s))
        }
        return order.map { ($0, map[$0] ?? []) }
    }

    func matchesQuery(_ acc: PriorityAccount) -> Bool {
        let q = query.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        return q.isEmpty || "\(acc.name) #\(acc.id)".lowercased().contains(q)
    }
}
