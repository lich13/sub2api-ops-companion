import SwiftUI

public struct RecordDatePickerView: View {
    public let value: RecordDateRange
    public let disabled: Bool
    public let onChange: (RecordDateRange) -> Void

    @State private var isOpen: Bool = false
    @State private var draft: RecordDateRange
    @State private var error: String = ""

    public init(value: RecordDateRange, disabled: Bool, onChange: @escaping (RecordDateRange) -> Void) {
        self.value = value
        self.disabled = disabled
        self.onChange = onChange
        self._draft = State(initialValue: value)
    }

    public var body: some View {
        Button {
            draft = value
            error = ""
            isOpen.toggle()
        } label: {
            HStack(spacing: 6) {
                Image(systemName: "calendar")
                    .font(DS.iconFont(12))
                    .foregroundStyle(DS.muted)
                Text(BeijingDateLogic.rangeLabel(value))
                    .font(DS.fontCompactMedium)
                    .monospacedDigit()
                    .foregroundStyle(DS.ink)
                Image(systemName: "chevron.down")
                    .font(DS.iconFont(10, weight: .semibold))
                    .foregroundStyle(DS.muted)
            }
            .padding(.horizontal, 9)
            .frame(minWidth: 104)
            .frame(height: DS.controlHeightDense)
            .background(DS.surface)
            .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
            .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
        }
        .buttonStyle(.linearPlain)
        .disabled(disabled)
        .accessibilityLabel("时间范围")
        .popover(isPresented: $isOpen, arrowEdge: .bottom) {
            VStack(alignment: .leading, spacing: 12) {
                HStack {
                    Text("时间范围（北京时间）")
                        .font(DS.fontCompactSemibold)
                        .foregroundStyle(DS.ink)
                    Spacer()
                    LinearIconButton(systemName: "xmark", label: "关闭时间范围", size: 11) {
                        isOpen = false
                    }
                }

                LazyVGrid(columns: Array(repeating: GridItem(.flexible(), spacing: 6), count: 4), spacing: 6) {
                    ForEach(DatePresetKey.allCases, id: \.rawValue) { key in
                        let selected = draft.preset == key
                        Button(key.label) {
                            draft = BeijingDateLogic.presetRange(key)
                            error = ""
                        }
                        .buttonStyle(LinearButtonStyle(variant: selected ? .selectedGhost : .secondary, compact: true))
                    }
                }

                HStack(spacing: 8) {
                    VStack(alignment: .leading, spacing: 4) {
                        Text("开始日期")
                            .font(DS.fontResetTime)
                            .foregroundStyle(DS.muted)
                        TextField(text: $draft.start, prompt: Text(verbatim: "YYYY-MM-DD")) { EmptyView() }
                        .textFieldStyle(.plain)
                        .font(DS.fontCompact).monospacedDigit()
                        .monospacedDigit()
                        .padding(.horizontal, 8)
                        .frame(height: DS.controlHeightCompact)
                        .background(DS.surface)
                        .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(DS.line, lineWidth: 1))
                        .onChange(of: draft.start) { old, next in
                            if old != next, let p = draft.preset, BeijingDateLogic.presetRange(p).start != next {
                                draft.preset = nil
                            }
                        }
                    }

                    VStack(alignment: .leading, spacing: 4) {
                        Text("结束日期")
                            .font(DS.fontResetTime)
                            .foregroundStyle(DS.muted)
                        TextField(text: $draft.end, prompt: Text(verbatim: "YYYY-MM-DD")) { EmptyView() }
                        .textFieldStyle(.plain)
                        .font(DS.fontCompact).monospacedDigit()
                        .monospacedDigit()
                        .padding(.horizontal, 8)
                        .frame(height: DS.controlHeightCompact)
                        .background(DS.surface)
                        .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(DS.line, lineWidth: 1))
                        .onChange(of: draft.end) { old, next in
                            if old != next, let p = draft.preset, BeijingDateLogic.presetRange(p).end != next {
                                draft.preset = nil
                            }
                        }
                    }
                }

                if !error.isEmpty {
                    Text(error)
                        .font(DS.fontResetTime)
                        .foregroundStyle(DS.bad)
                }

                HStack {
                    Spacer()
                    Button("应用") {
                        let next = draft.preset.map { BeijingDateLogic.presetRange($0) } ?? draft
                        guard BeijingDateLogic.isValidRange(start: next.start, end: next.end) else {
                            error = "请选择有效的日期范围（YYYY-MM-DD）"
                            return
                        }
                        onChange(next)
                        isOpen = false
                    }
                    .buttonStyle(LinearButtonStyle(variant: .primary, compact: true))
                }
            }
            .padding(14)
            .frame(width: 310)
            .background(DS.surface)
            .onExitCommand { isOpen = false }
        }
    }
}
