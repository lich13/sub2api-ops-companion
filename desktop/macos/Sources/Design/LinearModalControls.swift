import AppKit
import SwiftUI

public struct LinearModalCard<BodyContent: View, FooterContent: View>: View {
    public let title: String
    public var subtitle: String?
    public var width: CGFloat
    public var closeDisabled: Bool
    public let onClose: () -> Void
    private let bodyContent: BodyContent
    private let footerContent: FooterContent

    public init(
        title: String,
        subtitle: String? = nil,
        width: CGFloat = 540,
        closeDisabled: Bool = false,
        onClose: @escaping () -> Void,
        @ViewBuilder content: () -> BodyContent,
        @ViewBuilder footer: () -> FooterContent
    ) {
        self.title = title
        self.subtitle = subtitle
        self.width = width
        self.closeDisabled = closeDisabled
        self.onClose = onClose
        self.bodyContent = content()
        self.footerContent = footer()
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: .top, spacing: 12) {
                VStack(alignment: .leading, spacing: 4) {
                    Text(title)
                        .font(DS.fontDialogTitle)
                        .foregroundStyle(DS.ink)
                    if let subtitle, !subtitle.isEmpty {
                        Text(subtitle)
                            .font(DS.fontCompactMedium)
                            .monospacedDigit()
                            .foregroundStyle(DS.muted)
                            .lineLimit(1)
                    }
                }
                Spacer(minLength: 8)
                LinearIconButton(
                    systemName: "xmark",
                    label: "关闭\(title)",
                    size: 13,
                    boxSize: 28,
                    disabled: closeDisabled,
                    action: onClose
                )
            }
            .padding(.horizontal, 20)
            .padding(.vertical, 16)

            Rectangle().fill(DS.line).frame(height: 1)

            VStack(alignment: .leading, spacing: 14) {
                bodyContent
            }
            .padding(.horizontal, 20)
            .padding(.vertical, 18)

            Rectangle().fill(DS.line).frame(height: 1)

            HStack(spacing: 8) {
                footerContent
            }
            .padding(.horizontal, 20)
            .padding(.vertical, 14)
        }
        .frame(width: width)
        .background(DS.surface)
        .overlay(
            RoundedRectangle(cornerRadius: DS.modalRadius)
                .strokeBorder(DS.line, lineWidth: 1)
        )
        .clipShape(RoundedRectangle(cornerRadius: DS.modalRadius))
        .shadow(color: DS.modalShadowColor, radius: 24, x: 0, y: 12)
    }
}

// MARK: - LinearBoundedScrollView (Shrinks-to-fit short modal content; scrolls smoothly above maxHeight)

private struct BoundedVerticalLayout: Layout {
    let maxHeight: CGFloat

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        guard let first = subviews.first else { return .zero }
        let cappedHeight = min(proposal.height ?? maxHeight, maxHeight)
        let subSize = first.sizeThatFits(ProposedViewSize(width: proposal.width, height: cappedHeight))
        return CGSize(width: subSize.width, height: min(subSize.height, cappedHeight))
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        guard let first = subviews.first else { return }
        first.place(
            at: bounds.origin,
            anchor: .topLeading,
            proposal: ProposedViewSize(width: bounds.width, height: bounds.height)
        )
    }
}

@MainActor
private final class OverlayScrollerProbeNSView: NSView {
    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        applyOverlayScroller()
    }

    override func layout() {
        super.layout()
        applyOverlayScroller()
    }

    private func applyOverlayScroller() {
        var cursor: NSView? = superview
        while let current = cursor {
            if let scrollView = current as? NSScrollView {
                if scrollView.scrollerStyle != .overlay {
                    scrollView.scrollerStyle = .overlay
                }
                break
            }
            cursor = current.superview
        }
    }
}

private struct OverlayScrollerInjector: NSViewRepresentable {
    func makeNSView(context: Context) -> NSView {
        OverlayScrollerProbeNSView(frame: .zero)
    }

    func updateNSView(_ nsView: NSView, context: Context) {}
}

public struct LinearBoundedScrollView<Content: View>: View {
    public let maxHeight: CGFloat
    private let content: Content

    public init(maxHeight: CGFloat, @ViewBuilder content: () -> Content) {
        self.maxHeight = maxHeight
        self.content = content()
    }

    public var body: some View {
        BoundedVerticalLayout(maxHeight: maxHeight) {
            ViewThatFits(in: .vertical) {
                content
                    .fixedSize(horizontal: false, vertical: true)
                ScrollView {
                    content
                        .background(OverlayScrollerInjector())
                }
            }
        }
    }
}

// MARK: - LinearRetainedTabSlot (Keeps visited tabs mounted to preserve form drafts & pagination without inflating inactive layout)

private struct RetainedTabVisibilityLayout: Layout {
    let isActive: Bool

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        guard isActive, let first = subviews.first else { return .zero }
        return first.sizeThatFits(proposal)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        guard let first = subviews.first else { return }
        if isActive {
            first.place(
                at: bounds.origin,
                anchor: .topLeading,
                proposal: ProposedViewSize(width: bounds.width, height: bounds.height)
            )
        } else {
            first.place(
                at: bounds.origin,
                anchor: .topLeading,
                proposal: ProposedViewSize(width: max(bounds.width, proposal.width ?? 0), height: 0)
            )
        }
    }
}

public struct LinearRetainedTabSlot<Content: View>: View {
    public let isActive: Bool
    private let content: Content

    public init(isActive: Bool, @ViewBuilder content: () -> Content) {
        self.isActive = isActive
        self.content = content()
    }

    public var body: some View {
        RetainedTabVisibilityLayout(isActive: isActive) {
            content
        }
        .clipped()
        .opacity(isActive ? 1 : 0)
        .allowsHitTesting(isActive)
        .accessibilityHidden(!isActive)
    }
}

// MARK: - TrimmedEntryField (@State-backed text field avoiding Swift 6.1 Binding(get:set:) IRGen crash)

public struct TrimmedEntryField: View {
    public let value: String
    public let prompt: String
    public var font: Font
    public var horizontalPadding: CGFloat
    public var height: CGFloat
    public var cornerRadius: CGFloat
    public var borderColor: Color
    public var trimWhitespace: Bool
    public var disabled: Bool
    public let onCommit: (String) -> Void

    @State var text: String = ""

    public init(
        value: String,
        prompt: String,
        font: Font = DS.fontCompact,
        horizontalPadding: CGFloat = 10,
        height: CGFloat = DS.controlHeightCompact,
        cornerRadius: CGFloat = 6,
        borderColor: Color = DS.line,
        trimWhitespace: Bool = true,
        disabled: Bool = false,
        onCommit: @escaping (String) -> Void
    ) {
        self.value = value
        self.prompt = prompt
        self.font = font
        self.horizontalPadding = horizontalPadding
        self.height = height
        self.cornerRadius = cornerRadius
        self.borderColor = borderColor
        self.trimWhitespace = trimWhitespace
        self.disabled = disabled
        self.onCommit = onCommit
    }

    private func normalized(_ raw: String) -> String {
        trimWhitespace ? raw.trimmingCharacters(in: .whitespacesAndNewlines) : raw
    }

    public var body: some View {
        TextField(text: $text, prompt: Text(verbatim: prompt)) { EmptyView() }
            .textFieldStyle(.plain)
            .font(font)
            .monospacedDigit()
            .foregroundStyle(DS.ink)
            .disabled(disabled)
            .padding(.horizontal, horizontalPadding)
            .frame(height: height)
            .background(DS.surface)
            .overlay(RoundedRectangle(cornerRadius: cornerRadius).strokeBorder(borderColor, lineWidth: 1))
            .clipShape(RoundedRectangle(cornerRadius: cornerRadius))
            .onAppear { text = value }
            .onChange(of: value) { _, next in
                if normalized(text) != next {
                    text = next
                }
            }
            .onChange(of: text) { _, raw in
                let next = normalized(raw)
                if next != value {
                    onCommit(next)
                }
            }
    }
}

// MARK: - Synchronous Layout Width Observer & Window Drag Handle

@MainActor
private final class WidthReportingNSView: NSView {
    var onWidth: (@MainActor (CGFloat) -> Void)?
    private var lastReported: CGFloat = -1

    override func layout() {
        super.layout()
        let w = bounds.width
        if w > 0, abs(w - lastReported) >= 0.5 {
            lastReported = w
            // State must not change inside an AppKit layout pass; report on the next main-queue turn.
            DispatchQueue.main.async { [weak self] in
                MainActor.assumeIsolated { self?.onWidth?(w) }
            }
        }
    }
}

public struct LinearWidthObserver: NSViewRepresentable {
    public let onWidth: @MainActor (CGFloat) -> Void

    public init(onWidth: @escaping @MainActor (CGFloat) -> Void) {
        self.onWidth = onWidth
    }

    public func makeNSView(context: Context) -> NSView {
        let view = WidthReportingNSView()
        view.onWidth = onWidth
        return view
    }

    public func updateNSView(_ nsView: NSView, context: Context) {
        guard let view = nsView as? WidthReportingNSView else { return }
        view.onWidth = onWidth
    }
}

@MainActor
private final class WindowDragNSView: NSView {
    override var mouseDownCanMoveWindow: Bool { true }

    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    override func mouseDown(with event: NSEvent) {
        if event.clickCount == 2 {
            window?.performZoom(nil)
        } else {
            window?.performDrag(with: event)
        }
    }
}

public struct LinearWindowDragHandle: NSViewRepresentable {
    public init() {}

    public func makeNSView(context: Context) -> NSView {
        WindowDragNSView()
    }

    public func updateNSView(_ nsView: NSView, context: Context) {}
}
