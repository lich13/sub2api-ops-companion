import SwiftUI

public struct SchedulingParametersView: View {
    public let account: Account
    @Bindable public var store: AppStore

    @State private var editingConcurrency: Bool = false
    @State private var editingLoadFactor: Bool = false

    public init(account: Account, store: AppStore) {
        self.account = account
        self.store = store
    }

    private func canEdit(_ key: String) -> Bool {
        guard store.state.online else { return false }
        if let cap = account.settings_capabilities?[key] {
            return cap.editable
        }
        return account.operation_versions[key] != nil
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: 3) {
                Text("优先级")
                    .font(DS.fontPriorityMeta)
                    .foregroundStyle(DS.muted)
                    .lineLimit(1)
                    .frame(width: 48, alignment: .leading)
                Text("\(account.priority)")
                    .font(DS.fontPriorityMeta)
                    .monospacedDigit()
                    .lineLimit(1)
                    .foregroundStyle(DS.ink)
                    .padding(.horizontal, 4)
                    .frame(maxWidth: .infinity, minHeight: 22, alignment: .trailing)
            }

            HStack(spacing: 3) {
                Text("并发")
                    .font(DS.fontPriorityMeta)
                    .foregroundStyle(DS.muted)
                    .lineLimit(1)
                    .frame(width: 48, alignment: .leading)
                EditableParameterSlotButton(
                    text: account.concurrency.map(String.init) ?? "—",
                    editable: canEdit("concurrency"),
                    accessibilityLabel: "编辑\(account.name)并发"
                ) {
                    editingConcurrency = true
                }
                .popover(isPresented: $editingConcurrency, arrowEdge: .bottom) {
                    NumericParameterPopover(
                        title: "编辑并发",
                        fieldLabel: "并发",
                        initialValue: account.concurrency,
                        supportsAuto: false,
                        onSave: { nextVal in
                            try await saveParameter(action: "concurrency", key: "concurrency", value: nextVal)
                        },
                        onClose: { editingConcurrency = false }
                    )
                }
            }

            HStack(spacing: 3) {
                Text("负载因子")
                    .font(DS.fontPriorityMeta)
                    .foregroundStyle(DS.muted)
                    .lineLimit(1)
                    .frame(width: 48, alignment: .leading)
                EditableParameterSlotButton(
                    text: account.load_factor.map(String.init) ?? "自动",
                    editable: canEdit("load_factor"),
                    accessibilityLabel: "编辑\(account.name)负载因子"
                ) {
                    editingLoadFactor = true
                }
                .popover(isPresented: $editingLoadFactor, arrowEdge: .bottom) {
                    NumericParameterPopover(
                        title: "编辑负载因子",
                        fieldLabel: "负载因子",
                        initialValue: account.load_factor,
                        supportsAuto: true,
                        onSave: { nextVal in
                            try await saveParameter(action: "load_factor", key: "load_factor", value: nextVal)
                        },
                        onClose: { editingLoadFactor = false }
                    )
                }
            }
        }
        .frame(maxWidth: .infinity)
    }

    private func saveParameter(action: String, key: String, value: Int?) async throws {
        let payloadVal: JSONValue = value.map { .int(Int64($0)) } ?? .null
        let res = try await store.operations.accountOperation(
            account: account,
            action: action,
            payload: [key: payloadVal]
        )
        if res["verified"]?.boolValue == false {
            throw RustBridgeError("\(action == "concurrency" ? "并发" : "负载因子")写入未确认，请刷新核对")
        }
    }
}

private struct EditableParameterSlotButton: View {
    let text: String
    let editable: Bool
    let accessibilityLabel: String
    let action: () -> Void

    @State private var hovered: Bool = false

    var body: some View {
        let showHover = editable && hovered
        Button(action: action) {
            Text(text)
                .font(DS.fontPriorityMeta)
                .monospacedDigit()
                .lineLimit(1)
                .foregroundStyle(DS.ink)
                .padding(.horizontal, 4)
                .frame(maxWidth: .infinity, minHeight: 22, alignment: .trailing)
                .background(showHover ? DS.hover : DS.clear)
                .overlay {
                    if showHover {
                        RoundedRectangle(cornerRadius: 5)
                            .strokeBorder(DS.line, lineWidth: 1)
                    }
                }
                .clipShape(RoundedRectangle(cornerRadius: 5))
        }
        .buttonStyle(.linearPlain)
        .disabled(!editable)
        .onHover { hovered = $0 }
        .accessibilityLabel(accessibilityLabel)
    }
}

private struct NumericParameterPopover: View {
    let title: String
    let fieldLabel: String
    let initialValue: Int?
    let supportsAuto: Bool
    let onSave: @MainActor (Int?) async throws -> Void
    let onClose: () -> Void

    @State private var text: String = ""
    @State private var isAuto: Bool = false
    @State private var busy: Bool = false
    @State private var error: String = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(title)
                .font(DS.fontCompactSemibold)
                .foregroundStyle(DS.ink)

            if supportsAuto {
                Button {
                    isAuto.toggle()
                    error = ""
                    if !isAuto && text.isEmpty {
                        text = "1"
                    }
                } label: {
                    HStack(spacing: 6) {
                        Image(systemName: isAuto ? "checkmark.square.fill" : "square")
                            .font(DS.iconFont(13))
                            .foregroundStyle(isAuto ? DS.accent : DS.muted)
                        Text("自动计算")
                            .font(DS.fontCompact)
                            .foregroundStyle(DS.ink)
                    }
                }
                .buttonStyle(.linearPlain)
            }

            TextField(text: $text, prompt: Text(verbatim: "正整数")) { EmptyView() }
                .textFieldStyle(.plain)
                .font(DS.fontBody)
                .monospacedDigit()
                .disabled(isAuto || busy)
                .padding(.horizontal, 8)
                .frame(height: DS.controlHeightCompact)
                .background(DS.surface)
                .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(DS.line, lineWidth: 1))
                .clipShape(RoundedRectangle(cornerRadius: 6))
                .onSubmit {
                    Task { await submit() }
                }

            if !error.isEmpty {
                Text(error)
                    .font(DS.fontResetTime)
                    .foregroundStyle(DS.bad)
            }

            HStack(spacing: 8) {
                Spacer()
                Button("取消", action: onClose)
                    .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                    .disabled(busy)
                Button(busy ? "保存中" : "保存") {
                    Task { await submit() }
                }
                .buttonStyle(LinearButtonStyle(variant: .primary, compact: true))
                .disabled(busy)
            }
        }
        .padding(12)
        .frame(width: 210)
        .background(DS.surface)
        .onAppear {
            if let initialValue {
                isAuto = false
                text = String(initialValue)
            } else {
                isAuto = supportsAuto
                text = ""
            }
        }
        .onExitCommand(perform: onClose)
    }

    private func submit() async {
        guard !busy else { return }
        let parsed: Int?
        if supportsAuto && isAuto {
            parsed = nil
        } else {
            let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
            guard let n = Int(trimmed), n >= 1 else {
                error = "请输入大于等于 1 的\(fieldLabel)整数"
                return
            }
            parsed = n
        }
        busy = true
        error = ""
        defer { busy = false }
        do {
            try await onSave(parsed)
            onClose()
        } catch {
            self.error = error.localizedDescription
        }
    }
}
