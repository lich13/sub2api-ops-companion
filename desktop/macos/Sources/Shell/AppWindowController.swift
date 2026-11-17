import AppKit
import Observation
import SwiftUI

@MainActor
public final class AppWindowController: NSObject, NSWindowDelegate {
    public private(set) var window: NSWindow?
    private let store: AppStore
    private var initialFrameApplied = false
    private var observers: [NSObjectProtocol] = []
    public var onVisibilityChanged: (@MainActor () -> Void)?

    public init(store: AppStore) {
        self.store = store
        super.init()
        installNotifications()
    }

    public var isMainVisible: Bool {
        window?.isVisible ?? false
    }

    public var isMainMinimized: Bool {
        window?.isMiniaturized ?? false
    }

    private func ensureWindow() -> NSWindow {
        if let existing = window {
            return existing
        }
        let screenFrame = (NSScreen.main ?? NSScreen.screens.first)?.visibleFrame
            ?? CGRect(x: 0, y: 0, width: 1440, height: 900)
        let initial = PanelGeometry.mainWindowFrame(visibleFrame: screenFrame)
        let win = NSWindow(
            contentRect: initial,
            styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
        win.title = "Sub2Ops"
        win.titleVisibility = .hidden
        win.titlebarAppearsTransparent = true
        win.isMovableByWindowBackground = false
        win.isReleasedWhenClosed = false
        win.backgroundColor = DS.nsCanvas
        let minW = min(900, initial.width)
        let minH = min(620, initial.height)
        win.minSize = NSSize(width: minW, height: minH)
        win.delegate = self

        let rootView = MainShellView(store: store)
        let hosting = NSHostingView(rootView: rootView)
        hosting.frame = CGRect(origin: .zero, size: initial.size)
        win.contentView = hosting

        win.setFrame(initial, display: true)
        initialFrameApplied = true
        self.window = win
        return win
    }

    public func showMainWindow() {
        let win = ensureWindow()
        if !initialFrameApplied, let screen = win.screen ?? NSScreen.main {
            let initial = PanelGeometry.mainWindowFrame(visibleFrame: screen.visibleFrame)
            win.setFrame(initial, display: false)
            initialFrameApplied = true
        }
        NSApp?.setActivationPolicy(.regular)
        if win.isMiniaturized {
            win.deminiaturize(nil)
        }
        win.makeKeyAndOrderFront(nil)
        NSApp?.activate(ignoringOtherApps: true)
        RustBridge.shared.panelTrace("main_visibility", "open_dock_visible")
        onVisibilityChanged?()
    }

    public func hideMainWindow() {
        window?.orderOut(nil)
        NSApp?.setActivationPolicy(.accessory)
        RustBridge.shared.panelTrace("main_visibility", "closed_dock_hidden")
        onVisibilityChanged?()
    }

    public func windowShouldClose(_ sender: NSWindow) -> Bool {
        hideMainWindow()
        return false
    }

    public func windowDidMiniaturize(_ notification: Notification) {
        onVisibilityChanged?()
    }

    public func windowDidDeminiaturize(_ notification: Notification) {
        onVisibilityChanged?()
    }

    public func windowDidBecomeKey(_ notification: Notification) {
        onVisibilityChanged?()
    }

    private func installNotifications() {
        let center = NotificationCenter.default
        observers.append(
            center.addObserver(forName: NSApplication.didBecomeActiveNotification, object: nil, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated {
                    self?.onVisibilityChanged?()
                }
            }
        )
        observers.append(
            center.addObserver(forName: NSApplication.didResignActiveNotification, object: nil, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated {
                    self?.onVisibilityChanged?()
                }
            }
        )
    }
}

// MARK: - Host Coordinator

@MainActor
public final class MacHostCoordinator {
    public static let shared = MacHostCoordinator()

    public let store = AppStore()
    public lazy var mainWindowController = AppWindowController(store: store)
    public let statusItemController = StatusItemController()
    public lazy var quickPanelController = QuickPanelController(store: store)
    private var started = false

    private init() {}

    public func start(silentLaunch: Bool) {
        guard !started else { return }
        started = true

        store.onReopenMainWindow = { [weak self] in
            self?.mainWindowController.showMainWindow()
        }
        store.onShowQuickPanel = { [weak self] in
            guard let self else { return }
            if let (anchor, screen) = self.statusItemController.currentAnchor() {
                self.quickPanelController.showPanel(anchorBottomRight: anchor, screen: screen)
            } else {
                self.quickPanelController.showCenteredFallback()
            }
        }
        store.onHideQuickPanel = { [weak self] in
            self?.quickPanelController.hidePanel(reason: "explicit")
        }

        mainWindowController.onVisibilityChanged = { [weak self] in
            self?.syncVisibility()
        }
        quickPanelController.onVisibilityChanged = { [weak self] in
            self?.syncVisibility()
        }

        statusItemController.onLeftClick = { [weak self] anchor, screen in
            self?.quickPanelController.showPanel(anchorBottomRight: anchor, screen: screen)
        }
        statusItemController.onOpenMain = { [weak self] in
            self?.mainWindowController.showMainWindow()
        }
        statusItemController.onCheckUpdates = { [weak self] in
            guard let self else { return }
            Task { @MainActor in
                await self.store.checkUpdatesAndNotify()
                self.mainWindowController.showMainWindow()
            }
        }
        statusItemController.onQuit = {
            RustBridge.shared.quit()
        }

        statusItemController.install()
        observeTooltip()
        store.bindBridge()

        if silentLaunch {
            NSApp?.setActivationPolicy(.accessory)
            syncVisibility()
        } else {
            mainWindowController.showMainWindow()
        }
    }

    private func observeTooltip() {
        withObservationTracking {
            let tip = store.trayTooltip
            statusItemController.updateTooltip(tip)
        } onChange: { [weak self] in
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    self?.observeTooltip()
                }
            }
        }
    }

    public func syncVisibility() {
        let active = NSApp?.isActive ?? false
        RustBridge.shared.setVisibility(
            mainVisible: mainWindowController.isMainVisible,
            mainMinimized: mainWindowController.isMainMinimized,
            quickVisible: quickPanelController.isPanelVisible,
            appActive: active
        )
    }
}
