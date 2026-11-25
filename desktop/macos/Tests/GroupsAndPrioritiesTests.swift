import AppKit
import Foundation
import SwiftUI
import Testing
@testable import Sub2OpsMacOS

extension Sub2OpsMacOSTestRoot {
@Suite("Groups & Priorities Logic + UI Snapshots", .serialized)
struct GroupsAndPrioritiesTests {
    @Test("GroupDraftLogic moves accounts across left, both, right, none and clears when restored")
    func groupDraftMovesAndClearsWhenRestored() {
        let acc = Account(id: 101, name: "Codex-01", platform: "openai", group_ids: [1, 9], version: "v1", operation_versions: ["groups": "v1"])
        let groups = [
            Group(id: 1, name: "codex羊毛", platform: "openai"),
            Group(id: 3, name: "codex爽用", platform: "openai"),
            Group(id: 9, name: "其他组", platform: "openai"),
        ]

        var drafts: [Int: GroupDraftEntry] = [:]
        drafts = GroupDraftLogic.moveAccount(drafts: drafts, account: acc, pair: [1, 3], zone: .both)
        #expect(drafts[101]?.target == [1, 3, 9])
        #expect(drafts[101]?.scope == [1, 3])
        #expect(!GroupDraftLogic.hasConflict(draft: drafts[101]!, accounts: [acc], groups: groups))

        // Returning to left-only restores original [1, 9] and removes draft
        drafts = GroupDraftLogic.moveAccount(drafts: drafts, account: acc, pair: [1, 3], zone: .left)
        #expect(drafts[101] == nil)

        // Version change triggers conflict
        drafts = GroupDraftLogic.moveAccount(drafts: drafts, account: acc, pair: [1, 3], zone: .right)
        let mutated = Account(id: 101, name: "Codex-01", platform: "openai", group_ids: [1, 9], version: "v1", operation_versions: ["groups": "v2"])
        #expect(GroupDraftLogic.hasConflict(draft: drafts[101]!, accounts: [mutated], groups: groups))
    }

    @Test("PriorityDraftLogic manages tie slots and multi-column width threshold")
    func priorityDraftTieSlotsAndMultiColumn() {
        let items = [
            PriorityAccount(id: 1, name: "A", platform: "openai", priority: 1),
            PriorityAccount(id: 2, name: "B", platform: "openai", priority: 2),
        ]
        let slots = PriorityDraftLogic.prioritySlots(accounts: items, width: 2)
        #expect(slots.count == 6) // priority 1 (2 slots), priority 2 (2 slots), trailing priority 3 (2 empty slots)

        // Moving id=2 into the empty slot of priority 1 ties them at priority 1
        let moved = PriorityDraftLogic.movePrioritySlot(slots: slots, id: 2, target: 1, width: 2)
        #expect(moved[0] == PrioritySlot(priority: 1, id: 1))
        #expect(moved[1] == PrioritySlot(priority: 1, id: 2))

        let twoPlatforms = [
            PriorityPlatformGroup(platform: "openai", items: items, parallelism: 1),
            PriorityPlatformGroup(platform: "grok", items: items, parallelism: 1),
        ]
        #expect(PriorityDraftLogic.shouldUseMultiColumn(platforms: twoPlatforms, availableWidth: 1020))
        #expect(!PriorityDraftLogic.shouldUseMultiColumn(platforms: twoPlatforms, availableWidth: 800))
    }

    @Test("Render GroupsAndPriorities Light & Dark PNG snapshots when SUB2OPS_SNAPSHOT_DIR is set")
    @MainActor
    func renderM3Snapshots() async throws {
        guard let dirPath = ProcessInfo.processInfo.environment["SUB2OPS_SNAPSHOT_DIR"],
              !dirPath.isEmpty
        else {
            return
        }
        let baseURL = URL(fileURLWithPath: dirPath)

        let priorityMockJSON = """
        {
          "platforms": [
            {
              "platform": "openai",
              "version": "v1",
              "parallelism": 1,
              "items": [
                {"id": 101, "name": "Codex · 主力", "platform": "openai", "type": "oauth", "priority": 1, "version": "v1", "editable": true, "reason": "", "status": "active", "schedulable": true},
                {"id": 102, "name": "Codex · 备用账号", "platform": "openai", "type": "oauth", "priority": 2, "version": "v1", "editable": true, "reason": "", "status": "active", "schedulable": true}
              ]
            },
            {
              "platform": "grok",
              "version": "v1",
              "parallelism": 2,
              "items": [
                {"id": 201, "name": "Grok · Super", "platform": "grok", "type": "oauth", "priority": 1, "version": "v1", "editable": true, "reason": "", "status": "active", "schedulable": true},
                {"id": 391, "name": "Grok · Free", "platform": "grok", "type": "oauth", "priority": 1, "version": "v1", "editable": true, "reason": "", "status": "active", "schedulable": true}
              ]
            }
          ],
          "active_task": null
        }
        """
        RustBridge.shared.setMockHandler { _, payload in
            if payload.contains("/account-operations") {
                return "{\"items\":[]}"
            }
            return priorityMockJSON
        }

        let acc1 = Account(id: 101, name: "Codex · 主力", priority: 1, platform: "openai", type: "oauth", group_ids: [1], operation_versions: ["groups": "v1"])
        let acc2 = Account(id: 102, name: "Codex · 备用账号", priority: 2, platform: "openai", type: "oauth", group_ids: [1, 3], degradation_mark: DegradationMark(marked: true))
        let acc3 = Account(id: 104, name: "Codex · 独立爽用", priority: 3, platform: "openai", type: "oauth", group_ids: [3])
        let acc4 = Account(id: 105, name: "Codex · 待分组池", priority: 4, platform: "openai", type: "apikey", group_ids: [])
        let acc5 = Account(id: 201, name: "Grok · Super", priority: 1, platform: "grok", type: "oauth", group_ids: [2])
        let acc6 = Account(id: 391, name: "Grok · Free", priority: 1, platform: "grok", type: "oauth", group_ids: [2])

        let groups = [
            Group(id: 1, name: "codex羊毛", platform: "openai"),
            Group(id: 3, name: "codex爽用", platform: "openai"),
            Group(id: 2, name: "grok爽用", platform: "grok"),
        ]

        let groupStore = AppStore()
        let prioStore = AppStore()
        defer {
            groupStore.stop()
            prioStore.stop()
            RustBridge.shared.setMockHandler(nil)
        }
        groupStore.ready = true
        groupStore.page = .groups
        groupStore.applyViewState(
            ViewState(
                connected: true,
                online: true,
                snapshot: Snapshot(
                    observed_at: "2026-11-17T09:15:00Z",
                    accounts: [acc1, acc2, acc3, acc4, acc5, acc6],
                    groups: groups
                ),
                preferences: Preferences(
                    base_url: "https://demo.example/sub2ops",
                    connections: [SavedConnection(name: "demo.example", base_url: "https://demo.example/sub2ops")]
                )
            )
        )

        prioStore.ready = true
        prioStore.page = .priorities
        prioStore.applyViewState(groupStore.state)

        for dark in [false, true] {
            let suffix = dark ? "dark" : "light"
            try SnapshotRenderUtility.renderPNG(
                view: MainShellView(store: groupStore),
                size: CGSize(width: 1310, height: 860),
                dark: dark,
                outputURL: baseURL.appendingPathComponent("m3_groups_\(suffix).png")
            )
            try await SnapshotRenderUtility.renderPNGAsync(
                view: MainShellView(store: prioStore),
                size: CGSize(width: 1310, height: 860),
                dark: dark,
                outputURL: baseURL.appendingPathComponent("m3_priorities_\(suffix).png")
            )
        }
    }
}
}
