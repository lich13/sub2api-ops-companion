import AppKit
import SwiftUI

private struct CreationOutcome: Identifiable, Sendable, Equatable {
    var index: Int
    var status: String
    var message: String
    var id: Int { index }
}

private struct MixedChannelCheckResponse: Decodable, Sendable {
    var has_risk: Bool
}

private struct PreviewModelsResponse: Decodable, Sendable {
    var models: [String]
    var warnings: [String]?
}

public struct AccountCreationSheet: View {
    @Bindable public var store: AppStore
    public let onClose: () -> Void

    @State private var step: Int = 1
    @State private var draft = AccountCreationFormDraft()
    @State private var options: CreationOptionsResponse?
    @State private var tlsProfiles: [CreationTLSProfileOption] = []
    @State private var quotaNotifyEnabled: Bool = false
    @State private var webSearchEnabled: Bool = false
    @State private var sourceQuery: String = ""
    @State private var selectedSourceId: Int? = nil
    @State private var reusing: Bool = false
    @State private var reuseNotice: String = ""
    @State private var authMethod: String = "manual"
    @State private var authInput: String = ""
    @State private var oauthSession: OAuthAuthorizationSession?
    @State private var previewModels: [String] = []
    @State private var previewSelected: Set<String> = []
    @State private var modelsPreviewed: Bool = false
    @State private var mixedRisk: Bool = false
    @State private var mixedConfirmed: Bool = false
    @State private var outcomes: [CreationOutcome] = []
    @State private var busy: Bool = false
    @State private var error: String = ""

    public init(store: AppStore, onClose: @escaping () -> Void) {
        self.store = store
        self.onClose = onClose
    }

    private var catalog: AccountCreationCatalog {
        AccountCreationCatalog.shared
    }

    private var currentPlatformSpec: CreationPlatformSpec? {
        catalog.platforms.first(where: { $0.id == draft.platform })
    }

    private var reuseCandidates: [Account] {
        store.accounts.filter { $0.platform == draft.platform && $0.type == draft.type }
    }

    private var visibleReuseCandidates: [Account] {
        let q = sourceQuery.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard !q.isEmpty else { return reuseCandidates }
        return reuseCandidates.filter { "\($0.name) \($0.id)".lowercased().contains(q) }
    }

    private var availableGroups: [(id: Int, name: String)] {
        if let optGroups = options?.groups, !optGroups.isEmpty {
            return optGroups.filter { g in
                g.platform == draft.platform
                    || g.platform == "composite"
                    || (draft.platform == "antigravity"
                        && draft.values["extra.mixed_scheduling"]?.boolValue == true
                        && ["anthropic", "gemini"].contains(g.platform))
            }.map { ($0.id, $0.name) }
        }
        return store.groups.filter { $0.platform == draft.platform }.map { ($0.id, $0.name) }
    }

    private func typeDisplayName(_ raw: String) -> String {
        if draft.platform == "antigravity" && raw == "apikey" {
            return "Upstream"
        }
        return PlatformMeta.typeLabel(raw)
    }

    public var body: some View {
        LinearModalCard(
            title: "添加账号",
            subtitle: step == 1 ? "1 · 基本信息与配置" : "2 · 凭据与授权",
            width: 668,
            closeDisabled: busy,
            onClose: onClose
        ) {
            LinearBoundedScrollView(maxHeight: 590) {
                VStack(alignment: .leading, spacing: 12) {
                    if step == 1 {
                        stepOneBody
                    } else {
                        stepTwoBody
                    }

                    if mixedRisk {
                        VStack(alignment: .leading, spacing: 8) {
                            Text("所选分组混用 Claude 与 Antigravity 渠道，可能影响账号风险与调度。确认继续后再提交。")
                                .font(DS.fontCompact)
                                .foregroundStyle(DS.warn)
                            LinearCheckbox(isChecked: mixedConfirmed, label: "我确认混合渠道风险", disabled: busy) {
                                mixedConfirmed.toggle()
                            }
                        }
                        .padding(10)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(DS.warnBg)
                        .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(DS.warn, lineWidth: 1))
                        .clipShape(RoundedRectangle(cornerRadius: 6))
                    }

                    if !outcomes.isEmpty {
                        VStack(alignment: .leading, spacing: 4) {
                            ForEach(outcomes) { item in
                                Text("#\(item.index) · \(item.message)")
                                    .font(DS.fontCompact)
                                    .foregroundStyle(item.status == "success" ? DS.good : DS.bad)
                            }
                        }
                        .padding(10)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(DS.surfaceSubtle)
                        .clipShape(RoundedRectangle(cornerRadius: 6))
                    }
                }
            }

            if !error.isEmpty {
                Text(error)
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.bad)
            }
        } footer: {
            Button(step == 2 ? "上一步" : "取消") {
                if step == 2 {
                    step = 1
                    error = ""
                } else {
                    onClose()
                }
            }
            .buttonStyle(LinearButtonStyle(variant: .secondary))
            .disabled(busy)

            if !outcomes.isEmpty {
                Button("刷新账号列表") {
                    store.refresh()
                }
                .buttonStyle(LinearButtonStyle(variant: .secondary))
                .disabled(busy)
            }

            Spacer()

            Button {
                Task { await submitAction() }
            } label: {
                Label(
                    busy ? "正在处理…" : (step == 1 ? "下一步" : (outcomes.isEmpty ? "创建账号" : "提交未成功项")),
                    systemImage: step == 1 ? "plus" : "checkmark"
                )
            }
            .buttonStyle(LinearButtonStyle(variant: .primary))
            .disabled(!store.state.online || busy || reusing || (mixedRisk && !mixedConfirmed))
        }
        .sub2opsDialogFocus {
            if !busy { onClose() }
        }
        .task(id: store.state.online) {
            guard store.state.online else { return }
            await loadOptions()
        }
    }

    private var stepOneBody: some View {
        VStack(alignment: .leading, spacing: 12) {
            LazyVGrid(
                columns: [GridItem(.flexible(), spacing: 12), GridItem(.flexible(), spacing: 12)],
                alignment: .leading,
                spacing: 10
            ) {
                VStack(alignment: .leading, spacing: 5) {
                    Text("平台").font(DS.fontCompactMedium).foregroundStyle(DS.muted)
                    LinearSelect(
                        label: currentPlatformSpec?.label ?? draft.platform,
                        accessibilityTitle: "平台",
                        height: DS.controlHeight,
                        expandWidth: true,
                        disabled: busy,
                        entries: catalog.platforms.map { p in
                            LinearMenuEntry(id: p.id, title: p.label, checked: draft.platform == p.id) {
                                switchKind(platform: p.id, type: nil)
                            }
                        }
                    )
                }

                VStack(alignment: .leading, spacing: 5) {
                    Text("账号类型").font(DS.fontCompactMedium).foregroundStyle(DS.muted)
                    let types = currentPlatformSpec?.types ?? ["apikey"]
                    LinearSelect(
                        label: typeDisplayName(draft.type),
                        accessibilityTitle: "账号类型",
                        height: DS.controlHeight,
                        expandWidth: true,
                        disabled: busy,
                        entries: types.map { t in
                            LinearMenuEntry(id: t, title: typeDisplayName(t), checked: draft.type == t) {
                                switchKind(platform: draft.platform, type: t)
                            }
                        }
                    )
                }

                labeledTextField("账号名称", text: $draft.name, prompt: "例如：Codex 主力账号")
                labeledTextField("备注", text: $draft.notes, prompt: "可选备注")
            }

            reuseSection

            LazyVGrid(
                columns: [GridItem(.flexible(), spacing: 12), GridItem(.flexible(), spacing: 12)],
                alignment: .leading,
                spacing: 10
            ) {
                VStack(alignment: .leading, spacing: 5) {
                    Text("代理").font(DS.fontCompactMedium).foregroundStyle(DS.muted)
                    let proxies = options?.proxies ?? []
                    let currentProxyLabel = draft.proxyId.flatMap { id in
                        proxies.first(where: { $0.id == id }).map { "\($0.name)\(($0.status != nil && $0.status != "active") ? "（未启用）" : "")" }
                    } ?? "不使用代理"
                    LinearSelect(
                        label: currentProxyLabel,
                        accessibilityTitle: "代理",
                        height: DS.controlHeight,
                        expandWidth: true,
                        disabled: busy,
                        entries: [
                            LinearMenuEntry(id: "none", title: "不使用代理", checked: draft.proxyId == nil) {
                                draft.proxyId = nil
                                oauthSession = nil
                            },
                        ] + proxies.map { p in
                            let suffix = (p.status != nil && p.status != "active") ? "（未启用）" : ""
                            return LinearMenuEntry(id: "\(p.id)", title: "\(p.name)\(suffix)", checked: draft.proxyId == p.id) {
                                draft.proxyId = p.id
                                oauthSession = nil
                            }
                        }
                    )
                }

                labeledTextField("并发", text: $draft.concurrency, prompt: "10")
                labeledTextField("负载因子", text: $draft.loadFactor, prompt: "跟随并发数")
                labeledTextField("优先级", text: $draft.priority, prompt: "1")
                labeledTextField("计费倍率", text: $draft.rateMultiplier, prompt: "1")
                labeledTextField("到期时间（可选）", text: $draft.expiresAtText, prompt: "YYYY-MM-DD HH:mm")
            }

            VStack(alignment: .leading, spacing: 6) {
                Text("分组")
                    .font(DS.fontCompactMedium)
                    .foregroundStyle(DS.muted)
                if !availableGroups.isEmpty {
                    HStack(spacing: 14) {
                        ForEach(availableGroups, id: \.id) { g in
                            LinearCheckbox(
                                isChecked: draft.groupIds.contains(g.id),
                                label: g.name,
                                disabled: busy
                            ) {
                                if draft.groupIds.contains(g.id) {
                                    draft.groupIds.remove(g.id)
                                } else {
                                    draft.groupIds.insert(g.id)
                                }
                                mixedRisk = false
                                mixedConfirmed = false
                            }
                        }
                        Spacer(minLength: 0)
                    }
                    .padding(10)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(DS.surface)
                    .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(DS.line, lineWidth: 1))
                    .clipShape(RoundedRectangle(cornerRadius: 6))
                }
                if draft.groupIds.isEmpty {
                    Text("未选择时采用 Sub2API 的默认分组规则")
                        .font(DS.fontResetTime)
                        .foregroundStyle(DS.muted)
                }
            }

            AccountCreationFieldsView(
                draft: $draft,
                tlsProfiles: tlsProfiles,
                quotaNotifyEnabled: quotaNotifyEnabled,
                webSearchEnabled: webSearchEnabled
            )
        }
    }

    private var reuseSection: some View {
        let selectedCandidate = selectedSourceId.flatMap { id in reuseCandidates.first(where: { $0.id == id }) }
        let sourceLabel = selectedCandidate.map { "\($0.name) · #\($0.id)" }
            ?? (reuseCandidates.isEmpty ? "暂无可复用账号" : "选择来源账号")

        return VStack(alignment: .leading, spacing: 8) {
            Text("复用同平台、同类型账号")
                .font(DS.fontCompactSemibold)
                .foregroundStyle(DS.ink)

            if reuseCandidates.count > 1 {
                LinearSearchField(text: $sourceQuery, prompt: "搜索账号名称或 ID", height: 32, compactFont: true)
            }

            HStack(spacing: 8) {
                LinearSelect(
                    label: sourceLabel,
                    accessibilityTitle: "来源账号",
                    height: 34,
                    expandWidth: true,
                    compactFont: true,
                    mutedLabel: selectedCandidate == nil,
                    disabled: reuseCandidates.isEmpty || busy || reusing,
                    entries: visibleReuseCandidates.map { acc in
                        LinearMenuEntry(id: "\(acc.id)", title: "\(acc.name) · #\(acc.id)", checked: selectedSourceId == acc.id) {
                            selectedSourceId = acc.id
                            reuseNotice = ""
                        }
                    }
                )

                Button {
                    Task { await reuseConfiguration() }
                } label: {
                    Label(reusing ? "读取中…" : "复用配置", systemImage: "doc.on.doc")
                }
                .buttonStyle(LinearButtonStyle(variant: .secondary))
                .disabled(selectedSourceId == nil || reusing || busy || !store.state.online)
            }

            if !reuseNotice.isEmpty {
                Text(reuseNotice)
                    .font(DS.fontResetTime)
                    .foregroundStyle(DS.good)
            }
        }
        .padding(12)
        .background(DS.reuseCardBg)
        .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.accentBorder, lineWidth: 1))
        .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
    }

    @ViewBuilder
    private var stepTwoBody: some View {
        let pLabel = currentPlatformSpec?.label ?? draft.platform
        Text("\(draft.name.isEmpty ? "未命名账号" : draft.name) · \(pLabel) · \(typeDisplayName(draft.type))")
            .font(DS.fontCompactSemibold)
            .foregroundStyle(DS.ink)
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(DS.surfaceSubtle)
            .clipShape(RoundedRectangle(cornerRadius: 6))

        if draft.isOAuthFlow {
            let methods = draft.authMethods
            let currentMethodLabel = methods.first(where: { $0.id == authMethod })?.label ?? "授权码"
            VStack(alignment: .leading, spacing: 10) {
                VStack(alignment: .leading, spacing: 5) {
                    Text("添加方式").font(DS.fontCompactMedium).foregroundStyle(DS.muted)
                    LinearSelect(
                        label: currentMethodLabel,
                        accessibilityTitle: "添加方式",
                        height: DS.controlHeight,
                        expandWidth: true,
                        disabled: busy,
                        entries: methods.map { m in
                            LinearMenuEntry(id: m.id, title: m.label, checked: authMethod == m.id) {
                                authMethod = m.id
                                authInput = ""
                                oauthSession = nil
                                error = ""
                                outcomes = []
                            }
                        }
                    )
                }

                if authMethod == "manual" {
                    VStack(alignment: .leading, spacing: 8) {
                        Button(oauthSession == nil ? "生成授权链接" : "重新生成授权链接") {
                            Task { await generateAuthorizationURL() }
                        }
                        .buttonStyle(LinearButtonStyle(variant: .secondary))
                        .disabled(busy || !store.state.online)

                        if let session = oauthSession {
                            Text(verbatim: session.auth_url)
                                .font(DS.fontMono11)
                                .foregroundStyle(DS.muted)
                                .lineLimit(2)
                                .textSelection(.enabled)
                                .padding(8)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .background(DS.surface)
                                .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(DS.line, lineWidth: 1))

                            HStack(spacing: 8) {
                                Button {
                                    Task {
                                        try? await RustBridge.shared.commandVoid(
                                            "open_authorization_url",
                                            args: .object(["url": .string(session.auth_url)])
                                        )
                                    }
                                } label: {
                                    Label("系统浏览器授权", systemImage: "safari")
                                }
                                .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))

                                Button {
                                    NSPasteboard.general.clearContents()
                                    NSPasteboard.general.setString(session.auth_url, forType: .string)
                                    store.showToast("已复制授权链接")
                                } label: {
                                    Label("复制链接", systemImage: "doc.on.doc")
                                }
                                .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                            }
                        }
                    }
                    .padding(12)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(DS.surfaceSubtle)
                    .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                    .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
                }

                VStack(alignment: .leading, spacing: 5) {
                    Text(authMethod == "manual" ? "授权码或回调地址" : currentMethodLabel)
                        .font(DS.fontCompactMedium)
                        .foregroundStyle(DS.muted)
                    TextEditor(text: $authInput)
                        .font(DS.fontMono12)
                        .foregroundStyle(DS.ink)
                        .scrollContentBackground(.hidden)
                        .frame(height: 92)
                        .padding(6)
                        .background(DS.surface)
                        .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                        .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
                }

                Text(
                    ["refresh", "mobile-refresh", "cookie", "grok-sso"].contains(authMethod)
                        ? "支持批量，每行一个凭据。"
                        : (["codex-session", "agent-identity"].contains(authMethod)
                            ? "粘贴原生导出的 JSON；支持批量导入，匹配的已有账号将更新。"
                            : "授权完成后粘贴凭据并提交。")
                )
                .font(DS.fontResetTime)
                .foregroundStyle(DS.muted)
            }
        } else if draft.type == "service_account" && !(draft.platform == "gemini" && draft.values["credentials.auth_mode"]?.stringValue == "apikey") {
            VStack(alignment: .leading, spacing: 6) {
                Text("Service Account JSON")
                    .font(DS.fontCompactMedium)
                    .foregroundStyle(DS.muted)
                TextEditor(text: $draft.serviceAccountJSON)
                    .font(DS.fontMono11)
                    .foregroundStyle(DS.ink)
                    .scrollContentBackground(.hidden)
                    .frame(height: 120)
                    .padding(6)
                    .background(DS.surface)
                    .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                    .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
            }
        } else if draft.type == "bedrock" && (draft.values["credentials.auth_mode"]?.stringValue ?? "sigv4") == "sigv4" {
            VStack(alignment: .leading, spacing: 10) {
                labeledSecureField("AWS Access Key ID", text: $draft.awsAccessKeyId)
                labeledSecureField("AWS Secret Access Key", text: $draft.awsSecretAccessKey)
                labeledSecureField("AWS Session Token（可选）", text: $draft.awsSessionToken)
            }
        } else {
            VStack(alignment: .leading, spacing: 10) {
                labeledSecureField("API Key", text: $draft.apiKey)

                if draft.type == "apikey" {
                    HStack {
                        Button("读取上游模型") {
                            Task { await previewUpstreamModels() }
                        }
                        .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                        .disabled(busy || draft.apiKey.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                        Spacer()
                    }

                    if !previewModels.isEmpty {
                        VStack(alignment: .leading, spacing: 8) {
                            HStack {
                                Text("上游模型候选（\(previewModels.count)）")
                                    .font(DS.fontCompactSemibold)
                                    .foregroundStyle(DS.ink)
                                Spacer()
                                Button("添加所选模型") {
                                    for m in previewModels where previewSelected.contains(m) {
                                        if !draft.mappings.contains(where: { $0.key == m }) {
                                            draft.mappings.append(KeyValuePairDraft(key: m, value: m))
                                        }
                                    }
                                    previewModels = []
                                }
                                .buttonStyle(LinearButtonStyle(variant: .primary, compact: true))
                            }
                            LinearBoundedScrollView(maxHeight: 120) {
                                VStack(alignment: .leading, spacing: 4) {
                                    ForEach(previewModels, id: \.self) { m in
                                        LinearCheckbox(isChecked: previewSelected.contains(m), label: m) {
                                            if previewSelected.contains(m) {
                                                previewSelected.remove(m)
                                            } else {
                                                previewSelected.insert(m)
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        .padding(10)
                        .background(DS.surfaceSubtle)
                        .clipShape(RoundedRectangle(cornerRadius: 6))
                    }
                }
            }
        }
    }

    private func labeledTextField(_ title: String, text: Binding<String>, prompt: String, monospaced: Bool = false) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            Text(title).font(DS.fontCompactMedium).foregroundStyle(DS.muted)
            TextField(text: text, prompt: Text(verbatim: prompt).foregroundStyle(DS.placeholder)) { EmptyView() }
                .textFieldStyle(.plain)
                .font(monospaced ? DS.fontMono12 : DS.fontBody)
                .monospacedDigit()
                .foregroundStyle(DS.ink)
                .disabled(busy)
                .padding(.horizontal, 10)
                .frame(height: DS.controlHeight)
                .background(DS.surface)
                .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
        }
    }

    private func labeledSecureField(_ title: String, text: Binding<String>) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            Text(title).font(DS.fontCompactMedium).foregroundStyle(DS.muted)
            SecureField(text: text, prompt: Text(verbatim: "粘贴密钥").foregroundStyle(DS.placeholder)) { EmptyView() }
                .textFieldStyle(.plain)
                .font(DS.fontBody)
                .foregroundStyle(DS.ink)
                .disabled(busy)
                .padding(.horizontal, 10)
                .frame(height: DS.controlHeight)
                .background(DS.surface)
                .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
        }
    }

    private func switchKind(platform: String, type: String?) {
        let savedName = draft.name
        let savedNotes = draft.notes
        if let type {
            draft.applyPlatformDefaults(for: platform, authType: type)
        } else {
            draft.selectPlatform(platform)
        }
        draft.name = savedName
        draft.notes = savedNotes
        selectedSourceId = nil
        sourceQuery = ""
        reuseNotice = ""
        authMethod = "manual"
        authInput = ""
        oauthSession = nil
        outcomes = []
        error = ""
        mixedRisk = false
        mixedConfirmed = false
        previewModels = []
        modelsPreviewed = false
        step = 1
    }

    private func loadOptions() async {
        if let opt: CreationOptionsResponse = try? await RustBridge.shared.api("GET", "/account-creation/options") {
            options = opt
        }
        if let tls: [CreationTLSProfileOption] = try? await RustBridge.shared.api("GET", "/account-creation/catalog/tls") {
            tlsProfiles = tls
        }
        if let settingsObj: [String: JSONValue] = try? await RustBridge.shared.api("GET", "/account-creation/catalog/settings") {
            quotaNotifyEnabled = settingsObj["account_quota_notify_enabled"]?.boolValue ?? false
        }
        if let wsObj: [String: JSONValue] = try? await RustBridge.shared.api("GET", "/account-creation/catalog/web-search") {
            webSearchEnabled = wsObj["enabled"]?.boolValue ?? false
        }
        if reuseCandidates.count == 1 {
            selectedSourceId = reuseCandidates[0].id
        }
    }

    private func reuseConfiguration() async {
        guard let sourceId = selectedSourceId, store.state.online, !reusing else { return }
        reusing = true
        error = ""
        reuseNotice = ""
        defer { reusing = false }
        do {
            let sourceObj: [String: JSONValue] = try await RustBridge.shared.api("GET", "/account-creation/source/\(sourceId)")
            try draft.applyReusedSource(sourceObj)
            reuseNotice = "已复用配置，可继续修改；账号名称与新填凭据已保留"
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func generateAuthorizationURL() async {
        guard store.state.online, !busy else { return }
        busy = true
        error = ""
        oauthSession = nil
        defer { busy = false }
        do {
            let res: OAuthAuthorizationSession = try await RustBridge.shared.api(
                "POST",
                "/account-creation/auth/\(draft.authProvider)/authorize",
                body: .object(draft.authConfigBody)
            )
            var resolved = res
            if (resolved.state ?? "").isEmpty,
               let comps = URLComponents(string: res.auth_url),
               let st = comps.queryItems?.first(where: { $0.name == "state" })?.value
            {
                resolved.state = st
            }
            oauthSession = resolved
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func previewUpstreamModels() async {
        guard store.state.online, !busy else { return }
        busy = true
        error = ""
        defer { busy = false }
        do {
            let payload = try draft.buildPayload(requireCredentials: true)
            let creds = payload["credentials"]?.objectValue ?? [:]
            let res: PreviewModelsResponse = try await RustBridge.shared.api(
                "POST",
                "/account-creation/models",
                body: .object([
                    "platform": .string(draft.platform),
                    "type": .string(draft.type),
                    "base_url": creds["base_url"] ?? .null,
                    "api_key": creds["api_key"] ?? .null,
                    "model_mapping": creds["model_mapping"] ?? .null,
                ])
            )
            modelsPreviewed = true
            previewModels = res.models
            previewSelected = Set(res.models)
            if let warnings = res.warnings, !warnings.isEmpty {
                error = warnings.joined(separator: "；")
            }
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func exchangeCredential(item: String) async throws -> (credentials: [String: JSONValue], extra: [String: JSONValue]) {
        var config = draft.authConfigBody
        config.removeValue(forKey: "project_id")
        let action: String
        var body = config
        if authMethod == "manual" {
            guard let oauthSession else {
                throw RustBridgeError("请先生成授权链接")
            }
            var code = item.trimmingCharacters(in: .whitespacesAndNewlines)
            var state = oauthSession.state
            if let comps = URLComponents(string: code), comps.scheme != nil {
                code = comps.queryItems?.first(where: { $0.name == "code" })?.value ?? ""
                if let s = comps.queryItems?.first(where: { $0.name == "state" })?.value, !s.isEmpty {
                    state = s
                }
            }
            guard !code.isEmpty else {
                throw RustBridgeError("请填写授权码或完整回调地址")
            }
            body["session_id"] = .string(oauthSession.session_id)
            body["code"] = .string(code)
            if let state, !state.isEmpty { body["state"] = .string(state) }
            action = "exchange"
        } else if authMethod == "cookie" {
            body["code"] = .string(item)
            action = "cookie"
        } else {
            body["refresh_token"] = .string(item)
            if authMethod == "mobile-refresh" {
                body["client_id"] = .string("app_LlGpXReQgckcGGUo2JrYvtJK")
            }
            action = "refresh"
        }

        let tokenObj: [String: JSONValue] = try await RustBridge.shared.api(
            "POST",
            "/account-creation/auth/\(draft.authProvider)/\(action)",
            body: .object(body)
        )
        let resolved = AccountCreationFormDraft.extractTokenIdentity(
            platform: draft.platform,
            token: tokenObj,
            fallbackRefresh: action == "refresh" ? item : nil
        )
        guard resolved.credentials["access_token"] != nil else {
            throw RustBridgeError("授权未返回可用令牌，请重新授权")
        }
        return resolved
    }

    private func submitAction() async {
        guard store.state.online, !busy, !reusing else { return }
        let requireName = !(draft.platform == "grok" && draft.type == "oauth" && (step == 1 || authMethod == "grok-sso"))
        let stepProblem = draft.validateStepOne(requireName: requireName)
        if !stepProblem.isEmpty {
            error = stepProblem
            return
        }

        busy = true
        error = ""
        defer { busy = false }

        do {
            if !mixedConfirmed && ["anthropic", "antigravity"].contains(draft.platform) {
                let check: MixedChannelCheckResponse = try await RustBridge.shared.api(
                    "POST",
                    "/account-creation/check-mixed-channel",
                    body: .object([
                        "platform": .string(draft.platform),
                        "group_ids": .array(draft.groupIds.sorted().map { .int(Int64($0)) }),
                    ])
                )
                if check.has_risk {
                    mixedRisk = true
                    return
                }
            }

            if step == 1 {
                step = 2
                return
            }

            let isNativeImport = draft.isOAuthFlow && ["codex-session", "agent-identity", "codex-pat", "grok-sso"].contains(authMethod)
            let items: [String]
            if !draft.isOAuthFlow {
                items = ["direct"]
            } else if ["manual", "codex-session", "agent-identity", "codex-pat"].contains(authMethod) {
                let trimmed = authInput.trimmingCharacters(in: .whitespacesAndNewlines)
                items = trimmed.isEmpty ? [] : [trimmed]
            } else {
                items = authInput
                    .components(separatedBy: .newlines)
                    .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
                    .filter { !$0.isEmpty }
            }

            guard !items.isEmpty else {
                error = "请填写凭据"
                return
            }

            var nextOutcomes: [CreationOutcome] = []
            for (idx, item) in items.enumerated() {
                var itemDraft = draft
                if items.count > 1 && !draft.name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                    itemDraft.name = "\(draft.name) #\(idx + 1)"
                }

                do {
                    if isNativeImport {
                        var base = try itemDraft.buildPayload(requireCredentials: false, requireName: false)
                        let credExtras = base.removeValue(forKey: "credentials") ?? .object([:])
                        base.removeValue(forKey: "platform")
                        base.removeValue(forKey: "type")

                        if authMethod == "grok-sso" {
                            base["credentials"] = credExtras
                            base["sso_tokens"] = .array([.string(item)])
                            let res: [String: JSONValue] = try await RustBridge.shared.api("POST", "/account-creation/import/grok-sso", body: .object(base))
                            let createdArr = res["created"]?.arrayValue ?? []
                            if !(res["failed"]?.arrayValue ?? []).isEmpty || createdArr.isEmpty {
                                throw RustBridgeError("SSO 导入失败，请检查凭据后重试")
                            }
                            let newId = createdArr[0]["account"]?["id"]?.intValue ?? 0
                            nextOutcomes.append(CreationOutcome(index: idx + 1, status: "success", message: "已创建 #\(newId)"))
                        } else if authMethod == "codex-pat" {
                            base["credential_extras"] = credExtras
                            base["access_token"] = .string(item)
                            let res: CreationCreatedResponse = try await RustBridge.shared.api("POST", "/account-creation/import/codex-pat", body: .object(base))
                            nextOutcomes.append(CreationOutcome(index: idx + 1, status: "success", message: "已创建 #\(res.account.id)"))
                        } else {
                            base["credential_extras"] = credExtras
                            base["content"] = .string(item)
                            base["update_existing"] = .bool(true)
                            let res: [String: JSONValue] = try await RustBridge.shared.api("POST", "/account-creation/import/codex-session", body: .object(base))
                            let c = res["created"]?.intValue ?? 0
                            let u = res["updated"]?.intValue ?? 0
                            let f = res["failed"]?.intValue ?? 0
                            nextOutcomes.append(CreationOutcome(index: idx + 1, status: f > 0 ? "failed" : "success", message: "创建 \(c)，更新 \(u)，失败 \(f)"))
                        }
                    } else {
                        let resolved = draft.isOAuthFlow ? try await exchangeCredential(item: item) : nil
                        var payload = try itemDraft.buildPayload(
                            identityCredentials: resolved?.credentials,
                            identityExtra: resolved?.extra,
                            requireCredentials: true,
                            requireName: true
                        )
                        if mixedConfirmed {
                            payload["confirm_mixed_channel_risk"] = .bool(true)
                        }
                        let path = modelsPreviewed ? "/account-creation?sync_models=true" : "/account-creation"
                        let created: CreationCreatedResponse = try await RustBridge.shared.api("POST", path, body: .object(payload))
                        let warnSuffix = (created.warnings ?? []).isEmpty ? "" : "；\((created.warnings ?? []).joined(separator: "；"))"
                        nextOutcomes.append(CreationOutcome(index: idx + 1, status: "success", message: "已创建 #\(created.account.id)\(warnSuffix)"))
                    }
                } catch {
                    nextOutcomes.append(CreationOutcome(index: idx + 1, status: "failed", message: error.localizedDescription))
                }
                outcomes = nextOutcomes
            }

            if nextOutcomes.contains(where: { $0.status == "success" }) {
                store.refresh()
            }
            if !nextOutcomes.isEmpty && nextOutcomes.allSatisfy({ $0.status == "success" }) {
                store.showToast(nextOutcomes.map(\.message).joined(separator: "；"))
                onClose()
            }
        } catch {
            self.error = error.localizedDescription
        }
    }
}
