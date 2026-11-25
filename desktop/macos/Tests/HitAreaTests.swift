import AppKit
import Foundation
import SwiftUI
import Testing
@testable import Sub2OpsMacOS

@MainActor
enum SyntheticClick {
    /// Hosts `view` in an offscreen window so real mouse events can be routed through
    /// AppKit hit testing.
    static func host<V: View>(_ view: V, size: CGSize) -> NSWindow {
        SnapshotRenderUtility.ensureAppKit()
        let window = NSWindow(
            contentRect: CGRect(origin: .zero, size: size),
            styleMask: [.borderless],
            backing: .buffered,
            defer: false
        )
        window.isReleasedWhenClosed = false
        let hosting = NSHostingView(rootView: view.frame(width: size.width, height: size.height))
        hosting.frame = CGRect(origin: .zero, size: size)
        window.contentView = hosting
        window.setFrameOrigin(CGPoint(x: -20000, y: -20000))
        window.orderFrontRegardless()
        hosting.layoutSubtreeIfNeeded()
        RunLoop.main.run(until: Date().addingTimeInterval(0.2))
        hosting.layoutSubtreeIfNeeded()
        return window
    }

    /// Delivers a left click at `point` (SwiftUI coordinates, origin top-left).
    static func click(_ window: NSWindow, at point: CGPoint) {
        let height = window.contentView?.bounds.height ?? 0
        let location = CGPoint(x: point.x, y: height - point.y)
        func event(_ type: NSEvent.EventType) -> NSEvent? {
            NSEvent.mouseEvent(
                with: type,
                location: location,
                modifierFlags: [],
                timestamp: ProcessInfo.processInfo.systemUptime,
                windowNumber: window.windowNumber,
                context: nil,
                eventNumber: 0,
                clickCount: 1,
                pressure: 1
            )
        }
        guard let down = event(.leftMouseDown), let up = event(.leftMouseUp) else { return }
        NSApp.sendEvent(down)
        RunLoop.main.run(until: Date().addingTimeInterval(0.05))
        NSApp.sendEvent(up)
        RunLoop.main.run(until: Date().addingTimeInterval(0.1))
    }

    static func teardown(_ window: NSWindow) {
        SnapshotRenderUtility.teardownWindow(window)
    }
}

@MainActor
private final class ClickCounter {
    var hits = 0
}

extension Sub2OpsMacOSTestRoot {
@Suite("Button hit areas", .serialized)
struct HitAreaTests {
    @Test("linearPlain buttons respond across their whole frame, not only on drawn glyphs")
    @MainActor
    func linearPlainButtonFillsItsFrame() {
        let counter = ClickCounter()
        let view = VStack(spacing: 0) {
            Button { counter.hits += 1 } label: {
                HStack { Text("A"); Spacer(minLength: 0) }
                    .padding(.horizontal, 11)
                    .frame(height: 36)
                    .background(DS.clear)
            }
            .buttonStyle(.linearPlain)
            Spacer()
        }
        .frame(width: 160)
        let window = SyntheticClick.host(view, size: CGSize(width: 160, height: 200))
        defer { SyntheticClick.teardown(window) }

        // Empty area far from the glyph.
        SyntheticClick.click(window, at: CGPoint(x: 140, y: 18))
        #expect(counter.hits == 1)
        // On the glyph itself.
        SyntheticClick.click(window, at: CGPoint(x: 16, y: 18))
        #expect(counter.hits == 2)
        // Below the button nothing fires.
        SyntheticClick.click(window, at: CGPoint(x: 80, y: 120))
        #expect(counter.hits == 2)
    }

    @Test("Sidebar navigation rows switch pages when clicked beside the label")
    @MainActor
    func sidebarRowsAreClickableBesideLabel() {
        let store = AppStore()
        defer { store.stop() }
        store.ready = true
        store.page = .accounts
        store.applyViewState(
            ViewState(
                connected: true,
                online: false,
                snapshot: Snapshot(
                    observed_at: "2026-09-30T08:05:12Z",
                    accounts: [Account(id: 7, name: "Codex-Prod-01")],
                    groups: [Group(id: 1, name: "Codex 主力组")]
                ),
                preferences: Preferences(base_url: "https://prod.example.invalid/sub2ops")
            )
        )
        let window = SyntheticClick.host(MainShellView(store: store), size: CGSize(width: 1310, height: 860))
        defer { SyntheticClick.teardown(window) }

        // The 1310pt window uses the compact sidebar. Sweep up the right edge of the
        // navigation list, well clear of every icon and label; each row must navigate even
        // though the window drag handle sits behind the sidebar. Stops before reaching the
        // instance switcher above the list.
        let x = DS.sidebarCompactWidth - 22
        var visited: Set<AppPage> = []
        for y in stride(from: CGFloat(440), through: 152, by: -8) {
            SyntheticClick.click(window, at: CGPoint(x: x, y: y))
            visited.insert(store.page)
            if visited.count == AppPage.allCases.count { break }
        }
        #expect(visited == Set(AppPage.allCases))
    }

    @Test("SchedulingParametersView editable value slot responds in left whitespace away from right-aligned digits")
    @MainActor
    func schedulingParametersFullRightSlotHitArea() {
        let store = AppStore()
        defer { store.stop() }
        store.ready = true
        let account = Account(
            id: 101,
            name: "Codex",
            priority: 1,
            platform: "openai",
            type: "oauth",
            concurrency: 2,
            load_factor: nil,
            operation_versions: ["concurrency": "v1", "load_factor": "v1"]
        )
        store.applyViewState(
            ViewState(
                connected: true,
                online: true,
                snapshot: Snapshot(observed_at: "2026-11-17T08:00:00Z", accounts: [account])
            )
        )
        let view = VStack(spacing: 0) {
            SchedulingParametersView(account: account, store: store)
                .frame(width: 100)
            Spacer()
        }
        let window = SyntheticClick.host(view, size: CGSize(width: 100, height: 120))
        defer { SyntheticClick.teardown(window) }

        let beforeChildren = window.childWindows?.count ?? 0
        // Row 2 ("并发") spans y ≈ 24...46; x = 58 is in the left empty portion of the right value slot [51...100],
        // far away from the right-aligned "2" glyph near x ≈ 92.
        SyntheticClick.click(window, at: CGPoint(x: 58, y: 35))
        let afterChildren = window.childWindows?.count ?? 0
        #expect(afterChildren > beforeChildren)
    }
}
}
