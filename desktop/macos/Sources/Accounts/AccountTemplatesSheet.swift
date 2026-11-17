import SwiftUI

public struct AccountTemplateRecord: Codable, Sendable, Equatable, Identifiable {
    public var id: Int
    public var name: String
    public var whitelist: [String]?
    public var mapping: [String: String]?
    public var updated_at: String?

    public init(id: Int = 0, name: String = "", whitelist: [String]? = [], mapping: [String: String]? = [:]) {
        self.id = id
        self.name = name
        self.whitelist = whitelist
        self.mapping = mapping
    }
}

private struct AccountTemplateEnvelope: Decodable, Sendable {
    var items: [AccountTemplateRecord]?
    var templates: [AccountTemplateRecord]?
}

public struct AccountTemplatesSheet: View {
    @Bindable public var store: AppStore
    public let targetAccount: Account?
    public let onClose: () -> Void

    @State private var templates: [AccountTemplateRecord] = []
    @State private var selectedId: Int?
    @State private var draftName: String = ""
    @State private var draftWhitelist: String = ""
    @State private var draftMappings: [KeyValuePairDraft] = []
    @State private var editingNew: Bool = false
    @State private var loading: Bool = false
    @State private var busy: Bool = false
    @State private var error: String = ""

    public init(store: AppStore, targetAccount: Account?, onClose: @escaping () -> Void) {
        self.store = store
        self.targetAccount = targetAccount
        self.onClose = onClose
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    Text(targetAccount == nil ? "账号模板管理" : "应用模板 · \(targetAccount!.name)")
                        .font(DS.fontDialogTitle)
                        .foregroundStyle(DS.ink)
                    Text("管理 OpenAI 白名单与模型映射预设模板")
                        .font(DS.fontResetTime)
                        .foregroundStyle(DS.muted)
                }
                Spacer()
                Button("新建模板") {
                    startNewTemplate()
                }
                .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                .disabled(!store.state.online || busy)

                LinearIconButton(systemName: "xmark", label: "关闭模板窗口", disabled: busy, action: onClose)
            }

            HStack(alignment: .top, spacing: 14) {
                VStack(alignment: .leading, spacing: 6) {
                    if templates.isEmpty && !loading {
                        Text("暂无保存的模板")
                            .font(DS.fontCompact)
                            .foregroundStyle(DS.muted)
                            .padding(10)
                    }
                    ScrollView {
                        VStack(alignment: .leading, spacing: 4) {
                            ForEach(templates) { tpl in
                                let active = !editingNew && selectedId == tpl.id
                                Button {
                                    selectTemplate(tpl)
                                } label: {
                                    HStack {
                                        VStack(alignment: .leading, spacing: 2) {
                                            Text(tpl.name)
                                                .font(DS.fontCompactSemibold)
                                                .foregroundStyle(active ? DS.accent : DS.ink)
                                                .lineLimit(1)
                                            Text("\((tpl.whitelist ?? []).count) 白名单 · \((tpl.mapping ?? [:]).count) 映射")
                                                .font(DS.fontResetTime)
                                                .monospacedDigit()
                                                .foregroundStyle(DS.muted)
                                        }
                                        Spacer()
                                    }
                                    .padding(.horizontal, 10)
                                    .padding(.vertical, 7)
                                    .background(active ? DS.accentBg : DS.surfaceSubtle)
                                    .clipShape(RoundedRectangle(cornerRadius: 6))
                                }
                                .buttonStyle(.plain)
                            }
                        }
                    }
                }
                .frame(width: 190, height: 310)

                Rectangle().fill(DS.line).frame(width: 1, height: 310)

                ScrollView {
                    VStack(alignment: .leading, spacing: 12) {
                        VStack(alignment: .leading, spacing: 4) {
                            Text("模板名称")
                                .font(DS.fontCompactSemibold)
                                .foregroundStyle(DS.ink)
                            TextField(text: $draftName, prompt: Text(verbatim: "例如：主力 Codex 模型映射")) { EmptyView() }
                                .textFieldStyle(.plain)
                                .font(DS.fontBody)
                                .padding(.horizontal, 8)
                                .frame(height: DS.controlHeightCompact)
                                .background(DS.surface)
                                .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(DS.line, lineWidth: 1))
                                .clipShape(RoundedRectangle(cornerRadius: 6))
                        }

                        VStack(alignment: .leading, spacing: 4) {
                            Text("模型白名单（每行或逗号分隔）")
                                .font(DS.fontCompactSemibold)
                                .foregroundStyle(DS.ink)
                            TextEditor(text: $draftWhitelist)
                                .font(DS.fontMono12)
                                .frame(height: 82)
                                .padding(4)
                                .background(DS.surface)
                                .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(DS.line, lineWidth: 1))
                                .clipShape(RoundedRectangle(cornerRadius: 6))
                        }

                        VStack(alignment: .leading, spacing: 6) {
                            HStack {
                                Text("模型映射")
                                    .font(DS.fontCompactSemibold)
                                    .foregroundStyle(DS.ink)
                                Spacer()
                                Button("添加映射") {
                                    draftMappings.append(KeyValuePairDraft())
                                }
                                .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                            }
                            ForEach($draftMappings) { $pair in
                                HStack(spacing: 6) {
                                    TextField(text: $pair.key, prompt: Text(verbatim: "请求模型")) { EmptyView() }
                                        .textFieldStyle(.plain)
                                        .font(DS.fontMono11)
                                        .padding(.horizontal, 6)
                                        .frame(height: 28)
                                        .background(DS.surface)
                                        .overlay(RoundedRectangle(cornerRadius: 5).strokeBorder(DS.line, lineWidth: 1))
                                    Image(systemName: "arrow.right")
                                        .font(DS.iconFont(10))
                                        .foregroundStyle(DS.muted)
                                    TextField(text: $pair.value, prompt: Text(verbatim: "转发模型")) { EmptyView() }
                                        .textFieldStyle(.plain)
                                        .font(DS.fontMono11)
                                        .padding(.horizontal, 6)
                                        .frame(height: 28)
                                        .background(DS.surface)
                                        .overlay(RoundedRectangle(cornerRadius: 5).strokeBorder(DS.line, lineWidth: 1))
                                    LinearIconButton(systemName: "trash", label: "移除映射", size: 11) {
                                        draftMappings.removeAll { $0.id == pair.id }
                                    }
                                }
                            }
                        }
                    }
                }
                .frame(maxWidth: .infinity, maxHeight: 310)
            }

            if !error.isEmpty {
                Text(error)
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.bad)
            }

            HStack(spacing: 8) {
                if !editingNew, let id = selectedId {
                    Button("删除模板") {
                        Task { await deleteTemplate(id) }
                    }
                    .buttonStyle(LinearButtonStyle(variant: .danger, compact: true))
                    .disabled(!store.state.online || busy)
                }
                Spacer()
                Button("关闭", action: onClose)
                    .buttonStyle(LinearButtonStyle(variant: .secondary))
                    .disabled(busy)
                Button(busy ? "保存中…" : "保存模板") {
                    Task { await saveTemplate() }
                }
                .buttonStyle(LinearButtonStyle(variant: targetAccount == nil ? .primary : .secondary))
                .disabled(!store.state.online || busy || draftName.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)

                if let acc = targetAccount, let id = selectedId, !editingNew {
                    Button("应用到 \(acc.name)") {
                        Task { await applyTemplate(id: id, to: acc) }
                    }
                    .buttonStyle(LinearButtonStyle(variant: .primary))
                    .disabled(!store.state.online || busy)
                }
            }
        }
        .padding(22)
        .frame(width: 660)
        .background(DS.surface)
        .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(DS.line, lineWidth: 1))
        .clipShape(RoundedRectangle(cornerRadius: 12))
        .sub2opsDialogFocus {
            if !busy { onClose() }
        }
        .task {
            await loadTemplates()
        }
    }

    private func startNewTemplate() {
        editingNew = true
        selectedId = nil
        draftName = ""
        draftWhitelist = ""
        draftMappings = []
        error = ""
    }

    private func selectTemplate(_ tpl: AccountTemplateRecord) {
        editingNew = false
        selectedId = tpl.id
        draftName = tpl.name
        draftWhitelist = (tpl.whitelist ?? []).joined(separator: "\n")
        draftMappings = (tpl.mapping ?? [:]).sorted { $0.key < $1.key }.map { KeyValuePairDraft(key: $0.key, value: $0.value) }
        error = ""
    }

    private func loadTemplates() async {
        guard store.state.online else { return }
        loading = true
        defer { loading = false }
        do {
            let raw: JSONValue = try await RustBridge.shared.api("GET", "/account-templates")
            let data = try JSONEncoder().encode(raw)
            let list: [AccountTemplateRecord]
            if let direct = try? JSONDecoder().decode([AccountTemplateRecord].self, from: data) {
                list = direct
            } else if let env = try? JSONDecoder().decode(AccountTemplateEnvelope.self, from: data) {
                list = env.items ?? env.templates ?? []
            } else {
                list = []
            }
            templates = list
            if let first = list.first {
                selectTemplate(first)
            } else {
                startNewTemplate()
            }
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func saveTemplate() async {
        busy = true
        error = ""
        defer { busy = false }
        let wl = draftWhitelist
            .components(separatedBy: CharacterSet(charactersIn: ",\n\r"))
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty }
        var mapObj: [String: JSONValue] = [:]
        for pair in draftMappings {
            let k = pair.key.trimmingCharacters(in: .whitespacesAndNewlines)
            let v = pair.value.trimmingCharacters(in: .whitespacesAndNewlines)
            if !k.isEmpty && !v.isEmpty { mapObj[k] = .string(v) }
        }
        let body: JSONValue = .object([
            "name": .string(draftName.trimmingCharacters(in: .whitespacesAndNewlines)),
            "whitelist": .array(wl.map(JSONValue.string)),
            "mapping": .object(mapObj),
        ])
        do {
            if editingNew || selectedId == nil {
                let _: JSONValue = try await RustBridge.shared.api("POST", "/account-templates", body: body)
            } else if let id = selectedId {
                let _: JSONValue = try await RustBridge.shared.api("PUT", "/account-templates/\(id)", body: body)
            }
            await loadTemplates()
            store.showToast("模板已保存")
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func deleteTemplate(_ id: Int) async {
        busy = true
        error = ""
        defer { busy = false }
        do {
            let _: JSONValue = try await RustBridge.shared.api("DELETE", "/account-templates/\(id)")
            await loadTemplates()
            store.showToast("模板已删除")
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func applyTemplate(id: Int, to account: Account) async {
        busy = true
        error = ""
        defer { busy = false }
        do {
            _ = try await store.operations.accountOperation(
                account: account,
                action: "model_config",
                payload: ["template_id": .int(Int64(id))]
            )
            store.showToast("已将模板应用到 \(account.name)")
            onClose()
        } catch {
            self.error = error.localizedDescription
        }
    }
}
