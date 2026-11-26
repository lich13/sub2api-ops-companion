import Foundation

public typealias PriorityAccount = PriorityAccountReply
public typealias PriorityTask = PriorityTaskReply
public typealias PriorityPlatformGroup = PriorityPlatformReply
public typealias PriorityViewEnvelope = PriorityOverviewReply

extension PriorityAccountReply: Identifiable {
    public init(
        id: Int,
        name: String,
        platform: String,
        priority: Int,
        type: String = "oauth",
        version: String = "",
        editable: Bool = true,
        reason: String = "",
        status: String = "active",
        schedulable: Bool = true
    ) {
        self.init(
            id: id,
            name: name,
            platform: platform,
            type: type,
            priority: priority,
            version: version,
            editable: editable,
            reason: reason,
            status: status,
            schedulable: schedulable
        )
    }
}

extension PriorityTaskReply: Identifiable {
    public var isPending: Bool {
        ["queued", "running", "checking"].contains(status)
    }
}

extension PriorityPlatformReply: Identifiable {
    public var id: String { platform }

    public init(
        platform: String,
        items: [PriorityAccountReply],
        version: String = "",
        last_task: PriorityTaskReply? = nil,
        parallelism: Int? = 1
    ) {
        self.init(
            platform: platform,
            parallelism: parallelism,
            version: version,
            items: items,
            last_task: last_task
        )
    }
}

extension PriorityOverviewReply {
    public init(platforms: [PriorityPlatformReply]) {
        self.init(platforms: platforms, active_task: nil)
    }

    public func withActiveTask(_ task: PriorityTaskReply?) -> PriorityOverviewReply {
        PriorityOverviewReply(platforms: platforms, active_task: task)
    }
}

public struct PrioritySlot: Codable, Sendable, Equatable {
    public var priority: Int
    public var id: Int?

    public init(priority: Int, id: Int?) {
        self.priority = priority
        self.id = id
    }
}

public struct PriorityPlatformDraft: Sendable, Equatable {
    public var ids: [Int]
    public var version: String
    public var accounts: [PriorityAccount]
    public var parallelism: Int
    public var slots: [PrioritySlot]?

    public init(ids: [Int], version: String, accounts: [PriorityAccount], parallelism: Int = 1, slots: [PrioritySlot]? = nil) {
        self.ids = ids
        self.version = version
        self.accounts = accounts
        self.parallelism = parallelism
        self.slots = slots
    }
}

public enum PriorityDraftLogic {
    public static func samePriorityAccounts(ids: [Int], accounts: [PriorityAccount]) -> Bool {
        let unique = Set(ids)
        return ids.count == accounts.count && unique.count == ids.count && accounts.allSatisfy { unique.contains($0.id) }
    }

    public static func movePriority(ids: [Int], id: Int, target: Int) -> [Int] {
        var next = ids.filter { $0 != id }
        guard next.count == ids.count - 1, target >= 0, target <= next.count else { return ids }
        next.insert(id, at: target)
        return next
    }

    public static func prioritySlots(accounts: [PriorityAccount], width: Int) -> [PrioritySlot] {
        let clamped = max(1, min(8, width))
        let ordered = accounts.sorted { a, b in
            if a.priority != b.priority { return a.priority < b.priority }
            return a.id < b.id
        }
        if clamped == 1 {
            return ordered.enumerated().map { idx, acc in PrioritySlot(priority: idx + 1, id: acc.id) }
        }
        var groupedKeys: [Int] = []
        var grouped: [Int: [Int]] = [:]
        for acc in ordered {
            if grouped[acc.priority] == nil { groupedKeys.append(acc.priority) }
            grouped[acc.priority, default: []].append(acc.id)
        }
        var slots: [PrioritySlot] = []
        for p in groupedKeys {
            let bucket = grouped[p] ?? []
            let count = max(clamped, bucket.count)
            for idx in 0..<count {
                slots.append(PrioritySlot(priority: p, id: idx < bucket.count ? bucket[idx] : nil))
            }
        }
        let nextP = (groupedKeys.max() ?? 0) + 1
        for _ in 0..<clamped {
            slots.append(PrioritySlot(priority: nextP, id: nil))
        }
        return slots
    }

    public static func movePrioritySlot(slots: [PrioritySlot], id: Int, target: Int, width: Int) -> [PrioritySlot] {
        let clamped = max(1, min(8, width))
        guard let source = slots.firstIndex(where: { $0.id == id }),
              target >= 0,
              target < slots.count,
              source != target
        else {
            return slots
        }
        var next = slots
        next[source].id = next[target].id
        next[target].id = id
        if let lastP = next.last?.priority,
           next.contains(where: { $0.priority == lastP && $0.id != nil })
        {
            for _ in 0..<clamped {
                next.append(PrioritySlot(priority: lastP + 1, id: nil))
            }
        }
        return next
    }

    public static func sortPlatforms(_ groups: [PriorityPlatformGroup]) -> [PriorityPlatformGroup] {
        let order = PlatformMeta.preferredOrder
        return groups.sorted { a, b in
            let ia = order.firstIndex(of: a.platform) ?? order.count
            let ib = order.firstIndex(of: b.platform) ?? order.count
            if ia != ib { return ia < ib }
            return a.platform < b.platform
        }
    }

    public static func shouldUseMultiColumn(platforms: [PriorityPlatformGroup], availableWidth: CGFloat) -> Bool {
        guard platforms.count > 1 && platforms.count <= 3 else { return false }
        let minNeeded = platforms.reduce(CGFloat(0)) { sum, grp in
            let w = max(1, grp.parallelism ?? 1)
            return sum + max(320, CGFloat(w) * 220 + 72)
        } + CGFloat(platforms.count - 1) * 12
        return availableWidth >= max(960, minNeeded)
    }
}
