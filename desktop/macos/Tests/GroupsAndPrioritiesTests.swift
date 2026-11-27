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
                return "{\"pending\":0,\"items\":[]}"
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

    @Test("PriorityDraftLogic.applyMove rejects dragging editable A across locked B in single lane, allows empty tie slots when locked B stays put, and gates on editing")
    @MainActor
    func priorityMoveLockedDisplacementAndEditingGuards() {
        let items = [
            PriorityAccount(id: 1, name: "Editable-A", platform: "openai", priority: 1, editable: true),
            PriorityAccount(id: 2, name: "Locked-B", platform: "openai", priority: 2, editable: false, reason: "由模板锁定"),
            PriorityAccount(id: 3, name: "Editable-C", platform: "openai", priority: 3, editable: true),
        ]
        let singleLaneGroup = PriorityPlatformGroup(platform: "openai", items: items, version: "v1", parallelism: 1)

        // 1. Single lane: dragging editable A (idx 0) across locked B (idx 1) to editable C (idx 2) shifts B to priority 1 -> rejected!
        let rejectedAcrossLocked = PriorityDraftLogic.applyMove(
            existing: nil,
            group: singleLaneGroup,
            id: 1,
            targetIndex: 2,
            editing: true
        )
        #expect(rejectedAcrossLocked == nil)

        // 2. Editing gate: offline or active search filter disables move even when no locked account is displaced
        let canEditWhileFiltered = PriorityDraftLogic.canEditPlatform(
            online: true,
            hasView: true,
            loading: false,
            settingParallelism: "",
            query: "Editable",
            conflict: false,
            submittingPlatform: "",
            activeTask: nil,
            platform: "openai"
        )
        #expect(!canEditWhileFiltered)
        let canEditWhileOffline = PriorityDraftLogic.canEditPlatform(
            online: false,
            hasView: true,
            loading: false,
            settingParallelism: "",
            query: "",
            conflict: false,
            submittingPlatform: "",
            activeTask: nil,
            platform: "openai"
        )
        #expect(!canEditWhileOffline)

        // 3. Multi-column (parallelism = 2): moving editable A into C's empty tie slot (priority 3, slot 5) or swapping A and C (slot 4)
        //    leaves locked B at priority 2 -> allowed! Moving A onto locked B's occupied slot (slot 2) displaces B to priority 1 -> rejected!
        let parallelGroup = PriorityPlatformGroup(platform: "openai", items: items, version: "v1", parallelism: 2)
        // Slots: [0:(p1,id1), 1:(p1,nil), 2:(p2,id2), 3:(p2,nil), 4:(p3,id3), 5:(p3,nil), 6:(p4,nil), 7:(p4,nil)]
        let intoEmptySlot = PriorityDraftLogic.applyMove(
            existing: nil,
            group: parallelGroup,
            id: 1,
            targetIndex: 5,
            editing: true
        )
        #expect(intoEmptySlot != nil)
        #expect(intoEmptySlot?.slots?.first(where: { $0.id == 2 })?.priority == 2)
        #expect(intoEmptySlot?.slots?.first(where: { $0.id == 1 })?.priority == 3)

        let swapOntoLocked = PriorityDraftLogic.applyMove(
            existing: nil,
            group: parallelGroup,
            id: 1,
            targetIndex: 2,
            editing: true
        )
        #expect(swapOntoLocked == nil)

        // 4. Visibility gate and shared adaptive poll table
        #expect(OperationStore.adaptivePollDelaysMs == [35, 80, 180, 350, 500])
        let store = AppStore()
        defer { store.stop() }
        store.page = .priorities
        store.applyViewState(ViewState(foreground: true, connected: true, online: true))
        let pageView = PrioritiesPageView(store: store)
        #expect(pageView.isPollingVisible)
        store.page = .accounts
        #expect(!pageView.isPollingVisible)
        store.page = .priorities
        store.applyViewState(ViewState(foreground: false, connected: true, online: true))
        #expect(!pageView.isPollingVisible)
    }
}
}
