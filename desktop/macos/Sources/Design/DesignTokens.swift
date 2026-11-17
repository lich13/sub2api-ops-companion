import AppKit
import SwiftUI

public enum DS {
    // MARK: - Dynamic Light / Dark NSColors

    private static func dynamicNSColor(lightHex: UInt32, darkHex: UInt32, alpha: CGFloat = 1.0) -> NSColor {
        NSColor(name: nil) { appearance in
            let match = appearance.bestMatch(from: [.darkAqua, .aqua])
            let hex = (match == .darkAqua) ? darkHex : lightHex
            let r = CGFloat((hex >> 16) & 0xFF) / 255.0
            let g = CGFloat((hex >> 8) & 0xFF) / 255.0
            let b = CGFloat(hex & 0xFF) / 255.0
            return NSColor(srgbRed: r, green: g, blue: b, alpha: alpha)
        }
    }

    public static let nsCanvas = dynamicNSColor(lightHex: 0xF5F6F9, darkHex: 0x17191F)
    public static let nsSurface = dynamicNSColor(lightHex: 0xFFFFFF, darkHex: 0x20232B)
    public static let nsSurfaceSubtle = dynamicNSColor(lightHex: 0xF9FAFC, darkHex: 0x1C1F27)
    public static let nsTopbar = dynamicNSColor(lightHex: 0xFBFCFD, darkHex: 0x1E2129)
    public static let nsSidebar = dynamicNSColor(lightHex: 0xF0F1F5, darkHex: 0x1B1E25)
    public static let nsInk = dynamicNSColor(lightHex: 0x202432, darkHex: 0xEBEDF3)
    public static let nsMuted = dynamicNSColor(lightHex: 0x707789, darkHex: 0x969EAF)
    public static let nsLine = dynamicNSColor(lightHex: 0xE5E7ED, darkHex: 0x333844)
    public static let nsLineStrong = dynamicNSColor(lightHex: 0xD2D6E2, darkHex: 0x454C5C)
    public static let nsAccent = dynamicNSColor(lightHex: 0x6262CE, darkHex: 0x9797EF)

    // MARK: - SwiftUI Semantic Colors (DESIGN.md Linear Palette)

    public static let canvas = Color(nsColor: nsCanvas)
    public static let surface = Color(nsColor: nsSurface)
    public static let surfaceSubtle = Color(nsColor: nsSurfaceSubtle)
    public static let topbar = Color(nsColor: nsTopbar)
    public static let sidebar = Color(nsColor: nsSidebar)
    public static let ink = Color(nsColor: nsInk)
    public static let muted = Color(nsColor: nsMuted)
    public static let line = Color(nsColor: nsLine)
    public static let lineStrong = Color(nsColor: nsLineStrong)
    public static let accent = Color(nsColor: nsAccent)
    public static let accentBg = Color(nsColor: dynamicNSColor(lightHex: 0xE9E9F9, darkHex: 0x303047))
    public static let primaryBg = Color(nsColor: dynamicNSColor(lightHex: 0x6262CE, darkHex: 0x6C6CDE))
    public static let primaryHover = Color(nsColor: dynamicNSColor(lightHex: 0x5656C2, darkHex: 0x7C7CE8))
    public static let onPrimary = Color(nsColor: dynamicNSColor(lightHex: 0xFFFFFF, darkHex: 0xFFFFFF))
    public static let hover = Color(nsColor: dynamicNSColor(lightHex: 0xF5F5FA, darkHex: 0x282C36))

    public static let good = Color(nsColor: dynamicNSColor(lightHex: 0x278062, darkHex: 0x68BC9F))
    public static let goodBg = Color(nsColor: dynamicNSColor(lightHex: 0xEAF6F2, darkHex: 0x1F332D))
    public static let bad = Color(nsColor: dynamicNSColor(lightHex: 0xC74F5A, darkHex: 0xF18893))
    public static let badBg = Color(nsColor: dynamicNSColor(lightHex: 0xFFF1F1, darkHex: 0x3B272E))
    public static let warn = Color(nsColor: dynamicNSColor(lightHex: 0xAA761F, darkHex: 0xDCAD60))
    public static let warnBg = Color(nsColor: dynamicNSColor(lightHex: 0xFCF6EB, darkHex: 0x342C21))
    public static let offlineBg = Color(nsColor: dynamicNSColor(lightHex: 0xFAF5EC, darkHex: 0x2A2621))

    public static let switchOff = Color(nsColor: dynamicNSColor(lightHex: 0xB8BECC, darkHex: 0x555D6E))
    public static let switchThumb = Color(nsColor: dynamicNSColor(lightHex: 0xFFFFFF, darkHex: 0xFFFFFF))
    public static let backdrop = Color(nsColor: dynamicNSColor(lightHex: 0x101728, darkHex: 0x090B10, alpha: 0.36))
    public static let shadowColor = Color(nsColor: dynamicNSColor(lightHex: 0x0F172A, darkHex: 0x000000, alpha: 0.08))
    public static let clear = Color.clear

    // Usage & Group Domain Colors
    public static let windowIndigo = Color(nsColor: dynamicNSColor(lightHex: 0x6262CE, darkHex: 0x9797EF))
    public static let windowIndigoBg = Color(nsColor: dynamicNSColor(lightHex: 0xECECFA, darkHex: 0x2B2C44))
    public static let windowEmerald = Color(nsColor: dynamicNSColor(lightHex: 0x278062, darkHex: 0x68BC9F))
    public static let windowEmeraldBg = Color(nsColor: dynamicNSColor(lightHex: 0xE7F5EF, darkHex: 0x1E342D))
    public static let windowPurple = Color(nsColor: dynamicNSColor(lightHex: 0x8256D0, darkHex: 0xB392F0))
    public static let windowPurpleBg = Color(nsColor: dynamicNSColor(lightHex: 0xF1ECFA, darkHex: 0x312846))
    public static let tokenCyan = Color(nsColor: dynamicNSColor(lightHex: 0x1B7C83, darkHex: 0x56D4DD))
    public static let costOrange = Color(nsColor: dynamicNSColor(lightHex: 0xC46B16, darkHex: 0xF09A48))
    public static let latencySlow = Color(nsColor: dynamicNSColor(lightHex: 0xC46B16, darkHex: 0xF09A48))
    public static let groupTeal = Color(nsColor: dynamicNSColor(lightHex: 0x387F8B, darkHex: 0x78BAC4))
    public static let groupTealBg = Color(nsColor: dynamicNSColor(lightHex: 0xE8F5F6, darkHex: 0x1D3236))
    public static let groupLeftFill = Color(nsColor: dynamicNSColor(lightHex: 0x6262CE, darkHex: 0x9797EF, alpha: 0.04))
    public static let groupRightFill = Color(nsColor: dynamicNSColor(lightHex: 0x387F8B, darkHex: 0x78BAC4, alpha: 0.04))
    public static let groupLeftBorder = Color(nsColor: dynamicNSColor(lightHex: 0x9E9EE0, darkHex: 0x5C5DA0))
    public static let groupRightBorder = Color(nsColor: dynamicNSColor(lightHex: 0x7BAEB7, darkHex: 0x4A7882))

    // MARK: - Typography Scale (12 / 13 / 14 / 18 / 26pt + Explicit Micro Exceptions)

    /// 26pt Page Title
    public static let fontPageTitle = Font.system(size: 26, weight: .semibold)
    /// 22pt Numeric Summary Counter
    public static let fontStatValue = Font.system(size: 22, weight: .medium)
    /// 18pt Brand / Dialog Title
    public static let fontBrand = Font.system(size: 18, weight: .semibold)
    public static let fontDialogTitle = Font.system(size: 18, weight: .semibold)
    /// 14pt Card / Section Heading
    public static let fontSectionTitle = Font.system(size: 14, weight: .semibold)
    public static let fontSectionMedium = Font.system(size: 14, weight: .medium)
    /// 13pt Primary Body & Form Controls
    public static let fontBody = Font.system(size: 13, weight: .regular)
    public static let fontBodyMedium = Font.system(size: 13, weight: .medium)
    public static let fontBodySemibold = Font.system(size: 13, weight: .semibold)
    /// 12pt Table Body, Secondary Controls & Buttons
    public static let fontCompact = Font.system(size: 12, weight: .regular)
    public static let fontCompactMedium = Font.system(size: 12, weight: .medium)
    public static let fontCompactSemibold = Font.system(size: 12, weight: .semibold)

    /// Explicit DESIGN.md Micro-Size Exception Tokens:
    /// 11pt: Priority ID/type, table headers, status badges
    public static let fontPriorityMeta = Font.system(size: 11, weight: .regular)
    public static let fontTableHeader = Font.system(size: 11, weight: .medium)
    public static let fontBadge = Font.system(size: 11, weight: .semibold)
    /// 10pt: Usage window reset time, compact status code pills
    public static let fontResetTime = Font.system(size: 10, weight: .regular)
    public static let fontResetTimeSemibold = Font.system(size: 10, weight: .semibold)
    /// 9pt: Usage window top stat line
    public static let fontUsageStat = Font.system(size: 9, weight: .regular)
    public static let fontUsageStatMedium = Font.system(size: 9, weight: .medium)

    /// Monospaced Fonts (Strictly for code, IDs, paths & raw logs)
    public static let fontMono11 = Font.system(size: 11, weight: .regular, design: .monospaced)
    public static let fontMono12 = Font.system(size: 12, weight: .regular, design: .monospaced)
    public static let fontMono13 = Font.system(size: 13, weight: .regular, design: .monospaced)

    /// SF Symbol Icon Font Helper
    public static func iconFont(_ size: CGFloat, weight: Font.Weight = .regular) -> Font {
        Font.system(size: size, weight: weight)
    }

    // MARK: - Layout Metrics

    public static let controlHeightDense: CGFloat = 28
    public static let controlHeightCompact: CGFloat = 30
    public static let controlHeight: CGFloat = 34
    public static let controlHeightField: CGFloat = 36
    public static let sidebarWidth: CGFloat = 176
    public static let sidebarCompactWidth: CGFloat = 144
    public static let topbarHeight: CGFloat = 56
    public static let cornerRadius: CGFloat = 7
    public static let cardRadius: CGFloat = 10
    public static let quickPanelWidth: CGFloat = 420
    public static let quickPanelMinHeight: CGFloat = 128
    public static let quickPanelMaxHeight: CGFloat = 520
}

// MARK: - Shared Linear Controls

public struct LinearButtonStyle: ButtonStyle {
    public enum Variant: Sendable {
        case secondary
        case primary
        case danger
        case ghost
        case selectedGhost
    }

    public var variant: Variant
    public var compact: Bool

    public init(variant: Variant = .secondary, compact: Bool = false) {
        self.variant = variant
        self.compact = compact
    }

    public func makeBody(configuration: Configuration) -> some View {
        LinearButtonBody(configuration: configuration, variant: variant, compact: compact)
    }

    private struct LinearButtonBody: View {
        @Environment(\.isEnabled) private var isEnabled
        let configuration: Configuration
        let variant: Variant
        let compact: Bool

        var body: some View {
            configuration.label
                .font(compact ? DS.fontCompactMedium : DS.fontBodyMedium)
                .foregroundStyle(foregroundColor)
                .padding(.horizontal, horizontalPadding)
                .padding(.vertical, compact ? 4 : 6)
                .frame(minHeight: compact ? DS.controlHeightCompact : DS.controlHeight)
                .background(backgroundColor(pressed: configuration.isPressed))
                .overlay(
                    RoundedRectangle(cornerRadius: DS.cornerRadius)
                        .strokeBorder(borderColor, lineWidth: borderWidth)
                )
                .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
                .opacity(isEnabled ? 1.0 : 0.45)
        }

        private var horizontalPadding: CGFloat {
            switch variant {
            case .ghost, .selectedGhost: return compact ? 6 : 8
            default: return compact ? 10 : 12
            }
        }

        private var borderWidth: CGFloat {
            switch variant {
            case .ghost, .selectedGhost: return 0
            default: return 1
            }
        }

        private var foregroundColor: Color {
            switch variant {
            case .primary: return DS.onPrimary
            case .danger: return DS.bad
            case .ghost: return DS.muted
            case .selectedGhost: return DS.accent
            case .secondary: return DS.ink
            }
        }

        private func backgroundColor(pressed: Bool) -> Color {
            switch variant {
            case .primary:
                return pressed ? DS.primaryHover : DS.primaryBg
            case .danger:
                return pressed ? DS.badBg : DS.surface
            case .ghost:
                return pressed ? DS.hover : DS.clear
            case .selectedGhost:
                return DS.accentBg
            case .secondary:
                return pressed ? DS.hover : DS.surface
            }
        }

        private var borderColor: Color {
            switch variant {
            case .primary: return DS.primaryBg
            case .danger: return DS.line
            case .ghost, .selectedGhost: return DS.clear
            case .secondary: return DS.line
            }
        }
    }
}

public struct LinearIconButton: View {
    public let systemName: String
    public let label: String
    public var size: CGFloat
    public var selected: Bool
    public var disabled: Bool
    public let action: () -> Void

    public init(
        systemName: String,
        label: String,
        size: CGFloat = 14,
        selected: Bool = false,
        disabled: Bool = false,
        action: @escaping () -> Void
    ) {
        self.systemName = systemName
        self.label = label
        self.size = size
        self.selected = selected
        self.disabled = disabled
        self.action = action
    }

    public var body: some View {
        Button(action: action) {
            Image(systemName: systemName)
                .font(DS.iconFont(size, weight: .medium))
                .foregroundStyle(selected ? DS.accent : DS.muted)
                .frame(width: 28, height: 28)
                .background(selected ? DS.accentBg : DS.clear)
                .clipShape(RoundedRectangle(cornerRadius: 6))
        }
        .buttonStyle(.plain)
        .disabled(disabled)
        .opacity(disabled ? 0.45 : 1.0)
        .help(label)
        .accessibilityLabel(label)
    }
}

public struct LinearSwitch: View {
    public let isOn: Bool
    public var disabled: Bool
    public let label: String
    public let onToggle: () -> Void

    public init(isOn: Bool, disabled: Bool = false, label: String, onToggle: @escaping () -> Void) {
        self.isOn = isOn
        self.disabled = disabled
        self.label = label
        self.onToggle = onToggle
    }

    public var body: some View {
        Button(action: onToggle) {
            ZStack(alignment: isOn ? .trailing : .leading) {
                Capsule()
                    .fill(isOn ? DS.accent : DS.switchOff)
                    .frame(width: 32, height: 18)
                Circle()
                    .fill(DS.switchThumb)
                    .frame(width: 14, height: 14)
                    .padding(.horizontal, 2)
            }
        }
        .buttonStyle(.plain)
        .disabled(disabled)
        .opacity(disabled ? 0.45 : 1.0)
        .help(label)
        .accessibilityLabel(label)
        .accessibilityValue(isOn ? "开启" : "关闭")
    }
}

public struct StatusDot: View {
    public enum Tone: Sendable {
        case good
        case warning
        case bad
        case muted
    }

    public let tone: Tone
    public var size: CGFloat

    public init(_ tone: Tone, size: CGFloat = 6) {
        self.tone = tone
        self.size = size
    }

    public var body: some View {
        Circle()
            .fill(color)
            .frame(width: size, height: size)
    }

    private var color: Color {
        switch tone {
        case .good: return DS.good
        case .warning: return DS.warn
        case .bad: return DS.bad
        case .muted: return DS.muted
        }
    }
}

public struct CardSection<Content: View>: View {
    private let content: Content

    public init(@ViewBuilder content: () -> Content) {
        self.content = content()
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            content
        }
        .padding(18)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(DS.surface)
        .overlay(
            RoundedRectangle(cornerRadius: DS.cardRadius)
                .strokeBorder(DS.line, lineWidth: 1)
        )
        .clipShape(RoundedRectangle(cornerRadius: DS.cardRadius))
    }
}
