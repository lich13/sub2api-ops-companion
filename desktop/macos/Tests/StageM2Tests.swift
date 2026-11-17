import AppKit
import Foundation
import SwiftUI
import Testing
@testable import Sub2OpsMacOS

extension Sub2OpsMacOSTestRoot {
@Suite("M2 Accounts, Catalog, Actions & UI Snapshots", .serialized)
struct StageM2Tests {
    @Test("AccountCreationCatalog loads all 13 upstream platforms and defaults")
    func catalogLoadsAllPlatformsAndSeedsDefaults() throws {
        let catalog = AccountCreationCatalog.shared
        #expect(catalog.platforms.count == 13)
        #expect(catalog.platforms.contains(where: { $0.id == "openai" && $0.types.contains("oauth") }))
        #expect(catalog.provider_profiles["opencode_go"]?.default_mode == "go")

        var draft = AccountCreationFormDraft()
        draft.selectPlatform("deepseek")
        #expect(draft.type == "apikey")
        #expect(draft.values["credentials.base_url"]?.stringValue == "https://api.deepseek.com")
        draft.name = "DeepSeek 测试"
        draft.apiKey = "sk-example-invalid"
        let payload = try draft.buildPayload()
        #expect(payload["platform"]?.stringValue == "deepseek")
        #expect(payload["credentials"]?["api_key"]?.stringValue == "sk-example-invalid")
    }

    @Test("AccountActionEntry matches React fixtureAccountActions matrix")
    func accountActionsMatchReactMatrix() {
        var openaiOauth = Account(id: 101, name: "Codex", platform: "openai", type: "oauth")
        openaiOauth.recoverable = true
        let labels = AccountActionEntry.entries(for: openaiOauth).map(\.label)
        #expect(labels == [
            "模型测试", "标记降智", "定时检测", "测试连接", "账号保活",
            "重新授权", "恢复状态", "应用模板", "模型白名单", "模型映射",
            "选择代理", "删除",
        ])

        let grokOauth = Account(id: 201, name: "Grok", platform: "grok", type: "oauth")
        let grokLabels = AccountActionEntry.entries(for: grokOauth).map(\.label)
        #expect(grokLabels == [
            "测试连接", "账号保活", "模型白名单", "模型映射", "选择代理", "删除",
        ])
    }

    @Test("Render Stage M2 Light & Dark PNG snapshots when SUB2OPS_SNAPSHOT_DIR is set")
    @MainActor
    func renderM2Snapshots() throws {
        guard let dirPath = ProcessInfo.processInfo.environment["SUB2OPS_SNAPSHOT_DIR"],
              !dirPath.isEmpty
        else {
            return
        }
        let baseURL = URL(fileURLWithPath: dirPath)
        RustBridge.shared.setMockHandler { _, _ in "{}" }

        let stats = UsageStats(requests: 313, tokens: 46_900_000, cost: 76.8, standard_cost: 76.8, user_cost: 76.8)
        var w5h = UsageWindow(key: "5h", label: "5h", used_percent: 38, reset_at: "2026-09-25T05:00:00+08:00")
        w5h.stats = stats
        w5h.color = "indigo"
        var w7d = UsageWindow(key: "7d", label: "7d", used_percent: 81, reset_at: "2026-09-28T05:00:00+08:00")
        w7d.stats = stats
        w7d.color = "emerald"
        w7d.estimated_total_cost = 94.81

        var acc1 = Account(
            id: 101,
            name: "Codex · 主力",
            priority: 1,
            platform: "openai",
            type: "oauth",
            status: "active",
            schedulable: true,
            available: true,
            group_ids: [1],
            last_success_at: "2026-11-17T08:22:11Z",
            last_called_at: "2026-11-17T08:22:11Z",
            usage_windows: [w5h, w7d]
        )
        acc1.concurrency = 2
        acc1.usage = UsageSummary(
            branch: "openai_oauth",
            windows: [w5h, w7d],
            actions: ["query_usage", "query_reset_credits", "reset_quota"],
            reset_credits: ResetCreditsInfo(available: 1, expires_at: ["2026-10-23T02:52:00+08:00"], observed_at: "2026-11-17T08:22:11Z")
        )

        var w24h = UsageWindow(key: "grok_24h", label: "24h", used_percent: 90, reset_at: nil)
        w24h.stats = stats
        w24h.color = "emerald"
        var acc2 = Account(
            id: 201,
            name: "Grok · Super",
            priority: 2,
            platform: "grok",
            type: "oauth",
            status: "active",
            schedulable: true,
            available: false,
            group_ids: [2],
            blockers: [AccountBlocker(code: "rate_limit_reset_at", label: "限流中", until: "2026-11-29T03:00:00Z")],
            last_success_at: "2026-11-17T08:15:11Z",
            last_error_at: "2026-11-17T08:22:11Z",
            last_error_id: 9002,
            last_error_code: "rate_limit_exceeded",
            last_error_status: 429,
            usage_windows: [w24h]
        )
        acc2.concurrency = 2
        acc2.usage = UsageSummary(branch: "grok_paid", windows: [w24h], actions: ["probe_quota"])

        var acc3 = Account(
            id: 102,
            name: "Codex · 备用账号 · 用于验证长名称的显示与调度开关",
            priority: 5,
            platform: "openai",
            type: "oauth",
            status: "error",
            schedulable: true,
            available: false,
            group_ids: [1],
            blockers: [AccountBlocker(code: "reauth", label: "需要重新认证")],
            last_success_at: "2026-11-17T08:15:11Z",
            last_error_at: "2026-11-17T08:22:11Z",
            last_error_id: 9001,
            last_error_code: "invalid_grant",
            last_error_status: 401
        )
        acc3.concurrency = 2
        acc3.recoverable = true
        acc3.degradation_mark = DegradationMark(marked: true, version: String(repeating: "f", count: 64))

        let grp1 = Group(
            id: 1,
            name: "Codex",
            platform: "openai",
            recent_accounts: [
                RecentAccount(log_id: 100, account_id: 101, account_name: acc1.name, model: "gpt-6-astra", called_at: "2026-11-17T08:22:11Z"),
                RecentAccount(log_id: 99, account_id: 102, account_name: acc3.name, model: "gpt-6-astra", called_at: "2026-11-17T08:15:11Z"),
            ],
            called_at: "2026-11-17T08:22:11Z"
        )
        let grp2 = Group(
            id: 2,
            name: "Grok",
            platform: "grok",
            recent_accounts: [
                RecentAccount(log_id: 90, account_id: 201, account_name: acc2.name, model: "grok-4.6", called_at: "2026-11-17T08:20:00Z"),
            ],
            called_at: "2026-11-17T08:20:00Z"
        )

        let store = AppStore()
        store.ready = true
        store.page = .accounts
        store.applyViewState(
            ViewState(
                connected: true,
                online: true,
                snapshot: Snapshot(
                    observed_at: "2026-11-17T08:22:11Z",
                    accounts: [acc1, acc2, acc3],
                    groups: [grp1, grp2]
                ),
                preferences: Preferences(
                    base_url: "https://demo.example/sub2ops",
                    connections: [SavedConnection(name: "demo.example", base_url: "https://demo.example/sub2ops")]
                )
            )
        )

        for dark in [false, true] {
            let suffix = dark ? "dark" : "light"
            try SnapshotRenderUtility.renderPNG(
                view: MainShellView(store: store),
                size: CGSize(width: 1310, height: 860),
                dark: dark,
                outputURL: baseURL.appendingPathComponent("m2_accounts_\(suffix).png")
            )
            try SnapshotRenderUtility.renderPNG(
                view: QuickPanelRootView(store: store),
                size: CGSize(width: 420, height: 420),
                dark: dark,
                outputURL: baseURL.appendingPathComponent("m2_quick_panel_\(suffix).png")
            )
            try SnapshotRenderUtility.renderPNG(
                view: TestDialogSheet(account: acc1, store: store, onClose: {}),
                size: CGSize(width: 580, height: 380),
                dark: dark,
                outputURL: baseURL.appendingPathComponent("m2_test_dialog_\(suffix).png")
            )
            try SnapshotRenderUtility.renderPNG(
                view: AccountCreationSheet(store: store, onClose: {}),
                size: CGSize(width: 680, height: 560),
                dark: dark,
                outputURL: baseURL.appendingPathComponent("m2_creation_dialog_\(suffix).png")
            )
        }
    }
}
}
