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

private struct AccountSettingsPayload: Decodable, Sendable {
    var whitelist: [String]?
    var mapping: [String: String]?
    var proxy_id: Int?
    var version: String?
}

public struct AccountSettingsSheet: View {
    public let account: Account
    @Bindable public var store: AppStore
    public let initialTab: AccountSettingsTab
    public let onClose: () -> Void

    @State private var tab: AccountSettingsTab
    @State private var whitelistRows: [String] = []
    @State private var origWhitelist: [String] = []
    @State private var mappingRows: [KeyValuePairDraft] = []
    @State private var origMapping: [String: String] = [:]
    @State private var selectedProxyId: Int? = nil
    @State private var origProxyId: Int? = nil
    @State private var loadedVersion: String = ""
    @State private var loading: Bool = false
    @State private var saving: Bool = false
    @State private var confirmDiscard: Bool = false
    @State private var error: String = ""

    public init(account: Account, store: AppStore, initialTab: AccountSettingsTab, onClose: @escaping () -> Void) {
        self.account = account
        self.store = store
        self.initialTab = initialTab
        self.onClose = onClose
        self._tab = State(initialValue: initialTab)
    }

    private var currentWhitelistNormalized: [String] {
        whitelistRows.map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }.filter { !$0.isEmpty }
    }

    private var currentMappingNormalized: [String: String] {
        Dictionary(
            mappingRows.compactMap { pair -> (String, String)? in
                let k = pair.key.trimmingCharacters(in: .whitespacesAndNewlines)
                let v = pair.value.trimmingCharacters(in: .whitespacesAndNewlines)
                return (!k.isEmpty && !v.isEmpty) ? (k, v) : nil
            },
            uniquingKeysWith: { _, last in last }
        )
    }

    private var isModelDirty: Bool {
        currentWhitelistNormalized != origWhitelist || currentMappingNormalized != origMapping
    }

    private var isProxyDirty: Bool {
        selectedProxyId != origProxyId
    }

    private var isDirty: Bool {
        isModelDirty || isProxyDirty
    }

    public var body: some View {
        ZStack {
            VStack(alignment: .leading, spacing: 14) {
                HStack {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("\(tab.heading) · \(account.name)")
                            .font(DS.fontDialogTitle)
                            .foregroundStyle(DS.ink)
                        Text("#\(account.id) · \(PlatformMeta.displayName(account.platform))")
                            .font(DS.fontResetTime)
                            .monospacedDigit()
                            .foregroundStyle(DS.muted)
                    }
                    Spacer()
                    LinearIconButton(systemName: "xmark", label: "关闭账号设置", disabled: saving) {
                        requestClose(fromEscape: false)
                    }
                }

                HStack(spacing: 4) {
                    ForEach(AccountSettingsTab.allCases, id: \.rawValue) { item in
                        Button(item.label) {
                            tab = item
                        }
                        .buttonStyle(LinearButtonStyle(variant: tab == item ? .selectedGhost : .ghost, compact: true))
                    }
                }

                if loading {
                    Text("正在读取账号设置…")
                        .font(DS.fontCompact)
                        .foregroundStyle(DS.muted)
                        .padding(.vertical, 20)
                } else {
                    switch tab {
                    case .whitelist:
                        whitelistEditor
                    case .mapping:
                        mappingEditor
                    case .proxy:
                        ProxyOptionsPickerView(
                            selectedProxyId: $selectedProxyId,
                            online: store.state.online,
                            connectionKey: store.connectionKey
                        )
                    }
                }

                if !error.isEmpty {
                    Text(error)
                        .font(DS.fontCompact)
                        .foregroundStyle(DS.bad)
                }

                HStack(spacing: 10) {
                    Spacer()
                    Button("取消") {
                        requestClose(fromEscape: false)
                    }
                    .buttonStyle(LinearButtonStyle(variant: .secondary))
                    .disabled(saving)

                    Button(saving ? "保存中…" : "保存") {
                        Task { await save() }
                    }
                    .buttonStyle(LinearButtonStyle(variant: .primary))
                    .disabled(!store.state.online || loading || saving || !isDirty)
                }
            }
            .padding(22)
            .frame(width: 540)
            .background(DS.surface)
            .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(DS.line, lineWidth: 1))
            .clipShape(RoundedRectangle(cornerRadius: 12))
            .sub2opsDialogFocus {
                requestClose(fromEscape: true)
            }

            if confirmDiscard {
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
                .shadow(color: DS.shadowColor, radius: 12, x: 0, y: 4)
            }
        }
        .task {
            await load()
        }
    }

    private var whitelistEditor: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text("允许调度的模型 ID（留空表示不限制）")
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.muted)
                Spacer()
                Button("添加模型") {
                    whitelistRows.append("")
                }
                .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
            }
            ScrollView {
                VStack(spacing: 6) {
                    ForEach(whitelistRows.indices, id: \.self) { idx in
                        HStack(spacing: 6) {
                            TextField(text: $whitelistRows[idx], prompt: Text(verbatim: "白名单模型 \(idx + 1)")) { EmptyView() }
                                .textFieldStyle(.plain)
                                .font(DS.fontMono12)
                                .padding(.horizontal, 8)
                                .frame(height: DS.controlHeightCompact)
                                .background(DS.surface)
                                .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(DS.line, lineWidth: 1))
                            LinearIconButton(systemName: "trash", label: "删除白名单项", size: 12) {
                                whitelistRows.remove(at: idx)
                            }
                        }
                    }
                }
            }
            .frame(maxHeight: 220)
        }
    }

    private var mappingEditor: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text("请求模型 -> 上游转发模型（支持 * 通配符）")
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.muted)
                Spacer()
                Button("添加映射") {
                    mappingRows.append(KeyValuePairDraft())
                }
                .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
            }
            ScrollView {
                VStack(spacing: 6) {
                    ForEach($mappingRows) { $pair in
                        HStack(spacing: 6) {
                            TextField(text: $pair.key, prompt: Text(verbatim: "请求模型")) { EmptyView() }
                                .textFieldStyle(.plain)
                                .font(DS.fontMono12)
                                .padding(.horizontal, 8)
                                .frame(height: DS.controlHeightCompact)
                                .background(DS.surface)
                                .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(DS.line, lineWidth: 1))
                            Image(systemName: "arrow.right")
                                .font(DS.iconFont(11))
                                .foregroundStyle(DS.muted)
                            TextField(text: $pair.value, prompt: Text(verbatim: "转发模型")) { EmptyView() }
                                .textFieldStyle(.plain)
                                .font(DS.fontMono12)
                                .padding(.horizontal, 8)
                                .frame(height: DS.controlHeightCompact)
                                .background(DS.surface)
                                .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(DS.line, lineWidth: 1))
                            LinearIconButton(systemName: "trash", label: "删除映射项", size: 12) {
                                mappingRows.removeAll { $0.id == pair.id }
                            }
                        }
                    }
                }
            }
            .frame(maxHeight: 220)
        }
    }

    private func requestClose(fromEscape: Bool) {
        guard !saving else { return }
        if isDirty {
            if !fromEscape {
                confirmDiscard = true
            }
            return
        }
        onClose()
    }

    private func load() async {
        guard store.state.online else { return }
        loading = true
        defer { loading = false }
        do {
            let loaded: AccountSettingsPayload = try await RustBridge.shared.api("GET", "/accounts/\(account.id)/settings")
            let wl = loaded.whitelist ?? []
            origWhitelist = wl
            whitelistRows = wl.isEmpty ? [""] : wl
            let mp = loaded.mapping ?? [:]
            origMapping = mp
            mappingRows = mp.sorted { $0.key < $1.key }.map { KeyValuePairDraft(key: $0.key, value: $0.value) }
            origProxyId = loaded.proxy_id
            selectedProxyId = loaded.proxy_id
            loadedVersion = loaded.version ?? account.version
            error = ""
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func save() async {
        guard store.state.online, !saving else { return }
        saving = true
        error = ""
        defer { saving = false }
        do {
            if isModelDirty {
                let ver = account.operation_versions["model_config"] ?? loadedVersion
                _ = try await store.operations.accountOperation(
                    account: account,
                    action: "model_config",
                    payload: [
                        "whitelist": .array(currentWhitelistNormalized.map(JSONValue.string)),
                        "mapping": .object(currentMappingNormalized.mapValues(JSONValue.string)),
                    ],
                    expectedVersion: ver
                )
            }
            if isProxyDirty {
                let ver = account.operation_versions["proxy"] ?? loadedVersion
                _ = try await store.operations.accountOperation(
                    account: account,
                    action: "proxy",
                    payload: [
                        "proxy_id": selectedProxyId.map { .int(Int64($0)) } ?? .null,
                    ],
                    expectedVersion: ver
                )
            }
            store.showToast("已保存 \(account.name) 设置")
            onClose()
        } catch {
            self.error = error.localizedDescription
        }
    }
}
