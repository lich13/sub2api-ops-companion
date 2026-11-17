import AppKit
import Foundation
import SwiftUI
import Testing
@testable import Sub2OpsMacOS

extension Sub2OpsMacOSTestRoot {
@Suite("M4 Records & Events Logic + UI Snapshots", .serialized)
struct StageM4Tests {
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
    }

    @Test("Render Stage M4 Light & Dark PNG snapshots when SUB2OPS_SNAPSHOT_DIR is set")
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

        RustBridge.shared.setMockHandler { _, payload in
            if payload.contains("/detection-logs") { return logsJSON }
            if payload.contains("/errors") { return errorsJSON }
            return recordsJSON
        }

        let acc1 = Account(id: 101, name: "Codex · 主力", priority: 1, platform: "openai", type: "oauth")
        let recStore = AppStore()
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

        let eventsStore = AppStore()
        eventsStore.ready = true
        eventsStore.page = .events
        eventsStore.applyViewState(recStore.state)

        for dark in [false, true] {
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
        }
        recStore.page = .settings
        eventsStore.page = .settings
    }
}
}
