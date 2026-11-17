import SwiftUI

private struct AccountTemplatesConfig: Decodable, Sendable, Equatable {
    var version: String
    var configured: Bool
    var templates: [String: AccountModelProfile]
    var template_meta: [String: TemplateMeta]
    var template_order: [String]
    var template_versions: [String: String]?

    struct TemplateMeta: Decodable, Sendable, Equatable {
        var name: String
    }
}

private struct AccountTemplateTargetPreview: Decodable, Sendable {
    var account: AccountEntry?

    struct AccountEntry: Decodable, Sendable, Equatable {
        var id: Int
        var eligible: Bool
        var passthrough: Bool
        var version: String
        var config: AccountModelProfile
        var reason: String?
    }
}

private struct TemplateDraft: Sendable, Equatable {
    var name: String
    var profile: AccountModelProfile
}

public struct AccountTemplatesSheet: View {
    @Bindable public var store: AppStore
    public let targetAccount: Account?
    public let targetAccounts: [Account]?
    public let onClose: () -> Void

    @State private var saved: AccountTemplatesConfig?
    @State private var drafts: [String: TemplateDraft] = [:]
    @State private var creating: TemplateDraft?
    @State private var deleteId: String?
    @State private var selectedTemplateId: String = ""
    @State private var selectedAccountId: Int = 0
    @State private var accountConfigs: [Int: AccountTemplateTargetPreview.AccountEntry] = [:]
    @State private var accountErrors: [Int: String] = [:]
    @State private var batch: OperationBatch?
    @State private var batchError: String = ""
    @State private var pendingSignature: String = ""
    @State private var pendingRequestId: String = ""
    @State private var busy: Bool = false
    @State private var error: String = ""
    @State private var confirmDiscard: Bool = false

    public init(
        store: AppStore,
        targetAccount: Account?,
        targetAccounts: [Account]? = nil,
        onClose: @escaping () -> Void
    ) {
        self.store = store
        self.targetAccount = targetAccount
        self.targetAccounts = targetAccounts
        self.onClose = onClose
        let eligible = store.accounts.filter { $0.platform == "openai" && ["oauth", "apikey"].contains($0.type) }
        let seedId = targetAccount?.id ?? targetAccounts?.first?.id ?? eligible.first?.id ?? 0
        self._selectedAccountId = State(initialValue: seedId)
    }

    private var eligibleAccounts: [Account] {
        store.accounts.filter { $0.platform == "openai" && ["oauth", "apikey"].contains($0.type) }
    }

    private var targetIds: [Int] {
        if let targetAccounts {
            var seen = Set<Int>()
            return targetAccounts.map(\.id).filter { seen.insert($0).inserted }
        }
        return selectedAccountId > 0 ? [selectedAccountId] : []
    }

    private var targetKey: String {
        targetIds.map(String.init).joined(separator: ",")
    }

    private func isDirty(_ id: String) -> Bool {
        guard let saved,
              let draft = drafts[id],
              let meta = saved.template_meta[id],
              let orig = saved.templates[id]
        else { return false }
        return draft.name != meta.name || draft.profile != orig
    }

    private var dirtyAny: Bool {
        creating != nil || drafts.keys.contains(where: isDirty)
    }

    private var isBatchRunning: Bool {
        batch?.items.contains { ["queued", "running", "checking"].contains($0.status) } ?? false
    }

    private var readyToApply: Bool {
        !targetIds.isEmpty
            && targetIds.allSatisfy { accountConfigs[$0] != nil }
            && targetIds.contains { id in
                guard let entry = accountConfigs[id] else { return false }
                return entry.eligible && !entry.passthrough
            }
    }

    public var body: some View {
        ZStack {
            LinearModalCard(
                title: "账号模板",
                subtitle: targetAccount.map { "应用预设模型策略 · \($0.name) #\($0.id)" } ?? "管理 OpenAI 模型白名单与映射预设模板",
                width: 720,
                closeDisabled: busy,
                onClose: { requestClose(fromEscape: false) }
            ) {
                if saved == nil {
                    HStack(spacing: 8) {
                        if busy { ProgressView().controlSize(.small) }
                        Text(error.isEmpty ? "正在读取模板…" : error)
                            .font(DS.fontCompact)
                            .foregroundStyle(error.isEmpty ? DS.muted : DS.bad)
                        if !error.isEmpty {
                            Spacer()
                            Button("重新读取") {
                                Task { await reloadAll() }
                            }
                            .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                        }
                    }
                    .padding(.vertical, 18)
                } else if let saved {
                    LinearBoundedScrollView(maxHeight: 470) {
                        VStack(alignment: .leading, spacing: 14) {
                            LazyVGrid(
                                columns: [GridItem(.flexible(), spacing: 12), GridItem(.flexible(), spacing: 12)],
                                alignment: .leading,
                                spacing: 12
                            ) {
                                ForEach(saved.template_order, id: \.self) { id in
                                    if drafts[id] != nil {
                                        templateCard(id: id, saved: saved)
                                    }
                                }
                                if creating != nil {
                                    creatingCard
                                }
                            }

                            HStack(spacing: 8) {
                                Button("放弃修改") {
                                    resetDrafts(from: saved)
                                    creating = nil
                                    deleteId = nil
                                    error = ""
                                }
                                .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                                .disabled(!dirtyAny || busy || !store.state.online)

                                Button {
                                    creating = TemplateDraft(name: "", profile: AccountModelProfile())
                                } label: {
                                    Label("新增模板", systemImage: "plus")
                                }
                                .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                                .disabled(busy || !store.state.online || saved.template_order.count >= 32 || creating != nil)

                                Spacer()
                            }

                            Rectangle().fill(DS.line).frame(height: 1)

                            applySection(saved: saved)
                        }
                    }
                }

                let alertMsg = !error.isEmpty ? error : (accountErrors.values.first ?? "")
                if !alertMsg.isEmpty && saved != nil {
                    HStack(spacing: 8) {
                        Text(alertMsg)
                            .font(DS.fontCompact)
                            .foregroundStyle(DS.bad)
                        Spacer()
                        Button("重新读取") {
                            Task { await reloadAll() }
                        }
                        .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                        .disabled(busy || !store.state.online)
                    }
                }
            } footer: {
                Spacer()
                Button("关闭") {
                    requestClose(fromEscape: false)
                }
                .buttonStyle(LinearButtonStyle(variant: .secondary))
                .disabled(busy)
            }
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
                        Button("继续编辑") { confirmDiscard = false }
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
        }
        .task(id: store.state.online) {
            guard store.state.online else { return }
            await reloadAll()
        }
        .task(id: targetKey) {
            guard store.state.online else { return }
            await readAccountPreviews()
        }
    }

    private func templateCard(id: String, saved: AccountTemplatesConfig) -> some View {
        let currentDraft = drafts[id] ?? TemplateDraft(name: "", profile: AccountModelProfile())
        let dirty = isDirty(id)
        return VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 6) {
                TrimmedEntryField(
                    value: currentDraft.name,
                    prompt: "模板名称",
                    font: DS.fontCompactSemibold,
                    horizontalPadding: 8,
                    height: 30,
                    cornerRadius: 6,
                    trimWhitespace: false,
                    disabled: busy || !store.state.online
                ) { next in
                    var updated = drafts[id] ?? currentDraft
                    updated.name = next
                    drafts[id] = updated
                }

                LinearIconButton(
                    systemName: "square.and.arrow.down",
                    label: "保存\(currentDraft.name)模板",
                    size: 13,
                    boxSize: 28,
                    bordered: true,
                    disabled: busy || !store.state.online || (!dirty && saved.configured)
                ) {
                    Task { await mutateTemplate(method: "PUT", id: id) }
                }

                LinearIconButton(
                    systemName: "trash",
                    label: "删除\(currentDraft.name)模板",
                    size: 13,
                    boxSize: 28,
                    bordered: true,
                    destructive: true,
                    disabled: busy || !store.state.online
                ) {
                    deleteId = id
                }
            }

            if deleteId == id {
                HStack(spacing: 8) {
                    Text("删除“\(currentDraft.name)”？")
                        .font(DS.fontCompact)
                        .foregroundStyle(DS.bad)
                    Spacer()
                    Button("确认删除") {
                        Task { await mutateTemplate(method: "DELETE", id: id) }
                    }
                    .buttonStyle(LinearButtonStyle(variant: .danger, compact: true))
                    .disabled(busy || !store.state.online)
                    Button("取消") { deleteId = nil }
                        .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                        .disabled(busy)
                }
                .padding(8)
                .background(DS.badBg)
                .clipShape(RoundedRectangle(cornerRadius: 6))
            }

            profileFieldsEditor(
                profile: currentDraft.profile,
                labelPrefix: currentDraft.name
            ) { nextProfile in
                var updated = drafts[id] ?? currentDraft
                updated.profile = nextProfile
                drafts[id] = updated
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .topLeading)
        .background(DS.surfaceSubtle)
        .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(selectedTemplateId == id ? DS.accent : DS.line, lineWidth: 1))
        .clipShape(RoundedRectangle(cornerRadius: 8))
    }

    private var creatingCard: some View {
        let currentDraft = creating ?? TemplateDraft(name: "", profile: AccountModelProfile())
        return VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 6) {
                TrimmedEntryField(
                    value: currentDraft.name,
                    prompt: "新模板名称",
                    font: DS.fontCompactSemibold,
                    horizontalPadding: 8,
                    height: 30,
                    cornerRadius: 6,
                    borderColor: DS.accent,
                    trimWhitespace: false,
                    disabled: busy || !store.state.online
                ) { next in
                    var updated = creating ?? currentDraft
                    updated.name = next
                    creating = updated
                }

                LinearIconButton(
                    systemName: "checkmark",
                    label: "保存新模板",
                    size: 13,
                    boxSize: 28,
                    bordered: true,
                    disabled: busy || !store.state.online
                ) {
                    Task { await mutateTemplate(method: "POST", id: nil) }
                }

                LinearIconButton(
                    systemName: "xmark",
                    label: "取消新增模板",
                    size: 13,
                    boxSize: 28,
                    bordered: true,
                    disabled: busy
                ) {
                    creating = nil
                }
            }

            profileFieldsEditor(
                profile: currentDraft.profile,
                labelPrefix: currentDraft.name.isEmpty ? "新模板" : currentDraft.name
            ) { nextProfile in
                var updated = creating ?? currentDraft
                updated.profile = nextProfile
                creating = updated
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .topLeading)
        .background(DS.surfaceSubtle)
        .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(DS.accent, lineWidth: 1))
        .clipShape(RoundedRectangle(cornerRadius: 8))
    }

    private func profileFieldsEditor(
        profile: AccountModelProfile,
        labelPrefix: String,
        onUpdate: @escaping (AccountModelProfile) -> Void
    ) -> some View {
        let itemCount = profile.whitelist.count + profile.mappings.count
        return VStack(alignment: .leading, spacing: 8) {
            Text(itemCount == 0 ? "不限制模型" : "\(itemCount) 项策略")
                .font(DS.fontResetTime)
                .monospacedDigit()
                .foregroundStyle(DS.muted)

            HStack {
                Text("白名单")
                    .font(DS.fontCompactSemibold)
                    .foregroundStyle(DS.ink)
                Spacer()
                Button("+ 添加") {
                    var next = profile
                    next.whitelist.append("")
                    onUpdate(next)
                }
                .buttonStyle(LinearButtonStyle(variant: .ghost, compact: true))
                .disabled(busy || !store.state.online)
            }

            ForEach(Array(profile.whitelist.enumerated()), id: \.offset) { idx, current in
                HStack(spacing: 4) {
                    TrimmedEntryField(
                        value: current,
                        prompt: "\(labelPrefix)白名单 \(idx + 1)",
                        font: DS.fontCompact,
                        horizontalPadding: 7,
                        height: 28,
                        cornerRadius: 5,
                        disabled: busy || !store.state.online
                    ) { trimmed in
                        if profile.whitelist.indices.contains(idx) {
                            var next = profile
                            next.whitelist[idx] = trimmed
                            onUpdate(next)
                        }
                    }

                    LinearIconButton(systemName: "xmark", label: "删除白名单 \(idx + 1)", size: 11, boxSize: 24, disabled: busy) {
                        if profile.whitelist.indices.contains(idx) {
                            var next = profile
                            next.whitelist.remove(at: idx)
                            onUpdate(next)
                        }
                    }
                }
            }

            HStack {
                Text("模型映射")
                    .font(DS.fontCompactSemibold)
                    .foregroundStyle(DS.ink)
                Spacer()
                Button("+ 添加") {
                    var next = profile
                    next.mappings.append(AccountModelMappingRule())
                    onUpdate(next)
                }
                .buttonStyle(LinearButtonStyle(variant: .ghost, compact: true))
                .disabled(busy || !store.state.online)
            }

            ForEach(Array(profile.mappings.enumerated()), id: \.offset) { idx, rule in
                HStack(spacing: 4) {
                    TrimmedEntryField(
                        value: rule.source,
                        prompt: "请求模型",
                        font: DS.fontCompact,
                        horizontalPadding: 6,
                        height: 28,
                        cornerRadius: 5,
                        disabled: busy || !store.state.online
                    ) { trimmed in
                        if profile.mappings.indices.contains(idx) {
                            var next = profile
                            next.mappings[idx].source = trimmed
                            onUpdate(next)
                        }
                    }

                    Image(systemName: "arrow.right")
                        .font(DS.iconFont(10))
                        .foregroundStyle(DS.muted)

                    TrimmedEntryField(
                        value: rule.target,
                        prompt: "转发模型",
                        font: DS.fontCompact,
                        horizontalPadding: 6,
                        height: 28,
                        cornerRadius: 5,
                        disabled: busy || !store.state.online
                    ) { trimmed in
                        if profile.mappings.indices.contains(idx) {
                            var next = profile
                            next.mappings[idx].target = trimmed
                            onUpdate(next)
                        }
                    }

                    LinearIconButton(systemName: "xmark", label: "删除映射 \(idx + 1)", size: 11, boxSize: 24, disabled: busy) {
                        if profile.mappings.indices.contains(idx) {
                            var next = profile
                            next.mappings.remove(at: idx)
                            onUpdate(next)
                        }
                    }
                }
            }
        }
    }

    private func applySection(saved: AccountTemplatesConfig) -> some View {
        let selectedProfile = saved.templates[selectedTemplateId]
        let accountMenuLabel: String = {
            if targetAccounts != nil {
                return "已选择 \(targetIds.count) 个账号"
            }
            if let acc = eligibleAccounts.first(where: { $0.id == selectedAccountId }) ?? targetAccount {
                return "\(acc.name) #\(acc.id)"
            }
            return "选择 OpenAI OAuth 或 Key 账号"
        }()

        return VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text("应用到账号")
                    .font(DS.fontCompactSemibold)
                    .foregroundStyle(DS.ink)
                Spacer()
                LinearIconButton(
                    systemName: "arrow.clockwise",
                    label: "刷新模板应用预览",
                    size: 12,
                    boxSize: 28,
                    bordered: true,
                    disabled: busy || !store.state.online || isBatchRunning
                ) {
                    pendingSignature = ""
                    pendingRequestId = ""
                    batch = nil
                    batchError = ""
                    Task { await reloadAll() }
                }
            }

            HStack(spacing: 8) {
                LinearSelect(
                    label: accountMenuLabel,
                    accessibilityTitle: "选择应用账号",
                    height: DS.controlHeightCompact,
                    expandWidth: true,
                    compactFont: true,
                    disabled: busy || !store.state.online || targetAccount != nil || targetAccounts != nil || isBatchRunning,
                    entries: [
                        LinearMenuEntry(id: "0", title: "选择 OpenAI OAuth 或 Key 账号", checked: selectedAccountId == 0) {
                            selectedAccountId = 0
                            batch = nil
                        },
                    ] + eligibleAccounts.map { acc in
                        LinearMenuEntry(id: "\(acc.id)", title: "\(acc.name) #\(acc.id)", checked: selectedAccountId == acc.id) {
                            selectedAccountId = acc.id
                            batch = nil
                        }
                    }
                )

                LinearSelect(
                    label: saved.template_meta[selectedTemplateId]?.name ?? "选择账号模板",
                    accessibilityTitle: "选择账号模板",
                    height: DS.controlHeightCompact,
                    width: 200,
                    compactFont: true,
                    disabled: busy || !store.state.online || isBatchRunning || saved.template_order.isEmpty,
                    entries: saved.template_order.map { tplId in
                        LinearMenuEntry(
                            id: tplId,
                            title: saved.template_meta[tplId]?.name ?? tplId,
                            checked: selectedTemplateId == tplId
                        ) {
                            selectedTemplateId = tplId
                            batch = nil
                        }
                    }
                )

                Button("应用模板") {
                    Task { await applySelectedTemplate(saved: saved) }
                }
                .buttonStyle(LinearButtonStyle(variant: .primary))
                .disabled(
                    busy
                        || !store.state.online
                        || !readyToApply
                        || !saved.configured
                        || selectedProfile == nil
                        || isDirty(selectedTemplateId)
                        || isBatchRunning
                )
            }

            ForEach(targetIds, id: \.self) { id in
                let accName = store.accounts.first(where: { $0.id == id })?.name ?? targetAccount?.name ?? "账号"
                let entry = accountConfigs[id]
                let job = batch?.items.first(where: { $0.account_id == id })

                VStack(alignment: .leading, spacing: 4) {
                    HStack {
                        Text("\(accName) #\(id)")
                            .font(DS.fontCompactSemibold)
                            .monospacedDigit()
                            .foregroundStyle(DS.ink)
                        Spacer()
                        if let job {
                            Text(job.status)
                                .font(DS.fontPriorityMeta)
                                .foregroundStyle(job.status == "failed" ? DS.bad : DS.muted)
                        }
                    }
                    if let entry {
                        if let selectedProfile {
                            let unchanged = entry.config == selectedProfile ? "（无变化）" : ""
                            Text("当前：\(entry.config.policySummary)\n目标：\(selectedProfile.policySummary)\(unchanged)")
                                .font(DS.fontResetTime)
                                .foregroundStyle(DS.muted)
                        }
                        if !entry.eligible || entry.passthrough {
                            Text(entry.reason ?? (entry.passthrough ? "透传模式会绕过模型限制，不能自动修改。" : "此账号不支持独立应用模板。"))
                                .font(DS.fontResetTime)
                                .foregroundStyle(DS.bad)
                        }
                    } else {
                        Text(accountErrors[id] ?? "正在读取当前策略…")
                            .font(DS.fontResetTime)
                            .foregroundStyle(accountErrors[id] == nil ? DS.muted : DS.bad)
                    }
                    if let reason = job?.reason, !reason.isEmpty {
                        Text(reason)
                            .font(DS.fontResetTime)
                            .foregroundStyle(job?.status == "failed" || job?.status == "needs_confirmation" ? DS.bad : DS.muted)
                    }
                }
                .padding(10)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(DS.surfaceSubtle)
                .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(DS.line, lineWidth: 1))
                .clipShape(RoundedRectangle(cornerRadius: 6))
            }

            if !batchError.isEmpty {
                Text(batchError)
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.bad)
            }
        }
    }

    private func requestClose(fromEscape: Bool) {
        guard !busy else { return }
        if confirmDiscard {
            confirmDiscard = false
            return
        }
        if deleteId != nil {
            deleteId = nil
            return
        }
        if dirtyAny {
            if !fromEscape { confirmDiscard = true }
            return
        }
        onClose()
    }

    private func resetDrafts(from value: AccountTemplatesConfig) {
        var map: [String: TemplateDraft] = [:]
        for id in value.template_order {
            if let meta = value.template_meta[id], let prof = value.templates[id] {
                map[id] = TemplateDraft(name: meta.name, profile: prof)
            }
        }
        drafts = map
        if value.templates[selectedTemplateId] == nil {
            selectedTemplateId = value.template_order.first ?? ""
        }
    }

    private func reloadAll() async {
        await loadConfig()
        await readAccountPreviews()
    }

    private func loadConfig() async {
        guard store.state.online else { return }
        busy = true
        error = ""
        defer { busy = false }
        do {
            let loaded: AccountTemplatesConfig = try await RustBridge.shared.api("GET", "/account-templates")
            saved = loaded
            resetDrafts(from: loaded)
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func readAccountPreviews() async {
        guard store.state.online else { return }
        var nextConfigs: [Int: AccountTemplateTargetPreview.AccountEntry] = [:]
        var nextErrors: [Int: String] = [:]
        for id in targetIds {
            do {
                let res: AccountTemplateTargetPreview = try await RustBridge.shared.api("GET", "/account-templates?account_id=\(id)")
                if let entry = res.account {
                    nextConfigs[id] = entry
                } else {
                    nextErrors[id] = "账号配置读取失败"
                }
            } catch {
                nextErrors[id] = error.localizedDescription
            }
        }
        accountConfigs = nextConfigs
        accountErrors = nextErrors
    }

    private func mutateTemplate(method: String, id: String?) async {
        guard let currentSaved = saved, !busy, store.state.online else { return }
        let draftValue = id.flatMap { drafts[$0] } ?? creating
        if method != "DELETE" {
            let trimmedName = draftValue?.name.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
            if trimmedName.isEmpty || trimmedName.count > 40 {
                error = "模板名称需为 1–40 个字符"
                return
            }
            if let prof = draftValue?.profile {
                let invalid = AccountModelProfile.validate(prof)
                if !invalid.isEmpty {
                    error = invalid
                    return
                }
            }
        }
        busy = true
        error = ""
        defer { busy = false }
        var payload: [String: JSONValue] = [
            "expected_version": .string(currentSaved.version),
        ]
        if method != "DELETE", let draftValue {
            payload["name"] = .string(draftValue.name.trimmingCharacters(in: .whitespacesAndNewlines))
            payload["whitelist"] = .array(draftValue.profile.whitelist.map(JSONValue.string))
            payload["mappings"] = .array(draftValue.profile.mappings.map {
                .object(["source": .string($0.source), "target": .string($0.target)])
            })
        }
        do {
            let path = id.map { "/account-templates/\($0)" } ?? "/account-templates"
            let updated: AccountTemplatesConfig = try await RustBridge.shared.api(method, path, body: .object(payload))
            let previousIds = Set(currentSaved.template_order)
            saved = updated
            resetDrafts(from: updated)
            if method == "POST" {
                if let createdId = updated.template_order.first(where: { !previousIds.contains($0) }) {
                    selectedTemplateId = createdId
                }
                creating = nil
            }
            if method == "DELETE" {
                deleteId = nil
            }
            store.showToast(method == "DELETE" ? "模板已删除" : "模板已保存")
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func applySelectedTemplate(saved: AccountTemplatesConfig) async {
        guard !busy, store.state.online, readyToApply, !selectedTemplateId.isEmpty else { return }
        busy = true
        error = ""
        batchError = ""
        defer { busy = false }

        let tplVer = saved.template_versions?[selectedTemplateId] ?? saved.version
        let accountsPayload: [JSONValue] = targetIds.compactMap { id in
            guard let cfg = accountConfigs[id] else { return nil }
            return .object([
                "account_id": .int(Int64(id)),
                "expected_version": .string(cfg.version),
            ])
        }
        let clientClean = store.operations.clientId().replacingOccurrences(of: "-", with: "")
        let sig = "\(selectedTemplateId):\(tplVer):\(targetKey)"
        if pendingSignature != sig || pendingRequestId.isEmpty {
            pendingSignature = sig
            pendingRequestId = UUID().uuidString.replacingOccurrences(of: "-", with: "").lowercased()
        }

        let body: JSONValue = .object([
            "template_id": .string(selectedTemplateId),
            "template_version": .string(tplVer),
            "accounts": .array(accountsPayload),
            "client_id": .string(clientClean),
            "request_id": .string(pendingRequestId),
        ])

        do {
            let initialBatch: OperationBatch = try await RustBridge.shared.api("POST", "/account-templates/apply", body: body)
            batch = initialBatch
            let finalBatch = try await store.operations.followBatch(initial: initialBatch) { updated in
                batch = updated
            }
            store.refresh()
            if !finalBatch.items.isEmpty && finalBatch.items.allSatisfy({ $0.status == "completed" }) {
                store.showToast("模板已应用")
                onClose()
            }
        } catch {
            self.error = error.localizedDescription
        }
    }
}
