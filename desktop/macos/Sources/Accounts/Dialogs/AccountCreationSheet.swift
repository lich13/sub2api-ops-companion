import AppKit
import SwiftUI
import UniformTypeIdentifiers

private struct CreationOutcome: Identifiable, Sendable, Equatable {
    var index: Int
    /// success | failed | unknown
    var status: String
    var message: String
    var id: Int { index }
}

private struct ResolvedIdentity: Sendable, Equatable {
    var credentials: [String: JSONValue]
    var extra: [String: JSONValue]
}

private struct GeminiCatalogResponse: Decodable, Sendable {
    var ai_studio_oauth_enabled: Bool
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
    @State private var draft: AccountCreationFormDraft
    @State private var initialDraft: AccountCreationFormDraft?
    @State private var options: CreationOptionsResponse?
    @State private var optionError: String = ""
    @State private var aiStudioEnabled: Bool = false
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
    @State private var identity: ResolvedIdentity?
    @State private var previewModels: [String] = []
    @State private var previewSelected: Set<String> = []
    @State private var modelsPreviewed: Bool = false
    @State private var mixedRisk: Bool = false
    @State private var mixedConfirmed: Bool = false
    @State private var errorCodesConfirmed: String = ""
    @State private var outcomes: [CreationOutcome] = []
    /// Outcome per submitted credential, so a sent-but-unconfirmed item is never posted twice.
    @State private var ledger: [String: CreationOutcome] = [:]
    @State private var scope: Int = 0
    @State private var confirmDiscard: Bool = false
    @State private var busy: Bool = false
    @State private var error: String = ""

    public init(
        store: AppStore,
        draft: AccountCreationFormDraft = AccountCreationFormDraft(),
        onClose: @escaping () -> Void
    ) {
        self.store = store
        self._draft = State(initialValue: draft)
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

    private var groupLongContext: Bool {
        !draft.groupIds.isEmpty && draft.groupIds.allSatisfy { id in
            options?.groups?.contains(where: { $0.id == id && $0.long_context_pricing_enabled == true }) ?? false
        }
    }

    private var errorRiskKey: String {
        draft.customErrorRisks.map(String.init).joined(separator: ",")
    }

    private var isDirty: Bool {
        (initialDraft.map { $0 != draft } ?? false) || !authInput.isEmpty || oauthSession != nil
    }

    private var blockedDirect: Bool {
        ledger["direct:direct"]?.status == "unknown"
    }

    private var requireName: Bool {
        !(draft.platform == "grok" && draft.type == "oauth" && (step == 1 || authMethod == "grok-sso"))
    }

    private var authScopeKey: [String] {
        [
            draft.proxyId.map(String.init) ?? "",
            draft.values["credentials.oauth_type"]?.stringValue ?? "",
            draft.values["credentials.project_id"]?.stringValue ?? "",
            draft.values["credentials.tier_id"]?.stringValue ?? "",
        ]
    }

    private var previewScopeKey: [String] {
        [draft.apiKey, draft.values["credentials.base_url"]?.stringValue ?? ""]
            + draft.protocolURLs.keys.sorted().map { "\($0)=\(draft.protocolURLs[$0] ?? "")" }
    }

    private func typeDisplayName(_ raw: String) -> String {
        if draft.platform == "antigravity" && raw == "apikey" {
            return "Upstream"
        }
        return PlatformMeta.typeLabel(raw)
    }

    public var body: some View {
        ZStack {
            creationCard
            if confirmDiscard {
                discardConfirmCard
            }
        }
        .onAppear {
            if initialDraft == nil { initialDraft = draft }
        }
        .onChange(of: authScopeKey) { _, _ in clearAuth() }
        .onChange(of: previewScopeKey) { _, _ in modelsPreviewed = false }
        .task(id: store.state.online) {
            guard store.state.online else { return }
            await loadOptions()
        }
        .task(id: "\(draft.platform):\(draft.type):\(store.state.online)") {
            await loadAntigravityDefaults()
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
        guard !busy else { return }
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

    private var creationCard: some View {
        LinearModalCard(
            title: "添加账号",
            subtitle: step == 1 ? "1 · 基本信息与配置" : "2 · 凭据与授权",
            width: 668,
            closeDisabled: busy,
            onClose: { requestClose(fromEscape: false) }
        ) {
            LinearBoundedScrollView(maxHeight: 590) {
                VStack(alignment: .leading, spacing: 12) {
                    if !optionError.isEmpty {
                        HStack(spacing: 8) {
                            Text(optionError)
                                .font(DS.fontCompact)
                                .foregroundStyle(DS.bad)
                            Spacer()
                            Button("重新读取选项") {
                                Task { await loadOptions() }
                            }
                            .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                            .disabled(busy || !store.state.online)
                        }
                    } else if options == nil {
                        Text("正在读取账号选项…")
                            .font(DS.fontCompact)
                            .foregroundStyle(DS.muted)
                    }

                    VStack(alignment: .leading, spacing: 12) {
                        if step == 1 {
                            stepOneBody
                        } else {
                            stepTwoBody
                        }
                    }
                    .disabled(busy || !store.state.online)

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

                    if !draft.customErrorRisks.isEmpty {
                        VStack(alignment: .leading, spacing: 8) {
                            ForEach(draft.customErrorRisks, id: \.self) { code in
                                Text("将 \(String(code)) 加入自定义错误码后，该错误会停止账号调度，并覆盖原生的\(code == 429 ? "临时限流" : "临时过载")处理。")
                                    .font(DS.fontCompact)
                                    .foregroundStyle(DS.warn)
                                    .fixedSize(horizontal: false, vertical: true)
                            }
                            LinearCheckbox(
                                isChecked: errorCodesConfirmed == errorRiskKey,
                                label: "确认这些错误码停止账号调度",
                                disabled: busy
                            ) {
                                errorCodesConfirmed = errorCodesConfirmed == errorRiskKey ? "" : errorRiskKey
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
                                    .foregroundStyle(item.status == "success" ? DS.good : (item.status == "unknown" ? DS.warn : DS.bad))
                                    .fixedSize(horizontal: false, vertical: true)
                                    .textSelection(.enabled)
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
                    .fixedSize(horizontal: false, vertical: true)
            }
            if blockedDirect {
                Text("请刷新核对账号是否已经创建，再关闭此窗口。")
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.warn)
            }
        } footer: {
            Button(step == 2 ? "上一步" : "取消") {
                if step == 2 {
                    step = 1
                    error = ""
                } else {
                    requestClose(fromEscape: false)
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
            .disabled(!store.state.online || busy || reusing || options == nil || blockedDirect || (mixedRisk && !mixedConfirmed))
        }
        .sub2opsDialogFocus {
            requestClose(fromEscape: true)
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
                            },
                        ] + proxies.map { p in
                            let suffix = (p.status != nil && p.status != "active") ? "（未启用）" : ""
                            return LinearMenuEntry(id: "\(p.id)", title: "\(p.name)\(suffix)", checked: draft.proxyId == p.id) {
                                draft.proxyId = p.id
                            }
                        }
                    )
                }

                labeledTextField("并发", text: $draft.concurrency, prompt: "10")
                labeledTextField("负载因子", text: $draft.loadFactor, prompt: "跟随并发数")
                labeledTextField("优先级", text: $draft.priority, prompt: "1")
                labeledTextField("计费倍率", text: $draft.rateMultiplier, prompt: "1")
                labeledTextField("到期时间（本地时区）", text: $draft.expiresAtText, prompt: "YYYY-MM-DD HH:mm，可留空")
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
                webSearchEnabled: webSearchEnabled,
                aiStudioEnabled: aiStudioEnabled,
                groupLongContext: groupLongContext
            )

            if draft.platform == "anthropic" && draft.type == "apikey" {
                Text("全局 Web Search 模拟：\(webSearchEnabled ? "开启" : "关闭")")
                    .font(DS.fontResetTime)
                    .foregroundStyle(DS.muted)
            }
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
                                clearAuth()
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

                if ["codex-session", "agent-identity"].contains(authMethod) {
                    Button {
                        if let text = readCredentialFiles(multiple: true) { authInput = text }
                    } label: {
                        Label("导入凭据文件", systemImage: "doc.badge.plus")
                    }
                    .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                }
            }
            .onChange(of: authInput) { _, _ in identity = nil }
        } else if draft.type == "service_account" && !draft.isVertexApiKey {
            VStack(alignment: .leading, spacing: 6) {
                HStack {
                    Text("Service Account JSON")
                        .font(DS.fontCompactMedium)
                        .foregroundStyle(DS.muted)
                    Spacer()
                    Button {
                        if let text = readCredentialFiles(multiple: false) { draft.serviceAccountJSON = text }
                    } label: {
                        Label("选择 JSON 文件", systemImage: "doc.badge.plus")
                    }
                    .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                }
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
                        .disabled(draft.apiKey.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
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

    private func clearAuth() {
        oauthSession = nil
        identity = nil
    }

    private func readCredentialFiles(multiple: Bool) -> String? {
        let panel = NSOpenPanel()
        panel.allowedContentTypes = multiple ? [.json, .plainText] : [.json]
        panel.allowsMultipleSelection = multiple
        guard panel.runModal() == .OK else { return nil }
        var total = 0
        var texts: [String] = []
        for url in panel.urls {
            guard let data = try? Data(contentsOf: url), let text = String(data: data, encoding: .utf8) else {
                error = "凭据文件读取失败"
                return nil
            }
            total += data.count
            if total > 1_000_000 {
                error = "凭据文件过大"
                return nil
            }
            texts.append(text)
        }
        error = ""
        return texts.joined(separator: "\n")
    }

    private func switchKind(platform: String, type: String?) {
        let savedName = draft.name
        let savedNotes = draft.notes
        scope &+= 1
        if let type {
            draft.applyPlatformDefaults(for: platform, authType: type)
        } else {
            draft.selectPlatform(platform)
        }
        draft.name = savedName
        draft.notes = savedNotes
        selectedSourceId = reuseCandidates.count == 1 ? reuseCandidates[0].id : nil
        sourceQuery = ""
        reuseNotice = ""
        reusing = false
        busy = false
        authMethod = "manual"
        authInput = ""
        clearAuth()
        outcomes = []
        ledger = [:]
        error = ""
        mixedRisk = false
        mixedConfirmed = false
        errorCodesConfirmed = ""
        previewModels = []
        modelsPreviewed = false
        step = 1
    }

    private func loadOptions() async {
        optionError = ""
        var partial = false
        do {
            let opt: CreationOptionsResponse = try await RustBridge.shared.api("GET", "/account-creation/options")
            options = opt
        } catch {
            optionError = "账号选项读取失败，请重试"
        }
        do {
            let tls: [CreationTLSProfileOption] = try await RustBridge.shared.api("GET", "/account-creation/catalog/tls")
            tlsProfiles = tls
        } catch { partial = true }
        do {
            let gemini: GeminiCatalogResponse = try await RustBridge.shared.api("GET", "/account-creation/catalog/gemini")
            aiStudioEnabled = gemini.ai_studio_oauth_enabled
        } catch { partial = true }
        do {
            let settingsObj: [String: JSONValue] = try await RustBridge.shared.api("GET", "/account-creation/catalog/settings")
            quotaNotifyEnabled = settingsObj["account_quota_notify_enabled"]?.boolValue ?? false
        } catch { partial = true }
        do {
            let wsObj: [String: JSONValue] = try await RustBridge.shared.api("GET", "/account-creation/catalog/web-search")
            webSearchEnabled = wsObj["enabled"]?.boolValue ?? false
        } catch { partial = true }
        if partial && optionError.isEmpty {
            optionError = "部分高级选项读取失败，请重试以读取完整选项"
        }
        if selectedSourceId == nil, reuseCandidates.count == 1 {
            selectedSourceId = reuseCandidates[0].id
        }
    }

    /// Antigravity ships its default model mapping from the server; fill it while the
    /// mapping is still empty for the platform/type that requested it.
    private func loadAntigravityDefaults() async {
        guard draft.platform == "antigravity", store.state.online, draft.mappings.isEmpty else { return }
        let current = scope
        do {
            let map: [String: String] = try await RustBridge.shared.api("GET", "/account-creation/catalog/antigravity-models")
            guard current == scope, draft.platform == "antigravity", draft.mappings.isEmpty else { return }
            draft.mappings = map.keys.sorted().map { KeyValuePairDraft(key: $0, value: map[$0] ?? "") }
        } catch {
            if current == scope {
                self.error = "默认模型映射读取失败，可手动填写或切换平台后重试"
            }
        }
    }

    private func reuseConfiguration() async {
        guard let sourceId = selectedSourceId, store.state.online, !reusing, !busy else { return }
        let current = scope
        reusing = true
        error = ""
        reuseNotice = ""
        defer { if current == scope { reusing = false } }
        do {
            let sourceObj: [String: JSONValue] = try await RustBridge.shared.api("GET", "/account-creation/source/\(sourceId)")
            guard current == scope, selectedSourceId == sourceId else { return }
            try draft.applyReusedSource(sourceObj)
            mixedRisk = false
            mixedConfirmed = false
            errorCodesConfirmed = ""
            modelsPreviewed = false
            reuseNotice = "已复用配置，可继续修改；账号名称与新填凭据已保留"
        } catch {
            if current == scope { self.error = error.localizedDescription }
        }
    }

    private func generateAuthorizationURL() async {
        guard store.state.online, !busy else { return }
        let current = scope
        busy = true
        error = ""
        clearAuth()
        defer { if current == scope { busy = false } }
        do {
            let res: OAuthAuthorizationSession = try await RustBridge.shared.api(
                "POST",
                "/account-creation/auth/\(draft.authProvider)/authorize",
                body: .object(draft.authConfigBody)
            )
            guard current == scope else { return }
            var resolved = res
            if (resolved.state ?? "").isEmpty,
               let comps = URLComponents(string: res.auth_url),
               let st = comps.queryItems?.first(where: { $0.name == "state" })?.value
            {
                resolved.state = st
            }
            oauthSession = resolved
        } catch {
            if current == scope { self.error = error.localizedDescription }
        }
    }

    private func previewUpstreamModels() async {
        guard store.state.online, !busy else { return }
        let current = scope
        busy = true
        error = ""
        defer { if current == scope { busy = false } }
        do {
            let payload = try draft.buildPayload(requireCredentials: true, requireName: false)
            let creds = payload["credentials"]?.objectValue ?? [:]
            var body: [String: JSONValue] = [
                "platform": .string(draft.platform),
                "type": .string(draft.type),
            ]
            for key in ["base_url", "api_key", "model_mapping"] {
                if let value = creds[key], value != .null { body[key] = value }
            }
            let res: PreviewModelsResponse = try await RustBridge.shared.api(
                "POST",
                "/account-creation/models",
                body: .object(body)
            )
            guard current == scope else { return }
            modelsPreviewed = true
            previewModels = res.models
            previewSelected = Set(res.models)
            if let warnings = res.warnings, !warnings.isEmpty {
                error = warnings.joined(separator: "；")
            }
        } catch {
            if current == scope { self.error = error.localizedDescription }
        }
    }

    private func authorizedIdentity(item: String) async throws -> ResolvedIdentity {
        if authMethod == "manual", let identity {
            return identity
        }
        var body = draft.authConfigBody
        body.removeValue(forKey: "project_id")
        let action: String
        if authMethod == "manual" {
            guard let oauthSession else {
                throw RustBridgeError("请先生成授权链接")
            }
            var code = item.trimmingCharacters(in: .whitespacesAndNewlines)
            var state = oauthSession.state
            if let comps = URLComponents(string: code), comps.scheme != nil, comps.host != nil {
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
        let result = ResolvedIdentity(credentials: resolved.credentials, extra: resolved.extra)
        if authMethod == "manual" {
            identity = result
        }
        return result
    }

    private func checkMixedChannel() async throws -> Bool {
        if mixedConfirmed || !["anthropic", "antigravity"].contains(draft.platform) { return true }
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
            return false
        }
        return true
    }

    private func submitAction() async {
        guard store.state.online, !busy, !reusing, options != nil else { return }
        let problem = draft.validate(requireCredentials: step == 2 && !draft.isOAuthFlow, requireName: requireName)
        if !problem.isEmpty {
            error = problem
            return
        }
        if !errorRiskKey.isEmpty && errorCodesConfirmed != errorRiskKey {
            error = "请先确认自定义错误码对调度的影响"
            return
        }

        let current = scope
        busy = true
        error = ""
        defer { if current == scope { busy = false } }

        do {
            guard try await checkMixedChannel() else { return }
            guard current == scope else { return }
            if step == 1 {
                step = 2
                return
            }

            let isNativeImport = draft.isOAuthFlow && ["codex-session", "agent-identity", "codex-pat", "grok-sso"].contains(authMethod)
            let items: [String]
            if !draft.isOAuthFlow {
                items = ["direct"]
            } else if ["manual", "codex-session", "agent-identity", "codex-pat"].contains(authMethod) {
                items = [authInput.trimmingCharacters(in: .whitespacesAndNewlines)]
            } else {
                items = authInput
                    .components(separatedBy: .newlines)
                    .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
                    .filter { !$0.isEmpty }
            }
            guard !items.isEmpty, !items.contains(where: \.isEmpty) else {
                error = "请填写凭据"
                return
            }

            let baseName = draft.name.trimmingCharacters(in: .whitespacesAndNewlines)
            var nextOutcomes: [CreationOutcome] = []
            for (idx, item) in items.enumerated() {
                guard current == scope else { break }
                let key = "\(draft.isOAuthFlow ? authMethod : "direct"):\(item)"
                if let previous = ledger[key], previous.status != "failed" {
                    nextOutcomes.append(CreationOutcome(index: idx + 1, status: previous.status, message: previous.message))
                    continue
                }
                var sent = false
                let outcome: CreationOutcome
                do {
                    var itemDraft = draft
                    if items.count > 1 && !baseName.isEmpty {
                        itemDraft.name = "\(draft.name) #\(idx + 1)"
                    }
                    var detail = "已创建"
                    var partial = false
                    if isNativeImport {
                        var body = try itemDraft.importParameters()
                        sent = true
                        if authMethod == "grok-sso" {
                            body["credentials"] = body.removeValue(forKey: "credential_extras") ?? .object([:])
                            body["sso_tokens"] = .array([.string(item)])
                            let res: [String: JSONValue] = try await RustBridge.shared.api("POST", "/account-creation/import/grok-sso", body: .object(body))
                            if !(res["failed"]?.arrayValue ?? []).isEmpty {
                                sent = false
                                throw RustBridgeError("SSO 导入失败，请检查凭据后重试")
                            }
                            guard let first = res["created"]?.arrayValue?.first else {
                                throw RustBridgeError("SSO 导入未返回创建结果")
                            }
                            detail = "已创建 #\(first["account"]?["id"]?.intValue ?? 0)"
                        } else if authMethod == "codex-pat" {
                            body["access_token"] = .string(item)
                            let res: CreationCreatedResponse = try await RustBridge.shared.api("POST", "/account-creation/import/codex-pat", body: .object(body))
                            detail = "已创建 #\(res.account.id)"
                        } else {
                            body["content"] = .string(item)
                            body["update_existing"] = .bool(true)
                            let res: [String: JSONValue] = try await RustBridge.shared.api("POST", "/account-creation/import/codex-session", body: .object(body))
                            let failed = res["failed"]?.intValue ?? 0
                            detail = "创建 \(res["created"]?.intValue ?? 0)，更新 \(res["updated"]?.intValue ?? 0)，跳过 \(res["skipped"]?.intValue ?? 0)，失败 \(failed)"
                            let failedIndexes = (res["errors"]?.arrayValue ?? []).compactMap { $0["index"]?.intValue }
                            if !failedIndexes.isEmpty {
                                detail += "；失败项 \(failedIndexes.map { String($0) }.joined(separator: "、"))，请仅填入失败项重试"
                            }
                            let warnings = (res["warnings"]?.arrayValue ?? []).map {
                                "#\($0["index"]?.intValue ?? 0) \($0["message"]?.stringValue ?? "")"
                            }
                            if !warnings.isEmpty {
                                detail += "；\(warnings.joined(separator: "；"))"
                            }
                            partial = failed > 0
                        }
                    } else {
                        let auth = draft.isOAuthFlow ? try await authorizedIdentity(item: item) : nil
                        guard current == scope else { break }
                        var payload = try itemDraft.buildPayload(
                            identityCredentials: auth?.credentials,
                            identityExtra: auth?.extra,
                            requireCredentials: true,
                            requireName: true
                        )
                        if mixedConfirmed {
                            payload["confirm_mixed_channel_risk"] = .bool(true)
                        }
                        sent = true
                        let path = modelsPreviewed ? "/account-creation?sync_models=true" : "/account-creation"
                        let created: CreationCreatedResponse = try await RustBridge.shared.api("POST", path, body: .object(payload))
                        let warnings = created.warnings ?? []
                        detail = "已创建 #\(created.account.id)\(warnings.isEmpty ? "" : "；\(warnings.joined(separator: "；"))")"
                    }
                    outcome = CreationOutcome(index: idx + 1, status: partial ? "unknown" : "success", message: detail)
                } catch {
                    let text = error.localizedDescription
                    // A request that left the client without a definite 4xx answer may have created the account.
                    let unknown = sent && text.range(of: "\\[4\\d\\d\\]", options: .regularExpression) == nil
                    if text.contains("mixed_channel_warning") {
                        mixedRisk = true
                        mixedConfirmed = false
                    }
                    outcome = CreationOutcome(
                        index: idx + 1,
                        status: unknown ? "unknown" : "failed",
                        message: unknown ? "结果未知，请刷新账号列表核对。此项不会重复提交。" : text
                    )
                }
                ledger[key] = outcome
                nextOutcomes.append(outcome)
                if current == scope { outcomes = nextOutcomes }
            }

            guard current == scope else { return }
            outcomes = nextOutcomes
            if nextOutcomes.contains(where: { $0.status != "failed" }) {
                store.refresh()
            }
            if !nextOutcomes.isEmpty && nextOutcomes.allSatisfy({ $0.status == "success" }) {
                store.showToast(nextOutcomes.map(\.message).joined(separator: "；"))
                onClose()
            }
        } catch {
            if current == scope { self.error = error.localizedDescription }
        }
    }
}
