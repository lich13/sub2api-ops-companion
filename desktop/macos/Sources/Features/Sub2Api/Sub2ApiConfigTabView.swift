import SwiftUI

/// Matches `Sub2ApiConfig` in `desktop/src/Sub2ApiConfig.tsx`.
public struct Sub2ApiConfigTabView: View {
    public let active: Bool
    public let online: Bool
    public let connectionKey: String

    public struct CatalogGroup: Codable, Sendable, Equatable, Identifiable {
        public var id: String
        public var label: String
    }

    public struct CatalogField: Codable, Sendable, Equatable, Identifiable {
        public var id: String { path }
        public var path: String
        public var group: String
        public var type: String
        public var type_label: String
        public var description: String
        public var range: String
        public var default_text: String
        public var default_value: JSONValue?
        public var has_default: Bool
        public var sensitive: Bool
        public var `internal`: Bool
        public var editable: Bool
        public var status: String
        public var source_url: String
        public var env: [String]
        public var choices: [String]?
    }

    public struct FieldRuntimeValue: Codable, Sendable, Equatable {
        public var configured: Bool
        public var yaml: JSONValue?
        public var effective: JSONValue?
        public var source: String
        public var environment: String?
        public var editable: Bool
    }

    public struct CatalogPayload: Codable, Sendable, Equatable {
        public var version: String
        public var revision: String
        public var groups: [CatalogGroup]
        public var fields: [CatalogField]
    }

    public struct ConfigIssue: Codable, Sendable, Equatable, Identifiable {
        public var id: String { "\(path):\(message)" }
        public var path: String
        public var message: String
    }

    public struct ConfigDocumentDTO: Codable, Sendable, Equatable {
        public var available: Bool
        public var editable: Bool
        public var reason: String
        public var version: String
        public var commit: String
        public var healthy: Bool
        public var revision: String
        public var runtime_revision: String
        public var catalog: CatalogPayload
        public var values: [String: FieldRuntimeValue]
        public var unknown: [ConfigIssue]
    }

    public struct DraftChange: Sendable, Equatable {
        public var path: String
        public var remove: Bool
        public var value: JSONValue?
        public var rawStructuredText: String?

        public var jsonObject: JSONValue {
            var obj: [String: JSONValue] = [
                "path": .string(path),
                "remove": .bool(remove),
            ]
            if !remove, let value { obj["value"] = value }
            return .object(obj)
        }
    }

    private struct HostOperation: Codable, Sendable, Equatable {
        var status: String
        var message: String?
    }

    @State private var document: ConfigDocumentDTO? = nil
    @State private var draft: [String: DraftChange] = [:]
    @State private var selectedGroup: String = "server"
    @State private var query: String = ""
    @State private var filterMode: String = "all"
    @State private var busy: Bool = false
    @State private var errorText: String = ""
    @State private var messageText: String = ""
    @State private var issues: [ConfigIssue] = []
    @State private var showConfirmApply: Bool = false
    @State private var hostOp: HostOperation? = nil
    @State private var revealedPaths: Set<String> = []

    public init(active: Bool, online: Bool, connectionKey: String) {
        self.active = active
        self.online = online
        self.connectionKey = connectionKey
    }

    private var isOperationRunning: Bool {
        guard let s = hostOp?.status else { return false }
        return s == "saving" || s == "restarting" || s == "restoring"
    }

    private var isBlocked: Bool {
        busy || isOperationRunning || !online || document?.editable != true
    }

    private var visibleFields: [CatalogField] {
        guard let doc = document else { return [] }
        let q = query.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        return doc.catalog.fields.filter { f in
            if f.internal { return false }
            if q.isEmpty && f.group != selectedGroup { return false }
            if !q.isEmpty {
                let hay = "\(f.path) \(f.description) \(f.env.joined(separator: " "))".lowercased()
                if !hay.contains(q) { return false }
            }
            if filterMode == "configured" {
                return doc.values[f.path]?.configured == true || draft[f.path] != nil
            }
            if filterMode == "changed" {
                return draft[f.path] != nil
            }
            return true
        }
    }

    public var body: some View {
        ZStack {
            VStack(alignment: .leading, spacing: 12) {
                Sub2ApiVersionCardView(
                    active: active,
                    online: online,
                    knownVersion: document?.version ?? ""
                ) {
                    Task { await loadConfig(resetDraft: false) }
                }

                toolbarBar

                if let doc = document, !doc.editable, !doc.reason.isEmpty {
                    Text(doc.reason)
                        .font(DS.fontCompactMedium)
                        .foregroundStyle(DS.warn)
                        .padding(10)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(DS.warnBg)
                        .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
                }
                if let op = hostOp, !op.status.isEmpty, op.status != "idle" {
                    Text("宿主机操作 (\(op.status))：\(op.message ?? "正在执行")")
                        .font(DS.fontCompactMedium)
                        .foregroundStyle(op.status == "failed" ? DS.bad : DS.accent)
                }
                if !messageText.isEmpty {
                    Text(messageText).font(DS.fontCompactMedium).foregroundStyle(DS.good)
                }
                if !errorText.isEmpty {
                    Text(errorText).font(DS.fontCompact).foregroundStyle(DS.bad)
                }

                if let doc = document {
                    HStack(alignment: .top, spacing: 14) {
                        groupSidebar(doc)
                        fieldsColumn(doc)
                    }
                } else {
                    CardSection {
                        Text(busy ? "正在读取 Sub2API 配置目录…" : "暂无可用配置数据")
                            .font(DS.fontCompact)
                            .foregroundStyle(DS.muted)
                    }
                }
            }

            if showConfirmApply {
                confirmApplyDialog
            }
        }
        .task(id: "\(connectionKey):\(active):\(online)") {
            guard active, online else { return }
            await loadConfig(resetDraft: false)
        }
        .task(id: isOperationRunning) {
            guard isOperationRunning else { return }
            while !Task.isCancelled && isOperationRunning {
                try? await Task.sleep(for: .milliseconds(1500))
                if let nextOp: HostOperation = try? await RustBridge.shared.api("GET", "/sub2api-config/operation") {
                    hostOp = nextOp
                    if !["saving", "restarting", "restoring"].contains(nextOp.status) {
                        await loadConfig(resetDraft: nextOp.status == "succeeded")
                        break
                    }
                }
            }
        }
    }

    private var toolbarBar: some View {
        HStack(spacing: 8) {
            HStack(spacing: 6) {
                Image(systemName: "magnifyingglass").font(DS.iconFont(11)).foregroundStyle(DS.muted)
                TextField(text: $query, prompt: Text("搜索配置路径、说明或环境变量")) { EmptyView() }
                    .textFieldStyle(.plain)
                    .font(DS.fontCompact)
                    .frame(width: 220)
            }
            .padding(.horizontal, 8)
            .frame(height: DS.controlHeightCompact)
            .background(DS.surface)
            .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
            .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))

            ForEach([("all", "全部"), ("configured", "已配置"), ("changed", "已修改 (\(draft.count))")], id: \.0) { item in
                Button(item.1) { filterMode = item.0 }
                    .buttonStyle(LinearButtonStyle(variant: filterMode == item.0 ? .selectedGhost : .ghost, compact: true))
            }

            Spacer()

            LinearIconButton(systemName: "arrow.clockwise", label: "重新读取", disabled: busy || !online) {
                Task { await loadConfig(resetDraft: false) }
            }

            if !draft.isEmpty {
                Button("撤销全部") {
                    draft.removeAll()
                    issues.removeAll()
                    messageText = ""
                }
                .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                .disabled(busy || isOperationRunning)
            }

            Button("校验配置") {
                Task { await validateChanges(openConfirm: false) }
            }
            .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
            .disabled(isBlocked || draft.isEmpty)

            Button("保存并重启") {
                Task { await validateChanges(openConfirm: true) }
            }
            .buttonStyle(LinearButtonStyle(variant: .primary, compact: true))
            .disabled(isBlocked || draft.isEmpty)
        }
    }

    private func groupSidebar(_ doc: ConfigDocumentDTO) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            ForEach(doc.catalog.groups) { g in
                let changedCount = doc.catalog.fields.filter { $0.group == g.id && draft[$0.path] != nil }.count
                Button {
                    selectedGroup = g.id
                    query = ""
                } label: {
                    HStack {
                        Text(g.label)
                            .font(selectedGroup == g.id ? DS.fontCompactSemibold : DS.fontCompact)
                            .foregroundStyle(selectedGroup == g.id ? DS.accent : DS.ink)
                        Spacer()
                        if changedCount > 0 {
                            Text("\(changedCount)")
                                .font(DS.fontResetTimeSemibold)
                                .monospacedDigit()
                                .foregroundStyle(DS.accent)
                                .padding(.horizontal, 5)
                                .padding(.vertical, 1)
                                .background(DS.accentBg)
                                .clipShape(Capsule())
                        }
                    }
                    .padding(.horizontal, 10)
                    .padding(.vertical, 6)
                    .background(selectedGroup == g.id ? DS.accentBg : DS.clear)
                    .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
                }
                .buttonStyle(.plain)
            }
        }
        .padding(8)
        .frame(width: 184)
        .background(DS.surface)
        .overlay(RoundedRectangle(cornerRadius: DS.cardRadius).strokeBorder(DS.line, lineWidth: 1))
        .clipShape(RoundedRectangle(cornerRadius: DS.cardRadius))
    }

    private func fieldsColumn(_ doc: ConfigDocumentDTO) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            if visibleFields.isEmpty {
                CardSection {
                    Text("当前筛选下没有匹配的配置字段")
                        .font(DS.fontCompact)
                        .foregroundStyle(DS.muted)
                }
            } else {
                ForEach(visibleFields) { field in
                    fieldCard(field, doc: doc)
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func fieldCard(_ field: CatalogField, doc: ConfigDocumentDTO) -> some View {
        let runtime = doc.values[field.path]
        let change = draft[field.path]
        let overridden = runtime?.environment != nil
        let fieldEditable = !isBlocked && field.editable && runtime?.editable != false && !overridden
        let effectiveVal: JSONValue? = if let c = change {
            c.remove ? field.default_value : c.value
        } else if runtime?.configured == true {
            runtime?.yaml
        } else {
            runtime?.effective ?? field.default_value
        }
        let fieldIssues = issues.filter { $0.path == field.path }

        return VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(verbatim: field.path)
                    .font(DS.fontMono12)
                    .foregroundStyle(DS.ink)
                Text(field.type_label)
                    .font(DS.fontResetTime)
                    .foregroundStyle(DS.muted)
                    .padding(.horizontal, 5)
                    .padding(.vertical, 1)
                    .background(DS.surfaceSubtle)
                    .clipShape(RoundedRectangle(cornerRadius: 4))
                if let envName = runtime?.environment {
                    Text(verbatim: "ENV: \(envName)")
                        .font(DS.fontResetTimeSemibold)
                        .foregroundStyle(DS.warn)
                        .padding(.horizontal, 5)
                        .padding(.vertical, 1)
                        .background(DS.warnBg)
                        .clipShape(RoundedRectangle(cornerRadius: 4))
                }
                if change != nil {
                    Text(change?.remove == true ? "待恢复默认" : "已修改")
                        .font(DS.fontResetTimeSemibold)
                        .foregroundStyle(DS.accent)
                }
                Spacer()
                if runtime?.configured == true && fieldEditable {
                    Button("恢复默认") {
                        draft[field.path] = DraftChange(path: field.path, remove: true, value: nil)
                    }
                    .buttonStyle(LinearButtonStyle(variant: .ghost, compact: true))
                }
                if change != nil {
                    Button("撤销") {
                        draft.removeValue(forKey: field.path)
                    }
                    .buttonStyle(LinearButtonStyle(variant: .ghost, compact: true))
                }
            }

            Text(field.description.components(separatedBy: CharacterSet(charactersIn: "。\n")).first ?? field.description)
                .font(DS.fontCompact)
                .foregroundStyle(DS.muted)

            fieldInputControl(field: field, value: effectiveVal, change: change, editable: fieldEditable)

            HStack(spacing: 12) {
                Text("默认：\(field.default_text.isEmpty ? "—" : field.default_text)")
                if !field.range.isEmpty {
                    Text("范围：\(field.range)")
                }
            }
            .font(DS.fontResetTime)
            .foregroundStyle(DS.muted)

            ForEach(fieldIssues) { iss in
                Text(iss.message).font(DS.fontResetTime).foregroundStyle(DS.bad)
            }
        }
        .padding(12)
        .background(DS.surface)
        .overlay(
            RoundedRectangle(cornerRadius: DS.cardRadius)
                .strokeBorder(fieldIssues.isEmpty ? (change != nil ? DS.accent : DS.line) : DS.bad, lineWidth: 1)
        )
        .clipShape(RoundedRectangle(cornerRadius: DS.cardRadius))
    }

    @ViewBuilder
    private func fieldInputControl(
        field: CatalogField,
        value: JSONValue?,
        change: DraftChange?,
        editable: Bool
    ) -> some View {
        if field.type == "boolean" || field.type == "bool" {
            let b = value?.boolValue ?? false
            LinearSwitch(isOn: b, disabled: !editable, label: field.path) {
                setFieldValue(field, .bool(!b))
            }
        } else if let choices = field.choices, !choices.isEmpty {
            let cur = value?.stringValue ?? ""
            Menu {
                ForEach(choices, id: \.self) { opt in
                    Button { setFieldValue(field, .string(opt)) } label: { Text(verbatim: opt) }
                }
            } label: {
                HStack {
                    Text(verbatim: cur.isEmpty ? "未设置" : cur).font(DS.fontMono12).foregroundStyle(DS.ink)
                    Spacer()
                    Image(systemName: "chevron.down").font(DS.iconFont(10)).foregroundStyle(DS.muted)
                }
                .padding(.horizontal, 10)
                .frame(height: DS.controlHeightDense)
                .background(DS.surfaceSubtle)
                .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
            }
            .menuStyle(.borderlessButton)
            .menuIndicator(.hidden)
            .disabled(!editable)
        } else if field.type == "array" || field.type == "object" {
            let rawText = change?.rawStructuredText ?? LightYAML.stringify(value)
            TextEditor(
                text: Binding(
                    get: { rawText },
                    set: { txt in
                        let parsed = LightYAML.parseStructured(txt, expectedType: field.type)
                        draft[field.path] = DraftChange(
                            path: field.path,
                            remove: false,
                            value: parsed.value,
                            rawStructuredText: txt
                        )
                    }
                )
            )
            .font(DS.fontMono11)
            .frame(height: 84)
            .padding(6)
            .background(DS.surfaceSubtle)
            .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
            .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
            .disabled(!editable)
        } else {
            let str = scalarString(value)
            let isSecret = field.sensitive && !revealedPaths.contains(field.path)
            HStack(spacing: 6) {
                if isSecret {
                    SecureField(
                        text: Binding(get: { str }, set: { setScalarText(field, $0) }),
                        prompt: Text(verbatim: field.default_text)
                    ) { EmptyView() }
                    .textFieldStyle(.plain)
                    .font(DS.fontMono12)
                } else {
                    TextField(
                        text: Binding(get: { str }, set: { setScalarText(field, $0) }),
                        prompt: Text(verbatim: field.default_text)
                    ) { EmptyView() }
                    .textFieldStyle(.plain)
                    .font(DS.fontMono12)
                }
                if field.sensitive {
                    LinearIconButton(
                        systemName: isSecret ? "eye" : "eye.slash",
                        label: isSecret ? "显示" : "隐藏",
                        size: 12
                    ) {
                        if isSecret { revealedPaths.insert(field.path) } else { revealedPaths.remove(field.path) }
                    }
                }
            }
            .padding(.horizontal, 10)
            .frame(height: DS.controlHeightDense)
            .background(DS.surfaceSubtle)
            .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
            .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
            .disabled(!editable)
        }
    }

    private var confirmApplyDialog: some View {
        ZStack {
            DS.backdrop.ignoresSafeArea().onTapGesture { if !busy { showConfirmApply = false } }
            VStack(alignment: .leading, spacing: 12) {
                HStack {
                    Text("确认写入配置并重启 Sub2API")
                        .font(DS.fontDialogTitle)
                        .foregroundStyle(DS.ink)
                    Spacer()
                    LinearIconButton(systemName: "xmark", label: "关闭", disabled: busy) { showConfirmApply = false }
                }
                Text("共 \(draft.count) 项变更通过校验，保存后将重启宿主机 Sub2API 进程：")
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.muted)
                ScrollView {
                    VStack(alignment: .leading, spacing: 4) {
                        ForEach(draft.keys.sorted(), id: \.self) { k in
                            let c = draft[k]!
                            Text(verbatim: "\(k): \(c.remove ? "<恢复默认>" : LightYAML.stringify(c.value))")
                                .font(DS.fontMono11)
                                .foregroundStyle(DS.ink)
                        }
                    }
                }
                .frame(maxHeight: 180)
                HStack {
                    Spacer()
                    Button("取消") { showConfirmApply = false }
                        .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                        .disabled(busy)
                    Button(busy ? "提交中…" : "确认写入并重启") {
                        Task { await applyChanges() }
                    }
                    .buttonStyle(LinearButtonStyle(variant: .primary, compact: true))
                    .disabled(busy)
                }
            }
            .padding(20)
            .frame(width: 500)
            .background(DS.surface)
            .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(DS.line, lineWidth: 1))
            .clipShape(RoundedRectangle(cornerRadius: 12))
            .sub2opsDialogFocus(onEscape: { if !busy { showConfirmApply = false } })
        }
    }

    private func scalarString(_ v: JSONValue?) -> String {
        guard let v else { return "" }
        switch v {
        case .string(let s): return s
        case .int(let i): return "\(i)"
        case .double(let d): return "\(d)"
        case .bool(let b): return b ? "true" : "false"
        default: return ""
        }
    }

    private func setScalarText(_ field: CatalogField, _ raw: String) {
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

    private func setFieldValue(_ field: CatalogField, _ next: JSONValue) {
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

    private func loadConfig(resetDraft: Bool) async {
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

    private func validateChanges(openConfirm: Bool) async {
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
                if openConfirm { showConfirmApply = true }
            }
        } catch {
            errorText = error.localizedDescription
        }
    }

    private func applyChanges() async {
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
            showConfirmApply = false
            messageText = ""
        } catch {
            showConfirmApply = false
            errorText = error.localizedDescription
            hostOp = HostOperation(status: "restarting", message: "提交结果待确认，正在查询操作状态")
        }
    }
}
