import AppKit
import SwiftUI

// MARK: - AppKit NSMenu Anchor (Zero Bezel / Zero Disclosure Arrow Leakage)

public struct LinearMenuEntry: Identifiable, Sendable {
    public let id: String
    public let title: String
    public var icon: String?
    public var checked: Bool
    public var destructive: Bool
    public var disabled: Bool
    public var isSeparator: Bool
    public let action: @MainActor @Sendable () -> Void

    public init(
        id: String,
        title: String,
        icon: String? = nil,
        checked: Bool = false,
        destructive: Bool = false,
        disabled: Bool = false,
        isSeparator: Bool = false,
        action: @escaping @MainActor @Sendable () -> Void
    ) {
        self.id = id
        self.title = title
        self.icon = icon
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
            let destructiveColor = entry.disabled
                ? NSColor.systemRed.withAlphaComponent(0.45)
                : NSColor.systemRed
            if let iconName = entry.icon, let baseImage = NSImage(systemSymbolName: iconName, accessibilityDescription: nil) {
                var config = NSImage.SymbolConfiguration(pointSize: 12, weight: .medium)
                if entry.destructive {
                    config = config.applying(NSImage.SymbolConfiguration(paletteColors: [destructiveColor]))
                }
                let configured = baseImage.withSymbolConfiguration(config) ?? baseImage
                configured.isTemplate = !entry.destructive
                item.image = configured
            }
            if entry.destructive {
                item.attributedTitle = NSAttributedString(
                    string: entry.title,
                    attributes: [
                        .font: NSFont.menuFont(ofSize: 0),
                        .foregroundColor: destructiveColor,
                    ]
                )
            }
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
    @State var hovered: Bool = false

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
    @State var hovered: Bool = false

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

    @State var hovered: Bool = false

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
    @State var hovered: Bool = false

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
            TextField(text: $text, prompt: Text(verbatim: prompt).foregroundStyle(DS.placeholder)) {
                Text(verbatim: prompt)
            }
                .labelsHidden()
                .textFieldStyle(.plain)
                .font(compactFont ? DS.fontCompact : DS.fontBody)
                .foregroundStyle(DS.ink)
                .focused($focused)
                .accessibilityLabel(prompt)
                .onSubmit { onSubmit?() }
            if !text.isEmpty {
                Button {
                    text = ""
                    onSubmit?()
                } label: {
                    Image(systemName: "xmark.circle.fill")
                        .font(DS.iconFont(12))
                        .foregroundStyle(DS.muted)
                        .frame(width: 24, height: 24)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.linearPlain)
                .accessibilityLabel("清空搜索")
            }
        }
        .padding(.leading, 10)
        .padding(.trailing, text.isEmpty ? 10 : 4)
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
