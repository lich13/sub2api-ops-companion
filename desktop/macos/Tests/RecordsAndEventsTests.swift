import AppKit
import os
import Foundation
import SwiftUI
import Testing
@testable import Sub2OpsMacOS

extension Sub2OpsMacOSTestRoot {
@Suite("Records & Events Logic + UI Snapshots", .serialized)
struct RecordsAndEventsTests {
    @Test("RecordFormatting handles scientific notation decimals, output TPS, and Beijing date presets")
    func recordFormattingAndBeijingDatePresets() {
        #expect(RecordFormatting.money("1.25e-5", digits: 8) == "$0.00001250")
        #expect(RecordFormatting.money("0.1234567", digits: 6) == "$0.123457")
        #expect(RecordFormatting.decimal(nil, digits: 2) == "—")
        #expect(RecordFormatting.cacheTokenCount(46_900_000) == "46.9M")

        // 2026-10-01 01:00:00 Beijing = 2026-09-30 17:00:00 UTC
        let ref = BeijingTime.parseISO("2026-09-30T17:00:00Z")!
        let today = BeijingDateLogic.presetRange(.today, now: ref)
        #expect(today.start == "2026-10-01" && today.end == "2026-10-01")
        let lastMonth = BeijingDateLogic.presetRange(.lastMonth, now: ref)
        #expect(lastMonth.start == "2026-09-01" && lastMonth.end == "2026-09-30")
        #expect(!BeijingDateLogic.isValidRange(start: "2026-02-30", end: "2026-03-01"))

        let midnight = BeijingTime.parseISO("2026-10-01T16:00:00Z")!
        var range = today
        let deferred = range.refreshPreset(at: midnight, holding: true)
        #expect(deferred)
        #expect(range == today)
        let applied = range.refreshPreset(at: midnight, holding: false)
        #expect(!applied)
        #expect(range.start == "2026-10-02")
        var custom = RecordDateRange(preset: nil, start: today.start, end: today.end)
        let customPending = custom.refreshPreset(at: midnight, holding: false)
        #expect(!customPending)
        #expect(custom.start == today.start && custom.end == today.end)

        let defaultCols = RecordColumnSpec.normalize(nil)
        #expect(defaultCols.count == 10)
        // Default 1310pt window minus the sidebar, margins and always-visible scroller.
        #expect(RecordColumnSpec.userColumnWidth + 1 + RecordColumnSpec.dataWidth(columns: defaultCols) <= 1124)
    }

    @Test("Records pagination loads the next page 200pt before the footer scrolls into view")
    func recordsPaginationLookahead() {
        #expect(RecordPagination.footerNear(footerMinY: 800, viewportHeight: 600))
        #expect(!RecordPagination.footerNear(footerMinY: 800.5, viewportHeight: 600))
        #expect(RecordPagination.footerNear(footerMinY: 120, viewportHeight: 600))

        #expect(RecordPagination.autoLoads(footerNear: true, active: true, detailOpen: false))
        #expect(!RecordPagination.autoLoads(footerNear: false, active: true, detailOpen: false))
        #expect(!RecordPagination.autoLoads(footerNear: true, active: false, detailOpen: false))
        #expect(!RecordPagination.autoLoads(footerNear: true, active: true, detailOpen: true))
    }

    @MainActor
    private func waitFor(_ condition: () -> Bool) async -> Bool {
        let deadline = ContinuousClock.now + .seconds(3)
        while !condition() && ContinuousClock.now < deadline {
            try? await Task.sleep(for: .milliseconds(10))
        }
        return condition()
    }

    @Test("Changing the search discards an already-dispatched option page")
    @MainActor
    func searchWhilePaging() async throws {
        for kind in [RecordOptionPickerView.Kind.users, .api_keys] {
            let latePage = OSAllocatedUnfairLock<CheckedContinuation<String, Never>?>(initialState: nil)
            let directory = RecordOptionDirectory()
            RustBridge.shared.setMockHandler { _, payload in
                if payload.contains("cursor=more") {
                    return await withCheckedContinuation { reply in latePage.withLock { $0 = reply } }
                }
                if payload.contains("q=recent"), kind == .users || payload.contains("user_id=23") {
                    return #"{"items":[{"id":7,"email":"recent@example.invalid","deleted":false}],"next_cursor":null}"#
                }
                return #"{"items":[{"id":1,"deleted":false}],"next_cursor":"more"}"#
            }
            defer { directory.stop(); RustBridge.shared.setMockHandler(nil) }
            directory.search(kind: kind, query: "", userId: 4)
            #expect(await waitFor { directory.cursor == "more" })
            directory.loadMoreOrRetry()
            #expect(await waitFor { latePage.withLock { $0 != nil } })
            directory.search(kind: kind, query: "recent", userId: 23)
            #expect(await waitFor { directory.items.map(\.id) == [7] })
            latePage.withLock { $0 }?.resume(returning: #"{"items":[{"id":2,"deleted":false}],"next_cursor":"obsolete"}"#)
            try await Task.sleep(for: .milliseconds(30))
            #expect(directory.items.map(\.id) == [7] && directory.cursor == nil && !directory.loading)
        }
    }

    @Test("New records wait during reading and refresh the head on return to the top")
    @MainActor
    func recordReadingPosition() async {
        let heads = OSAllocatedUnfairLock(initialState: 9)
        RustBridge.shared.setMockHandler { _, payload in
            if payload.contains("after_id=") { return #"{"new_count":1,"latest_id":11}"# }
            let page = payload.contains("cursor=more")
            let id = page ? 8 : heads.withLock { $0 += 1; return $0 }
            let cursor = page ? "null" : #""more""#
            return #"{"items":[{"id":\#(id)}],"next_cursor":\#(cursor),"latest_id":\#(id),"observed_at":"2026-11-20T00:01:00Z"}"#
        }
        let feed = RecordFeedStore()
        defer { feed.stop(); RustBridge.shared.setMockHandler(nil) }
        feed.bind(query: "limit=50", active: true)
        #expect(await waitFor { feed.items.map(\.id) == [10] })
        feed.holding = true
        await feed.loadMore()
        feed.bind(query: "limit=50", active: true)
        #expect(await waitFor { feed.newCount == 1 })
        #expect(feed.items.map(\.id) == [10, 8])
        feed.holding = false
        feed.bind(query: "limit=50", active: true)
        #expect(await waitFor { feed.items.map(\.id) == [11] && feed.newCount == 0 })
    }

    @Test("Render native short and scrollable record dropdowns when snapshot output is enabled")
    @MainActor
    func renderRecordPopovers() async throws {
        guard let dir = ProcessInfo.processInfo.environment["SUB2OPS_SNAPSHOT_DIR"] else { return }
        for (count, dark) in [(1, true), (30, false)] {
            let options = (1...count).map { id in
                #"{"id":\#(id),"name":"用户 \#(id)","email":"user\#(id)@example.invalid","deleted":false}"#
            }.joined(separator: ",")
            RustBridge.shared.setMockHandler { _, _ in #"{"items":[\#(options)],"next_cursor":null}"# }
            let view = RecordOptionPickerView(kind: .users, active: true, selected: nil) { _ in }
                .environment(\.colorScheme, dark ? .dark : .light)
            let window = SyntheticClick.host(view, size: CGSize(width: 250, height: 100))
            defer { SyntheticClick.teardown(window); RustBridge.shared.setMockHandler(nil) }
            window.setFrameOrigin(CGPoint(x: 200, y: 450))
            window.appearance = NSAppearance(named: dark ? .darkAqua : .aqua)
            let original = Set(NSApp.windows.filter(\.isVisible).map(ObjectIdentifier.init))
            SyntheticClick.click(window, at: CGPoint(x: 125, y: 50))
            try await Task.sleep(for: .milliseconds(400))
            let popover = try #require(NSApp.windows.first { $0.isVisible && !original.contains(ObjectIdentifier($0)) && $0.frame.width > 320 })
            let content = try #require(popover.contentView)
            content.layoutSubtreeIfNeeded()
            #expect(count == 1 ? (105..<200).contains(content.bounds.height) : (275..<360).contains(content.bounds.height))
            let rep = try #require(content.bitmapImageRepForCachingDisplay(in: content.bounds))
            content.cacheDisplay(in: content.bounds, to: rep)
            let png = try #require(rep.representation(using: .png, properties: [:]))
            try png.write(to: URL(fileURLWithPath: dir).appendingPathComponent("m4_picker_\(count).png"))
            popover.orderOut(nil)
        }
    }

    @Test("An open bank follows a slow update and stops following the previous source or connection")
    @MainActor
    func renderBankTracking() async throws {
        guard let dir = ProcessInfo.processInfo.environment["SUB2OPS_SNAPSHOT_DIR"] else { return }
        let counts = OSAllocatedUnfairLock(initialState: [String: Int]())
        RustBridge.shared.setMockHandler { _, payload in
            guard payload.contains("/fingerprint-bank") else { return "null" }
            let lm = payload.contains("/lm-detector/")
            let name = lm ? "lm" : "trace"
            let count = counts.withLock { $0[name, default: 0] += 1; return $0[name]! }
            let status = lm && count > 16 ? "idle" : "syncing"
            return #"{"version":{"revision":"\#(lm ? "bbbbbbbb" : "aaaaaaaa")"},"status":"\#(status)","source":"remote","result":"updated"}"#
        }
        defer { RustBridge.shared.setMockHandler(nil) }
        func panel(_ engine: ModelTestEngine, _ connection: String, online: Bool = true) -> some View {
            FingerprintBankStatusView(online: online, connectionKey: connection, engines: [engine])
                .padding(16).frame(width: 418, height: 120).background(DS.surface)
        }
        SnapshotRenderUtility.ensureAppKit()
        let view = NSHostingView(rootView: panel(.modeltrace, "first"))
        let window = NSWindow(contentRect: NSRect(x: -20000, y: -20000, width: 418, height: 120),
                              styleMask: .borderless, backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.contentView = view
        window.orderFrontRegardless()
        defer { SnapshotRenderUtility.teardownWindow(window) }
        #expect(await waitFor { counts.withLock { $0["trace", default: 0] >= 1 } })
        view.rootView = panel(.lm_detector, "first")
        #expect(await waitFor { counts.withLock { $0["lm", default: 0] >= 1 } })
        let traceReads = counts.withLock { $0["trace"] }
        let end = ContinuousClock.now + .seconds(21)
        while counts.withLock({ $0["lm", default: 0] < 17 }) && ContinuousClock.now < end {
            try await Task.sleep(for: .milliseconds(100))
        }
        let finished = counts.withLock { $0 }
        #expect(finished["lm"] == 17)
        #expect(finished["trace"] == traceReads)
        try await Task.sleep(for: .milliseconds(1100))
        #expect(counts.withLock { $0["lm"] } == 17)
        view.layoutSubtreeIfNeeded()
        let rep = try #require(view.bitmapImageRepForCachingDisplay(in: view.bounds))
        view.cacheDisplay(in: view.bounds, to: rep)
        try #require(rep.representation(using: .png, properties: [:])).write(
            to: URL(fileURLWithPath: dir).appendingPathComponent("m4_bank_updated.png"))
        view.rootView = panel(.modeltrace, "second")
        #expect(await waitFor { counts.withLock { $0["trace"] != traceReads } })
        view.rootView = panel(.modeltrace, "second", online: false)
        let beforeClose = counts.withLock { $0 }
        try await Task.sleep(for: .milliseconds(1150))
        #expect(counts.withLock { $0 } == beforeClose)
    }

    @Test("Render RecordsAndEvents Light & Dark PNG snapshots when SUB2OPS_SNAPSHOT_DIR is set")
    @MainActor
    func renderM4Snapshots() async throws {
        guard let dirPath = ProcessInfo.processInfo.environment["SUB2OPS_SNAPSHOT_DIR"],
              !dirPath.isEmpty
        else {
            return
        }
        let baseURL = URL(fileURLWithPath: dirPath)

        let recordsJSON = """
        {
          "items": [
            {
              "id": 200,
              "created_at": "2026-11-17T09:10:00Z",
              "user_id": 12,
              "user_name": "研发主力组",
              "api_key_id": 45,
              "api_key_name": "CI-Pipeline-Key",
              "account_id": 101,
              "account_name": "Codex · 主力",
              "group_id": 1,
              "group_name": "Codex",
              "model": "gpt-6-astra",
              "requested_model": "gpt-6",
              "upstream_model": "gpt-6-astra",
              "upstream_response_model": "gpt-6-astra",
              "upstream_model_mismatch": false,
              "reasoning_effort": "high",
              "requested_reasoning_effort": "high",
              "request_type": "stream",
              "native_compaction_v2": true,
              "input_tokens": 18420,
              "output_tokens": 2460,
              "cache_read_tokens": 14000,
              "cache_creation_tokens": 1200,
              "input_cost": "0.03684",
              "output_cost": "0.03690",
              "total_cost": "0.07374",
              "actual_cost": "0.07374",
              "account_cost": "0.058992",
              "rate_multiplier": "1.0",
              "account_rate_multiplier": "0.8",
              "first_token_ms": 1420,
              "duration_ms": 18600,
              "user_agent": "CodexCLI/0.42",
              "ip_address": "198.51.100.24"
            },
            {
              "id": 199,
              "created_at": "2026-11-17T09:09:12Z",
              "user_id": 12,
              "user_name": "研发主力组",
              "api_key_id": 45,
              "api_key_name": "CI-Pipeline-Key",
              "account_id": 101,
              "account_name": "Codex · 主力",
              "group_id": 1,
              "group_name": "Codex",
              "model": "gpt-6-astra",
              "requested_model": "gpt-6-astra",
              "upstream_model": "gpt-6-astra",
              "upstream_response_model": "gpt-6-astra",
              "upstream_model_mismatch": false,
              "reasoning_effort": "medium",
              "requested_reasoning_effort": "high",
              "request_type": "ws_v2",
              "native_compaction_v2": true,
              "input_tokens": 9200,
              "output_tokens": 1120,
              "cache_read_tokens": 8000,
              "cache_creation_tokens": 0,
              "input_cost": "0.01840",
              "output_cost": "0.01680",
              "total_cost": "0.03520",
              "actual_cost": "0.03520",
              "account_cost": "0.028160",
              "rate_multiplier": "1.0",
              "account_rate_multiplier": "0.8",
              "first_token_ms": 860,
              "duration_ms": 9400,
              "user_agent": "CodexCLI/0.42",
              "ip_address": "198.51.100.24"
            }
          ],
          "next_cursor": null,
          "latest_id": 200,
          "observed_at": "2026-11-17T09:12:00Z",
          "summary": { "actual_cost": "12.480512" }
        }
        """

        let logsJSON = """
        {
          "items": [
            {
              "job_id": "job-demo-1",
              "account_id": 101,
              "account_name": "Codex · 主力",
              "model_id": "gpt-6-luna",
              "status": "completed",
              "created_at": "2026-11-17T09:05:00Z",
              "started_at": "2026-11-17T09:04:40Z",
              "completed_at": "2026-11-17T09:05:00Z",
              "triggers": ["scheduled"],
              "error": "",
              "details_available": true,
              "summary": {
                "engine": "modeltrace",
                "sample_count": 3,
                "outcome": "normal",
                "analysis_available": true,
                "first_prediction": "gpt-6-luna",
                "prediction": "gpt-6-luna",
                "probability": 0.964,
                "first_probability": 0.941
              },
              "notification": {
                "status": "delivered",
                "merged": false,
                "delivered_at": "2026-11-17T09:05:01Z",
                "reason": ""
              }
            },
            {
              "job_id": "job-demo-2",
              "account_id": 101,
              "account_name": "Codex · 主力",
              "model_id": "gpt-6.2-sol",
              "status": "completed",
              "created_at": "2026-11-17T09:08:00Z",
              "started_at": "2026-11-17T09:07:45Z",
              "completed_at": "2026-11-17T09:08:00Z",
              "triggers": ["recovery"],
              "error": "",
              "details_available": true,
              "summary": {
                "engine": "lm_detector",
                "sample_count": 3,
                "verdict_mode": "relaxed",
                "degraded_model_id": "gpt-5.6-luna",
                "outcome": "degraded",
                "analysis_available": true,
                "first_prediction": "gpt-5.6-luna",
                "prediction": "gpt-5.6-luna",
                "probability": 0.912,
                "first_probability": 0.884,
                "disposition": {
                  "status": "marked",
                  "basis": "sample_family",
                  "group_indexes": [1, 2],
                  "marked": true,
                  "schedule_verified": true,
                  "reason": ""
                },
                "verification": {
                  "status": "failed",
                  "reason": "命中降智指纹"
                }
              },
              "notification": {
                "status": "delivered",
                "merged": true,
                "delivered_at": "2026-11-17T09:08:02Z",
                "reason": ""
              }
            }
          ],
          "next_cursor": null
        }
        """

        let errorsJSON = """
        {
          "items": [
            {
              "id": 901,
              "created_at": "2026-11-17T09:02:00Z",
              "status_code": 429,
              "account_id": 101,
              "account_name": "Codex · 主力",
              "model": "gpt-6-astra",
              "provider_error_code": "usage_limit_reached",
              "message": "You have reached your weekly usage limit. Please wait until reset."
            }
          ],
          "next_cursor": null
        }
        """

        let recoveriesJSON = """
        {
          "items": [
            {
              "id": 301,
              "account_id": 101,
              "account_name": "Codex · 主力",
              "model_id": "gpt-6-sol",
              "test_completed_at": "2026-11-17T11:43:05Z",
              "recovered_at": null,
              "legacy": false,
              "kind": "connection_test",
              "result_status": "completed",
              "attempt_count": 1,
              "reset_credit": null
            },
            {
              "id": 302,
              "account_id": 101,
              "account_name": "Codex · 主力",
              "model_id": "gpt-6-luna",
              "test_completed_at": "2026-11-17T11:43:05Z",
              "recovered_at": "2026-11-17T11:50:05Z",
              "legacy": false,
              "kind": "reset_credit",
              "result_status": "completed",
              "attempt_count": 3,
              "reset_credit": {
                "consumed": true,
                "completed_at": "2026-11-17T11:43:05Z",
                "verification_method": "model_test"
              }
            }
          ],
          "next_cursor": null
        }
        """

        RustBridge.shared.setMockHandler { _, payload in
            if payload.contains("/detection-logs") { return logsJSON }
            if payload.contains("/recoveries") { return recoveriesJSON }
            if payload.contains("/errors") { return errorsJSON }
            return recordsJSON
        }

        let acc1 = Account(id: 101, name: "Codex · 主力", priority: 1, platform: "openai", type: "oauth")
        let recStore = AppStore()
        let eventsStore = AppStore()
        defer {
            recStore.stop()
            eventsStore.stop()
            RustBridge.shared.setMockHandler(nil)
        }
        recStore.ready = true
        recStore.page = .records
        recStore.applyViewState(
            ViewState(
                connected: true,
                online: true,
                snapshot: Snapshot(observed_at: "2026-11-17T09:12:00Z", accounts: [acc1]),
                preferences: Preferences(
                    base_url: "https://demo.example/sub2ops",
                    connections: [SavedConnection(name: "demo.example", base_url: "https://demo.example/sub2ops")]
                )
            )
        )

        eventsStore.ready = true
        eventsStore.page = .events
        eventsStore.applyViewState(recStore.state)

        var detail = try #require(JSONDecoder().decode(RecordPageEnvelope.self, from: Data(recordsJSON.utf8)).items.first)
        detail.upstream_model_mismatch = true
        detail.user_agent = "FixtureDesktop/1.0 (Macintosh; arm64) " + String(repeating: "LongUserAgent/", count: 8)
        detail.request_id = "client:" + String(repeating: "123456789abcdef", count: 5)
        detail.upstream_endpoint = "/v1/" + String(repeating: "long-path/", count: 7) + "responses"
        detail.service_tier = "priority"
        detail.billing_mode = "token"
        detail.cache_creation_5m_tokens = 900
        detail.cache_creation_1h_tokens = 300
        detail.image_count = 1
        detail.video_count = 1
        try await SnapshotRenderUtility.renderPNGAsync(
            view: MainShellView(store: recStore), size: CGSize(width: 980, height: 680), dark: false,
            outputURL: baseURL.appendingPathComponent("m4_records_narrow.png")
        )
        for dark in [false, true] {
            try await SnapshotRenderUtility.renderPNGAsync(
                view: RecordDetailDrawerView(row: detail, loading: false, error: "", onClose: {}),
                size: CGSize(width: 460, height: 1360), dark: dark,
                outputURL: baseURL.appendingPathComponent(dark ? "m4_detail_dark.png" : "m4_detail_light.png")
            )

            let suffix = dark ? "dark" : "light"
            try await SnapshotRenderUtility.renderPNGAsync(
                view: MainShellView(store: recStore),
                size: CGSize(width: 1310, height: 860),
                dark: dark,
                outputURL: baseURL.appendingPathComponent("m4_records_\(suffix).png")
            )
            try await SnapshotRenderUtility.renderPNGAsync(
                view: MainShellView(store: eventsStore),
                size: CGSize(width: 1310, height: 860),
                dark: dark,
                outputURL: baseURL.appendingPathComponent("m4_events_errors_\(suffix).png")
            )
            let logsView = EventsPageView(store: eventsStore, initialTab: .logs)
                .padding(24)
                .frame(width: 1090, height: 420, alignment: .topLeading)
                .background(DS.canvas)
            try await SnapshotRenderUtility.renderPNGAsync(
                view: logsView,
                size: CGSize(width: 1090, height: 420),
                dark: dark,
                outputURL: baseURL.appendingPathComponent("m4_detection_logs_\(suffix).png")
            )
            let recoveriesView = EventsPageView(store: eventsStore, initialTab: .recoveries)
                .padding(24)
                .frame(width: 1090, height: 360, alignment: .topLeading)
                .background(DS.canvas)
            try await SnapshotRenderUtility.renderPNGAsync(
                view: recoveriesView,
                size: CGSize(width: 1090, height: 360),
                dark: dark,
                outputURL: baseURL.appendingPathComponent("m4_recoveries_\(suffix).png")
            )
        }
        recStore.page = .settings
        eventsStore.page = .settings

        // Also render 25-row vertical scroll case to ensure sticky header + sticky left column + footer stay intact
        let manyItems = (1...25).map { idx in
            """
            {
              "id": \(300 + idx),
              "created_at": "2026-11-17T09:\(String(format: "%02d", idx % 60)):00Z",
              "user_id": \(10 + idx),
              "user_name": "并发测试组-\(idx)",
              "api_key_id": \(40 + idx),
              "api_key_name": "Key-\(idx)",
              "account_id": 101,
              "account_name": "Codex · 主力",
              "group_id": 1,
              "group_name": "Codex",
              "model": "gpt-6-astra",
              "requested_model": "gpt-6",
              "upstream_model": "gpt-6-astra",
              "upstream_response_model": "gpt-6-astra",
              "upstream_model_mismatch": false,
              "reasoning_effort": "high",
              "request_type": "stream",
              "native_compaction_v2": true,
              "input_tokens": 18420,
              "output_tokens": 2460,
              "cache_read_tokens": 14000,
              "cache_creation_tokens": 1200,
              "total_cost": "0.07374",
              "actual_cost": "0.07374",
              "account_cost": "0.058992",
              "rate_multiplier": "1.0",
              "account_rate_multiplier": "0.8",
              "first_token_ms": 1420,
              "duration_ms": 18600,
              "user_agent": "Fixture Desktop/1.0",
              "ip_address": "198.51.100.254"
            }
            """
        }.joined(separator: ",")
        let manyRecordsJSON = """
        {
          "items": [\(manyItems)],
          "next_cursor": "cursor-page-2",
          "latest_id": 325,
          "observed_at": "2026-11-17T09:30:00Z",
          "summary": { "actual_cost": "88.240000" }
        }
        """
        RustBridge.shared.setMockHandler { _, _ in manyRecordsJSON }
        let scrollRecStore = AppStore()
        defer { scrollRecStore.stop() }
        scrollRecStore.ready = true
        scrollRecStore.page = .records
        scrollRecStore.applyViewState(eventsStore.state)
        try await SnapshotRenderUtility.renderPNGAsync(
            view: MainShellView(store: scrollRecStore),
            size: CGSize(width: 1310, height: 860),
            dark: false,
            alwaysShowScrollers: true,
            outputURL: baseURL.appendingPathComponent("m4_records_25rows_light.png")
        )
        scrollRecStore.page = .settings
    }
}
}
