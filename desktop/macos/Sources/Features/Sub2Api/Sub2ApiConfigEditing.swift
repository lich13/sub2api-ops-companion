import SwiftUI

extension Sub2ApiConfigTabView {
    public func setScalarText(_ field: CatalogField, _ raw: String) {
        let isInt = field.type == "integer" || field.type == "int"
        let isFloat = field.type == "number" || field.type == "float"
        guard isInt || isFloat else {
            setFieldValue(field, .string(raw))
            return
        }
        let trimmed = raw.trimmingCharacters(in: .whitespaces)
        let current = document?.values[field.path]
        if trimmed.isEmpty {
            if field.nullable == true {
                setFieldValue(field, .null)
            } else if current?.configured != true {
                draft.removeValue(forKey: field.path)
                issues.removeAll()
                messageText = ""
            } else {
                draft[field.path] = DraftChange(
                    path: field.path,
                    remove: false,
                    value: nil,
                    rawScalarText: raw
                )
                issues.removeAll()
                messageText = ""
            }
            return
        }
        if isInt {
            if let i = Int64(trimmed) {
                setFieldValue(field, .int(i))
            } else {
                draft[field.path] = DraftChange(
                    path: field.path,
                    remove: false,
                    value: nil,
                    rawScalarText: raw
                )
                issues.removeAll()
                messageText = ""
            }
        } else {
            if let d = Double(trimmed), d.isFinite {
                setFieldValue(field, .double(d))
            } else {
                draft[field.path] = DraftChange(
                    path: field.path,
                    remove: false,
                    value: nil,
                    rawScalarText: raw
                )
                issues.removeAll()
                messageText = ""
            }
        }
    }

    public func setFieldValue(_ field: CatalogField, _ next: JSONValue, explicit: Bool = false) {
        let current = document?.values[field.path]
        if !explicit, current?.configured == true, current?.yaml == next {
            draft.removeValue(forKey: field.path)
        } else if !explicit, current?.configured != true, field.has_default, field.default_value == next {
            draft.removeValue(forKey: field.path)
        } else {
            draft[field.path] = DraftChange(path: field.path, remove: false, value: next)
        }
        issues.removeAll()
        messageText = ""
    }

    func bindConnectionIfNeeded() {
        guard boundConnectionKey != connectionKey else { return }
        boundConnectionKey = connectionKey
        statusLoadedKey = ""
        loadSeq &+= 1
        busy = false
        session.resetForConnection()
        selectedGroup = "server"
        resetTransientViewState()
    }

    func loadConfig(resetDraft: Bool, protectDraft: Bool = true) async {
        bindConnectionIfNeeded()
        guard online else { return }
        let capturedKey = connectionKey
        loadSeq &+= 1
        let seq = loadSeq
        busy = true
        errorText = ""
        defer {
            if seq == loadSeq && connectionKey == capturedKey {
                busy = false
            }
        }
        do {
            let next: ConfigDocumentDTO = try await RustBridge.shared.api("GET", "/sub2api-config")
            guard !Task.isCancelled, seq == loadSeq, connectionKey == capturedKey else { return }
            session.applyLoaded(next, resetDraft: resetDraft, protectDraft: protectDraft)
            if !next.catalog.groups.contains(where: { $0.id == selectedGroup }),
               let first = next.catalog.groups.first
            {
                selectedGroup = first.id
            }
        } catch is CancellationError {
            return
        } catch {
            guard !Task.isCancelled, seq == loadSeq, connectionKey == capturedKey else { return }
            session.handleLoadFailure(error.localizedDescription)
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
        var localIssues: [HostFieldIssue] = []
        for change in draft.values where !change.remove {
            let field = doc.catalog.fields.first(where: { $0.path == change.path })
            if let raw = change.rawStructuredText, let field {
                let res = LightYAML.parseStructured(raw, expectedType: field.type)
                if !res.error.isEmpty {
                    localIssues.append(HostFieldIssue(path: change.path, message: res.error))
                }
            }
            if let rawScalar = change.rawScalarText {
                let trimmed = rawScalar.trimmingCharacters(in: .whitespaces)
                let isInt = field?.type == "integer" || field?.type == "int"
                let message = trimmed.isEmpty
                    ? "使用恢复默认移除配置或填写有效数值"
                    : (isInt ? "需要有效整数" : "需要有效数值")
                localIssues.append(HostFieldIssue(path: change.path, message: message))
            }
        }
        if !localIssues.isEmpty {
            issues = localIssues
            return
        }
        busy = true
        errorText = ""
        messageText = ""
        defer { busy = false }
        do {
            let res: HostValidationReply = try await RustBridge.shared.api(
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
        session.applyUncertain = false
        defer { busy = false }
        do {
            let op: HostOperationReply = try await RustBridge.shared.api(
                "POST",
                "/sub2api-config/apply",
                body: buildPayload(doc)
            )
            hostOp = op
            dismissConfirmApply()
            messageText = ""
        } catch {
            dismissConfirmApply()
            session.handleApplyFailure(error.localizedDescription)
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
    @FocusState private var isFocused: Bool

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
                SecureField(text: $text, prompt: Text(verbatim: prompt).foregroundStyle(DS.placeholder)) { EmptyView() }
                    .textFieldStyle(.plain)
                    .font(DS.fontBody)
                    .foregroundStyle(DS.ink)
                    .focused($isFocused)
            } else {
                TextField(text: $text, prompt: Text(verbatim: prompt).foregroundStyle(DS.placeholder)) { EmptyView() }
                    .textFieldStyle(.plain)
                    .font(DS.fontBody)
                    .foregroundStyle(DS.ink)
                    .focused($isFocused)
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
            if text != next && (!isFocused || !text.trimmingCharacters(in: .whitespaces).isEmpty) {
                text = next
            }
        }
        .onChange(of: isFocused) { _, focused in
            if !focused && text != textValue {
                text = textValue
            }
        }
        .onChange(of: text) { _, next in
            if next != textValue { onTextChange(next) }
        }
    }
}
