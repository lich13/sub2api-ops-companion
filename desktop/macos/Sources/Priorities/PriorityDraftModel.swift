import Foundation

public struct PriorityAccount: Codable, Sendable, Equatable, Identifiable {
    public var id: Int
    public var name: String
    public var platform: String
    public var type: String
    public var priority: Int
    public var version: String
    public var editable: Bool
    public var reason: String
    public var status: String
    public var schedulable: Bool

    public init(
        id: Int,
        name: String,
        platform: String,
        type: String = "oauth",
        priority: Int,
        version: String = "",
        editable: Bool = true,
        reason: String = "",
        status: String = "active",
        schedulable: Bool = true
    ) {
        self.id = id
        self.name = name
        self.platform = platform
        self.type = type
        self.priority = priority
        self.version = version
        self.editable = editable
        self.reason = reason
        self.status = status
        self.schedulable = schedulable
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(Int.self, forKey: .id)
        name = try c.decodeIfPresent(String.self, forKey: .name) ?? ""
        platform = try c.decodeIfPresent(String.self, forKey: .platform) ?? ""
        type = try c.decodeIfPresent(String.self, forKey: .type) ?? ""
        priority = try c.decodeIfPresent(Int.self, forKey: .priority) ?? 1
        version = try c.decodeIfPresent(String.self, forKey: .version) ?? ""
        editable = try c.decodeIfPresent(Bool.self, forKey: .editable) ?? true
        reason = try c.decodeIfPresent(String.self, forKey: .reason) ?? ""
        status = try c.decodeIfPresent(String.self, forKey: .status) ?? "active"
        schedulable = try c.decodeIfPresent(Bool.self, forKey: .schedulable) ?? true
    }
}

public struct PriorityOrderItem: Codable, Sendable, Equatable {
    public var account_id: Int
    public var account_name: String
    public var platform: String?
    public var original_priority: Int
    public var target_priority: Int
    public var observed_priority: Int?
    public var status: String
}

public struct PriorityTask: Codable, Sendable, Equatable, Identifiable {
    public var id: String
    public var status: String
    public var cancel_requested: Bool
    public var reason: String
    public var total: Int
    public var change_count: Int
    public var verified_count: Int
    public var created_at: String
    public var updated_at: String
    public var completed_at: String?
    public var items: [PriorityOrderItem]?
    public var platform: String
    public var waiting_reason: String
    public var waiting_account_ids: [Int]
    public var parallelism: Int?

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        status = try c.decodeIfPresent(String.self, forKey: .status) ?? ""
        cancel_requested = try c.decodeIfPresent(Bool.self, forKey: .cancel_requested) ?? false
        reason = try c.decodeIfPresent(String.self, forKey: .reason) ?? ""
        total = try c.decodeIfPresent(Int.self, forKey: .total) ?? 0
        change_count = try c.decodeIfPresent(Int.self, forKey: .change_count) ?? 0
        verified_count = try c.decodeIfPresent(Int.self, forKey: .verified_count) ?? 0
        created_at = try c.decodeIfPresent(String.self, forKey: .created_at) ?? ""
        updated_at = try c.decodeIfPresent(String.self, forKey: .updated_at) ?? ""
        completed_at = try c.decodeIfPresent(String.self, forKey: .completed_at)
        items = try c.decodeIfPresent([PriorityOrderItem].self, forKey: .items)
        platform = try c.decodeIfPresent(String.self, forKey: .platform) ?? ""
        waiting_reason = try c.decodeIfPresent(String.self, forKey: .waiting_reason) ?? ""
        waiting_account_ids = try c.decodeIfPresent([Int].self, forKey: .waiting_account_ids) ?? []
        parallelism = try c.decodeIfPresent(Int.self, forKey: .parallelism)
    }

    public var isPending: Bool {
        ["queued", "running", "checking"].contains(status)
    }
}

public struct PriorityPlatformGroup: Codable, Sendable, Equatable, Identifiable {
    public var platform: String
    public var items: [PriorityAccount]
    public var version: String
    public var last_task: PriorityTask?
    public var parallelism: Int?

    public var id: String { platform }

    public init(platform: String, items: [PriorityAccount], version: String = "", last_task: PriorityTask? = nil, parallelism: Int? = 1) {
        self.platform = platform
        self.items = items
        self.version = version
        self.last_task = last_task
        self.parallelism = parallelism
    }
}

public struct PriorityViewEnvelope: Codable, Sendable, Equatable {
    public var platforms: [PriorityPlatformGroup]
    public var active_task: PriorityTask?

    public init(platforms: [PriorityPlatformGroup], active_task: PriorityTask? = nil) {
        self.platforms = platforms
        self.active_task = active_task
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
