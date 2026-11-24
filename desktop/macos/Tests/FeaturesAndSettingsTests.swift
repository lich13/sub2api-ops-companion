import AppKit
import Foundation
import SwiftUI
import Testing
@testable import Sub2OpsMacOS

extension Sub2OpsMacOSTestRoot {
@Suite("Features (3 Tabs) & Settings Logic + UI Snapshots", .serialized)
struct FeaturesAndSettingsTests {
    @Test("LightYAML parses block/flow structures, rejects duplicate keys, and enforces expected root type")
    func lightYAMLValidationAndFormatting() {
        #expect(FeaturesPageView.Tab.allCases.map(\.label) == ["恢复与检测", "IP 管理", "Sub2API 配置"])
        let mapRes = LightYAML.parseStructured("host: 127.0.0.1\nport: 8080\nenabled: true # inline comment", expectedType: "object")
        #expect(mapRes.error.isEmpty)
        #expect(mapRes.value?.objectValue?["port"] == .int(8080))
        #expect(mapRes.value?.objectValue?["enabled"] == .bool(true))

        let wrongRoot = LightYAML.parseStructured("- a\n- b", expectedType: "object")
        #expect(wrongRoot.error == "需要 YAML 映射")

        let seqRes = LightYAML.parseStructured("- gpt-5.4\n- claude-opus-4-6", expectedType: "array")
        #expect(seqRes.error.isEmpty)
        #expect(seqRes.value == .array([.string("gpt-5.4"), .string("claude-opus-4-6")]))

        let dupBlock = LightYAML.parseStructured("a: 1\na: 2", expectedType: "object")
        #expect(!dupBlock.error.isEmpty)

        let dupJSON = LightYAML.parseStructured("{\"a\": 1, \"a\": 2}", expectedType: "object")
        #expect(!dupJSON.error.isEmpty)
    }

    @Test("ProxyDataParser parses valid IPv4/IPv6 auth lines, deduplicates, and validates JSON import limits")
    func proxyDataParserRules() throws {
        let lines = """
        socks5://user%40a:p%3Ass@198.51.100.10:1080
        socks5://user%40a:p%3Ass@198.51.100.10:1080
        http://[2001:db8::1]:8080
        invalid://host:99999
        """
        let out = ProxyDataParser.parseProxyLines(lines)
        #expect(out.proxies.count == 2)
        #expect(out.duplicates == 1)
        #expect(out.errors == [4])
        #expect(out.proxies[0].username == "user@a" && out.proxies[0].password == "p:ss")
        #expect(out.proxies[1].host == "2001:db8::1" && out.proxies[1].port == 8080)

        let validImport = try ProxyDataParser.parseImport("{\"proxies\":[{\"name\":\"HK-1\",\"protocol\":\"socks5\",\"host\":\"198.51.100.2\",\"port\":1080}]}")
        #expect(validImport.objectValue?["proxies"]?.arrayValue?.count == 1)

        #expect(throws: RustBridgeError.self) {
            _ = try ProxyDataParser.parseImport("{\"proxies\":[{\"name\":\"A\"}],\"accounts\":[{\"id\":1}]}")
        }
    }

    @Test("FingerprintBankStatusInfo decodes nested server version object and Unix timestamps")
    func fingerprintBankDecodesNestedServerPayloadAndUnixTimestamps() throws {
        let serverJSON = Data("""
        {
          "version": {
            "revision": "8f31c2a09b1e4d00",
            "sha256": "0123456789abcdef",
            "built_at": "2026-11-17T06:00:00Z",
            "analyzer_version": "0.2.0"
          },
          "source": "remote",
          "status": "idle",
          "result": "updated",
          "checked_at": 1794902400,
          "synced_at": "2026-11-17T08:00:00Z"
        }
        """.utf8)
        let info = try JSONDecoder().decode(FingerprintBankStatusInfo.self, from: serverJSON)
        #expect(info.version?.revision == "8f31c2a09b1e4d00")
        #expect(String((info.version?.revision ?? "").prefix(8)) == "8f31c2a0")
        #expect(info.available)
        #expect(info.result == "updated")
        #expect(info.checked_at != nil && !(info.checked_at ?? "").isEmpty)
        #expect(info.synced_at == "2026-11-17T08:00:00Z")

        let reportJSON = Data("""
        {
          "engine": "modeltrace",
          "prediction": "gpt-5.4",
          "probability": 0.98,
          "results": [
            { "model": "gpt-5.4", "display_name": "GPT-5.4", "probability": 0.98 },
            { "model": "gpt-5.4-mini", "display_name": "GPT-5.4 Mini", "probability": 0.02 }
          ],
          "bank_version": { "revision": "deadbeef12345678", "analyzer_version": 1 }
        }
        """.utf8)
        let report = try JSONDecoder().decode(ModelFingerprintReport.self, from: reportJSON)
        #expect(report.bank_version?.revision == "deadbeef12345678")
        #expect(report.bank_version?.analyzer_version == "1")
        #expect(report.results?.count == 2)
        #expect(report.results?.first?.model == "gpt-5.4")
    }

    @Test("Contract regressions: null remove_connection, float proxy metrics, model test classification/results, partial detection summary, and hex config revisions")
    @MainActor
    func fiveContractBugRegressions() async throws {
        // Bug 1: remove_connection returns JSON "null" and commandVoid must succeed without ViewState decode failure
        RustBridge.shared.setMockHandler { cmd, _ in
            cmd == "remove_connection" ? "null" : "{}"
        }
        defer { RustBridge.shared.setMockHandler(nil) }
        try await RustBridge.shared.commandVoid("remove_connection", args: .object(["baseUrl": .string("https://old.example.invalid/sub2ops")]))

        // Bug 2: ProxyTestReply, ProxyQualityReply, and ProxyQualityCheck decode floating-point latency_ms / score / base_latency_ms
        let proxyTest = try JSONDecoder().decode(ProxyTestReply.self, from: Data("""
        {
          "success": true, "message": "连接成功", "latency_ms": 19.4,
          "ip_address": "198.51.100.18", "city": "Singapore", "region": "SG", "country": "新加坡", "country_code": "SG"
        }
        """.utf8))
        #expect(proxyTest.latency_ms == 19.4)

        let proxyQuality = try JSONDecoder().decode(ProxyQualityReply.self, from: Data("""
        {
          "score": 91.8, "grade": "A", "summary": "原生质量检测通过",
          "exit_ip": "198.51.100.18", "country": "新加坡", "country_code": "SG",
          "base_latency_ms": 32.4, "passed_count": 3, "warn_count": 0, "failed_count": 0, "challenge_count": 0,
          "checked_at": 1794902400,
          "items": [{ "target": "openai", "status": "ok", "message": "200 OK", "latency_ms": 44.6, "http_status": 200 }]
        }
        """.utf8))
        #expect(proxyQuality.score == 91.8 && proxyQuality.base_latency_ms == 32.4)
        #expect(proxyQuality.items.first?.latency_ms == 44.6)

        // Bug 3: ModelTestJob decodes top-level classification + report.results
        let job = try JSONDecoder().decode(ModelTestJob.self, from: Data("""
        {
          "id": "mt-101",
          "status": "completed",
          "engine": "modeltrace",
          "classification": {
            "outcome": "degraded",
            "reason": "第 1 组命中 gpt-5.6-luna",
            "basis": "first_group",
            "group_indexes": [1]
          },
          "report": {
            "engine": "modeltrace",
            "prediction": "gpt-5.6-luna",
            "probability": 0.94,
            "results": [
              { "model": "gpt-5.6-luna", "display_name": "GPT-5.6 Luna", "probability": 0.94 },
              { "model": "gpt-6-astra", "display_name": "GPT-6 Astra", "probability": 0.06 }
            ]
          }
        }
        """.utf8))
        #expect(job.classification?.outcome == "degraded")
        #expect(job.classification?.reason == "第 1 组命中 gpt-5.6-luna")
        #expect(job.report?.results?.count == 2)

        // Bug 4: Partial/error ModelDetectionSummary inside Snapshot does not break Snapshot decoding
        let snap = try JSONDecoder().decode(Snapshot.self, from: Data("""
        {
          "observed_at": "2026-11-17T09:15:00Z",
          "accounts": [{
            "id": 101,
            "name": "Codex-Partial",
            "priority": 1,
            "platform": "openai",
            "type": "oauth",
            "status": "active",
            "schedulable": true,
            "available": true,
            "group_ids": [],
            "blockers": [],
            "version": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "operation_versions": {},
            "usage_windows": [],
            "model_detection": {
              "status": "unavailable",
              "error": "upstream model detection state degraded"
            }
          }],
          "groups": [],
          "errors": [],
          "recoveries": []
        }
        """.utf8))
        #expect(snap.accounts.count == 1)
        #expect(snap.accounts[0].model_detection?.status == "unavailable")
        #expect(snap.accounts[0].model_detection?.enabled == nil)

        // Bug 5: Config revision must be a non-empty string token (never integer 0 fallback)
        let missingRevConfig: [String: JSONValue] = ["oauth": .object(["oauth_recovery_monitor_enabled": .bool(true)])]
        let parsedRev = missingRevConfig["oauth"]?.objectValue?["revision"]?.stringValue ?? ""
        #expect(parsedRev.isEmpty)
    }

    @Test("Render FeaturesAndSettings Light & Dark PNG snapshots when SUB2OPS_SNAPSHOT_DIR is set")
    @MainActor
    func renderM5Snapshots() async throws {
        guard let dirPath = ProcessInfo.processInfo.environment["SUB2OPS_SNAPSHOT_DIR"],
              !dirPath.isEmpty
        else {
            return
        }
        let baseURL = URL(fileURLWithPath: dirPath)

        let configJSON = """
        {
          "oauth": {
            "revision": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "oauth_recovery_monitor_enabled": true,
            "oauth_early_reset_enabled": true,
            "oauth_early_reset_interval_seconds": 30,
            "oauth_auto_reset_credit_enabled": true,
            "oauth_daily_test_enabled": true,
            "oauth_daily_test_time": "08:30",
            "oauth_usage_refresh_concurrency": 4,
            "oauth_recovery_test_concurrency": 2,
            "connection_test_retry_interval_seconds": 60,
            "connection_test_max_retries": 3,
            "oauth_recovery_test_model_id": "gpt-5.4",
            "model_detection_model_id": "gpt-5.4",
            "oauth_recovery_connection_account_ids": [101],
            "oauth_recovery_model_account_ids": [102],
            "model_detection_platforms": {
              "openai": { "engine": "modeltrace", "sample_count": 3 },
              "anthropic": { "engine": "lm_detector", "sample_count": 3 },
              "grok": { "engine": "modeltrace", "sample_count": 1 },
              "deepseek": { "engine": "modeltrace", "sample_count": 3 }
            }
          },
          "bark": {
            "revision": "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210",
            "enabled": true,
            "device_key_set": true
          }
        }
        """

        let proxiesJSON = """
        {
          "items": [
            {
              "id": 11,
              "name": "SG-Edge-01",
              "protocol": "socks5",
              "host": "198.51.100.18",
              "port": 1080,
              "status": "active",
              "username_set": true,
              "password_set": true,
              "account_count": 6,
              "latency_ms": 48,
              "ip_address": "198.51.100.18",
              "country": "新加坡",
              "city": "Singapore",
              "quality_status": "healthy",
              "quality_score": 96,
              "quality_grade": "A"
            }
          ],
          "page": 1,
          "page_size": 20,
          "total": 1,
          "pages": 1
        }
        """

        let sub2apiCfgJSON = """
        {
          "available": true,
          "editable": true,
          "reason": "",
          "version": "0.3.18",
          "commit": "abc1234",
          "healthy": true,
          "revision": "doc-1",
          "runtime_revision": "rt-1",
          "catalog": {
            "version": "0.3.18",
            "revision": "cat-1",
            "groups": [
              { "id": "server", "label": "服务与监听" },
              { "id": "ratelimit", "label": "限流与重试" }
            ],
            "fields": [
              {
                "path": "server.listen_addr",
                "group": "server",
                "type": "string",
                "type_label": "字符串",
                "description": "HTTP 服务监听地址与端口。",
                "range": "host:port",
                "default_text": "127.0.0.1:8080",
                "default_value": "127.0.0.1:8080",
                "has_default": true,
                "sensitive": false,
                "internal": false,
                "editable": true,
                "status": "stable",
                "source_url": "https://example.invalid/config",
                "env": ["SUB2API_LISTEN_ADDR"]
              },
              {
                "path": "server.cors_enabled",
                "group": "server",
                "type": "boolean",
                "type_label": "布尔值",
                "description": "是否启用跨域响应头。",
                "range": "true / false",
                "default_text": "false",
                "default_value": false,
                "has_default": true,
                "sensitive": false,
                "internal": false,
                "editable": true,
                "status": "stable",
                "source_url": "https://example.invalid/config",
                "env": []
              }
            ]
          },
          "values": {
            "server.listen_addr": {
              "configured": true,
              "yaml": "0.0.0.0:8080",
              "effective": "0.0.0.0:8080",
              "source": "yaml",
              "environment": null,
              "editable": true
            }
          },
          "unknown": []
        }
        """

        RustBridge.shared.setMockHandler { _, payload in
            if payload.contains("/fingerprint-bank") {
                return "{\"version\":{\"revision\":\"8f31c2a09b1e4d00\",\"sha256\":\"0123456789abcdef\",\"built_at\":\"2026-11-17T06:00:00Z\",\"analyzer_version\":\"0.2.0\"},\"source\":\"cache\",\"status\":\"idle\",\"result\":\"up-to-date\",\"checked_at\":\"2026-11-17T08:00:00Z\",\"synced_at\":\"2026-11-17T08:00:00Z\"}"
            }
            if payload.contains("/account-operations") {
                return "{\"items\":[]}"
            }
            if payload.contains("/accounts/101/models") {
                return "{\"models\":[{\"id\":\"gpt-5.4\",\"display_name\":\"GPT-5.4\"}]}"
            }
            if payload.contains("/proxy-management") {
                return proxiesJSON
            }
            if payload.contains("/sub2api-config/operation") {
                return "{\"status\":\"idle\"}"
            }
            if payload.contains("/sub2api-config") {
                return sub2apiCfgJSON
            }
            if payload.contains("/connection") {
                return "{\"state\":\"ok\",\"endpoint\":\"/api/v1/admin/accounts\",\"http_status\":200,\"message\":\"Sub2API 管理接口连通正常 (18 ms)\",\"retryable\":false,\"checked_at\":\"2026-11-17T09:15:00Z\"}"
            }
            return configJSON
        }

        let acc1 = Account(id: 101, name: "Codex · 主力", priority: 1, platform: "openai", type: "oauth")
        let acc2 = Account(id: 102, name: "Codex · 备用", priority: 2, platform: "openai", type: "oauth")

        let featStore = AppStore()
        featStore.ready = true
        featStore.page = .features
        featStore.applyViewState(
            ViewState(
                connected: true,
                online: true,
                snapshot: Snapshot(observed_at: "2026-11-17T09:15:00Z", accounts: [acc1, acc2]),
                preferences: Preferences(
                    base_url: "https://demo.example/sub2ops",
                    connections: [
                        SavedConnection(name: "主力生产节点", base_url: "https://demo.example/sub2ops"),
                        SavedConnection(name: "灾备节点", base_url: "https://backup.example.invalid/sub2ops"),
                    ],
                    launch_at_login: true
                )
            )
        )
        await featStore.reloadConfig()

        let setStore = AppStore()
        setStore.ready = true
        setStore.page = .settings
        setStore.applyViewState(featStore.state)
        await setStore.reloadConfig()

        for dark in [false, true] {
            let suffix = dark ? "dark" : "light"
            try await SnapshotRenderUtility.renderPNGAsync(
                view: MainShellView(store: featStore),
                size: CGSize(width: 1310, height: 920),
                dark: dark,
                outputURL: baseURL.appendingPathComponent("m5_features_recovery_\(suffix).png")
            )
            let ipView = FeaturesPageView(store: featStore, initialTab: .ip)
                .padding(24)
                .frame(width: 1090, height: 420, alignment: .topLeading)
                .background(DS.canvas)
            try await SnapshotRenderUtility.renderPNGAsync(
                view: ipView,
                size: CGSize(width: 1090, height: 420),
                dark: dark,
                outputURL: baseURL.appendingPathComponent("m5_features_ip_\(suffix).png")
            )
            let narrowIpView = FeaturesPageView(store: featStore, initialTab: .ip)
                .padding(16)
                .frame(width: 760, height: 420, alignment: .topLeading)
                .background(DS.canvas)
            try await SnapshotRenderUtility.renderPNGAsync(
                view: narrowIpView,
                size: CGSize(width: 760, height: 420),
                dark: dark,
                outputURL: baseURL.appendingPathComponent("m5_features_ip_narrow_\(suffix).png")
            )
            try SnapshotRenderUtility.renderPNG(
                view: ZStack { DS.backdrop; ProxyBatchAddSheet(online: true, onSaved: { _ in }, onClose: {}) }
                    .frame(width: 640, height: 860),
                size: CGSize(width: 640, height: 860),
                dark: dark,
                outputURL: baseURL.appendingPathComponent("m5_proxy_batch_add_\(suffix).png")
            )
            let sub2apiView = FeaturesPageView(store: featStore, initialTab: .sub2api)
                .padding(24)
                .frame(width: 1090, height: 520, alignment: .topLeading)
                .background(DS.canvas)
            try await SnapshotRenderUtility.renderPNGAsync(
                view: sub2apiView,
                size: CGSize(width: 1090, height: 520),
                dark: dark,
                outputURL: baseURL.appendingPathComponent("m5_features_sub2api_\(suffix).png")
            )
            try await SnapshotRenderUtility.renderPNGAsync(
                view: MainShellView(store: setStore),
                size: CGSize(width: 1310, height: 920),
                dark: dark,
                outputURL: baseURL.appendingPathComponent("m5_settings_\(suffix).png")
            )
        }
    }

    @Test("Sub2ApiConfigSession locks draft on remote revision change, load failure, and uncertain apply")
    func sub2ApiConfigConcurrencyAndUncertainApplyProtection() throws {
        var session = Sub2ApiConfigSession()
        let initial = Sub2ApiConfigTabView.ConfigDocumentDTO(
            available: true,
            editable: true,
            revision: "rev-1",
            runtime_revision: "run-1"
        )
        session.applyLoaded(initial, resetDraft: false)
        #expect(session.document?.editable == true)

        // User edits a field locally
        session.draft["server.port"] = Sub2ApiConfigTabView.DraftChange(path: "server.port", remove: false, value: .int(8088))

        // Tab switch reloads a newer remote revision while local draft exists -> lock editable & preserve draft
        let remoteChanged = Sub2ApiConfigTabView.ConfigDocumentDTO(
            available: true,
            editable: true,
            revision: "rev-2",
            runtime_revision: "run-1"
        )
        session.applyLoaded(remoteChanged, resetDraft: false)
        #expect(session.conflicted == true)
        #expect(session.document?.editable == false)
        #expect(session.document?.reason.contains("远程配置或运行态已变更") == true)
        #expect(session.draft["server.port"] != nil)

        // Discarding local draft requests reload and clears conflict
        let needsReload = session.discardAll()
        #expect(needsReload == true)
        #expect(session.draft.isEmpty)
        session.applyLoaded(remoteChanged, resetDraft: true, protectDraft: false)
        #expect(session.document?.editable == true)

        // Load failure locks stale document editable = false, and discardAll() recovers via reload
        session.draft["server.port"] = Sub2ApiConfigTabView.DraftChange(path: "server.port", remove: false, value: .int(8091))
        session.handleLoadFailure("连接超时")
        #expect(session.document?.editable == false)
        #expect(session.errorText == "连接超时")
        #expect(session.discardAll() == true)
        #expect(session.errorText.isEmpty)

        // Per-field undo of the last draft item also clears conflict lock & path-scoped issues
        session.applyLoaded(remoteChanged, resetDraft: true, protectDraft: false)
        session.draft["server.port"] = Sub2ApiConfigTabView.DraftChange(path: "server.port", remove: false, value: .int(8092))
        session.issues = [HostFieldIssue(path: "server.port", message: "端口冲突")]
        session.applyLoaded(
            Sub2ApiConfigTabView.ConfigDocumentDTO(available: true, editable: true, revision: "rev-3", runtime_revision: "run-1"),
            resetDraft: false
        )
        #expect(session.conflicted == true)
        #expect(session.undoField("server.port") == true)
        #expect(session.conflicted == false)
        #expect(session.issues.isEmpty)

        // Restore clean editable state with draft, then simulate apply network error -> applyUncertain
        session.applyLoaded(remoteChanged, resetDraft: true, protectDraft: false)
        session.draft["server.port"] = Sub2ApiConfigTabView.DraftChange(path: "server.port", remove: false, value: .int(8090))
        session.handleApplyFailure("请求中断")
        #expect(session.applyUncertain == true)
        #expect(Sub2ApiConfigSession.isRunning(session.hostOp) == true)

        // Polling encounters historical "succeeded" while uncertain -> MUST NOT reset draft!
        let resolution = session.handlePolledOperation(HostOperationReply(status: "succeeded", message: "上次重启完成", updated_at: nil))
        #expect(resolution == .reloadKeepingDraft)
        #expect(session.applyUncertain == false)
        #expect(session.hostOp?.status == "failed")
        #expect(session.hostOp?.message?.contains("草稿已保留") == true)
        #expect(session.draft["server.port"] != nil)

        // Switching connection resets session completely so cross-server drafts cannot leak or lock
        session.resetForConnection()
        #expect(session.document == nil)
        #expect(session.draft.isEmpty)
        #expect(session.hostOp == nil)
        #expect(session.conflicted == false)
        #expect(session.applyUncertain == false)

        // Also verify unavailable wire payload decodes without missing-key failure
        let unavailableData = Data("{\"available\":false,\"editable\":false,\"reason\":\"此服务尚未配置宿主机配置接口\"}".utf8)
        let unavailableDoc = try JSONDecoder().decode(Sub2ApiConfigTabView.ConfigDocumentDTO.self, from: unavailableData)
        #expect(unavailableDoc.available == false)
        #expect(unavailableDoc.editable == false)
        #expect(unavailableDoc.reason == "此服务尚未配置宿主机配置接口")
    }

    @Test("ModelTestSheet gates test start until policy version is loaded and clean")
    func modelTestPolicyReadinessGate() {
        let models = [ModelOption(id: "gpt-6-luna", display_name: "GPT-6 Luna")]
        // Loading policy -> blocked
        #expect(ModelTestSheet.canStartTest(
            online: true, loadingModels: false, isRunning: false,
            policyLoading: true, policySaving: false, policyVersion: "",
            policyDirty: false, policyError: "", selectedModel: "gpt-6-luna", models: models
        ) == false)

        // Policy load failed (empty version) -> blocked
        #expect(ModelTestSheet.canStartTest(
            online: true, loadingModels: false, isRunning: false,
            policyLoading: false, policySaving: false, policyVersion: "",
            policyDirty: false, policyError: "读取失败", selectedModel: "gpt-6-luna", models: models
        ) == false)

        // Policy loaded, clean, model valid -> allowed
        #expect(ModelTestSheet.canStartTest(
            online: true, loadingModels: false, isRunning: false,
            policyLoading: false, policySaving: false, policyVersion: String(repeating: "a", count: 64),
            policyDirty: false, policyError: "", selectedModel: "gpt-6-luna", models: models
        ) == true)
    }
}
}
