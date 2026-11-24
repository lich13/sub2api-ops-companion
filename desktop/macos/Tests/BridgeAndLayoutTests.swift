import AppKit
import CoreGraphics
import Foundation
import Testing
@testable import Sub2OpsMacOS

@Suite("Sub2Ops macOS Test Suite", .serialized)
enum Sub2OpsMacOSTestRoot {}

extension Sub2OpsMacOSTestRoot {
@Suite("M1 Bridge, Domain & Panel Layout", .serialized)
struct BridgeAndLayoutTests {
    @Test("Obsolete blur cannot close a reopened panel")
    func obsoleteBlurCannotCloseReopenedPanel() {
        var panel = PanelBlurLifecycle()
        panel.show(focused: false)
        #expect(panel.blur() == nil)
        panel.focus()
        let blur1 = panel.blur()
        #expect(blur1 != nil)
        #expect(panel.canHide(generation: blur1!))
        panel.show(focused: false)
        #expect(!panel.canHide(generation: blur1!))
        panel.focus()
        let blur2 = panel.blur()
        #expect(blur2 != nil)
        panel.focus()
        #expect(!panel.canHide(generation: blur2!))
    }

    @Test("Panel stays inside secondary and scaled displays")
    func panelStaysInsideSecondaryAndScaledDisplays() {
        let displays: [(CGRect, CGPoint)] = [
            (CGRect(x: -1920, y: 24, width: 1920, height: 1056), CGPoint(x: -8, y: 1080)),
            (CGRect(x: 0, y: 50, width: 2560, height: 1614), CGPoint(x: 2540, y: 1664)),
            (CGRect(x: 0, y: -900, width: 800, height: 850), CGPoint(x: 200, y: -50)),
        ]
        for (visibleFrame, anchor) in displays {
            let frame = PanelGeometry.panelFrame(
                anchorBottomRight: anchor,
                visibleFrame: visibleFrame,
                contentHeight: 520
            )
            #expect(frame.minX >= visibleFrame.minX)
            #expect(frame.minY >= visibleFrame.minY)
            #expect(frame.maxX <= visibleFrame.maxX)
            #expect(frame.maxY <= visibleFrame.maxY)
        }
    }

    @Test("Compact panel height is content bounded between 128 and 520")
    func compactPanelHeightIsContentBounded() {
        let visibleFrame = CGRect(x: 0, y: 24, width: 1400, height: 900)
        let anchor = CGPoint(x: 800, y: 924)
        for (height, expected): (CGFloat, CGFloat) in [(100, 128), (386, 386), (800, 520)] {
            let frame = PanelGeometry.panelFrame(
                anchorBottomRight: anchor,
                visibleFrame: visibleFrame,
                contentHeight: height
            )
            #expect(frame.width == 420)
            #expect(frame.height == expected)
        }
    }

    @Test("Beijing time formatting matches MM-DD HH:mm:ss and fallbacks")
    func beijingTimeFormatting() {
        #expect(BeijingTime.fullTime(nil) == "暂无记录")
        #expect(BeijingTime.fullTime("") == "暂无记录")
        #expect(BeijingTime.fullTime("invalid-date") == "时间未知")
        #expect(BeijingTime.fullTime("2026-02-29T00:00:00Z") == "时间未知")
        #expect(BeijingTime.fullTime("2024-02-29T18:30:00Z") == "03-01 02:30:00")
        #expect(BeijingTime.fullTime("2026-12-31T16:00:00.987654321Z") == "01-01 00:00:00")
        #expect(BeijingTime.fullTime("2026-09-30T08:05:09Z") == "09-30 16:05:09")
        #expect(BeijingTime.fullTime("2026-09-30 16:05:09+00:00") == "10-01 00:05:09")
        #expect(BeijingTime.fullTime("2026-09-30T11:05:09-05:00") == "10-01 00:05:09")
        #expect(BeijingTime.unixSeconds(1_790_755_509) == "09-30 16:05:09")
        #expect(BeijingTime.isoUTCString(fromEpochSeconds: 1_790_755_509) == "2026-09-30T08:05:09Z")
        #expect(BeijingTime.isoUTCString(fromEpochSeconds: .nan) == "1970-01-01T00:00:00Z")
        #expect(BeijingTime.isoUTCString(fromEpochSeconds: .infinity) == "1970-01-01T00:00:00Z")
        #expect(BeijingTime.parseBeijingLocal("2026-02-29 12:00") == nil)
    }

    @MainActor
    @Test("IME marked text suppresses Escape, ModalBackdropGuard blocks dirty close, and retained tabs reset per connection")
    func modalBackdropImeAndRetainedTabGuards() {
        let textView = NSTextView(frame: NSRect(x: 0, y: 0, width: 120, height: 24))
        #expect(!ResponderRestorer.shouldSuppressEscape(for: textView))
        textView.setMarkedText(
            "nihao",
            selectedRange: NSRange(location: 5, length: 0),
            replacementRange: NSRange(location: NSNotFound, length: 0)
        )
        #expect(ResponderRestorer.shouldSuppressEscape(for: textView))
        textView.unmarkText()
        #expect(!ResponderRestorer.shouldSuppressEscape(for: textView))

        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 200, height: 80),
            styleMask: [.titled],
            backing: .buffered,
            defer: false
        )
        let textField = NSTextField(frame: NSRect(x: 10, y: 10, width: 160, height: 24))
        window.contentView?.addSubview(textField)
        _ = window.makeFirstResponder(textField)
        if let editor = textField.currentEditor() as? NSTextView {
            editor.setMarkedText(
                "pinyin",
                selectedRange: NSRange(location: 6, length: 0),
                replacementRange: NSRange(location: NSNotFound, length: 0)
            )
            #expect(ResponderRestorer.shouldSuppressEscape(for: textField))
            editor.unmarkText()
            #expect(!ResponderRestorer.shouldSuppressEscape(for: textField))
        }

        let opStore = OperationStore()
        opStore.bindConnection("https://a.example.invalid:true:1")
        opStore.setOperations([
            Operation(id: "op-1", account_id: 101, account_name: "Codex", action: "schedulable", status: "running"),
            Operation(id: "op-2", account_id: 101, account_name: "Codex", action: "recover", status: "completed"),
        ])
        #expect(opStore.pendingForAccount(101).map(\.id) == ["op-1"])
        #expect(opStore.pendingForAccount(999).isEmpty)
        opStore.bindConnection("https://b.example.invalid:true:2")
        #expect(opStore.pendingForAccount(101).isEmpty)

        let backdropGuard = ModalBackdropGuard()
        var closedCount = 0
        var discardPromptCount = 0
        backdropGuard.bind(canDismiss: false, onBackdrop: nil)
        backdropGuard.attemptDismiss { closedCount += 1 }
        #expect(closedCount == 0)

        backdropGuard.updateCanDismiss(true)
        backdropGuard.attemptDismiss { closedCount += 1 }
        #expect(closedCount == 1)

        backdropGuard.bind(canDismiss: false) { discardPromptCount += 1 }
        backdropGuard.attemptDismiss { closedCount += 1 }
        #expect(closedCount == 1)
        #expect(discardPromptCount == 1)

        var visited: Set<AppPage> = [.accounts]
        visited = PageContentRouterView.updatedVisitedPages(current: visited, activePage: .groups)
        visited = PageContentRouterView.updatedVisitedPages(current: visited, activePage: .priorities)
        visited = PageContentRouterView.updatedVisitedPages(current: visited, activePage: .records)
        #expect(visited.contains(.groups) && visited.contains(.priorities))
        visited = PageContentRouterView.updatedVisitedPages(
            current: visited,
            activePage: .accounts,
            connectionChanged: true
        )
        #expect(visited == [.accounts])
    }

    @Test("Synchronous C ABI response inside invoke does not deadlock and zero-len slice is safe")
    func cAbiSynchronousResponseAndZeroLenGuard() async throws {
        RustBridge.shared.setMockHandler(nil)
        let callbacks = Sub2OpsHostCallbacks(
            invoke: { requestId, cmdPtr, cmdLen, _, _ in
                let cmd = cmdPtr.map { String(decoding: UnsafeBufferPointer(start: $0, count: cmdLen), as: UTF8.self) } ?? ""
                if cmd == "sync_ok" {
                    let reply = Array("\"pong\"".utf8)
                    reply.withUnsafeBufferPointer { buf in
                        sub2ops_macos_on_response(requestId: requestId, ok: true, ptr: buf.baseAddress, len: buf.count)
                    }
                } else {
                    let err = Array("同步拒绝".utf8)
                    err.withUnsafeBufferPointer { buf in
                        sub2ops_macos_on_response(requestId: requestId, ok: false, ptr: buf.baseAddress, len: buf.count)
                    }
                }
            },
            set_visibility: { _, _, _, _ in },
            panel_trace: { _, _, _, _ in },
            quit: {}
        )
        RustBridge.shared.installHostCallbacks(callbacks)

        let pong: String = try await RustBridge.shared.command("sync_ok")
        #expect(pong == "pong")

        await #expect(throws: RustBridgeError.self) {
            let _: String = try await RustBridge.shared.command("sync_fail")
        }

        // Zero-length slice with nil pointer must not crash
        sub2ops_macos_on_event(kind: 4, ptr: nil, len: 0)
    }
}
}
