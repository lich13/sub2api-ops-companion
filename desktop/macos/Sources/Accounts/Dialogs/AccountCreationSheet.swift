import AppKit
import SwiftUI
import UniformTypeIdentifiers

struct CreationOutcome: Identifiable, Sendable, Equatable {
    var index: Int
    /// success | failed | unknown
    var status: String
    var message: String
    var id: Int { index }
}

struct ResolvedIdentity: Sendable, Equatable {
    var credentials: [String: JSONValue]
    var extra: [String: JSONValue]
}

struct GeminiCatalogResponse: Decodable, Sendable {
    var ai_studio_oauth_enabled: Bool
}

public struct AccountCreationSheet: View {
    @Bindable public var store: AppStore
    public let onClose: () -> Void

    @State var step: Int = 1
    @State var draft: AccountCreationFormDraft
    @State private var initialDraft: AccountCreationFormDraft?
    @State var options: CreationOptionsReply?
    @State var optionError: String = ""
    @State var aiStudioEnabled: Bool = false
    @State var tlsProfiles: [ChoiceRecord] = []
    @State var quotaNotifyEnabled: Bool = false
    @State var webSearchEnabled: Bool = false
    @State var sourceQuery: String = ""
    @State var selectedSourceId: Int? = nil
    @State var reusing: Bool = false
    @State var reuseNotice: String = ""
    @State var authMethod: String = "manual"
    @State var authInput: String = ""
    @State var oauthSession: TokenAuthorization?
    @State var identity: ResolvedIdentity?
    @State var previewModels: [String] = []
    @State var previewSelected: Set<String> = []
    @State var modelsPreviewed: Bool = false
    @State var mixedRisk: Bool = false
    @State var mixedConfirmed: Bool = false
    @State var errorCodesConfirmed: String = ""
    @State var outcomes: [CreationOutcome] = []
    /// Outcome per submitted credential, so a sent-but-unconfirmed item is never posted twice.
    @State var ledger: [String: CreationOutcome] = [:]
    @State var scope: Int = 0
    @State private var confirmDiscard: Bool = false
    @State var busy: Bool = false
    @State var error: String = ""

    public init(
        store: AppStore,
        draft: AccountCreationFormDraft = AccountCreationFormDraft(),
        onClose: @escaping () -> Void
    ) {
        self.store = store
        self._draft = State(initialValue: draft)
        self.onClose = onClose
    }

    var catalog: AccountCreationCatalog {
        AccountCreationCatalog.shared
    }

    var currentPlatformSpec: CreationPlatformSpec? {
        catalog.platforms.first(where: { $0.id == draft.platform })
    }

    var reuseCandidates: [Account] {
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
                        && ["anthropic", "gemini"].contains(g.platform ?? ""))
            }.map { ($0.id, $0.name) }
        }
        return store.groups.filter { $0.platform == draft.platform }.map { ($0.id, $0.name) }
    }

    private var groupLongContext: Bool {
        !draft.groupIds.isEmpty && draft.groupIds.allSatisfy { id in
            options?.groups.contains(where: { $0.id == id && $0.long_context_pricing_enabled == true }) ?? false
        }
    }

    var errorRiskKey: String {
        draft.customErrorRisks.map(String.init).joined(separator: ",")
    }

    private var isDirty: Bool {
        (initialDraft.map { $0 != draft } ?? false) || !authInput.isEmpty || oauthSession != nil
    }

    private var blockedDirect: Bool {
        ledger["direct:direct"]?.status == "unknown"
    }

    var requireName: Bool {
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

    func typeDisplayName(_ raw: String) -> String {
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
}
