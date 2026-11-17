import AppKit
import Foundation
import SwiftUI
import Testing
@testable import Sub2OpsMacOS

@MainActor
enum SnapshotRenderUtility {
    static func ensureAppKit() {
        if NSApp == nil {
            _ = NSApplication.shared
            NSApp?.setActivationPolicy(.prohibited)
        }
    }

    static func renderPNG<V: View>(
        view: V,
        size: CGSize,
        dark: Bool,
        outputURL: URL
    ) throws {
        ensureAppKit()
        let appearanceName: NSAppearance.Name = dark ? .darkAqua : .aqua
        guard let appearance = NSAppearance(named: appearanceName) else { return }

        let window = NSWindow(
            contentRect: CGRect(origin: .zero, size: size),
            styleMask: [.borderless],
            backing: .buffered,
            defer: false
        )
        window.appearance = appearance
        window.backgroundColor = dark
            ? NSColor(srgbRed: 0x17 / 255.0, green: 0x19 / 255.0, blue: 0x1F / 255.0, alpha: 1.0)
            : NSColor(srgbRed: 0xF5 / 255.0, green: 0xF6 / 255.0, blue: 0xF9 / 255.0, alpha: 1.0)
        window.isReleasedWhenClosed = false

        let themedView = view
            .environment(\.colorScheme, dark ? .dark : .light)
            .frame(width: size.width, height: size.height)
        let hosting = NSHostingView(rootView: themedView)
        hosting.appearance = appearance
        hosting.frame = CGRect(origin: .zero, size: size)
        window.contentView = hosting
        hosting.layoutSubtreeIfNeeded()
        RunLoop.main.run(until: Date().addingTimeInterval(0.03))
        hosting.layoutSubtreeIfNeeded()

        guard let rep = hosting.bitmapImageRepForCachingDisplay(in: hosting.bounds) else {
            return
        }
        NSAppearance.current = appearance
        hosting.cacheDisplay(in: hosting.bounds, to: rep)
        if let pngData = rep.representation(using: .png, properties: [:]) {
            try FileManager.default.createDirectory(
                at: outputURL.deletingLastPathComponent(),
                withIntermediateDirectories: true
            )
            try pngData.write(to: outputURL)
        }
        window.orderOut(nil)
    }
}

@Suite("UI Light & Dark Snapshot Renderer")
struct SnapshotRenderTests {
    @Test("Render Stage M1 Light & Dark PNG snapshots when SUB2OPS_SNAPSHOT_DIR is set")
    @MainActor
    func renderStageSnapshots() throws {
        guard let dirPath = ProcessInfo.processInfo.environment["SUB2OPS_SNAPSHOT_DIR"],
              !dirPath.isEmpty
        else {
            return
        }
        let baseURL = URL(fileURLWithPath: dirPath)
        RustBridge.shared.setMockHandler { cmd, _ in
            if cmd == "api_request" {
                return """
                {"state":"ok","endpoint":"https://companion.example.invalid","http_status":200,"message":"Sub2API 管理接口正常","retryable":false,"checked_at":"2026-09-30T08:00:00Z"}
                """
            }
            return "null"
        }

        // 1. Disconnected Onboarding Shell
        let onboardingStore = AppStore()
        onboardingStore.ready = true
        onboardingStore.applyViewState(
            ViewState(
                connected: false,
                online: false,
                preferences: Preferences(
                    base_url: "",
                    connections: [
                        SavedConnection(name: "主力生产节点", base_url: "https://prod.example.invalid/sub2ops"),
                        SavedConnection(name: "备用容灾节点", base_url: "https://dr.example.invalid/sub2ops"),
                    ]
                )
            )
        )

        // 2. Connected Shell (Settings + Offline Banner + Pending Operations Badge)
        let shellStore = AppStore()
        shellStore.ready = true
        shellStore.page = .settings
        shellStore.applyViewState(
            ViewState(
                connected: true,
                online: false,
                error: "网关连接超时 [504]",
                snapshot: Snapshot(
                    observed_at: "2026-09-30T08:05:12Z",
                    accounts: [Account(id: 7, name: "Codex-Prod-01")],
                    groups: [Group(id: 1, name: "Codex 主力组")]
                ),
                preferences: Preferences(
                    base_url: "https://prod.example.invalid/sub2ops",
                    connections: [
                        SavedConnection(name: "主力生产节点", base_url: "https://prod.example.invalid/sub2ops"),
                    ]
                )
            )
        )
        shellStore.operations.setOperations([
            Operation(
                id: String(repeating: "a", count: 32),
                account_id: 7,
                account_name: "Codex-Prod-01",
                action: "schedulable",
                status: "needs_confirmation",
                reason: "账号版本已变化，请核对当前调度状态",
                requested: ["schedulable": .bool(true)],
                current: ["schedulable": .bool(false)]
            ),
        ])

        for dark in [false, true] {
            let suffix = dark ? "dark" : "light"
            try SnapshotRenderUtility.renderPNG(
                view: MainShellView(store: onboardingStore),
                size: CGSize(width: 1310, height: 860),
                dark: dark,
                outputURL: baseURL.appendingPathComponent("m1_onboarding_\(suffix).png")
            )
            try SnapshotRenderUtility.renderPNG(
                view: MainShellView(store: shellStore),
                size: CGSize(width: 1310, height: 860),
                dark: dark,
                outputURL: baseURL.appendingPathComponent("m1_shell_offline_\(suffix).png")
            )
            try SnapshotRenderUtility.renderPNG(
                view: OperationPanelSheet(store: shellStore),
                size: CGSize(width: 440, height: 320),
                dark: dark,
                outputURL: baseURL.appendingPathComponent("m1_operations_\(suffix).png")
            )
        }
    }
}
