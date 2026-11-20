import AppKit
import Foundation
import SwiftUI
import Testing
@testable import Sub2OpsMacOS

/// Renders every creation field group expanded, outside the bounded dialog scroll view.
private struct AccountCreationFieldsPreview: View {
    @State var draft: AccountCreationFormDraft

    var body: some View {
        AccountCreationFieldsView(draft: $draft, expandAll: true)
            .padding(24)
            .frame(width: 668)
            .background(DS.surface)
    }
}

extension Sub2OpsMacOSTestRoot {
@Suite("Accounts, Catalog, Actions & UI Snapshots", .serialized)
struct AccountsAndActionsTests {
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

    @Test("AccountModelProfile validates exact IDs, trailing wildcards, duplicates, and conflicts")
    func accountModelProfileValidationEdgeCases() {
        let empty = AccountModelProfile()
        #expect(AccountModelProfile.validate(empty) == "")
        #expect(empty.policySummary == "不限制模型")

        let valid = AccountModelProfile(
            whitelist: ["gpt-5.4", "claude-opus-4-6"],
            mappings: [AccountModelMappingRule(source: "gpt-4o-*", target: "gpt-5.4")]
        )
        #expect(AccountModelProfile.validate(valid) == "")
        #expect(valid.policySummary == "gpt-5.4、claude-opus-4-6、gpt-4o-* → gpt-5.4")

        let badWhitelist = AccountModelProfile(whitelist: ["gpt 5*"])
        #expect(AccountModelProfile.validate(badWhitelist) == "白名单必须是精确模型 ID")

        let dupWhitelist = AccountModelProfile(whitelist: ["gpt-5.4", "gpt-5.4"])
        #expect(AccountModelProfile.validate(dupWhitelist) == "重复的模型：gpt-5.4")

        let badTargetWildcard = AccountModelProfile(
            mappings: [AccountModelMappingRule(source: "gpt-4o", target: "gpt-5*")]
        )
        #expect(AccountModelProfile.validate(badTargetWildcard) == "映射源只允许末尾通配符，目标必须是精确模型 ID")

        let selfMap = AccountModelProfile(
            mappings: [AccountModelMappingRule(source: "gpt-5.4", target: "gpt-5.4")]
        )
        #expect(AccountModelProfile.validate(selfMap) == "模型条目冲突：gpt-5.4")

        let crossConflict = AccountModelProfile(
            whitelist: ["gpt-5.4"],
            mappings: [AccountModelMappingRule(source: "gpt-5.4", target: "gpt-5.4-mini")]
        )
        #expect(AccountModelProfile.validate(crossConflict) == "模型条目冲突：gpt-5.4")
    }

    @Test("AccountCreationFormDraft validates boundaries, reuses source config, and normalizes token identity")
    func accountCreationEdgeCasesAndReuse() throws {
        var draft = AccountCreationFormDraft()
        draft.selectPlatform("openai")
        draft.selectType("oauth")

        // Boundary validation
        #expect(draft.validateStepOne(requireName: true) == "请输入账号名称")
        draft.name = "Codex 新账号"
        draft.concurrency = "0"
        #expect(draft.validateStepOne() == "并发数需为正整数")
        draft.concurrency = "5"
        draft.loadFactor = "0"
        #expect(draft.validateStepOne() == "负载因子需为正整数")
        draft.loadFactor = ""
        draft.priority = "-1"
        #expect(draft.validateStepOne() == "优先级需为非负整数")
        draft.priority = "0"
        draft.rateMultiplier = "-0.5"
        #expect(draft.validateStepOne() == "倍率需为非负数")
        draft.rateMultiplier = "1.25"
        #expect(draft.validateStepOne() == "")

        // Platform mismatch on reuse throws
        #expect(throws: RustBridgeError.self) {
            try draft.applyReusedSource(["platform": .string("grok"), "type": .string("oauth")])
        }

        // Valid source reuse preserves name while copying config & mappings
        try draft.applyReusedSource([
            "platform": .string("openai"),
            "type": .string("oauth"),
            "notes": .string("复用自 #101"),
            "proxy_id": .int(7),
            "concurrency": .int(12),
            "priority": .int(3),
            "rate_multiplier": .double(0.8),
            "group_ids": .array([.int(1), .int(3)]),
            "credentials": .object([
                "model_mapping": .object(["gpt-4o": .string("gpt-5.4")]),
            ]),
        ])
        #expect(draft.name == "Codex 新账号")
        #expect(draft.notes == "复用自 #101")
        #expect(draft.proxyId == 7)
        #expect(draft.concurrency == "12")
        #expect(draft.groupIds == [1, 3])
        #expect(draft.mappings.count == 1 && draft.mappings[0].key == "gpt-4o" && draft.mappings[0].value == "gpt-5.4")

        // Token identity extraction for Gemini stringifies expires_at and keeps fallback refresh token
        let geminiIdent = AccountCreationFormDraft.extractTokenIdentity(
            platform: "gemini",
            token: [
                "access_token": .string("tok-example"),
                "expires_at": .int(1_900_000_000),
                "email": .string("ops@example.invalid"),
                "extra": .object(["drive_storage_limit": .int(1024)]),
            ],
            fallbackRefresh: "ref-fallback"
        )
        #expect(geminiIdent.credentials["access_token"] == .string("tok-example"))
        #expect(geminiIdent.credentials["refresh_token"] == .string("ref-fallback"))
        #expect(geminiIdent.credentials["expires_at"] == .string("1900000000"))
        #expect(geminiIdent.extra["drive_storage_limit"] == .int(1024))
        #expect(geminiIdent.extra["email"] == .string("ops@example.invalid"))

        // Service account missing private_key throws
        var saDraft = AccountCreationFormDraft()
        saDraft.selectPlatform("gemini")
        saDraft.selectType("service_account")
        saDraft.name = "Vertex SA"
        saDraft.serviceAccountJSON = "{\"project_id\":\"demo\"}"
        #expect(throws: RustBridgeError.self) {
            _ = try saDraft.buildPayload(requireCredentials: true)
        }
    }

    @Test("TestDialog modes/appointments, Accounts TableColumnMetrics, and QuickPanel child-menu blur guards")
    @MainActor
    func testDialogAndTableMetricsAndQuickPanelGuards() {
        let grokModels = [
            AccountModelItem(id: "grok-4.5", display_name: "Grok 4.5"),
            AccountModelItem(id: "grok-imagine-image-v1", display_name: "Grok Imagine"),
            AccountModelItem(id: "grok-video-v1", display_name: "Grok Video"),
        ]
        #expect(TestDialogSheet.modelsForMode(grokModels, platform: "grok", mode: "text").map(\.id) == ["grok-4.5"])
        #expect(TestDialogSheet.modelsForMode(grokModels, platform: "grok", mode: "image").map(\.id) == ["grok-imagine-image-v1"])
        #expect(TestDialogSheet.modelsForMode(grokModels, platform: "grok", mode: "video").map(\.id) == ["grok-video-v1"])
        #expect(TestDialogSheet.modelsForMode(grokModels, platform: "grok", mode: "tts").isEmpty)

        let parsed = TestDialogSheet.parseBeijingAppointment("2026-11-17 20:00:03")
        #expect(parsed == BeijingTime.parseISO("2026-11-17T12:00:03Z"))

        let now = BeijingTime.parseISO("2026-11-17T11:00:00Z")!
        let win = UsageWindow(key: "5h", label: "5h", used_percent: 80, reset_at: "2026-11-17T12:00:00Z")
        let nextAppt = TestDialogSheet.nextCycleAppointmentDate(usage: nil, windows: [win], now: now)
        #expect(nextAppt == BeijingTime.parseISO("2026-11-17T12:00:03Z"))

        // TableColumnMetrics follows responsive.css: fixed middle columns, only actions absorb extra width,
        // scroll floor is 734 + 110 + name (128, or 240 expanded)
        let narrow = AccountsPageView.TableColumnMetrics(containerWidth: 900, nameExpanded: false)
        #expect(narrow.totalWidth == 972)
        let expandedNarrow = AccountsPageView.TableColumnMetrics(containerWidth: 900, nameExpanded: true)
        #expect(expandedNarrow.totalWidth == 1084)
        let regular = AccountsPageView.TableColumnMetrics(containerWidth: 1133, nameExpanded: false)
        #expect(regular.totalWidth == 1133)
        #expect(regular.nameW == narrow.nameW && regular.usageW == narrow.usageW && regular.actionsW > narrow.actionsW)
        let sumRegular = regular.selectW + regular.nameW + regular.paramsW + regular.statusW
            + regular.usageW + regular.errorW + regular.scheduleW + regular.actionsW
            + (regular.gap * 7) + (regular.horizontalPad * 2)
        #expect(abs(sumRegular - 1133) < 0.001)
        let ultrawide = AccountsPageView.TableColumnMetrics(containerWidth: 2200, nameExpanded: false)
        #expect(ultrawide.totalWidth == 2200 && ultrawide.actionsW > 600)

        // QuickPanel blur guard stays active while NSMenu or instance-switcher popover is open
        let store = AppStore()
        let controller = QuickPanelController(store: store)
        #expect(!controller.isPanelOrAttachedWindowActive())
        LinearMenuTracking.begin()
        #expect(controller.isPanelOrAttachedWindowActive())
        LinearMenuTracking.end()
        #expect(!controller.isPanelOrAttachedWindowActive())
        store.switcherAnchor = .quick
        #expect(controller.isPanelOrAttachedWindowActive())
        store.switcherAnchor = nil
        #expect(!controller.isPanelOrAttachedWindowActive())
    }

    @Test("Render AccountsAndActions Light & Dark PNG snapshots when SUB2OPS_SNAPSHOT_DIR is set")
    @MainActor
    func renderM2Snapshots() async throws {
        guard let dirPath = ProcessInfo.processInfo.environment["SUB2OPS_SNAPSHOT_DIR"],
              !dirPath.isEmpty
        else {
            return
        }
        let baseURL = URL(fileURLWithPath: dirPath)
        let longWhitelist = (1...24).map { "\"gpt-5.4-variant-\($0)\"" }.joined(separator: ",")
        let accountSettingsJSON = """
        {
          "account_id": 101,
          "priority": 1,
          "concurrency": 2,
          "load_factor": null,
          "effective_load_factor": 2,
          "proxy_id": 11,
          "model_config": {
            "whitelist": [\(longWhitelist)],
            "mappings": [{"source": "gpt-4o-*", "target": "gpt-5.4"}]
          },
          "capabilities": {
            "model_config": {"editable": true, "reason": ""},
            "proxy": {"editable": true, "reason": ""}
          },
          "versions": {"model_config": "v1", "proxy": "v1"}
        }
        """
        let templatesJSON = """
        {
          "version": "tpl-v1",
          "configured": true,
          "templates": {
            "prod-default": {
              "whitelist": ["gpt-5.4", "gpt-5.4-mini"],
              "mappings": [{"source": "gpt-4o", "target": "gpt-5.4"}]
            }
          },
          "template_meta": {"prod-default": {"name": "生产主力模板"}},
          "template_order": ["prod-default"]
        }
        """
        let templatePreviewJSON = """
        {
          "account": {
            "id": 101,
            "eligible": true,
            "passthrough": false,
            "version": "v1",
            "config": {"whitelist": ["gpt-5.4"], "mappings": []}
          }
        }
        """
        let detectionJSON = """
        {
          "version": "det-v1",
          "enabled": true,
          "schedule_mode": "smart",
          "interval_minutes": 60,
          "min_interval_minutes": 5,
          "global_model_id": "gpt-5.4",
          "model_id": "gpt-5.4",
          "status": "waiting",
          "engine": "modeltrace",
          "sample_count": 3,
          "recent_request_count": 42,
          "effective_interval_minutes": 10,
          "last_automatic_started_at": "2026-11-17T08:10:00Z",
          "next_at": "2026-11-17T08:30:00Z"
        }
        """
        let keepaliveJSON = """
        {
          "enabled": true,
          "version": "ka-v1",
          "status": "waiting",
          "restore_count": 1,
          "last_restore_at": "2026-11-17T07:55:00Z",
          "reason": "",
          "paused_reason": ""
        }
        """

        RustBridge.shared.setMockHandler { _, payload in
            if payload.contains("/fingerprint-bank") {
                return "{\"version\":{\"revision\":\"8f31c2a09b1e4d00\",\"sha256\":\"0123456789abcdef\",\"built_at\":\"2026-11-17T06:00:00Z\",\"analyzer_version\":\"0.2.0\"},\"source\":\"cache\",\"status\":\"idle\",\"result\":\"up-to-date\",\"checked_at\":\"2026-11-17T08:00:00Z\",\"synced_at\":\"2026-11-17T08:00:00Z\"}"
            }
            if payload.contains("/accounts/101/models") {
                return "{\"models\":[{\"id\":\"gpt-5.4\",\"display_name\":\"GPT-5.4\"},{\"id\":\"gpt-5.4-mini\",\"display_name\":\"GPT-5.4 Mini\"}]}"
            }
            if payload.contains("/accounts/101/settings") {
                return accountSettingsJSON
            }
            if payload.contains("/account-templates/accounts/101") {
                return templatePreviewJSON
            }
            if payload.contains("/account-templates") {
                return templatesJSON
            }
            if payload.contains("/accounts/101/model-detection") {
                return detectionJSON
            }
            if payload.contains("/accounts/101/keepalive") {
                return keepaliveJSON
            }
            if payload.contains("/account-operations") {
                return "{\"items\":[]}"
            }
            return "{}"
        }

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
                size: CGSize(width: 420, height: 294),
                dark: dark,
                outputURL: baseURL.appendingPathComponent("m2_quick_panel_\(suffix).png")
            )
            try await SnapshotRenderUtility.renderPNGAsync(
                view: ZStack { DS.backdrop; TestDialogSheet(account: acc1, store: store, onClose: {}) }
                    .frame(width: 620, height: 280),
                size: CGSize(width: 620, height: 280),
                dark: dark,
                outputURL: baseURL.appendingPathComponent("m2_test_dialog_\(suffix).png")
            )
            try SnapshotRenderUtility.renderPNG(
                view: ZStack { DS.backdrop; AccountCreationSheet(store: store, onClose: {}) }
                    .frame(width: 720, height: 780),
                size: CGSize(width: 720, height: 780),
                dark: dark,
                outputURL: baseURL.appendingPathComponent("m2_creation_dialog_\(suffix).png")
            )
            var routedDraft = AccountCreationFormDraft()
            routedDraft.applyPlatformDefaults(for: "opencode_go", authType: "apikey")
            routedDraft.name = "OpenCode 路由账号"
            routedDraft.mappings = Array(routedDraft.mappings.prefix(2))
            routedDraft.setValue("credentials.temp_unschedulable_enabled", .bool(true))
            routedDraft.pauseRules = [PauseRuleDraft(keywords: "rate limit, overloaded", description: "上游限流")]
            try SnapshotRenderUtility.renderPNG(
                view: ZStack { DS.backdrop; AccountCreationFieldsPreview(draft: routedDraft) }
                    .frame(width: 720, height: 1500),
                size: CGSize(width: 720, height: 1500),
                dark: dark,
                outputURL: baseURL.appendingPathComponent("m2_creation_routing_fields_\(suffix).png")
            )
            try await SnapshotRenderUtility.renderPNGAsync(
                view: ZStack { DS.backdrop; KeyQuotaSettingsSheet(store: store, targetAccount: nil, onClose: {}) }
                    .frame(width: 560, height: 320),
                size: CGSize(width: 560, height: 320),
                dark: dark,
                outputURL: baseURL.appendingPathComponent("m2_key_quota_settings_\(suffix).png")
            )
            try await SnapshotRenderUtility.renderPNGAsync(
                view: ZStack { DS.backdrop; ModelTestSheet(account: acc1, store: store, onClose: {}) }
                    .frame(width: 680, height: 860),
                size: CGSize(width: 680, height: 860),
                dark: dark,
                outputURL: baseURL.appendingPathComponent("m2_model_test_dialog_\(suffix).png")
            )
            try await SnapshotRenderUtility.renderPNGAsync(
                view: ZStack { DS.backdrop; AccountSettingsSheet(account: acc1, store: store, initialTab: .whitelist, onClose: {}) }
                    .frame(width: 680, height: 860),
                size: CGSize(width: 680, height: 860),
                dark: dark,
                outputURL: baseURL.appendingPathComponent("m2_account_settings_dialog_\(suffix).png")
            )
            try await SnapshotRenderUtility.renderPNGAsync(
                view: ZStack { DS.backdrop; AccountTemplatesSheet(store: store, targetAccount: acc1, onClose: {}) }
                    .frame(width: 820, height: 860),
                size: CGSize(width: 820, height: 860),
                dark: dark,
                outputURL: baseURL.appendingPathComponent("m2_account_templates_dialog_\(suffix).png")
            )
            try await SnapshotRenderUtility.renderPNGAsync(
                view: ZStack { DS.backdrop; ModelDetectionSheet(account: acc1, store: store, onClose: {}) }
                    .frame(width: 640, height: 860),
                size: CGSize(width: 640, height: 860),
                dark: dark,
                outputURL: baseURL.appendingPathComponent("m2_model_detection_dialog_\(suffix).png")
            )
            try await SnapshotRenderUtility.renderPNGAsync(
                view: ZStack { DS.backdrop; KeepaliveSheet(account: acc1, store: store, onClose: {}) }
                    .frame(width: 600, height: 860),
                size: CGSize(width: 600, height: 860),
                dark: dark,
                outputURL: baseURL.appendingPathComponent("m2_keepalive_dialog_\(suffix).png")
            )
            try SnapshotRenderUtility.renderPNG(
                view: ZStack { DS.backdrop; DeleteAccountsSheet(accounts: [acc1, acc2], store: store, onDeleted: { _ in }, onClose: {}) }
                    .frame(width: 600, height: 860),
                size: CGSize(width: 600, height: 860),
                dark: dark,
                outputURL: baseURL.appendingPathComponent("m2_delete_dialog_\(suffix).png")
            )
            let sampleErr = try JSONDecoder().decode(
                OpsError.self,
                from: Data("""
                {
                  "id": 9002,
                  "created_at": "2026-11-17T08:22:11Z",
                  "status_code": 429,
                  "upstream_status_code": 429,
                  "account_id": 101,
                  "account_name": "Codex · 主力",
                  "group_id": 1,
                  "group_name": "Codex",
                  "model": "gpt-5.4",
                  "requested_model": "gpt-5.4",
                  "upstream_model": "gpt-5.4-2026",
                  "provider_error_code": "usage_limit_reached",
                  "message": "You have reached your 7d usage limit.",
                  "request_id": "req_demo_1234567890abcdef",
                  "content": "{\\n  \\"error\\": {\\n    \\"code\\": \\"usage_limit_reached\\",\\n    \\"resets_at\\": 1795400000\\n  }\\n}"
                }
                """.utf8)
            )
            try SnapshotRenderUtility.renderPNG(
                view: ErrorDetailDrawerView(error: sampleErr, busy: false, account: acc1, store: store, onClose: {}),
                size: CGSize(width: 1310, height: 860),
                dark: dark,
                outputURL: baseURL.appendingPathComponent("m2_error_drawer_\(suffix).png")
            )
        }
    }
}
}
