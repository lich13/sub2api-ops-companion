import Foundation

public enum GroupMembershipZone: String, CaseIterable, Sendable, Equatable {
    case left
    case both
    case right
    case none
}

public struct GroupDraftEntry: Sendable, Equatable, Identifiable {
    public var id: Int
    public var name: String
    public var version: String
    public var original: [Int]
    public var target: [Int]
    public var scope: [Int]

    public init(id: Int, name: String, version: String, original: [Int], target: [Int], scope: [Int]) {
        self.id = id
        self.name = name
        self.version = version
        self.original = original
        self.target = target
        self.scope = scope
    }
}

public enum GroupDraftLogic {
    public static func sortedUnique(_ values: [Int]) -> [Int] {
        Array(Set(values)).sorted()
    }

    public static func sameIds(_ a: [Int], _ b: [Int]) -> Bool {
        sortedUnique(a) == sortedUnique(b)
    }

    public static func platformGroups(_ groups: [Group], platform: String) -> [Group] {
        groups
            .filter { $0.platform == platform }
            .sorted { a, b in
                let sa = a.sort_order ?? 0
                let sb = b.sort_order ?? 0
                if sa != sb { return sa < sb }
                return a.id < b.id
            }
    }

    public static func membershipZone(memberships: [Int], pair: [Int]) -> GroupMembershipZone {
        guard let first = pair.first else { return .none }
        let inLeft = memberships.contains(first)
        let inRight = pair.count > 1 && memberships.contains(pair[1])
        if inLeft && inRight { return .both }
        if inLeft { return .left }
        if inRight { return .right }
        return .none
    }

    public static func moveAccount(
        drafts: [Int: GroupDraftEntry],
        account: Account,
        pair: [Int],
        zone: GroupMembershipZone
    ) -> [Int: GroupDraftEntry] {
        guard !pair.isEmpty else { return drafts }
        let existing = drafts[account.id]
        let targetPair: [Int]
        switch zone {
        case .both: targetPair = pair
        case .left: targetPair = Array(pair.prefix(1))
        case .right: targetPair = pair.count > 1 ? [pair[1]] : []
        case .none: targetPair = []
        }
        let baseTarget = (existing?.target ?? account.group_ids).filter { !pair.contains($0) }
        let nextTarget = sortedUnique(baseTarget + targetPair)
        let original = existing?.original ?? sortedUnique(account.group_ids)
        var next = drafts
        if sameIds(original, nextTarget) {
            next.removeValue(forKey: account.id)
        } else {
            let ver = existing?.version ?? account.operation_versions["groups"] ?? account.version
            let scope = sortedUnique((existing?.scope ?? []) + pair)
            next[account.id] = GroupDraftEntry(
                id: account.id,
                name: account.name,
                version: ver,
                original: original,
                target: nextTarget,
                scope: scope
            )
        }
        return next
    }

    public static func hasConflict(draft: GroupDraftEntry, accounts: [Account], groups: [Group]) -> Bool {
        guard let account = accounts.first(where: { $0.id == draft.id }) else { return true }
        let liveVer = account.operation_versions["groups"] ?? account.version
        if liveVer != draft.version { return true }
        return draft.scope.contains { gid in
            !groups.contains(where: { $0.id == gid && $0.platform == account.platform })
        }
    }

    public static func hasConflict(
        draft: GroupDraftEntry,
        accountById: [Int: Account],
        groupById: [Int: Group]
    ) -> Bool {
        guard let account = accountById[draft.id] else { return true }
        let liveVer = account.operation_versions["groups"] ?? account.version
        if liveVer != draft.version { return true }
        return draft.scope.contains { gid in
            groupById[gid]?.platform != account.platform
        }
    }
}
