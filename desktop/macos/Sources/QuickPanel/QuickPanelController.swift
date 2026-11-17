import AppKit
import SwiftUI

private final class NonActivatingQuickPanel: NSPanel {
    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { false }
}

@MainActor
public final class QuickPanelController: NSObject, NSWindowDelegate {
    public private(set) var panel: NSPanel?
    private let store: AppStore
    private var lifecycle = PanelBlurLifecycle()
    private var lastAnchor: CGPoint?
    private weak var lastScreen: NSScreen?
    private var lastContentHeight: CGFloat = 320
    private var globalClickMonitor: Any?
    private var appDeactivateObserver: NSObjectProtocol?
    private var blurTask: Task<Void, Never>?
    public var onVisibilityChanged: (@MainActor () -> Void)?

    public init(store: AppStore) {
        self.store = store
        super.init()
        appDeactivateObserver = NotificationCenter.default.addObserver(
            forName: NSApplication.didResignActiveNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated {
                self?.scheduleBlurCheck()
            }
        }
    }

    public var isPanelVisible: Bool {
        panel?.isVisible ?? false
    }

    private func ensurePanel() -> NSPanel {
        if let existing = panel {
            return existing
        }
        let initialRect = CGRect(x: 0, y: 0, width: DS.quickPanelWidth, height: 320)
        let p = NonActivatingQuickPanel(
            contentRect: initialRect,
            styleMask: [.borderless, .nonactivatingPanel],
            backing: .buffered,
            defer: false
        )
        p.title = "Sub2Ops 快捷面板"
        p.level = .statusBar
        p.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .transient]
        p.isFloatingPanel = true
        p.hidesOnDeactivate = false
        p.isOpaque = false
        p.backgroundColor = .clear
        p.hasShadow = true
        p.isReleasedWhenClosed = false
        p.delegate = self

        let rootView = QuickPanelContainerView(
            store: store,
            onContentHeightChange: { [weak self] height in
                self?.updateContentHeight(height)
            }
        )
        let hosting = NSHostingView(rootView: rootView)
        hosting.frame = initialRect
        p.contentView = hosting
        self.panel = p
        RustBridge.shared.panelTrace("precreated", "hidden")
        return p
    }

    public func showPanel(anchorBottomRight: CGPoint, screen: NSScreen?) {
        RustBridge.shared.panelTrace("show_begin", "")
        let p = ensurePanel()
        lifecycle.show(focused: false)
        lifecycle.showing = true
        lastAnchor = anchorBottomRight
        lastScreen = screen ?? NSScreen.main

        let visibleFrame = (lastScreen ?? NSScreen.main)?.visibleFrame
            ?? CGRect(x: 0, y: 0, width: 1440, height: 900)
        let frame = PanelGeometry.panelFrame(
            anchorBottomRight: anchorBottomRight,
            visibleFrame: visibleFrame,
            contentHeight: lastContentHeight
        )
        p.setFrame(frame, display: true)
        RustBridge.shared.panelTrace(
            "positioned",
            "x=\(Int(frame.origin.x)) y=\(Int(frame.origin.y)) width=\(Int(frame.width)) height=\(Int(frame.height))"
        )

        p.makeKeyAndOrderFront(nil)
        lifecycle.showing = false
        lifecycle.focus()
        installGlobalClickMonitor()
        RustBridge.shared.panelTrace("shown", "focused")
        onVisibilityChanged?()
    }

    public func showCenteredFallback() {
        let visibleFrame = (NSScreen.main ?? NSScreen.screens.first)?.visibleFrame
            ?? CGRect(x: 0, y: 0, width: 1440, height: 900)
        let anchor = CGPoint(x: visibleFrame.maxX - 24, y: visibleFrame.maxY)
        showPanel(anchorBottomRight: anchor, screen: NSScreen.main)
    }

    public func updateContentHeight(_ rawHeight: CGFloat) {
        guard rawHeight.isFinite else { return }
        let clamped = min(max(rawHeight.rounded(.up), DS.quickPanelMinHeight), DS.quickPanelMaxHeight)
        guard abs(clamped - lastContentHeight) >= 1 else { return }
        lastContentHeight = clamped
        guard let p = panel, p.isVisible else { return }
        let visibleFrame = (lastScreen ?? p.screen ?? NSScreen.main)?.visibleFrame
            ?? CGRect(x: 0, y: 0, width: 1440, height: 900)
        if let anchor = lastAnchor {
            let nextFrame = PanelGeometry.panelFrame(
                anchorBottomRight: anchor,
                visibleFrame: visibleFrame,
                contentHeight: clamped
            )
            p.setFrame(nextFrame, display: true)
        } else {
            var frame = p.frame
            let delta = clamped - frame.height
            frame.origin.y -= delta
            frame.size.height = clamped
            p.setFrame(frame, display: true)
        }
    }

    public func hidePanel(reason: String) {
        blurTask?.cancel()
        lifecycle.show(focused: false)
        removeGlobalClickMonitor()
        panel?.orderOut(nil)
        RustBridge.shared.panelTrace("hidden", reason)
        onVisibilityChanged?()
    }

    public func windowDidBecomeKey(_ notification: Notification) {
        lifecycle.focus()
        RustBridge.shared.panelTrace("focused", "event")
    }

    public func windowDidResignKey(_ notification: Notification) {
        scheduleBlurCheck()
    }

    private func scheduleBlurCheck() {
        RustBridge.shared.panelTrace("blur", "")
        guard let gen = lifecycle.blur() else { return }
        blurTask?.cancel()
        blurTask = Task { @MainActor [weak self] in
            try? await Task.sleep(for: .milliseconds(150))
            guard !Task.isCancelled, let self else { return }
            guard !self.store.state.preferences.pinned else { return }
            guard self.lifecycle.canHide(generation: gen) else { return }
            if let p = self.panel, !p.isKeyWindow {
                self.hidePanel(reason: "blur")
            }
        }
    }

    private func installGlobalClickMonitor() {
        guard globalClickMonitor == nil else { return }
        globalClickMonitor = NSEvent.addGlobalMonitorForEvents(
            matching: [.leftMouseDown, .rightMouseDown, .otherMouseDown]
        ) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self, let p = self.panel, p.isVisible else { return }
                guard !self.store.state.preferences.pinned else { return }
                let mouseLoc = NSEvent.mouseLocation
                if !p.frame.contains(mouseLoc) {
                    self.scheduleBlurCheck()
                }
            }
        }
    }

    private func removeGlobalClickMonitor() {
        if let monitor = globalClickMonitor {
            NSEvent.removeMonitor(monitor)
            globalClickMonitor = nil
        }
    }
}

struct QuickPanelContainerView: View {
    let store: AppStore
    let onContentHeightChange: @MainActor (CGFloat) -> Void

    var body: some View {
        QuickPanelRootView(store: store, onContentHeightChange: onContentHeightChange)
    }
}
