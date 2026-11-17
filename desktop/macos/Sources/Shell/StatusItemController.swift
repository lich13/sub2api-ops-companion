import AppKit

@MainActor
public final class StatusItemController: NSObject {
    private var statusItem: NSStatusItem?
    public var onLeftClick: (@MainActor (CGPoint, NSScreen?) -> Void)?
    public var onOpenMain: (@MainActor () -> Void)?
    public var onCheckUpdates: (@MainActor () -> Void)?
    public var onQuit: (@MainActor () -> Void)?

    public override init() {
        super.init()
    }

    public func install() {
        guard statusItem == nil else { return }
        let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
        self.statusItem = item
        if let button = item.button {
            button.image = Self.makePulseTemplateImage()
            button.imagePosition = .imageOnly
            button.toolTip = "Sub2Ops"
            button.target = self
            button.action = #selector(handleStatusBarClick(_:))
            button.sendAction(on: [.leftMouseUp, .rightMouseDown])
        }
        RustBridge.shared.panelTrace("tray_ready", "native_nsstatusitem")
    }

    public func updateTooltip(_ text: String) {
        statusItem?.button?.toolTip = text
    }

    public func currentAnchor() -> (CGPoint, NSScreen?)? {
        guard let button = statusItem?.button, let btnWindow = button.window else {
            return nil
        }
        let rectInWindow = button.convert(button.bounds, to: nil)
        let screenRect = btnWindow.convertToScreen(rectInWindow)
        return (CGPoint(x: screenRect.maxX, y: screenRect.minY), btnWindow.screen ?? NSScreen.main)
    }

    @objc private func handleStatusBarClick(_ sender: NSStatusBarButton) {
        guard let event = NSApp?.currentEvent else {
            triggerLeftClick(from: sender)
            return
        }
        let isRight = event.type == .rightMouseDown
            || event.type == .rightMouseUp
            || (event.type == .leftMouseUp && event.modifierFlags.contains(.control))
        RustBridge.shared.panelTrace("tray_click", isRight ? "right" : "left")
        if isRight {
            presentContextMenu(from: sender)
        } else {
            triggerLeftClick(from: sender)
        }
    }

    private func triggerLeftClick(from button: NSStatusBarButton) {
        guard let btnWindow = button.window else { return }
        let rectInWindow = button.convert(button.bounds, to: nil)
        let screenRect = btnWindow.convertToScreen(rectInWindow)
        let anchor = CGPoint(x: screenRect.maxX, y: screenRect.minY)
        onLeftClick?(anchor, btnWindow.screen ?? NSScreen.main)
    }

    private func presentContextMenu(from button: NSStatusBarButton) {
        guard let statusItem else { return }
        let menu = NSMenu(title: "Sub2Ops")
        let openItem = NSMenuItem(title: "打开 Sub2Ops", action: #selector(menuOpenMain), keyEquivalent: "")
        openItem.target = self
        menu.addItem(openItem)

        let updateItem = NSMenuItem(title: "检查更新", action: #selector(menuCheckUpdates), keyEquivalent: "")
        updateItem.target = self
        menu.addItem(updateItem)

        menu.addItem(NSMenuItem.separator())

        let quitItem = NSMenuItem(title: "退出", action: #selector(menuQuit), keyEquivalent: "q")
        quitItem.target = self
        menu.addItem(quitItem)

        statusItem.menu = menu
        button.performClick(nil)
        statusItem.menu = nil
    }

    @objc private func menuOpenMain() {
        onOpenMain?()
    }

    @objc private func menuCheckUpdates() {
        onCheckUpdates?()
    }

    @objc private func menuQuit() {
        onQuit?()
    }

    /// Original 22×22 monochrome pulse mark matching `lib.rs` template icon coordinates.
    public static func makePulseTemplateImage() -> NSImage {
        let width = 22
        let height = 22
        guard let rep = NSBitmapImageRep(
            bitmapDataPlanes: nil,
            pixelsWide: width,
            pixelsHigh: height,
            bitsPerSample: 8,
            samplesPerPixel: 4,
            hasAlpha: true,
            isPlanar: false,
            colorSpaceName: .deviceRGB,
            bytesPerRow: width * 4,
            bitsPerPixel: 32
        ), let data = rep.bitmapData else {
            let fallback = NSImage(size: NSSize(width: width, height: height))
            fallback.isTemplate = true
            return fallback
        }
        memset(data, 0, width * height * 4)
        for x in 2..<20 {
            let y: Int
            switch x {
            case 6...8: y = 11 - (x - 5) * 2
            case 9...12: y = 5 + (x - 8) * 3
            case 13...15: y = 17 - (x - 12) * 2
            default: y = 11
            }
            for d in 0..<2 {
                let row = min(21, y + d)
                let idx = (row * width + x) * 4
                data[idx + 3] = 255
            }
        }
        let image = NSImage(size: NSSize(width: width, height: height))
        image.addRepresentation(rep)
        image.isTemplate = true
        return image
    }
}
