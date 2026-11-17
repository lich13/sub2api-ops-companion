import AppKit
import SwiftUI

/// Preserves and restores the previously focused AppKit responder when a modal or sheet closes,
/// matching `desktop/src/dialogFocus.ts`.
@MainActor
public final class ResponderRestorer {
    private weak var window: NSWindow?
    private weak var previousResponder: NSResponder?

    public init() {}

    public func capture() {
        guard let keyWindow = NSApp?.keyWindow else { return }
        self.window = keyWindow
        self.previousResponder = keyWindow.firstResponder
    }

    public func restore() {
        guard let window, let previousResponder else { return }
        if window.isVisible {
            window.makeFirstResponder(previousResponder)
        }
    }
}

private struct DialogFocusModifier: ViewModifier {
    let onEscape: (() -> Void)?
    @State private var restorer = ResponderRestorer()

    func body(content: Content) -> some View {
        content
            .onAppear {
                restorer.capture()
            }
            .onDisappear {
                restorer.restore()
            }
            .onExitCommand {
                onEscape?()
            }
    }
}

extension View {
    public func sub2opsDialogFocus(onEscape: (() -> Void)? = nil) -> some View {
        modifier(DialogFocusModifier(onEscape: onEscape))
    }
}
