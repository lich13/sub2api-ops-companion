import SwiftUI

/// Matches `Sub2ApiConfig` in `desktop/src/Sub2ApiConfig.tsx` (`features_sub2api_light.png`).
public struct Sub2ApiConfigTabView: View {
    public var store: AppStore?
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
    @State private var fallbackShowConfirm: Bool = false
    @State private var showSourceLegend: Bool = false
    @State private var expandedDescriptions: Set<String> = []
    @State private var hostOp: HostOperation? = nil
    @State private var revealedPaths: Set<String> = []

    public init(store: AppStore? = nil, active: Bool, online: Bool, connectionKey: String) {
        self.store = store
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
            VStack(alignment: .leading, spacing: 0) {
                headerStrip
                    .padding(.horizontal, 18)
                    .padding(.vertical, 12)

                Rectangle().fill(DS.line).frame(height: 1)

                Sub2ApiVersionCardView(
                    active: active,
                    online: online,
                    knownVersion: document?.version ?? "",
                    embedded: true
                ) {
                    Task { await loadConfig(resetDraft: false) }
                }
                .padding(.horizontal, 18)
                .padding(.vertical, 10)

                Rectangle().fill(DS.line).frame(height: 1)

                searchAndFilterBar
                    .padding(.horizontal, 18)
                    .padding(.vertical, 10)

                if let doc = document, !doc.editable, !doc.reason.isEmpty {
                    Rectangle().fill(DS.line).frame(height: 1)
                    Text(doc.reason)
                        .font(DS.fontCompactMedium)
                        .foregroundStyle(DS.warn)
                        .padding(.horizontal, 18)
                        .padding(.vertical, 8)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(DS.warnBg)
                }

                Rectangle().fill(DS.line).frame(height: 1)

                if let doc = document {
                    HStack(alignment: .top, spacing: 0) {
                        groupSidebar(doc)
                        Rectangle().fill(DS.line).frame(width: 1)
                        fieldsPane(doc)
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else {
                    VStack {
                        Spacer()
                        Text(busy ? "正在读取 Sub2API 配置目录…" : (errorText.isEmpty ? "暂无可用配置数据" : errorText))
                            .font(DS.fontCompact)
                            .foregroundStyle(errorText.isEmpty ? DS.muted : DS.bad)
                        Spacer()
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                }

                Rectangle().fill(DS.line).frame(height: 1)

                bottomActionBar
                    .padding(.horizontal, 18)
                    .padding(.vertical, 12)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .background(DS.surface)
            .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(DS.line, lineWidth: 1))
            .clipShape(RoundedRectangle(cornerRadius: 12))

            if fallbackShowConfirm {
                ZStack {
                    DS.backdrop.ignoresSafeArea().onTapGesture {
                        if !busy { fallbackShowConfirm = false }
                    }
                    confirmApplyCard(dismiss: { fallbackShowConfirm = false })
                }
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

    private var headerStrip: some View {
        HStack(spacing: 10) {
            Text("Sub2API 配置")
                .font(DS.fontDialogTitle)
                .foregroundStyle(DS.ink)

            if let doc = document {
                if !doc.commit.isEmpty {
                    Text(verbatim: String(doc.commit.prefix(8)))
                        .font(DS.fontMono12)
                        .foregroundStyle(DS.ink)
                }
                Text(doc.healthy ? "服务健康" : "服务异常")
                    .font(DS.fontCompactMedium)
                    .foregroundStyle(doc.healthy ? DS.good : DS.bad)
                if !doc.catalog.version.isEmpty {
                    Text("说明 \(doc.catalog.version)")
                        .font(DS.fontCompact)
                        .monospacedDigit()
                        .foregroundStyle(DS.muted)
                }
            }

            Spacer()

            Button {
                Task { await loadConfig(resetDraft: false) }
            } label: {
                HStack(spacing: 5) {
                    Image(systemName: "arrow.clockwise")
                        .font(DS.iconFont(12, weight: .medium))
                    Text("刷新")
                }
            }
            .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
            .disabled(busy || !online)
        }
    }

    private var searchAndFilterBar: some View {
        let filterLabel: String = {
            switch filterMode {
            case "configured": return "已配置"
            case "changed": return draft.isEmpty ? "已修改" : "已修改 (\(draft.count))"
            default: return "全部字段"
            }
        }()
        return HStack(spacing: 10) {
            LinearSearchField(
                text: $query,
                prompt: "搜索字段、路径或中文含义",
                height: DS.controlHeight
            )

            LinearSelect(
                label: filterLabel,
                accessibilityTitle: "字段筛选",
                height: DS.controlHeight,
                minWidth: 124,
                entries: [
                    LinearMenuEntry(id: "all", title: "全部字段", checked: filterMode == "all") { filterMode = "all" },
                    LinearMenuEntry(id: "configured", title: "已配置", checked: filterMode == "configured") { filterMode = "configured" },
                    LinearMenuEntry(id: "changed", title: "已修改 (\(draft.count))", checked: filterMode == "changed") { filterMode = "changed" },
                ]
            )
        }
    }

    private func groupSidebar(_ doc: ConfigDocumentDTO) -> some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 3) {
                ForEach(doc.catalog.groups) { g in
                    let groupFields = doc.catalog.fields.filter { !$0.internal && $0.group == g.id }
                    let changedCount = groupFields.filter { draft[$0.path] != nil }.count
                    let isSelected = query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && selectedGroup == g.id
                    Button {
                        selectedGroup = g.id
                        query = ""
                    } label: {
                        HStack(spacing: 6) {
                            Text(g.label)
                                .font(isSelected ? DS.fontCompactSemibold : DS.fontCompact)
                                .foregroundStyle(isSelected ? DS.accent : DS.ink)
                                .lineLimit(1)
                            Spacer(minLength: 4)
                            if changedCount > 0 {
                                Circle()
                                    .fill(DS.accent)
                                    .frame(width: 5, height: 5)
                            }
                            Text("\(groupFields.count)")
                                .font(DS.fontResetTime)
                                .monospacedDigit()
                                .foregroundStyle(isSelected ? DS.accent : DS.muted)
                        }
                        .padding(.horizontal, 12)
                        .padding(.vertical, 8)
                        .background(isSelected ? DS.accentBg : DS.clear)
                        .clipShape(RoundedRectangle(cornerRadius: 7))
                    }
                    .buttonStyle(.linearPlain)
                }
            }
            .padding(10)
        }
        .frame(width: 218)
        .frame(maxHeight: .infinity)
    }

    private func fieldsPane(_ doc: ConfigDocumentDTO) -> some View {
        let activeGroupLabel = doc.catalog.groups.first(where: { $0.id == selectedGroup })?.label ?? selectedGroup
        let isSearching = !query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty

        return ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                VStack(alignment: .leading, spacing: 8) {
                    HStack {
                        Text("\(visibleFields.count) 个字段")
                            .font(DS.fontResetTime)
                            .monospacedDigit()
                            .foregroundStyle(DS.muted)
                        Spacer()
                        Button {
                            showSourceLegend.toggle()
                        } label: {
                            HStack(spacing: 4) {
                                Image(systemName: showSourceLegend ? "arrowtriangle.down.fill" : "arrowtriangle.right.fill")
                                    .font(DS.iconFont(8))
                                Text("配置值来源")
                                    .font(DS.fontResetTime)
                            }
                            .foregroundStyle(DS.muted)
                        }
                        .buttonStyle(.linearPlain)
                    }

                    if showSourceLegend {
                        Text("优先级：宿主机环境变量 (ENV) > config.yaml 显式配置 > Sub2API 内置默认值。被 ENV 覆盖的项只读。")
                            .font(DS.fontResetTime)
                            .foregroundStyle(DS.muted)
                            .padding(8)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .background(DS.surfaceSubtle)
                            .clipShape(RoundedRectangle(cornerRadius: 6))
                    }

                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        Text(isSearching ? "搜索结果" : activeGroupLabel)
                            .font(DS.fontSectionTitle)
                            .foregroundStyle(DS.ink)
                        Text("\(visibleFields.count)")
                            .font(DS.fontCompact)
                            .monospacedDigit()
                            .foregroundStyle(DS.muted)
                    }
                }
                .padding(.horizontal, 20)
                .padding(.vertical, 14)

                Rectangle().fill(DS.line).frame(height: 1)

                if visibleFields.isEmpty {
                    Text("当前筛选下没有匹配的配置字段")
                        .font(DS.fontCompact)
                        .foregroundStyle(DS.muted)
                        .padding(24)
                } else {
                    ForEach(Array(visibleFields.enumerated()), id: \.element.id) { idx, field in
                        fieldRow(field, doc: doc)
                        if idx < visibleFields.count - 1 {
                            Rectangle().fill(DS.line).frame(height: 1)
                        }
                    }
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private func fieldRow(_ field: CatalogField, doc: ConfigDocumentDTO) -> some View {
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
        let summaryLine = field.description.components(separatedBy: CharacterSet(charactersIn: "。\n")).first ?? field.description
        let hasExtraNote = field.description.trimmingCharacters(in: .whitespacesAndNewlines).count > summaryLine.count + 1
        let isNoteOpen = expandedDescriptions.contains(field.path)
        let sourceText: String = {
            if let envName = runtime?.environment { return "环境变量 \(envName)" }
            if runtime?.configured == true { return "config.yaml 已配置" }
            return "使用默认值"
        }()

        return HStack(alignment: .top, spacing: 24) {
            VStack(alignment: .leading, spacing: 6) {
                Text(summaryLine)
                    .font(DS.fontBodySemibold)
                    .foregroundStyle(DS.ink)

                Text(verbatim: field.path)
                    .font(DS.fontMono12)
                    .foregroundStyle(DS.muted)
                    .textSelection(.enabled)

                HStack(spacing: 10) {
                    Text(field.type_label)
                        .foregroundStyle(DS.muted)
                    Text(sourceText)
                        .foregroundStyle(overridden ? DS.warn : DS.muted)
                    if let change {
                        Text(change.remove ? "待恢复默认" : "已修改")
                            .font(DS.fontResetTimeSemibold)
                            .foregroundStyle(DS.accent)
                    }
                }
                .font(DS.fontResetTime)

                if hasExtraNote {
                    Button {
                        if isNoteOpen {
                            expandedDescriptions.remove(field.path)
                        } else {
                            expandedDescriptions.insert(field.path)
                        }
                    } label: {
                        HStack(spacing: 4) {
                            Image(systemName: isNoteOpen ? "chevron.down" : "chevron.right")
                                .font(DS.iconFont(9, weight: .semibold))
                            Text("完整说明")
                                .font(DS.fontResetTime)
                        }
                        .foregroundStyle(DS.muted)
                        .padding(.top, 2)
                    }
                    .buttonStyle(.linearPlain)

                    if isNoteOpen {
                        Text(field.description)
                            .font(DS.fontResetTime)
                            .foregroundStyle(DS.muted)
                            .fixedSize(horizontal: false, vertical: true)
                            .padding(8)
                            .background(DS.surfaceSubtle)
                            .clipShape(RoundedRectangle(cornerRadius: 6))
                    }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)

            VStack(alignment: .leading, spacing: 6) {
                fieldInputControl(field: field, value: effectiveVal, change: change, editable: fieldEditable)

                VStack(alignment: .leading, spacing: 3) {
                    Text("默认: \(field.default_text.isEmpty ? "\"\"" : field.default_text)")
                    Text("范围: \(field.range.isEmpty ? "官方未声明额外范围" : field.range)")
                }
                .font(DS.fontResetTime)
                .foregroundStyle(DS.muted)

                if (runtime?.configured == true && fieldEditable) || change != nil {
                    HStack(spacing: 8) {
                        if runtime?.configured == true && fieldEditable && change?.remove != true {
                            Button("恢复默认") {
                                draft[field.path] = DraftChange(path: field.path, remove: true, value: nil)
                            }
                            .buttonStyle(LinearButtonStyle(variant: .ghost, compact: true))
                        }
                        if change != nil {
                            Button("撤销修改") {
                                draft.removeValue(forKey: field.path)
                            }
                            .buttonStyle(LinearButtonStyle(variant: .ghost, compact: true))
                        }
                    }
                }

                ForEach(fieldIssues) { iss in
                    Text(iss.message).font(DS.fontResetTime).foregroundStyle(DS.bad)
                }
            }
            .frame(width: 320, alignment: .leading)
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 16)
        .background(change != nil ? DS.accentBg.opacity(0.28) : DS.clear)
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
            HStack(spacing: 8) {
                LinearSwitch(isOn: b, disabled: !editable, label: field.path) {
                    setFieldValue(field, .bool(!b))
                }
                Text(b ? "开启" : "关闭")
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.muted)
            }
            .frame(height: 28)
        } else if let choices = field.choices, !choices.isEmpty {
            let cur = value?.stringValue ?? ""
            LinearSelect(
                label: cur.isEmpty ? "未设置" : cur,
                accessibilityTitle: field.path,
                height: DS.controlHeight,
                expandWidth: true,
                monospaced: true,
                mutedLabel: cur.isEmpty,
                disabled: !editable,
                entries: choices.map { opt in
                    LinearMenuEntry(id: opt, title: opt, checked: cur == opt) {
                        setFieldValue(field, .string(opt))
                    }
                }
            )
        } else if field.type == "array" || field.type == "object" {
            let rawText = change?.rawStructuredText ?? LightYAML.stringify(value)
            ConfigStructuredEditorView(textValue: rawText, editable: editable) { txt in
                let parsed = LightYAML.parseStructured(txt, expectedType: field.type)
                draft[field.path] = DraftChange(
                    path: field.path,
                    remove: false,
                    value: parsed.value,
                    rawStructuredText: txt
                )
            }
        } else {
            let str = scalarString(value)
            let isSecret = field.sensitive && !revealedPaths.contains(field.path)
            let promptText = field.default_text == "\"\"" || field.default_text.isEmpty ? "空字符串" : field.default_text
            ConfigScalarEditorView(
                textValue: str,
                prompt: promptText,
                isSecret: isSecret,
                isSensitive: field.sensitive,
                editable: editable,
                onToggleSecret: {
                    if isSecret { revealedPaths.insert(field.path) } else { revealedPaths.remove(field.path) }
                },
                onTextChange: { setScalarText(field, $0) }
            )
        }
    }

    private var bottomActionBar: some View {
        HStack(spacing: 10) {
            if let op = hostOp, !op.status.isEmpty, op.status != "idle" {
                Text("宿主机操作 (\(op.status))：\(op.message ?? "正在执行")")
                    .font(DS.fontCompactMedium)
                    .foregroundStyle(op.status == "failed" ? DS.bad : DS.accent)
            } else if !errorText.isEmpty {
                Text(errorText)
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.bad)
                    .lineLimit(1)
            } else if !messageText.isEmpty {
                Text(messageText)
                    .font(DS.fontCompactMedium)
                    .foregroundStyle(DS.good)
            } else {
                Text(draft.isEmpty ? "没有待应用的修改" : "已修改 \(draft.count) 项")
                    .font(DS.fontCompact)
                    .monospacedDigit()
                    .foregroundStyle(draft.isEmpty ? DS.muted : DS.accent)
            }

            if !draft.isEmpty {
                Button("撤销全部") {
                    draft.removeAll()
                    issues.removeAll()
                    messageText = ""
                }
                .buttonStyle(LinearButtonStyle(variant: .ghost, compact: true))
                .disabled(busy || isOperationRunning)
            }

            Spacer()

            Button("检查配置") {
                Task { await validateChanges(openConfirm: false) }
            }
            .buttonStyle(LinearButtonStyle(variant: .secondary))
            .disabled(isBlocked || draft.isEmpty)

            Button("应用并重启") {
                Task { await validateChanges(openConfirm: true) }
            }
            .buttonStyle(LinearButtonStyle(variant: .primary))
            .disabled(isBlocked || draft.isEmpty)
        }
    }

    private func presentConfirmApply() {
        if let store {
            store.presentWindowModal(onBackdrop: {
                if !busy { store.dismissWindowModal() }
            }) {
                confirmApplyCard(dismiss: { store.dismissWindowModal() })
            }
        } else {
            fallbackShowConfirm = true
        }
    }

    private func dismissConfirmApply() {
        if let store {
            store.dismissWindowModal()
        } else {
            fallbackShowConfirm = false
        }
    }

    private func confirmApplyCard(dismiss: @escaping () -> Void) -> some View {
        LinearModalCard(
            title: "确认写入配置并重启 Sub2API",
            subtitle: "共 \(draft.count) 项变更通过校验，保存后将重启宿主机 Sub2API 进程",
            width: 520,
            closeDisabled: busy,
            onClose: dismiss
        ) {
            LinearBoundedScrollView(maxHeight: 190) {
                VStack(alignment: .leading, spacing: 5) {
                    ForEach(draft.keys.sorted(), id: \.self) { k in
                        if let c = draft[k] {
                            Text(verbatim: "\(k): \(c.remove ? "<恢复默认>" : LightYAML.stringify(c.value))")
                                .font(DS.fontMono11)
                                .foregroundStyle(DS.ink)
                        }
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .padding(10)
            .background(DS.surfaceSubtle)
            .clipShape(RoundedRectangle(cornerRadius: 6))
        } footer: {
            Spacer()
            Button("取消", action: dismiss)
                .buttonStyle(LinearButtonStyle(variant: .secondary))
                .disabled(busy)
            Button(busy ? "提交中…" : "确认写入并重启") {
                Task { await applyChanges() }
            }
            .buttonStyle(LinearButtonStyle(variant: .primary))
            .disabled(busy)
        }
        .sub2opsDialogFocus(onEscape: { if !busy { dismiss() } })
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
                if openConfirm { presentConfirmApply() }
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
            dismissConfirmApply()
            messageText = ""
        } catch {
            dismissConfirmApply()
            errorText = error.localizedDescription
            hostOp = HostOperation(status: "restarting", message: "提交结果待确认，正在查询操作状态")
        }
    }
}

private struct ConfigStructuredEditorView: View {
    let textValue: String
    let editable: Bool
    let onTextChange: (String) -> Void
    @State private var text: String

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

private struct ConfigScalarEditorView: View {
    let textValue: String
    let prompt: String
    let isSecret: Bool
    let isSensitive: Bool
    let editable: Bool
    let onToggleSecret: () -> Void
    let onTextChange: (String) -> Void
    @State private var text: String

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
