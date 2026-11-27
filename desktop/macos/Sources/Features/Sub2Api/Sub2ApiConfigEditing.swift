import SwiftUI

extension Sub2ApiConfigController {
    private func buildPayload(_ doc: Sub2ApiConfigTabView.ConfigDocumentDTO) -> JSONValue {
        .object([
            "expected_revision": .string(doc.revision),
            "runtime_revision": .string(doc.runtime_revision),
            "changes": .array(session.draft.values.map(\.jsonObject)),
        ])
    }

    public func refreshOperationStatus(capturedKey: String, store: AppStore?) async {
        guard !isOperationRunning else { return }
        let sSeq = beginStatusRead()
        do {
            let op: HostOperationReply = try await RustBridge.shared.api("GET", "/sub2api-config/operation")
            guard !Task.isCancelled, isCurrentStatusRead(sSeq, capturedKey: capturedKey, store: store) else { return }
            session.recordObservedOperation(op)
        } catch is CancellationError {
            return
        } catch {
            guard !Task.isCancelled, isCurrentStatusRead(sSeq, capturedKey: capturedKey, store: store) else { return }
            session.errorText = error.localizedDescription
        }
    }

    public func pollOperationOnce(
        capturedKey: String,
        store: AppStore?
    ) async -> Sub2ApiConfigSession.PollResolution? {
        guard isMounted,
              boundConnectionKey == capturedKey,
              store == nil || store?.connectionKey == capturedKey
        else { return nil }
        do {
            let nextOp: HostOperationReply = try await RustBridge.shared.api("GET", "/sub2api-config/operation")
            guard !Task.isCancelled,
                  isMounted,
                  boundConnectionKey == capturedKey,
                  store == nil || store?.connectionKey == capturedKey
            else { return nil }
            return session.handlePolledOperation(nextOp)
        } catch is CancellationError {
            return nil
        } catch {
            if !Task.isCancelled
                && isMounted
                && boundConnectionKey == capturedKey
                && (store == nil || store?.connectionKey == capturedKey)
            {
                session.errorText = "\(error.localizedDescription)；应用结果待确认，请保持此页或稍后查询状态"
            }
            return .keepPolling
        }
    }

    public func loadConfig(
        online: Bool,
        capturedKey: String,
        store: AppStore?,
        resetDraft: Bool,
        protectDraft: Bool = true
    ) async -> String? {
        guard online, boundConnectionKey == capturedKey, store == nil || store?.connectionKey == capturedKey else {
            return nil
        }
        let seq = beginLoad()
        session.errorText = ""
        defer {
            finishLoad(seq, capturedKey: capturedKey, store: store)
        }
        do {
            let next: Sub2ApiConfigTabView.ConfigDocumentDTO = try await RustBridge.shared.api("GET", "/sub2api-config")
            guard !Task.isCancelled, isCurrentLoad(seq, capturedKey: capturedKey, store: store) else { return nil }
            session.applyLoaded(next, resetDraft: resetDraft, protectDraft: protectDraft)
            return next.catalog.groups.first?.id
        } catch is CancellationError {
            return nil
        } catch {
            guard !Task.isCancelled, isCurrentLoad(seq, capturedKey: capturedKey, store: store) else { return nil }
            session.handleLoadFailure(error.localizedDescription)
            return nil
        }
    }

    public func validateChanges(
        online: Bool,
        capturedKey: String,
        store: AppStore?,
        openConfirm: Bool,
        onPresentConfirm: () -> Void
    ) async {
        guard let doc = session.document, !isBlocked(online: online), !session.draft.isEmpty else { return }
        var localIssues: [HostFieldIssue] = []
        for change in session.draft.values where !change.remove {
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
            session.issues = localIssues
            return
        }
        let seq = beginMutation()
        session.errorText = ""
        session.messageText = ""
        defer {
            finishMutation(seq, capturedKey: capturedKey, store: store)
        }
        do {
            let res: HostValidationReply = try await RustBridge.shared.api(
                "POST",
                "/sub2api-config/validate",
                body: buildPayload(doc)
            )
            guard !Task.isCancelled,
                  canAcceptMutation(seq, capturedKey: capturedKey, store: store)
            else { return }
            session.issues = res.errors
            if res.valid {
                session.messageText = "配置检查通过"
                if openConfirm && canPresentConfirmModal(seq, capturedKey: capturedKey, store: store) {
                    onPresentConfirm()
                }
            }
        } catch {
            guard !Task.isCancelled,
                  canAcceptMutation(seq, capturedKey: capturedKey, store: store)
            else { return }
            session.errorText = error.localizedDescription
        }
    }

    public func applyChanges(
        online: Bool,
        capturedKey: String,
        store: AppStore?
    ) async {
        guard let doc = session.document, !isBlocked(online: online) else { return }
        let seq = beginMutation()
        session.errorText = ""
        session.applyUncertain = false
        session.ownOperation = false
        defer {
            finishMutation(seq, capturedKey: capturedKey, store: store)
        }
        do {
            let op: HostOperationReply = try await RustBridge.shared.api(
                "POST",
                "/sub2api-config/apply",
                body: buildPayload(doc)
            )
            guard canAcceptMutation(seq, capturedKey: capturedKey, store: store) else { return }
            session.recordSubmittedOperation(op)
            dismissConfirmModal(store: store)
            if !Sub2ApiConfigSession.isRunning(op) {
                if op.status == "succeeded" {
                    _ = await loadConfig(
                        online: true,
                        capturedKey: capturedKey,
                        store: store,
                        resetDraft: true,
                        protectDraft: false
                    )
                } else {
                    _ = await loadConfig(
                        online: true,
                        capturedKey: capturedKey,
                        store: store,
                        resetDraft: false,
                        protectDraft: false
                    )
                }
            }
        } catch {
            guard canAcceptMutation(seq, capturedKey: capturedKey, store: store) else { return }
            dismissConfirmModal(store: store)
            session.handleApplyFailure(error.localizedDescription)
        }
    }
}

extension Sub2ApiConfigTabView {
    public func setScalarText(_ field: CatalogField, _ raw: String) {
        let isInt = field.type == "integer" || field.type == "int"
        let isFloat = field.type == "number" || field.type == "float"
        guard isInt || isFloat else {
            setFieldValue(field, .string(raw))
            return
        }
        let trimmed = raw.trimmingCharacters(in: .whitespaces)
        let current = controller.session.document?.values[field.path]
        if trimmed.isEmpty {
            if field.nullable == true {
                setFieldValue(field, .null)
            } else if current?.configured != true {
                controller.session.draft.removeValue(forKey: field.path)
                controller.session.issues.removeAll()
                controller.session.messageText = ""
            } else {
                controller.session.draft[field.path] = DraftChange(
                    path: field.path,
                    remove: false,
                    value: nil,
                    rawScalarText: raw
                )
                controller.session.issues.removeAll()
                controller.session.messageText = ""
            }
            return
        }
        if isInt {
            if let i = Int64(trimmed) {
                setFieldValue(field, .int(i))
            } else {
                controller.session.draft[field.path] = DraftChange(
                    path: field.path,
                    remove: false,
                    value: nil,
                    rawScalarText: raw
                )
                controller.session.issues.removeAll()
                controller.session.messageText = ""
            }
        } else {
            if let d = Double(trimmed), d.isFinite {
                setFieldValue(field, .double(d))
            } else {
                controller.session.draft[field.path] = DraftChange(
                    path: field.path,
                    remove: false,
                    value: nil,
                    rawScalarText: raw
                )
                controller.session.issues.removeAll()
                controller.session.messageText = ""
            }
        }
    }

    public func setFieldValue(_ field: CatalogField, _ next: JSONValue, explicit: Bool = false) {
        let current = controller.session.document?.values[field.path]
        if !explicit, current?.configured == true, current?.yaml == next {
            controller.session.draft.removeValue(forKey: field.path)
        } else if !explicit, current?.configured != true, field.has_default, field.default_value == next {
            controller.session.draft.removeValue(forKey: field.path)
        } else {
            controller.session.draft[field.path] = DraftChange(path: field.path, remove: false, value: next)
        }
        controller.session.issues.removeAll()
        controller.session.messageText = ""
    }

    func bindConnectionIfNeeded() {
        let liveKey = store?.connectionKey ?? connectionKey
        if controller.bindConnection(liveKey) {
            selectedGroup = "server"
            resetTransientViewState()
        }
    }

    func loadConfig(resetDraft: Bool, protectDraft: Bool = true, forceOnline: Bool = false) async {
        bindConnectionIfNeeded()
        let liveKey = store?.connectionKey ?? connectionKey
        let liveOnline = forceOnline || (store?.state.online ?? online)
        if let firstGroup = await controller.loadConfig(
            online: liveOnline,
            capturedKey: liveKey,
            store: store,
            resetDraft: resetDraft,
            protectDraft: protectDraft
        ), let groups = controller.session.document?.catalog.groups,
           !groups.contains(where: { $0.id == selectedGroup })
        {
            selectedGroup = firstGroup
        }
    }

    func validateChanges(openConfirm: Bool) async {
        let liveKey = store?.connectionKey ?? connectionKey
        await controller.validateChanges(
            online: online,
            capturedKey: liveKey,
            store: store,
            openConfirm: openConfirm,
            onPresentConfirm: { presentConfirmApply() }
        )
    }

    func applyChanges() async {
        let liveKey = store?.connectionKey ?? connectionKey
        await controller.applyChanges(online: online, capturedKey: liveKey, store: store)
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
