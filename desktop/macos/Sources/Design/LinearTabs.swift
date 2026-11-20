import AppKit
import SwiftUI

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
        @State var hovered: Bool = false

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
