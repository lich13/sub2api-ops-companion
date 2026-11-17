import AppKit
import SwiftUI

// MARK: - AppKit NSMenu Anchor (Zero Bezel / Zero Disclosure Arrow Leakage)

public struct LinearMenuEntry: Identifiable, Sendable {
    public let id: String
    public let title: String
    public var checked: Bool
    public var destructive: Bool
    public var disabled: Bool
    public var isSeparator: Bool
    public let action: @MainActor @Sendable () -> Void

    public init(
        id: String,
        title: String,
        checked: Bool = false,
        destructive: Bool = false,
        disabled: Bool = false,
        isSeparator: Bool = false,
        action: @escaping @MainActor @Sendable () -> Void
    ) {
        self.id = id
        self.title = title
        self.checked = checked
        self.destructive = destructive
        self.disabled = disabled
        self.isSeparator = isSeparator
        self.action = action
    }

    public static func separator(_ id: String = UUID().uuidString) -> LinearMenuEntry {
        LinearMenuEntry(id: id, title: "", isSeparator: true, action: {})
    }
}

public enum LinearMenuTracking {
    @MainActor public private(set) static var activeCount: Int = 0
    @MainActor public static var isTracking: Bool { activeCount > 0 }
    @MainActor public static func begin() { activeCount += 1 }
    @MainActor public static func end() { activeCount = max(0, activeCount - 1) }
}

@MainActor
private final class MenuPopupCoordinator: NSObject {
    var entries: [LinearMenuEntry] = []

    func present(from anchorView: NSView) {
        guard !entries.isEmpty else { return }
        let menu = NSMenu()
        menu.autoenablesItems = false
        menu.appearance = anchorView.effectiveAppearance
        for (idx, entry) in entries.enumerated() {
            if entry.isSeparator {
                menu.addItem(.separator())
                continue
            }
            let item = NSMenuItem(title: entry.title, action: #selector(handleSelect(_:)), keyEquivalent: "")
            item.target = self
            item.tag = idx
            item.isEnabled = !entry.disabled
            item.state = entry.checked ? .on : .off
            menu.addItem(item)
        }
        let origin = NSPoint(x: 0, y: anchorView.isFlipped ? anchorView.bounds.height + 4 : -4)
        LinearMenuTracking.begin()
        defer { LinearMenuTracking.end() }
        menu.popUp(positioning: nil, at: origin, in: anchorView)
    }

    @objc private func handleSelect(_ sender: NSMenuItem) {
        let idx = sender.tag
        guard entries.indices.contains(idx) else { return }
        entries[idx].action()
    }
}

@MainActor
private final class FlippedAnchorNSView: NSView {
    override var isFlipped: Bool { true }
}

private struct MenuAnchorRepresentable: NSViewRepresentable {
    let triggerToken: Int
    let entries: [LinearMenuEntry]

    func makeCoordinator() -> MenuPopupCoordinator {
        MenuPopupCoordinator()
    }

    func makeNSView(context: Context) -> NSView {
        let view = FlippedAnchorNSView()
        context.coordinator.entries = entries
        return view
    }

    func updateNSView(_ nsView: NSView, context: Context) {
        context.coordinator.entries = entries
        if triggerToken > 0, context.coordinator.lastToken != triggerToken {
            context.coordinator.lastToken = triggerToken
            let coordinator = context.coordinator
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    coordinator.present(from: nsView)
                }
            }
        }
    }
}

private extension MenuPopupCoordinator {
    private static var tokenKey: UInt8 = 0
    var lastToken: Int {
        get { (objc_getAssociatedObject(self, &Self.tokenKey) as? Int) ?? 0 }
        set { objc_setAssociatedObject(self, &Self.tokenKey, newValue, .OBJC_ASSOCIATION_RETAIN_NONATOMIC) }
    }
}

// MARK: - LinearSelect (Crisp Bordered Dropdown Matching CSS <select>)

public struct LinearSelect: View {
    public let label: String
    public var accessibilityTitle: String
    public var leadingIcon: String?
    public var trailingIcon: String
    public var height: CGFloat
    public var width: CGFloat?
    public var minWidth: CGFloat?
    public var expandWidth: Bool
    public var compactFont: Bool
    public var monospaced: Bool
    public var mutedLabel: Bool
    public var disabled: Bool
    public let entries: [LinearMenuEntry]

    @State private var triggerToken: Int = 0
    @State private var hovered: Bool = false

    public init(
        label: String,
        accessibilityTitle: String? = nil,
        leadingIcon: String? = nil,
        trailingIcon: String = "chevron.down",
        height: CGFloat = DS.controlHeight,
        width: CGFloat? = nil,
        minWidth: CGFloat? = nil,
        expandWidth: Bool = false,
        compactFont: Bool = false,
        monospaced: Bool = false,
        mutedLabel: Bool = false,
        disabled: Bool = false,
        entries: [LinearMenuEntry]
    ) {
        self.label = label
        self.accessibilityTitle = accessibilityTitle ?? label
        self.leadingIcon = leadingIcon
        self.trailingIcon = trailingIcon
        self.height = height
        self.width = width
        self.minWidth = minWidth
        self.expandWidth = expandWidth
        self.compactFont = compactFont
        self.monospaced = monospaced
        self.mutedLabel = mutedLabel
        self.disabled = disabled
        self.entries = entries
    }

    public init(
        selection: Binding<String>,
        options: [(value: String, label: String)],
        accessibilityTitle: String? = nil,
        height: CGFloat = DS.controlHeight,
        width: CGFloat? = nil,
        minWidth: CGFloat? = nil,
        expandWidth: Bool = false,
        compactFont: Bool = false,
        monospaced: Bool = false,
        disabled: Bool = false
    ) {
        let current = selection.wrappedValue
        let currentLabel = options.first(where: { $0.value == current })?.label ?? current
        self.init(
            label: currentLabel,
            accessibilityTitle: accessibilityTitle,
            height: height,
            width: width,
            minWidth: minWidth,
            expandWidth: expandWidth,
            compactFont: compactFont,
            monospaced: monospaced,
            disabled: disabled,
            entries: options.map { item in
                let val = item.value
                let setter = selection.wrappedValue
                _ = setter
                return LinearMenuEntry(
                    id: val,
                    title: item.label,
                    checked: val == current
                ) {
                    selection.wrappedValue = val
                }
            }
        )
    }

    public var body: some View {
        Button {
            guard !disabled, !entries.isEmpty else { return }
            triggerToken &+= 1
        } label: {
            HStack(spacing: 6) {
                if let leadingIcon {
                    Image(systemName: leadingIcon)
                        .font(DS.iconFont(12, weight: .medium))
                        .foregroundStyle(DS.muted)
                }
                Text(verbatim: label)
                    .font(textFont)
                    .monospacedDigit()
                    .foregroundStyle(mutedLabel ? DS.muted : DS.ink)
                    .lineLimit(1)
                if expandWidth || width != nil || minWidth != nil {
                    Spacer(minLength: 4)
                }
                Image(systemName: trailingIcon)
                    .font(DS.iconFont(10, weight: .semibold))
                    .foregroundStyle(DS.muted)
                    .padding(.leading, 2)
            }
            .padding(.horizontal, 10)
            .frame(width: width)
            .frame(
                minWidth: minWidth,
                maxWidth: expandWidth ? .infinity : width,
                alignment: .leading
            )
            .frame(height: height)
            .background(hovered && !disabled ? DS.hover : DS.surface)
            .overlay(
                RoundedRectangle(cornerRadius: DS.cornerRadius)
                    .strokeBorder(hovered && !disabled ? DS.lineStrong : DS.line, lineWidth: 1)
            )
            .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
            .background(MenuAnchorRepresentable(triggerToken: triggerToken, entries: entries))
        }
        .buttonStyle(.linearPlain)
        .disabled(disabled || entries.isEmpty)
        .compositingGroup()
        .opacity(disabled ? 0.5 : 1.0)
        .onHover { hovered = $0 }
        .accessibilityLabel(accessibilityTitle)
    }

    private var textFont: Font {
        if monospaced {
            return compactFont ? DS.fontMono11 : DS.fontMono12
        }
        return compactFont ? DS.fontCompact : DS.fontBody
    }
}

// MARK: - LinearMenuIconTrigger (Ellipsis / Action Menu Trigger Without Native Arrow)

public struct LinearMenuIconTrigger: View {
    public let systemName: String
    public let label: String
    public var titleText: String?
    public var boxSize: CGFloat
    public var bordered: Bool
    public var disabled: Bool
    public let entries: [LinearMenuEntry]

    @State private var triggerToken: Int = 0
    @State private var hovered: Bool = false

    public init(
        systemName: String = "ellipsis",
        label: String,
        titleText: String? = nil,
        boxSize: CGFloat = 28,
        bordered: Bool = false,
        disabled: Bool = false,
        entries: [LinearMenuEntry]
    ) {
        self.systemName = systemName
        self.label = label
        self.titleText = titleText
        self.boxSize = boxSize
        self.bordered = bordered
        self.disabled = disabled
        self.entries = entries
    }

    public var body: some View {
        Button {
            guard !disabled, !entries.isEmpty else { return }
            triggerToken &+= 1
        } label: {
            HStack(spacing: 5) {
                Image(systemName: systemName)
                    .font(DS.iconFont(12, weight: .medium))
                if let titleText {
                    Text(titleText)
                        .font(DS.fontCompactMedium)
                }
            }
            .foregroundStyle(hovered && !disabled ? DS.ink : (titleText != nil ? DS.ink : DS.muted))
            .padding(.horizontal, titleText != nil ? 10 : 0)
            .frame(width: titleText == nil ? boxSize : nil, height: boxSize)
            .background(bordered ? (hovered && !disabled ? DS.hover : DS.surface) : (hovered && !disabled ? DS.hover : DS.clear))
            .overlay(
                RoundedRectangle(cornerRadius: bordered ? DS.cornerRadius : 6)
                    .strokeBorder(bordered ? (hovered && !disabled ? DS.lineStrong : DS.line) : DS.clear, lineWidth: bordered ? 1 : 0)
            )
            .clipShape(RoundedRectangle(cornerRadius: bordered ? DS.cornerRadius : 6))
            .background(MenuAnchorRepresentable(triggerToken: triggerToken, entries: entries))
        }
        .buttonStyle(.linearPlain)
        .disabled(disabled || entries.isEmpty)
        .compositingGroup()
        .opacity(disabled ? 0.45 : 1.0)
        .onHover { hovered = $0 }
        .help(label)
        .accessibilityLabel(label)
    }
}

// MARK: - SixDotGripIcon (Matches Lucide GripVertical 2x3 Dots)

public struct SixDotGripIcon: View {
    public var color: Color

    public init(color: Color = DS.muted) {
        self.color = color
    }

    public var body: some View {
        VStack(spacing: 2.5) {
            ForEach(0..<3, id: \.self) { _ in
                HStack(spacing: 2.5) {
                    Circle().fill(color).frame(width: 2.5, height: 2.5)
                    Circle().fill(color).frame(width: 2.5, height: 2.5)
                }
            }
        }
        .frame(width: 14, height: 18)
    }
}

// MARK: - LinearCheckbox

public struct LinearCheckbox: View {
    public let isChecked: Bool
    public var label: String?
    public var accessibilityTitle: String
    public var disabled: Bool
    public let onToggle: () -> Void

    @State private var hovered: Bool = false

    public init(
        isChecked: Bool,
        label: String? = nil,
        accessibilityTitle: String = "",
        disabled: Bool = false,
        onToggle: @escaping () -> Void
    ) {
        self.isChecked = isChecked
        self.label = label
        self.accessibilityTitle = accessibilityTitle.isEmpty ? (label ?? "选择") : accessibilityTitle
        self.disabled = disabled
        self.onToggle = onToggle
    }

    public var body: some View {
        Button(action: onToggle) {
            HStack(spacing: 6) {
                ZStack {
                    RoundedRectangle(cornerRadius: 4)
                        .fill(isChecked ? DS.accent : DS.surface)
                        .frame(width: 15, height: 15)
                    RoundedRectangle(cornerRadius: 4)
                        .strokeBorder(isChecked ? DS.accent : (hovered && !disabled ? DS.accent : DS.lineStrong), lineWidth: 1.2)
                        .frame(width: 15, height: 15)
                    if isChecked {
                        Image(systemName: "checkmark")
                            .font(DS.iconFont(9, weight: .bold))
                            .foregroundStyle(DS.onPrimary)
                    }
                }
                if let label {
                    Text(label)
                        .font(DS.fontCompact)
                        .foregroundStyle(DS.ink)
                        .lineLimit(1)
                }
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.linearPlain)
        .disabled(disabled)
        .compositingGroup()
        .opacity(disabled ? 0.45 : 1.0)
        .onHover { hovered = $0 }
        .accessibilityLabel(accessibilityTitle)
        .accessibilityValue(isChecked ? "已选中" : "未选中")
    }
}

// MARK: - LinearSearchField & LinearInputChrome

public struct LinearSearchField: View {
    @Binding public var text: String
    public let prompt: String
    public var height: CGFloat
    public var width: CGFloat?
    public var compactFont: Bool
    public var onSubmit: (() -> Void)?

    @FocusState private var focused: Bool
    @State private var hovered: Bool = false

    public init(
        text: Binding<String>,
        prompt: String,
        height: CGFloat = DS.controlHeight,
        width: CGFloat? = nil,
        compactFont: Bool = false,
        onSubmit: (() -> Void)? = nil
    ) {
        self._text = text
        self.prompt = prompt
        self.height = height
        self.width = width
        self.compactFont = compactFont
        self.onSubmit = onSubmit
    }

    public var body: some View {
        HStack(spacing: 7) {
            Image(systemName: "magnifyingglass")
                .font(DS.iconFont(12, weight: .medium))
                .foregroundStyle(DS.muted)
            TextField(text: $text, prompt: Text(verbatim: prompt).foregroundStyle(DS.placeholder)) { EmptyView() }
                .textFieldStyle(.plain)
                .font(compactFont ? DS.fontCompact : DS.fontBody)
                .foregroundStyle(DS.ink)
                .focused($focused)
                .onSubmit { onSubmit?() }
            if !text.isEmpty {
                Button {
                    text = ""
                    onSubmit?()
                } label: {
                    Image(systemName: "xmark.circle.fill")
                        .font(DS.iconFont(12))
                        .foregroundStyle(DS.muted)
                }
                .buttonStyle(.linearPlain)
                .accessibilityLabel("清空搜索")
            }
        }
        .padding(.horizontal, 10)
        .frame(width: width)
        .frame(maxWidth: width == nil ? .infinity : nil)
        .frame(height: height)
        .background(DS.surface)
        .overlay(
            RoundedRectangle(cornerRadius: DS.cornerRadius)
                .strokeBorder(focused ? DS.accent : (hovered ? DS.lineStrong : DS.line), lineWidth: 1)
        )
        .overlay(
            RoundedRectangle(cornerRadius: DS.cornerRadius + 1)
                .strokeBorder(focused ? DS.focusRing : DS.clear, lineWidth: 2)
                .padding(-1)
        )
        .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
        .onHover { hovered = $0 }
    }
}

// MARK: - LinearUnderlineTabs (Matches .feature-tabs / .event-tabs in style.css)

public struct LinearTabItem<ID: Hashable & Sendable>: Identifiable, Sendable {
    public let id: ID
    public let label: String
    public var count: Int?
    public var dot: Bool

    public init(id: ID, label: String, count: Int? = nil, dot: Bool = false) {
        self.id = id
        self.label = label
        self.count = count
        self.dot = dot
    }
}

public struct LinearUnderlineTabs<ID: Hashable & Sendable>: View {
    public let items: [LinearTabItem<ID>]
    public let selection: ID
    public let onSelect: (ID) -> Void

    public init(items: [LinearTabItem<ID>], selection: ID, onSelect: @escaping (ID) -> Void) {
        self.items = items
        self.selection = selection
        self.onSelect = onSelect
    }

    public var body: some View {
        HStack(spacing: 24) {
            ForEach(items) { item in
                UnderlineTabButton(
                    item: item,
                    selected: item.id == selection,
                    onSelect: { onSelect(item.id) }
                )
            }
            Spacer(minLength: 0)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .overlay(alignment: .bottom) {
            Rectangle().fill(DS.line).frame(height: 1)
        }
    }

    private struct UnderlineTabButton: View {
        let item: LinearTabItem<ID>
        let selected: Bool
        let onSelect: () -> Void
        @State private var hovered: Bool = false

        var body: some View {
            Button(action: onSelect) {
                VStack(spacing: 0) {
                    HStack(spacing: 5) {
                        Text(item.label)
                            .font(selected ? DS.fontBodySemibold : DS.fontBodyMedium)
                            .foregroundStyle(selected ? DS.accent : (hovered ? DS.ink : DS.muted))
                        if let count = item.count {
                            Text("\(count)")
                                .font(DS.fontPriorityMeta)
                                .monospacedDigit()
                                .foregroundStyle(selected ? DS.accent : DS.muted)
                        }
                        if item.dot {
                            Circle()
                                .fill(DS.accent)
                                .frame(width: 5, height: 5)
                        }
                    }
                    .padding(.top, 7)
                    .padding(.bottom, 9)
                    .padding(.horizontal, 2)

                    Rectangle()
                        .fill(selected ? DS.accent : DS.clear)
                        .frame(height: 2)
                }
                .fixedSize(horizontal: true, vertical: false)
                .contentShape(Rectangle())
            }
            .buttonStyle(.linearPlain)
            .onHover { hovered = $0 }
        }
    }
}

// MARK: - LinearSegmentedTabs (Matches Platform Segmented Track in Groups)

public struct LinearSegmentedTabs<ID: Hashable & Sendable>: View {
    public let items: [LinearTabItem<ID>]
    public let selection: ID
    public var disabled: Bool
    public let onSelect: (ID) -> Void

    public init(
        items: [LinearTabItem<ID>],
        selection: ID,
        disabled: Bool = false,
        onSelect: @escaping (ID) -> Void
    ) {
        self.items = items
        self.selection = selection
        self.disabled = disabled
        self.onSelect = onSelect
    }

    public var body: some View {
        HStack(spacing: 2) {
            ForEach(items) { item in
                let active = item.id == selection
                Button {
                    onSelect(item.id)
                } label: {
                    HStack(spacing: 5) {
                        Text(item.label)
                            .font(active ? DS.fontCompactSemibold : DS.fontCompactMedium)
                            .foregroundStyle(active ? DS.ink : DS.muted)
                        if let count = item.count {
                            Text("\(count)")
                                .font(DS.fontPriorityMeta)
                                .monospacedDigit()
                                .foregroundStyle(DS.muted)
                        }
                        if item.dot {
                            Circle()
                                .fill(DS.accent)
                                .frame(width: 5, height: 5)
                        }
                    }
                    .padding(.horizontal, 12)
                    .frame(height: 28)
                    .background(active ? DS.surface : DS.clear)
                    .overlay(
                        RoundedRectangle(cornerRadius: 6)
                            .strokeBorder(active ? DS.line : DS.clear, lineWidth: 1)
                    )
                    .clipShape(RoundedRectangle(cornerRadius: 6))
                }
                .buttonStyle(.linearPlain)
                .disabled(disabled)
            }
        }
        .padding(3)
        .background(DS.surfaceSubtle)
        .overlay(
            RoundedRectangle(cornerRadius: 9)
                .strokeBorder(DS.line, lineWidth: 1)
        )
        .clipShape(RoundedRectangle(cornerRadius: 9))
    }
}

// MARK: - LinearSettingRow (Consistent 50pt Form Row With Bottom Divider)

public struct LinearSettingRow<Control: View>: View {
    public let label: String
    public var muted: Bool
    public var showDivider: Bool
    private let control: Control

    public init(
        _ label: String,
        muted: Bool = false,
        showDivider: Bool = true,
        @ViewBuilder control: () -> Control
    ) {
        self.label = label
        self.muted = muted
        self.showDivider = showDivider
        self.control = control()
    }

    public var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 16) {
                Text(label)
                    .font(DS.fontBody)
                    .foregroundStyle(muted ? DS.muted : DS.ink)
                Spacer(minLength: 12)
                control
            }
            .padding(.vertical, 10)
            .frame(minHeight: 48)

            if showDivider {
                Rectangle().fill(DS.line).frame(height: 1)
            }
        }
    }
}

// MARK: - LinearModalCard (Standard Dialog Surface With Header & Footer Dividers)

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

    @State private var text: String = ""

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
