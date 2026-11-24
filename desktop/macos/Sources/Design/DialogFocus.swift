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

    /// Suppresses modal `Escape` dismissal when an AppKit text responder has uncommitted IME marked text.
    public static func shouldSuppressEscape(for responder: Any?) -> Bool {
        if (responder as? NSTextInputClient)?.hasMarkedText() == true {
            return true
        }
        if let control = responder as? NSControl,
           (control.currentEditor() as? NSTextInputClient)?.hasMarkedText() == true {
            return true
        }
        return false
    }

    @MainActor
    public static func shouldSuppressActiveEscape() -> Bool {
        let activeResponder = NSApp?.keyWindow?.firstResponder ?? NSApp?.mainWindow?.firstResponder
        return shouldSuppressEscape(for: activeResponder)
    }
}

/// Coordinates backdrop click dismissal between modal hosts (`AccountModalHostView`, `presentWindowModal`)
/// and child sheets that guard unsaved drafts (`isDirty`) or in-flight submissions (`busy` / `saving`).
@MainActor
public final class ModalBackdropGuard {
    public private(set) var canDismiss: Bool = true
    private var customHandler: (() -> Void)?

    public init() {}

    public func bind(canDismiss: Bool, onBackdrop: (() -> Void)? = nil) {
        self.canDismiss = canDismiss
        self.customHandler = onBackdrop
    }

    public func updateCanDismiss(_ allowed: Bool) {
        self.canDismiss = allowed
    }

    public func attemptDismiss(defaultClose: () -> Void) {
        if let customHandler {
            customHandler()
            return
        }
        guard canDismiss else { return }
        defaultClose()
    }
}

private struct ModalBackdropGuardKey: EnvironmentKey {
    static let defaultValue: ModalBackdropGuard? = nil
}

extension EnvironmentValues {
    public var modalBackdropGuard: ModalBackdropGuard? {
        get { self[ModalBackdropGuardKey.self] }
        set { self[ModalBackdropGuardKey.self] = newValue }
    }
}

private struct DialogFocusModifier: ViewModifier {
    let onEscape: (() -> Void)?
    let canBackdropDismiss: Bool
    let onBackdrop: (() -> Void)?
    @Environment(\.modalBackdropGuard) private var backdropGuard
    @State private var restorer = ResponderRestorer()

    func body(content: Content) -> some View {
        content
            .onAppear {
                restorer.capture()
                backdropGuard?.bind(canDismiss: canBackdropDismiss, onBackdrop: onBackdrop)
            }
            .onChange(of: canBackdropDismiss) { _, next in
                backdropGuard?.bind(canDismiss: next, onBackdrop: onBackdrop)
            }
            .onDisappear {
                restorer.restore()
            }
            .onExitCommand {
                guard !ResponderRestorer.shouldSuppressActiveEscape() else {
                    return
                }
                onEscape?()
            }
    }
}

private struct SwitcherPopoverModifier<PopoverContent: View>: ViewModifier {
    let anchor: SwitcherAnchor
    @Bindable var store: AppStore
    let arrowEdge: Edge
    @ViewBuilder let popoverContent: () -> PopoverContent
    @State private var isPresented: Bool = false

    func body(content: Content) -> some View {
        content
            .onAppear {
                isPresented = (store.switcherAnchor == anchor)
            }
            .onChange(of: store.switcherAnchor) { _, next in
                let shouldOpen = (next == anchor)
                if isPresented != shouldOpen {
                    isPresented = shouldOpen
                }
            }
            .onChange(of: isPresented) { _, open in
                if !open && store.switcherAnchor == anchor {
                    store.switcherAnchor = nil
                }
            }
            .popover(isPresented: $isPresented, arrowEdge: arrowEdge, content: popoverContent)
    }
}

extension View {
    public func sub2opsDialogFocus(
        canBackdropDismiss: Bool = true,
        onBackdrop: (() -> Void)? = nil,
        onEscape: (() -> Void)? = nil
    ) -> some View {
        modifier(
            DialogFocusModifier(
                onEscape: onEscape,
                canBackdropDismiss: canBackdropDismiss,
                onBackdrop: onBackdrop
            )
        )
    }

    @MainActor
    public func sub2opsSwitcherPopover<PopoverContent: View>(
        anchor: SwitcherAnchor,
        store: AppStore,
        arrowEdge: Edge,
        @ViewBuilder content: @escaping () -> PopoverContent
    ) -> some View {
        modifier(
            SwitcherPopoverModifier(
                anchor: anchor,
                store: store,
                arrowEdge: arrowEdge,
                popoverContent: content
            )
        )
    }
}
