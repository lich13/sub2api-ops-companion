import SwiftUI

extension Sub2ApiConfigTabView {
    func setScalarText(_ field: CatalogField, _ raw: String) {
        if field.type == "integer" || field.type == "int" {
            if let i = Int64(raw.trimmingCharacters(in: .whitespaces)) {
                setFieldValue(field, .int(i))
            }
        } else if field.type == "number" || field.type == "float" {
            if let d = Double(raw.trimmingCharacters(in: .whitespaces)) {
                setFieldValue(field, .double(d))
            }
        } else {
            setFieldValue(field, .string(raw))
        }
    }

    func setFieldValue(_ field: CatalogField, _ next: JSONValue) {
        let current = document?.values[field.path]
        if current?.configured == true, current?.yaml == next {
            draft.removeValue(forKey: field.path)
        } else if current?.configured != true, field.has_default, field.default_value == next {
            draft.removeValue(forKey: field.path)
        } else {
            draft[field.path] = DraftChange(path: field.path, remove: false, value: next)
        }
        issues.removeAll()
        messageText = ""
    }

    func loadConfig(resetDraft: Bool) async {
        guard online else { return }
        busy = true
        errorText = ""
        defer { busy = false }
        do {
            let next: ConfigDocumentDTO = try await RustBridge.shared.api("GET", "/sub2api-config")
            document = next
            if !next.catalog.groups.contains(where: { $0.id == selectedGroup }),
               let first = next.catalog.groups.first
            {
                selectedGroup = first.id
            }
            if resetDraft {
                draft.removeAll()
                issues.removeAll()
            }
        } catch {
            errorText = error.localizedDescription
        }
    }

    private func buildPayload(_ doc: ConfigDocumentDTO) -> JSONValue {
        .object([
            "expected_revision": .string(doc.revision),
            "runtime_revision": .string(doc.runtime_revision),
            "changes": .array(draft.values.map(\.jsonObject)),
        ])
    }

    func validateChanges(openConfirm: Bool) async {
        guard let doc = document, !isBlocked, !draft.isEmpty else { return }
        var localIssues: [ConfigIssue] = []
        for change in draft.values where !change.remove {
            if let raw = change.rawStructuredText,
               let field = doc.catalog.fields.first(where: { $0.path == change.path })
            {
                let res = LightYAML.parseStructured(raw, expectedType: field.type)
                if !res.error.isEmpty {
                    localIssues.append(ConfigIssue(path: change.path, message: res.error))
                }
            }
        }
        if !localIssues.isEmpty {
            issues = localIssues
            return
        }
        struct ValidateReply: Decodable, Sendable { var valid: Bool; var errors: [ConfigIssue] }
        busy = true
        errorText = ""
        messageText = ""
        defer { busy = false }
        do {
            let res: ValidateReply = try await RustBridge.shared.api(
                "POST",
                "/sub2api-config/validate",
                body: buildPayload(doc)
            )
            issues = res.errors
            if res.valid {
                messageText = "配置检查通过"
                if openConfirm { presentConfirmApply() }
            }
        } catch {
            errorText = error.localizedDescription
        }
    }

    func applyChanges() async {
        guard let doc = document, !isBlocked else { return }
        busy = true
        errorText = ""
        defer { busy = false }
        do {
            let op: HostOperation = try await RustBridge.shared.api(
                "POST",
                "/sub2api-config/apply",
                body: buildPayload(doc)
            )
            hostOp = op
            dismissConfirmApply()
            messageText = ""
        } catch {
            dismissConfirmApply()
            errorText = error.localizedDescription
            hostOp = HostOperation(status: "restarting", message: "提交结果待确认，正在查询操作状态")
        }
    }
}

struct ConfigStructuredEditorView: View {
    let textValue: String
    let editable: Bool
    let onTextChange: (String) -> Void
    @State var text: String

    init(textValue: String, editable: Bool, onTextChange: @escaping (String) -> Void) {
        self.textValue = textValue
        self.editable = editable
        self.onTextChange = onTextChange
        self._text = State(initialValue: textValue)
    }

    var body: some View {
        TextEditor(text: $text)
            .font(DS.fontMono11)
            .foregroundStyle(DS.ink)
            .scrollContentBackground(.hidden)
            .frame(height: 84)
            .padding(6)
            .background(DS.surface)
            .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
            .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
            .disabled(!editable)
            .onChange(of: textValue) { _, next in
                if text != next { text = next }
            }
            .onChange(of: text) { _, next in
                if next != textValue { onTextChange(next) }
            }
    }
}

struct ConfigScalarEditorView: View {
    let textValue: String
    let prompt: String
    let isSecret: Bool
    let isSensitive: Bool
    let editable: Bool
    let onToggleSecret: () -> Void
    let onTextChange: (String) -> Void
    @State var text: String

    init(
        textValue: String,
        prompt: String,
        isSecret: Bool,
        isSensitive: Bool,
        editable: Bool,
        onToggleSecret: @escaping () -> Void,
        onTextChange: @escaping (String) -> Void
    ) {
        self.textValue = textValue
        self.prompt = prompt
        self.isSecret = isSecret
        self.isSensitive = isSensitive
        self.editable = editable
        self.onToggleSecret = onToggleSecret
        self.onTextChange = onTextChange
        self._text = State(initialValue: textValue)
    }

    var body: some View {
        HStack(spacing: 6) {
            if isSecret {
                SecureField(text: $text, prompt: Text(verbatim: prompt)) { EmptyView() }
                    .textFieldStyle(.plain)
                    .font(DS.fontBody)
                    .foregroundStyle(DS.ink)
            } else {
                TextField(text: $text, prompt: Text(verbatim: prompt)) { EmptyView() }
                    .textFieldStyle(.plain)
                    .font(DS.fontBody)
                    .foregroundStyle(DS.ink)
            }
            if isSensitive {
                LinearIconButton(
                    systemName: isSecret ? "eye" : "eye.slash",
                    label: isSecret ? "显示" : "隐藏",
                    size: 12,
                    action: onToggleSecret
                )
            }
        }
        .padding(.horizontal, 12)
        .frame(height: DS.controlHeight)
        .background(DS.surface)
        .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
        .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
        .disabled(!editable)
        .onChange(of: textValue) { _, next in
            if text != next { text = next }
        }
        .onChange(of: text) { _, next in
            if next != textValue { onTextChange(next) }
        }
    }
}
