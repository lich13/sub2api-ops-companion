import AppKit
import Foundation
import SwiftUI
import Testing
@testable import Sub2OpsMacOS

extension Sub2OpsMacOSTestRoot {
@Suite("M5 Features (4 Tabs) & Settings Logic + UI Snapshots", .serialized)
struct StageM5Tests {
    @Test("LightYAML parses block/flow structures, rejects duplicate keys, and enforces expected root type")
    func lightYAMLValidationAndFormatting() {
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

    @Test("Render Stage M5 Light & Dark PNG snapshots when SUB2OPS_SNAPSHOT_DIR is set")
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
            "revision": 4,
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
            "revision": 2,
            "enabled": true,
            "device_key_set": true
          }
        }
        """

        let modelGroupsJSON = """
        {
          "groups": [
            { "id": 1, "name": "Codex 主力组", "platform": "openai", "version": "v1" }
          ]
        }
        """

        let reasoningJSON = """
        {
          "group": { "id": 1, "name": "Codex 主力组", "platform": "openai", "version": "v1" },
          "revision": "rev-1",
          "items": [
            {
              "model": "gpt-5.4",
              "efforts": ["low", "medium", "high", "xhigh"],
              "default_effort": "high",
              "state": "active",
              "reason": "",
              "source": "upstream",
              "updated_at": "2026-11-17T09:00:00Z"
            }
          ],
          "status": { "state": "active", "message": "已同步 1 项思考档位覆盖" }
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
                return "{\"available\":true,\"model_count\":28,\"revision\":\"8f31c2a0\",\"updated_at\":\"2026-11-17T08:00:00Z\"}"
            }
            if payload.contains("/accounts/101/models") {
                return "{\"models\":[{\"id\":\"gpt-5.4\",\"display_name\":\"GPT-5.4\"}]}"
            }
            if payload.contains("/model-groups/1/reasoning") {
                return reasoningJSON
            }
            if payload.contains("/model-groups") {
                return modelGroupsJSON
            }
            if payload.contains("/proxy-management") {
                return proxiesJSON
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
            let modelsView = FeaturesPageView(store: featStore, initialTab: .models)
                .padding(24)
                .frame(width: 1090, height: 420, alignment: .topLeading)
                .background(DS.canvas)
            try await SnapshotRenderUtility.renderPNGAsync(
                view: modelsView,
                size: CGSize(width: 1090, height: 420),
                dark: dark,
                outputURL: baseURL.appendingPathComponent("m5_features_models_\(suffix).png")
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
}
}
