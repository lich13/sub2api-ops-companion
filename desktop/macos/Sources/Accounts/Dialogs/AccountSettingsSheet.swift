import SwiftUI

public enum AccountSettingsTab: String, CaseIterable, Sendable {
    case whitelist
    case mapping
    case proxy

    public var label: String {
        switch self {
        case .whitelist: return "模型白名单"
        case .mapping: return "模型映射"
        case .proxy: return "选择代理"
        }
    }

    public var heading: String {
        switch self {
        case .whitelist, .mapping: return "模型配置"
        case .proxy: return "选择代理"
        }
    }
}

public struct AccountModelMappingRule: Codable, Sendable, Equatable {
    public var source: String
    public var target: String

    public init(source: String = "", target: String = "") {
        self.source = source
        self.target = target
    }
}

public struct AccountModelProfile: Codable, Sendable, Equatable {
    public var whitelist: [String]
    public var mappings: [AccountModelMappingRule]

    public init(whitelist: [String] = [], mappings: [AccountModelMappingRule] = []) {
        self.whitelist = whitelist
        self.mappings = mappings
    }

    public init(_ reply: ModelProfileReply) {
        self.whitelist = reply.whitelist
        self.mappings = reply.mappings.map { AccountModelMappingRule(source: $0.source, target: $0.target) }
    }

    public static func validate(_ profile: AccountModelProfile) -> String {
        var seen = Set<String>()
        let exactRegex = try? NSRegularExpression(pattern: "^[A-Za-z0-9._:/-]{1,200}$")
        let sourceRegex = try? NSRegularExpression(pattern: "^(?:[A-Za-z0-9._:/-]{1,199}\\*?|\\*)$")

        func matches(_ text: String, _ regex: NSRegularExpression?) -> Bool {
            guard let regex else { return !text.isEmpty }
            let range = NSRange(text.startIndex..<text.endIndex, in: text)
            return regex.firstMatch(in: text, range: range) != nil
        }

        for model in profile.whitelist {
            if !matches(model, exactRegex) {
                return "白名单必须是精确模型 ID"
            }
            if !seen.insert(model).inserted {
                return "重复的模型：\(model)"
            }
        }
        for rule in profile.mappings {
            if !matches(rule.source, sourceRegex) || !matches(rule.target, exactRegex) {
                return "映射源只允许末尾通配符，目标必须是精确模型 ID"
            }
            if seen.contains(rule.source) || rule.source == rule.target {
                return "模型条目冲突：\(rule.source)"
            }
            seen.insert(rule.source)
        }
        return ""
    }

    public var policySummary: String {
        if whitelist.isEmpty && mappings.isEmpty {
            return "不限制模型"
        }
        let items = whitelist + mappings.map { "\($0.source) → \($0.target)" }
        return items.joined(separator: "、")
    }
}

private struct SyncedModelCatalog: Sendable, Equatable {
    var models: [String]
    var warnings: [String]
    var synced_at: String?
}

public struct AccountSettingsSheet: View {
    public let account: Account
    @Bindable public var store: AppStore
    public let initialTab: AccountSettingsTab
    public let onClose: () -> Void

    @State private var tab: AccountSettingsTab
    @State private var settings: AccountSettingsReply?
    @State private var profile = AccountModelProfile()
    @State private var selectedProxyId: Int? = nil
    @State private var proxyRefreshNonce: Int = 0
    @State private var catalog: SyncedModelCatalog?
    @State private var query: String = ""
    @State private var selectedCatalogModels: Set<String> = []
    @State private var loading: Bool = true
    @State private var saving: Bool = false
    @State private var syncing: Bool = false
    @State private var confirmDiscard: Bool = false
    @State private var error: String = ""
    @State private var notice: String = ""

    public init(account: Account, store: AppStore, initialTab: AccountSettingsTab, onClose: @escaping () -> Void) {
        self.account = account
        self.store = store
        self.initialTab = initialTab
        self.onClose = onClose
        self._tab = State(initialValue: initialTab)
    }

    private var isProxyMode: Bool {
        initialTab == .proxy || tab == .proxy
    }

    private var currentAction: String {
        isProxyMode ? "proxy" : "model_config"
    }

    private var writable: Bool {
        settings?.capabilities[currentAction]?.editable ?? false
    }

    private var isDirty: Bool {
        guard let settings else { return false }
        if isProxyMode {
            return selectedProxyId != settings.proxy_id
        }
        return profile != AccountModelProfile(settings.model_config)
    }

    private var validationMessage: String {
        isProxyMode ? "" : AccountModelProfile.validate(profile)
    }

    public var body: some View {
        ZStack {
            LinearModalCard(
                title: isProxyMode ? "选择代理" : "模型配置",
                subtitle: "\(account.name) · #\(account.id)",
                width: 560,
                closeDisabled: saving,
                onClose: { requestClose(fromEscape: false) }
            ) {
                if !isProxyMode {
                    LinearUnderlineTabs(
                        items: [
                            LinearTabItem(id: AccountSettingsTab.whitelist, label: "模型白名单"),
                            LinearTabItem(id: AccountSettingsTab.mapping, label: "模型映射"),
                        ],
                        selection: tab,
                        onSelect: { next in
                            tab = next
                            query = ""
                        }
                    )
                }

                if loading {
                    HStack(spacing: 8) {
                        ProgressView().controlSize(.small)
                        Text("正在读取账号设置…")
                            .font(DS.fontCompact)
                            .foregroundStyle(DS.muted)
                    }
                    .padding(.vertical, 20)
                } else if settings == nil {
                    HStack {
                        Text(error.isEmpty ? "无法读取账号设置" : error)
                            .font(DS.fontCompact)
                            .foregroundStyle(DS.bad)
                        Spacer()
                        Button("重新读取") {
                            Task { await load() }
                        }
                        .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                    }
                } else {
                    if !writable, let reason = settings?.capabilities[currentAction]?.reason, !reason.isEmpty {
                        Text(reason)
                            .font(DS.fontCompact)
                            .foregroundStyle(DS.warn)
                            .padding(.horizontal, 10)
                            .padding(.vertical, 8)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .background(DS.warnBg)
                            .clipShape(RoundedRectangle(cornerRadius: 6))
                    }

                    if isProxyMode {
                        HStack {
                            Text("配置此账号发起上游请求时使用的出口代理")
                                .font(DS.fontCompact)
                                .foregroundStyle(DS.muted)
                            Spacer()
                            Button {
                                error = ""
                                proxyRefreshNonce &+= 1
                            } label: {
                                Label("刷新代理", systemImage: "arrow.clockwise")
                            }
                            .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                            .disabled(saving || !store.state.online)
                        }

                        ProxyOptionsPickerView(
                            selectedProxyId: $selectedProxyId,
                            online: store.state.online,
                            connectionKey: "\(store.connectionKey):\(proxyRefreshNonce)"
                        )
                        .disabled(!writable || saving)
                    } else {
                        if profile.whitelist.isEmpty && profile.mappings.isEmpty {
                            Text("当前不限制模型（上游支持的全部模型均可调度）")
                                .font(DS.fontCompact)
                                .foregroundStyle(DS.muted)
                                .padding(.horizontal, 10)
                                .padding(.vertical, 7)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .background(DS.surfaceSubtle)
                                .clipShape(RoundedRectangle(cornerRadius: 6))
                        }

                        if tab == .whitelist {
                            whitelistEditor
                        } else {
                            mappingEditor
                        }
                    }
                }

                let alertText = !error.isEmpty ? error : validationMessage
                if !alertText.isEmpty {
                    Text(alertText)
                        .font(DS.fontCompact)
                        .foregroundStyle(DS.bad)
                }
                if !notice.isEmpty {
                    Text(notice)
                        .font(DS.fontCompact)
                        .foregroundStyle(DS.muted)
                }
            } footer: {
                Button("放弃修改") {
                    if let settings {
                        profile = AccountModelProfile(settings.model_config)
                        selectedProxyId = settings.proxy_id
                        error = ""
                        notice = ""
                    }
                }
                .buttonStyle(LinearButtonStyle(variant: .secondary))
                .disabled(!isDirty || saving)

                Spacer()

                Button("取消") {
                    requestClose(fromEscape: false)
                }
                .buttonStyle(LinearButtonStyle(variant: .secondary))
                .disabled(saving)

                Button {
                    Task { await save() }
                } label: {
                    Label(saving ? "保存中…" : "保存", systemImage: "checkmark")
                }
                .buttonStyle(LinearButtonStyle(variant: .primary))
                .disabled(!store.state.online || loading || saving || !writable || !isDirty || !validationMessage.isEmpty)
            }
            .sub2opsDialogFocus {
                requestClose(fromEscape: true)
            }

            if confirmDiscard {
                discardConfirmCard
            }
        }
        .task(id: account.id) {
            await load()
        }
    }

    private var whitelistEditor: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                Text("放行模型")
                    .font(DS.fontCompactSemibold)
                    .foregroundStyle(DS.ink)
                Spacer()
                Button {
                    profile.whitelist.append("")
                } label: {
                    Label("添加模型", systemImage: "plus")
                }
                .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                .disabled(!writable || saving)

                let canSync = settings?.capabilities["sync_models"]?.editable ?? false
                Button {
                    Task { await syncModels() }
                } label: {
                    Label(syncing ? "同步中…" : "同步上游模型", systemImage: "arrow.triangle.2.circlepath")
                }
                .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                .disabled(!store.state.online || saving || syncing || !canSync)
            }

            if !profile.whitelist.isEmpty {
                LinearBoundedScrollView(maxHeight: 190) {
                    VStack(spacing: 6) {
                        ForEach(Array(profile.whitelist.enumerated()), id: \.offset) { idx, current in
                            HStack(spacing: 6) {
                                TrimmedEntryField(
                                    value: current,
                                    prompt: "白名单模型 \(idx + 1)",
                                    font: DS.fontCompact,
                                    horizontalPadding: 10,
                                    height: DS.controlHeightCompact,
                                    cornerRadius: 6,
                                    disabled: !writable || saving
                                ) { trimmed in
                                    if profile.whitelist.indices.contains(idx) {
                                        profile.whitelist[idx] = trimmed
                                    }
                                }

                                LinearIconButton(
                                    systemName: "xmark",
                                    label: "移除白名单 \(idx + 1)",
                                    size: 12,
                                    boxSize: 28,
                                    disabled: !writable || saving
                                ) {
                                    if profile.whitelist.indices.contains(idx) {
                                        profile.whitelist.remove(at: idx)
                                    }
                                }
                            }
                        }
                    }
                }
            }

            if let catalog {
                syncedCatalogSection(catalog)
            }
        }
    }

    private func syncedCatalogSection(_ catalog: SyncedModelCatalog) -> some View {
        let present = Set(profile.whitelist)
        let sourceSet = Set(profile.mappings.map(\.source))
        let q = query.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        let candidates = catalog.models.filter { q.isEmpty || $0.lowercased().contains(q) }
        let missing = candidates.filter { !present.contains($0) && !sourceSet.contains($0) }

        return VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                LinearSearchField(text: $query, prompt: "搜索支持模型", height: 30, compactFont: true)
                Text("\(catalog.models.count) 个模型")
                    .font(DS.fontPriorityMeta)
                    .monospacedDigit()
                    .foregroundStyle(DS.muted)
                Button("全选缺少项") {
                    selectedCatalogModels.formUnion(missing)
                }
                .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                .disabled(saving || !writable || missing.isEmpty)

                Button("添加所选") {
                    let additions = catalog.models.filter {
                        selectedCatalogModels.contains($0) && !present.contains($0) && !sourceSet.contains($0)
                    }
                    if profile.whitelist.count + additions.count > 300 {
                        error = "白名单最多 300 项"
                        return
                    }
                    profile.whitelist.append(contentsOf: additions)
                    selectedCatalogModels.removeAll()
                }
                .buttonStyle(LinearButtonStyle(variant: .primary, compact: true))
                .disabled(saving || !writable || selectedCatalogModels.isEmpty)
            }

            ForEach(catalog.warnings, id: \.self) { w in
                Text(w).font(DS.fontResetTime).foregroundStyle(DS.muted)
            }

            LinearBoundedScrollView(maxHeight: 130) {
                VStack(alignment: .leading, spacing: 4) {
                    ForEach(candidates, id: \.self) { model in
                        let conflict = sourceSet.contains(model)
                        let added = present.contains(model)
                        HStack(spacing: 8) {
                            LinearCheckbox(
                                isChecked: selectedCatalogModels.contains(model),
                                label: model,
                                disabled: saving || !writable || conflict || added
                            ) {
                                if selectedCatalogModels.contains(model) {
                                    selectedCatalogModels.remove(model)
                                } else {
                                    selectedCatalogModels.insert(model)
                                }
                            }
                            Spacer()
                            if conflict || added {
                                Text(conflict ? "与映射源冲突" : "已添加")
                                    .font(DS.fontResetTime)
                                    .foregroundStyle(DS.muted)
                            }
                        }
                    }
                }
            }
        }
        .padding(10)
        .background(DS.surfaceSubtle)
        .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(DS.line, lineWidth: 1))
        .clipShape(RoundedRectangle(cornerRadius: 8))
    }

    private var mappingEditor: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text("请求模型 → 转发模型（源末尾支持 * 通配符）")
                    .font(DS.fontCompactSemibold)
                    .foregroundStyle(DS.ink)
                Spacer()
                Button {
                    profile.mappings.append(AccountModelMappingRule())
                } label: {
                    Label("添加映射", systemImage: "plus")
                }
                .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                .disabled(!writable || saving)
            }

            if !profile.mappings.isEmpty {
                LinearBoundedScrollView(maxHeight: 220) {
                    VStack(spacing: 6) {
                        ForEach(Array(profile.mappings.enumerated()), id: \.offset) { idx, rule in
                            HStack(spacing: 6) {
                                TrimmedEntryField(
                                    value: rule.source,
                                    prompt: "请求模型",
                                    font: DS.fontCompact,
                                    horizontalPadding: 10,
                                    height: DS.controlHeightCompact,
                                    cornerRadius: 6,
                                    disabled: !writable || saving
                                ) { trimmed in
                                    if profile.mappings.indices.contains(idx) {
                                        profile.mappings[idx].source = trimmed
                                    }
                                }

                                Image(systemName: "arrow.right")
                                    .font(DS.iconFont(11, weight: .semibold))
                                    .foregroundStyle(DS.muted)

                                TrimmedEntryField(
                                    value: rule.target,
                                    prompt: "转发模型",
                                    font: DS.fontCompact,
                                    horizontalPadding: 10,
                                    height: DS.controlHeightCompact,
                                    cornerRadius: 6,
                                    disabled: !writable || saving
                                ) { trimmed in
                                    if profile.mappings.indices.contains(idx) {
                                        profile.mappings[idx].target = trimmed
                                    }
                                }

                                LinearIconButton(
                                    systemName: "xmark",
                                    label: "移除映射 \(idx + 1)",
                                    size: 12,
                                    boxSize: 28,
                                    disabled: !writable || saving
                                ) {
                                    if profile.mappings.indices.contains(idx) {
                                        profile.mappings.remove(at: idx)
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    private var discardConfirmCard: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("放弃未保存的更改？")
                .font(DS.fontSectionTitle)
                .foregroundStyle(DS.ink)
            HStack(spacing: 8) {
                Spacer()
                Button("继续编辑") {
                    confirmDiscard = false
                }
                .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                Button("放弃修改", action: onClose)
                    .buttonStyle(LinearButtonStyle(variant: .danger, compact: true))
            }
        }
        .padding(18)
        .frame(width: 320)
        .background(DS.surface)
        .overlay(RoundedRectangle(cornerRadius: 10).strokeBorder(DS.lineStrong, lineWidth: 1))
        .clipShape(RoundedRectangle(cornerRadius: 10))
        .shadow(color: DS.modalShadowColor, radius: 16, x: 0, y: 8)
    }

    private func requestClose(fromEscape: Bool) {
        guard !saving else { return }
        if confirmDiscard {
            confirmDiscard = false
            return
        }
        if isDirty {
            if !fromEscape {
                confirmDiscard = true
            }
            return
        }
        onClose()
    }

    private func load() async {
        guard store.state.online else {
            loading = false
            return
        }
        loading = true
        error = ""
        defer { loading = false }
        do {
            let loaded: AccountSettingsReply = try await RustBridge.shared.api("GET", "/accounts/\(account.id)/settings")
            settings = loaded
            profile = AccountModelProfile(loaded.model_config)
            selectedProxyId = loaded.proxy_id
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func syncModels() async {
        guard let settings, store.state.online, !syncing else { return }
        syncing = true
        error = ""
        catalog = nil
        selectedCatalogModels.removeAll()
        notice = "正在同步上游模型…"
        defer { syncing = false }
        do {
            let result = try await store.operations.accountOperation(
                account: account,
                action: "sync_models",
                payload: [:],
                expectedVersion: settings.versions["sync_models"]
            )
            catalog = SyncedModelCatalog(
                models: result?.models ?? [],
                warnings: result?.warnings ?? [],
                synced_at: result?.synced_at
            )
            notice = ""
        } catch {
            notice = ""
            self.error = error.localizedDescription
        }
    }

    private func save() async {
        guard let settings, store.state.online, writable, isDirty, validationMessage.isEmpty, !saving else { return }
        saving = true
        error = ""
        notice = "正在保存…"
        defer { saving = false }
        do {
            let payload: [String: JSONValue]
            if isProxyMode {
                payload = ["proxy_id": selectedProxyId.map { .int(Int64($0)) } ?? .null]
            } else {
                payload = [
                    "whitelist": .array(profile.whitelist.map(JSONValue.string)),
                    "mappings": .array(profile.mappings.map {
                        .object(["source": .string($0.source), "target": .string($0.target)])
                    }),
                ]
            }
            let result = try await store.operations.accountOperation(
                account: account,
                action: currentAction,
                payload: payload,
                expectedVersion: settings.versions[currentAction]
            )
            if result?.verified == false {
                throw RustBridgeError("保存尚未确认，请在待办中查看")
            }
            store.showToast("已保存 \(account.name) 设置")
            onClose()
        } catch {
            notice = ""
            self.error = error.localizedDescription
        }
    }
}
