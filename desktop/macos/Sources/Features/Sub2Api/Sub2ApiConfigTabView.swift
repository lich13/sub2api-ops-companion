import AppKit
import SwiftUI

extension HostFieldIssue: Identifiable {
    public var id: String { "\(path):\(message)" }
}

/// Pure concurrency and uncertainty state machine matching `Sub2ApiConfig.tsx:480-560`.
public struct Sub2ApiConfigSession: Sendable, Equatable {
    public var document: Sub2ApiConfigTabView.ConfigDocumentDTO? = nil
    public var draft: [String: Sub2ApiConfigTabView.DraftChange] = [:]
    public var issues: [HostFieldIssue] = []
    public var errorText: String = ""
    public var messageText: String = ""
    public var hostOp: HostOperationReply? = nil
    public var applyUncertain: Bool = false
    public var conflicted: Bool = false

    public init() {}

    public static func isRunning(_ op: HostOperationReply?) -> Bool {
        guard let s = op?.status else { return false }
        return s == "saving" || s == "restarting" || s == "restoring"
    }

    public mutating func applyLoaded(_ next: Sub2ApiConfigTabView.ConfigDocumentDTO, resetDraft: Bool, protectDraft: Bool = true) {
        if resetDraft || draft.isEmpty {
            conflicted = false
        } else if protectDraft,
                  let prev = document,
                  next.available,
                  (prev.revision != next.revision || prev.runtime_revision != next.runtime_revision)
        {
            conflicted = true
        }
        if conflicted {
            var locked = next
            locked.editable = false
            locked.reason = "远程配置或运行态已变更，请放弃本地更改或核对后重新编辑"
            document = locked
        } else {
            document = next
        }
        if resetDraft {
            draft.removeAll()
            issues.removeAll()
        }
    }

    public mutating func handleLoadFailure(_ message: String) {
        errorText = message
        if var existing = document {
            existing.editable = false
            existing.reason = message
            document = existing
        }
    }

    public mutating func handleApplyFailure(_ message: String) {
        applyUncertain = true
        errorText = message
        hostOp = HostOperationReply(status: "restarting", message: "提交结果待确认，正在查询操作状态", updated_at: nil)
    }

    public enum PollResolution: Sendable, Equatable {
        case keepPolling
        case reloadKeepingDraft
        case reloadResettingDraft
    }

    public mutating func handlePolledOperation(_ nextOp: HostOperationReply) -> PollResolution {
        if !Self.isRunning(nextOp) && applyUncertain {
            let hostDesc = (nextOp.message?.isEmpty == false)
                ? nextOp.message!
                : (nextOp.status == "idle" ? "无操作记录" : nextOp.status)
            hostOp = HostOperationReply(
                status: "failed",
                message: "提交结果未确认；宿主机当前状态：\(hostDesc)。草稿已保留。",
                updated_at: nil
            )
            applyUncertain = false
            return .reloadKeepingDraft
        }
        hostOp = nextOp
        errorText = ""
        if !Self.isRunning(nextOp) {
            applyUncertain = false
            return nextOp.status == "succeeded" ? .reloadResettingDraft : .reloadKeepingDraft
        }
        return .keepPolling
    }

    public mutating func resetForConnection() {
        self = Sub2ApiConfigSession()
    }

    private static func containsPath(_ parent: String, _ candidate: String) -> Bool {
        parent == candidate || candidate.hasPrefix("\(parent).")
    }

    public mutating func removeField(_ path: String, configured: Bool) -> Bool {
        if configured {
            draft[path] = Sub2ApiConfigTabView.DraftChange(path: path, remove: true, value: nil)
            issues.removeAll { Self.containsPath(path, $0.path) }
            messageText = ""
            return false
        }
        return undoField(path)
    }

    /// Removes a single field's local draft and returns `true` if the last draft item was
    /// cleared while the document was locked by a conflict or load failure.
    public mutating func undoField(_ path: String) -> Bool {
        draft.removeValue(forKey: path)
        issues.removeAll { Self.containsPath(path, $0.path) }
        messageText = ""
        guard draft.isEmpty else { return false }
        let needsReload = conflicted || document?.editable == false || document == nil
        conflicted = false
        if needsReload {
            errorText = ""
        }
        return needsReload
    }

    /// Returns `true` if a reload is required to clear a remote conflict or load-failure lock.
    public mutating func discardAll() -> Bool {
        let needsReload = conflicted || document?.editable == false || document == nil
        draft.removeAll()
        issues.removeAll()
        messageText = ""
        conflicted = false
        if needsReload {
            errorText = ""
        }
        return needsReload
    }
}

/// Matches `Sub2ApiConfig` in `desktop/src/Sub2ApiConfig.tsx` (`features_sub2api_light.png`).
public struct Sub2ApiConfigTabView: View {
    public var store: AppStore?
    public let active: Bool
    public let online: Bool
    public let connectionKey: String

    public struct CatalogGroup: Codable, Sendable, Equatable, Identifiable {
        public var id: String
        public var label: String

        public init(id: String, label: String) {
            self.id = id
            self.label = label
        }
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
        public var nullable: Bool?
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

        public init(version: String = "", revision: String = "", groups: [CatalogGroup] = [], fields: [CatalogField] = []) {
            self.version = version
            self.revision = revision
            self.groups = groups
            self.fields = fields
        }
    }

    public typealias ConfigIssue = HostFieldIssue

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
        public var unknown: [HostFieldIssue]

        public init(
            available: Bool = true,
            editable: Bool = true,
            reason: String = "",
            version: String = "",
            commit: String = "",
            healthy: Bool = true,
            revision: String = "",
            runtime_revision: String = "",
            catalog: CatalogPayload = CatalogPayload(),
            values: [String: FieldRuntimeValue] = [:],
            unknown: [HostFieldIssue] = []
        ) {
            self.available = available
            self.editable = editable
            self.reason = reason
            self.version = version
            self.commit = commit
            self.healthy = healthy
            self.revision = revision
            self.runtime_revision = runtime_revision
            self.catalog = catalog
            self.values = values
            self.unknown = unknown
        }

        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            available = try c.decodeIfPresent(Bool.self, forKey: .available) ?? false
            editable = try c.decodeIfPresent(Bool.self, forKey: .editable) ?? false
            reason = try c.decodeIfPresent(String.self, forKey: .reason) ?? ""
            version = try c.decodeIfPresent(String.self, forKey: .version) ?? ""
            commit = try c.decodeIfPresent(String.self, forKey: .commit) ?? ""
            healthy = try c.decodeIfPresent(Bool.self, forKey: .healthy) ?? false
            revision = try c.decodeIfPresent(String.self, forKey: .revision) ?? ""
            runtime_revision = try c.decodeIfPresent(String.self, forKey: .runtime_revision) ?? ""
            catalog = try c.decodeIfPresent(CatalogPayload.self, forKey: .catalog) ?? CatalogPayload()
            values = try c.decodeIfPresent([String: FieldRuntimeValue].self, forKey: .values) ?? [:]
            unknown = try c.decodeIfPresent([HostFieldIssue].self, forKey: .unknown) ?? []
        }
    }

    public struct DraftChange: Sendable, Equatable {
        public var path: String
        public var remove: Bool
        public var value: JSONValue?
        public var rawStructuredText: String?
        public var rawScalarText: String?

        public init(
            path: String,
            remove: Bool,
            value: JSONValue? = nil,
            rawStructuredText: String? = nil,
            rawScalarText: String? = nil
        ) {
            self.path = path
            self.remove = remove
            self.value = value
            self.rawStructuredText = rawStructuredText
            self.rawScalarText = rawScalarText
        }

        public var jsonObject: JSONValue {
            var obj: [String: JSONValue] = [
                "path": .string(path),
                "remove": .bool(remove),
            ]
            if !remove, let value { obj["value"] = value }
            return .object(obj)
        }
    }

    @State var session = Sub2ApiConfigSession()
    @State var selectedGroup: String = "server"
    @State private var query: String = ""
    @State private var filterMode: String = "all"
    @State var busy: Bool = false
    @State var loadSeq: UInt64 = 0
    @State var boundConnectionKey: String = ""
    @State var statusLoadedKey: String = ""
    @State private var fallbackShowConfirm: Bool = false
    @State private var showSourceLegend: Bool = false
    @State private var expandedDescriptions: Set<String> = []
    @State private var revealedPaths: Set<String> = []

    func resetTransientViewState() {
        query = ""
        filterMode = "all"
        fallbackShowConfirm = false
        expandedDescriptions.removeAll()
        revealedPaths.removeAll()
    }

    var document: ConfigDocumentDTO? {
        get { session.document }
        nonmutating set { session.document = newValue }
    }
    var draft: [String: DraftChange] {
        get { session.draft }
        nonmutating set { session.draft = newValue }
    }
    var errorText: String {
        get { session.errorText }
        nonmutating set { session.errorText = newValue }
    }
    var messageText: String {
        get { session.messageText }
        nonmutating set { session.messageText = newValue }
    }
    var issues: [HostFieldIssue] {
        get { session.issues }
        nonmutating set { session.issues = newValue }
    }
    var hostOp: HostOperationReply? {
        get { session.hostOp }
        nonmutating set { session.hostOp = newValue }
    }

    public init(store: AppStore? = nil, active: Bool, online: Bool, connectionKey: String) {
        self.store = store
        self.active = active
        self.online = online
        self.connectionKey = connectionKey
    }

    var isOperationRunning: Bool {
        Sub2ApiConfigSession.isRunning(hostOp)
    }

    var isBlocked: Bool {
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
                    connectionKey: connectionKey,
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

                if let doc = document, doc.available {
                    HStack(alignment: .top, spacing: 0) {
                        groupSidebar(doc)
                        Rectangle().fill(DS.line).frame(width: 1)
                        fieldsPane(doc)
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else {
                    VStack {
                        Spacer()
                        let unavailableReason = document?.reason ?? ""
                        let fallbackMsg = !errorText.isEmpty ? errorText : (!unavailableReason.isEmpty ? unavailableReason : "暂无可用配置数据")
                        Text(busy ? "正在读取 Sub2API 配置目录…" : fallbackMsg)
                            .font(DS.fontCompact)
                            .foregroundStyle((errorText.isEmpty && unavailableReason.isEmpty) ? DS.muted : DS.bad)
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
        .onChange(of: connectionKey) { _, _ in
            bindConnectionIfNeeded()
        }
        .task(id: "\(connectionKey):\(active):\(online)") {
            bindConnectionIfNeeded()
            guard active, online else { return }
            let capturedKey = connectionKey
            if statusLoadedKey != capturedKey {
                do {
                    let op: HostOperationReply = try await RustBridge.shared.api("GET", "/sub2api-config/operation")
                    guard !Task.isCancelled, connectionKey == capturedKey else { return }
                    statusLoadedKey = capturedKey
                    hostOp = op
                } catch is CancellationError {
                    return
                } catch {
                    guard !Task.isCancelled, connectionKey == capturedKey else { return }
                    errorText = error.localizedDescription
                }
            }
            guard !Task.isCancelled, connectionKey == capturedKey else { return }
            if !isOperationRunning {
                await loadConfig(resetDraft: false, protectDraft: true)
            }
        }
        .task(id: "\(connectionKey):\(isOperationRunning)") {
            guard isOperationRunning else { return }
            let capturedKey = connectionKey
            while !Task.isCancelled && isOperationRunning && connectionKey == capturedKey {
                try? await Task.sleep(for: .milliseconds(1500))
                guard !Task.isCancelled, connectionKey == capturedKey else { return }
                do {
                    let nextOp: HostOperationReply = try await RustBridge.shared.api("GET", "/sub2api-config/operation")
                    guard !Task.isCancelled, connectionKey == capturedKey else { return }
                    let resolution = session.handlePolledOperation(nextOp)
                    switch resolution {
                    case .keepPolling:
                        break
                    case .reloadResettingDraft:
                        Task { await loadConfig(resetDraft: true, protectDraft: false) }
                        return
                    case .reloadKeepingDraft:
                        Task { await loadConfig(resetDraft: false, protectDraft: true) }
                        return
                    }
                } catch is CancellationError {
                    return
                } catch {
                    if !Task.isCancelled && connectionKey == capturedKey {
                        errorText = "\(error.localizedDescription)；应用结果待确认，请保持此页或稍后查询状态"
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
                Task { await loadConfig(resetDraft: true, protectDraft: false) }
            } label: {
                HStack(spacing: 5) {
                    Image(systemName: "arrow.clockwise")
                        .font(DS.iconFont(12, weight: .medium))
                    Text("刷新")
                }
            }
            .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
            .disabled(busy || isOperationRunning || !online || !draft.isEmpty)
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
                    if !groupFields.isEmpty {
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

    nonisolated static func effectiveFieldValue(
        field: CatalogField,
        runtime: FieldRuntimeValue?,
        change: DraftChange?
    ) -> JSONValue? {
        if let c = change {
            return c.remove ? field.default_value : c.value
        }
        if runtime?.environment != nil {
            return runtime?.effective ?? field.default_value
        }
        if runtime?.configured == true {
            return runtime?.yaml
        }
        return runtime?.effective ?? field.default_value
    }

    private func fieldRow(_ field: CatalogField, doc: ConfigDocumentDTO) -> some View {
        let runtime = doc.values[field.path]
        let change = draft[field.path]
        let configured = change != nil ? (change?.remove != true) : (runtime?.configured == true)
        let overridden = runtime?.environment != nil
        let fieldEditable = !isBlocked && field.editable && runtime?.editable != false && !overridden
        let effectiveVal = Self.effectiveFieldValue(field: field, runtime: runtime, change: change)
        let fieldIssues = issues.filter { $0.path == field.path }
        let summaryLine = field.description.components(separatedBy: CharacterSet(charactersIn: "。\n")).first ?? field.description
        let children = doc.catalog.fields.filter { c in
            c.internal && c.status == "internal"
                && (c.path == field.path || c.path.hasPrefix("\(field.path).") || c.path.hasPrefix("\(field.path)["))
        }
        let isNoteOpen = expandedDescriptions.contains(field.path)
        let sourceText: String = {
            if let envName = runtime?.environment { return "环境变量 \(envName)" }
            if field.status == "reserved" { return "保留字段 · 只读" }
            if !field.editable || runtime?.editable == false { return "只读" }
            if configured { return "config.yaml 已配置" }
            return field.has_default ? "使用默认值" : "未配置"
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
                        Text(children.isEmpty ? "完整说明" : "完整说明与内部字段（\(children.count)）")
                            .font(DS.fontResetTime)
                    }
                    .foregroundStyle(DS.muted)
                    .padding(.top, 2)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.linearPlain)

                if isNoteOpen {
                    VStack(alignment: .leading, spacing: 6) {
                        Text(field.description)
                            .font(DS.fontResetTime)
                            .foregroundStyle(DS.muted)
                            .fixedSize(horizontal: false, vertical: true)

                        Text("允许范围：\(field.range.isEmpty ? "官方未声明额外范围" : field.range)")
                            .font(DS.fontResetTime)
                            .foregroundStyle(DS.muted)

                        ForEach(children) { child in
                            VStack(alignment: .leading, spacing: 2) {
                                Text(verbatim: child.path)
                                    .font(DS.fontMono11)
                                    .foregroundStyle(DS.ink)
                                Text(child.description)
                                    .font(DS.fontResetTime)
                                    .foregroundStyle(DS.muted)
                                    .fixedSize(horizontal: false, vertical: true)
                                Text("\(child.type_label) · \(child.range)")
                                    .font(DS.fontUsageStat)
                                    .foregroundStyle(DS.muted)
                            }
                            .padding(6)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .background(DS.surface)
                            .clipShape(RoundedRectangle(cornerRadius: 4))
                        }

                        HStack(spacing: 12) {
                            if let url = URL(string: field.source_url), !field.source_url.isEmpty {
                                Button {
                                    NSWorkspace.shared.open(url)
                                } label: {
                                    Text("官方源码")
                                        .font(DS.fontResetTimeSemibold)
                                        .foregroundStyle(DS.accent)
                                        .contentShape(Rectangle())
                                }
                                .buttonStyle(.linearPlain)
                            }
                            if fieldEditable, !configured, field.has_default, let defVal = field.default_value {
                                Button("写入默认值") {
                                    setFieldValue(field, defVal, explicit: true)
                                }
                                .buttonStyle(LinearButtonStyle(variant: .ghost, compact: true))
                            }
                        }
                    }
                    .padding(8)
                    .background(DS.surfaceSubtle)
                    .clipShape(RoundedRectangle(cornerRadius: 6))
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
                                let shouldReload = session.removeField(field.path, configured: runtime?.configured == true)
                                if shouldReload {
                                    Task { await loadConfig(resetDraft: true, protectDraft: false) }
                                }
                            }
                            .buttonStyle(LinearButtonStyle(variant: .ghost, compact: true))
                        }
                        if change != nil {
                            Button("撤销修改") {
                                let shouldReload = session.undoField(field.path)
                                if shouldReload {
                                    Task { await loadConfig(resetDraft: true, protectDraft: false) }
                                }
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
            let str = change?.rawScalarText ?? scalarString(value)
            let isSecret = field.sensitive && !revealedPaths.contains(field.path)
            let promptText: String = if value == nil {
                "未配置"
            } else if field.default_text == "\"\"" || field.default_text.isEmpty {
                "空字符串"
            } else {
                field.default_text
            }
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
                    let shouldReload = session.discardAll()
                    if shouldReload {
                        Task { await loadConfig(resetDraft: true, protectDraft: false) }
                    }
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

    func presentConfirmApply() {
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

    func dismissConfirmApply() {
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
}
