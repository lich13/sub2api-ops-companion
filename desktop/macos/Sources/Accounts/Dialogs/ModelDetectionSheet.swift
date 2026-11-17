import SwiftUI

/// Matches `desktop/src/ModelDetectionDialog.tsx` (`schedule_mode: "smart" | "fixed"`, `last_automatic_started_at`, API-key `model_override`).
public struct ModelDetectionSheet: View {
    public let account: Account
    @Bindable public var store: AppStore
    public let onClose: () -> Void

    private static let statusLabels: [String: String] = [
        "disabled": "未启用定时检测",
        "waiting": "等待下次检测",
        "paused": "已暂停",
        "queued": "已排队",
        "running": "检测中",
        "handling": "正在处置",
        "completed": "已完成",
        "failed": "失败",
        "cancelled": "已取消",
        "interrupted": "已中断",
        "needs_confirmation": "需确认",
    ]

    private static let notificationLabels: [String: String] = [
        "queued": "待推送",
        "retry": "待重试",
        "delivered": "已推送",
        "suppressed": "已取消",
        "failed": "投递失败",
    ]

    @State private var enabled: Bool = false
    @State private var scheduleMode: String = "smart"
    @State private var fixedIntervalText: String = "60"
    @State private var smartMinIntervalText: String = "5"
    @State private var modelOverride: String = ""
    @State private var globalModelId: String = ""
    @State private var loadedModelId: String = ""
    @State private var serverModelId: String = ""
    @State private var candidateModels: [AccountModelItem] = []
    @State private var modelsReady: Bool = false
    @State private var loadingModels: Bool = false
    @State private var version: String = ""
    @State private var serverState: [String: JSONValue] = [:]
    @State private var loading: Bool = true
    @State private var saving: Bool = false
    @State private var error: String = ""

    public init(account: Account, store: AppStore, onClose: @escaping () -> Void) {
        self.account = account
        self.store = store
        self.onClose = onClose
    }

    private var isKeyAccount: Bool {
        account.type == "apikey"
    }

    private var isSmartMode: Bool {
        scheduleMode == "smart"
    }

    private var parsedFixedMinutes: Int? {
        guard let v = Int(fixedIntervalText.trimmingCharacters(in: .whitespacesAndNewlines)), v >= 1 else { return nil }
        return v
    }

    private var parsedSmartMinutes: Int? {
        guard let v = Int(smartMinIntervalText.trimmingCharacters(in: .whitespacesAndNewlines)), (5...30).contains(v) else { return nil }
        return v
    }

    private var invalidInterval: Bool {
        isSmartMode ? (parsedSmartMinutes == nil) : (parsedFixedMinutes == nil)
    }

    private var effectiveSelectedModel: String {
        if isKeyAccount {
            return modelOverride.isEmpty ? globalModelId : modelOverride
        }
        return serverModelId.isEmpty ? globalModelId : serverModelId
    }

    private var modelAvailable: Bool {
        !effectiveSelectedModel.isEmpty && candidateModels.contains(where: { $0.id == effectiveSelectedModel })
    }

    private var modelChangedExternally: Bool {
        !loadedModelId.isEmpty && !serverModelId.isEmpty && loadedModelId != serverModelId && !isKeyAccount
    }

    private var lastDetectionDisplay: String {
        if let raw = serverState["last_automatic_started_at"] {
            if let s = raw.stringValue {
                return s.isEmpty ? "时间未知" : BeijingTime.fullTime(s)
            }
        }
        if serverState["last_result"] != nil && serverState["last_result"] != .null {
            return "时间未知"
        }
        return "暂无记录"
    }

    public var body: some View {
        let activeJobId = serverState["job_id"]?.stringValue ?? ""
        let rawStatus = serverState["status"]?.stringValue ?? (enabled ? "waiting" : "disabled")

        LinearModalCard(
            title: "定时检测",
            subtitle: "\(account.name) #\(account.id)",
            width: 520,
            closeDisabled: saving,
            onClose: onClose
        ) {
            if loading && serverState.isEmpty {
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                    Text("正在读取检测设置…")
                        .font(DS.fontCompact)
                        .foregroundStyle(DS.muted)
                }
                .padding(.vertical, 16)
            } else {
                HStack {
                    Text("启用定时检测")
                        .font(DS.fontBodyMedium)
                        .foregroundStyle(DS.ink)
                    Spacer()
                    LinearSwitch(isOn: enabled, disabled: !store.state.online || saving, label: "启用定时检测") {
                        enabled.toggle()
                    }
                }
                .padding(12)
                .background(DS.surfaceSubtle)
                .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))

                HStack(alignment: .top, spacing: 12) {
                    VStack(alignment: .leading, spacing: 6) {
                        Text("检测模式")
                            .font(DS.fontCompactMedium)
                            .foregroundStyle(DS.muted)
                        LinearSelect(
                            selection: $scheduleMode,
                            options: [
                                ("smart", "智能"),
                                ("fixed", "固定"),
                            ],
                            accessibilityTitle: "检测模式",
                            height: DS.controlHeight,
                            expandWidth: true,
                            disabled: saving || !store.state.online
                        )
                    }
                    .frame(maxWidth: .infinity)

                    VStack(alignment: .leading, spacing: 6) {
                        Text(isSmartMode ? "最小间隔（分钟）" : "检测间隔（分钟）")
                            .font(DS.fontCompactMedium)
                            .foregroundStyle(DS.muted)
                        if isSmartMode {
                            TextField(text: $smartMinIntervalText, prompt: Text(verbatim: "5–30")) { EmptyView() }
                                .textFieldStyle(.plain)
                                .font(DS.fontBody)
                                .monospacedDigit()
                                .padding(.horizontal, 10)
                                .frame(height: DS.controlHeight)
                                .background(DS.surface)
                                .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(invalidInterval ? DS.bad : DS.line, lineWidth: 1))
                                .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
                                .disabled(saving || !store.state.online)
                        } else {
                            TextField(text: $fixedIntervalText, prompt: Text(verbatim: "≥ 1")) { EmptyView() }
                                .textFieldStyle(.plain)
                                .font(DS.fontBody)
                                .monospacedDigit()
                                .padding(.horizontal, 10)
                                .frame(height: DS.controlHeight)
                                .background(DS.surface)
                                .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(invalidInterval ? DS.bad : DS.line, lineWidth: 1))
                                .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
                                .disabled(saving || !store.state.online)
                        }
                    }
                    .frame(maxWidth: .infinity)
                }

                if isSmartMode {
                    Text("根据最近一小时调用量在 5–30 分钟内调整，不低于此下限。没有调用时暂停定时检测及降智复查。")
                        .font(DS.fontResetTime)
                        .foregroundStyle(DS.muted)
                }

                if isKeyAccount {
                    let defaultOptionLabel = account.platform == "openai"
                        ? "跟随全局（\(globalModelId.isEmpty ? "默认" : globalModelId)）"
                        : "请选择检测模型"
                    let currentLabel: String = {
                        if modelOverride.isEmpty { return defaultOptionLabel }
                        if let found = candidateModels.first(where: { $0.id == modelOverride }) {
                            return found.display_name.isEmpty ? found.id : found.display_name
                        }
                        return "\(modelOverride)（当前不可用）"
                    }()

                    VStack(alignment: .leading, spacing: 6) {
                        Text("检测模型")
                            .font(DS.fontCompactMedium)
                            .foregroundStyle(DS.muted)
                        HStack(spacing: 8) {
                            LinearSelect(
                                label: currentLabel,
                                accessibilityTitle: "检测模型",
                                height: DS.controlHeight,
                                expandWidth: true,
                                disabled: saving || !store.state.online || !modelsReady,
                                entries: [
                                    LinearMenuEntry(id: "__default__", title: defaultOptionLabel, checked: modelOverride.isEmpty) {
                                        modelOverride = ""
                                    },
                                ] + (candidateModels.isEmpty ? [] : [.separator()]) + candidateModels.map { item in
                                    LinearMenuEntry(
                                        id: item.id,
                                        title: item.display_name.isEmpty ? item.id : item.display_name,
                                        checked: modelOverride == item.id
                                    ) {
                                        modelOverride = item.id
                                    }
                                }
                            )

                            LinearIconButton(
                                systemName: "arrow.clockwise",
                                label: "刷新模型列表",
                                size: 13,
                                boxSize: DS.controlHeight,
                                bordered: true,
                                disabled: !store.state.online || loadingModels || saving
                            ) {
                                Task { await readAll(replaceDraft: false, refreshDirectory: true) }
                            }
                        }
                    }
                }

                if modelChangedExternally {
                    Text("自动检测模型已变化，请重新读取后保存；当前草稿已保留。")
                        .font(DS.fontCompact)
                        .foregroundStyle(DS.bad)
                } else if modelsReady && !loadingModels && store.state.online && error.isEmpty && !modelAvailable {
                    Text(effectiveSelectedModel.isEmpty ? "请先选择该账号的检测模型。" : "所选模型不在当前账号上游模型目录中。")
                        .font(DS.fontCompact)
                        .foregroundStyle(DS.bad)
                }

                VStack(alignment: .leading, spacing: 6) {
                    Rectangle().fill(DS.line).frame(height: 1).padding(.bottom, 4)
                    metaLine("状态", Self.statusLabels[rawStatus] ?? "等待核对")
                    let eng = ModelTestEngine.name(for: serverState["engine"]?.stringValue)
                    let samples = serverState["sample_count"]?.intValue ?? 3
                    metaLine("检测源", "\(eng) · \(samples) 组")
                    metaLine("上次检测（开始）", lastDetectionDisplay)
                    if serverState["schedule_mode"]?.stringValue == "smart" {
                        let reqCount = serverState["recent_request_count"]?.intValue
                        metaLine("最近一小时调用", reqCount.map { "\($0) 次" } ?? "暂不可用")
                        let eff = serverState["effective_interval_minutes"]?.intValue
                        let effText = eff.map { "\($0) 分钟" } ?? (reqCount == 0 ? "无调用，已暂停" : "等待调用统计")
                        metaLine("当前周期", effText)
                    }
                    metaLine("下次执行", BeijingTime.fullTime(serverState["next_at"]?.stringValue))
                    if let reason = serverState["reason"]?.stringValue, !reason.isEmpty {
                        metaLine("原因", reason)
                    }
                    if let cool = serverState["next_allowed_at"]?.stringValue,
                       let d = BeijingTime.parseISO(cool), d > Date()
                    {
                        metaLine("冷却至", BeijingTime.fullTime(cool))
                    }
                    if let disp = serverState["disposition"]?.objectValue {
                        let m = disp["marked"]?.boolValue == true ? "标记已保存" : "标记待保存"
                        let sv = disp["schedule_verified"]?.boolValue == true ? "停调度已确认" : "停调度待确认"
                        metaLine("处置", "\(m) · \(sv)")
                    }
                    if let notif = (serverState["recovery_notification"]?.objectValue ?? serverState["disposition_notification"]?.objectValue),
                       let st = notif["status"]?.stringValue
                    {
                        metaLine("结果通知", Self.notificationLabels[st] ?? "—")
                    }
                    let lastPred = serverState["last_result"]?["report"]?["prediction_name"]?.stringValue
                        ?? (serverState["last_result"]?["status"]?.stringValue.flatMap { Self.statusLabels[$0] })
                        ?? "—"
                    metaLine("最近结果", lastPred)
                }

                if !activeJobId.isEmpty && ["queued", "running", "retrying"].contains(rawStatus) {
                    HStack {
                        Button("取消当前检测任务") {
                            Task { await cancelActiveJob(activeJobId) }
                        }
                        .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                        .disabled(saving || !store.state.online)
                        Spacer()
                    }
                }
            }

            if !error.isEmpty {
                HStack(spacing: 8) {
                    Text(error)
                        .font(DS.fontCompact)
                        .foregroundStyle(DS.bad)
                    Button("重试") {
                        Task { await readAll(replaceDraft: false, refreshDirectory: false) }
                    }
                    .font(DS.fontCompactMedium)
                    .foregroundStyle(DS.accent)
                    .buttonStyle(.linearPlain)
                }
            }
        } footer: {
            Button("重新读取") {
                Task { await readAll(replaceDraft: true, refreshDirectory: false) }
            }
            .buttonStyle(LinearButtonStyle(variant: .secondary))
            .disabled(saving || loading || !store.state.online)

            Spacer()

            Button("关闭", action: onClose)
                .buttonStyle(LinearButtonStyle(variant: .secondary))
                .disabled(saving)

            let blockedByModel = enabled && (!modelsReady || !modelAvailable || !error.isEmpty)
            Button(saving ? "保存中…" : "保存") {
                Task { await save() }
            }
            .buttonStyle(LinearButtonStyle(variant: .primary))
            .disabled(!store.state.online || loading || loadingModels || saving || invalidInterval || modelChangedExternally || blockedByModel)
        }
        .sub2opsDialogFocus {
            if !saving { onClose() }
        }
        .task(id: "\(account.id):\(store.state.online)") {
            guard store.state.online else {
                loading = false
                return
            }
            await readAll(replaceDraft: true, refreshDirectory: false)
            while !Task.isCancelled && store.state.online {
                try? await Task.sleep(for: .seconds(2))
                guard !Task.isCancelled, store.state.online, !saving else { continue }
                if let latest: [String: JSONValue] = try? await RustBridge.shared.api("GET", "/accounts/\(account.id)/model-detection") {
                    serverState = latest
                    serverModelId = latest["model_id"]?.stringValue ?? serverModelId
                }
            }
        }
    }

    private func metaLine(_ label: String, _ value: String) -> some View {
        HStack(alignment: .firstTextBaseline) {
            Text(label)
                .font(DS.fontCompact)
                .foregroundStyle(DS.muted)
                .frame(width: 112, alignment: .leading)
            Text(value)
                .font(DS.fontCompact)
                .monospacedDigit()
                .foregroundStyle(DS.ink)
            Spacer()
        }
    }

    private func readAll(replaceDraft: Bool, refreshDirectory: Bool) async {
        guard store.state.online else { return }
        ModelDirectoryStore.shared.bindConnection(store.connectionKey)
        if serverState.isEmpty { loading = true }
        loadingModels = true
        error = ""
        defer {
            loading = false
            loadingModels = false
        }

        do {
            let raw: [String: JSONValue] = try await RustBridge.shared.api("GET", "/accounts/\(account.id)/model-detection")
            serverState = raw
            version = raw["version"]?.stringValue ?? ""
            globalModelId = raw["global_model_id"]?.stringValue ?? ""
            serverModelId = raw["model_id"]?.stringValue ?? ""
            if replaceDraft || loadedModelId.isEmpty {
                enabled = raw["enabled"]?.boolValue ?? false
                let mode = raw["schedule_mode"]?.stringValue ?? "fixed"
                scheduleMode = (mode == "smart") ? "smart" : "fixed"
                fixedIntervalText = String(raw["interval_minutes"]?.intValue ?? 60)
                smartMinIntervalText = String(raw["min_interval_minutes"]?.intValue ?? 5)
                modelOverride = raw["model_override"]?.stringValue ?? ""
                loadedModelId = serverModelId
            }
        } catch {
            self.error = error.localizedDescription
            return
        }

        do {
            let list = try await ModelDirectoryStore.shared.readModels(accountId: account.id, refresh: refreshDirectory)
            candidateModels = list
            modelsReady = true
        } catch {
            if ModelDirectoryStore.isRevoked(error) {
                candidateModels = []
                modelsReady = false
            }
            self.error = error.localizedDescription
        }
    }

    private func cancelActiveJob(_ jobId: String) async {
        guard !saving, store.state.online else { return }
        saving = true
        defer { saving = false }
        do {
            let _: JSONValue = try await RustBridge.shared.api("POST", "/model-tests/\(jobId)/cancel", body: .object([:]))
            await readAll(replaceDraft: false, refreshDirectory: false)
        } catch {
            store.report(error)
        }
    }

    private func save() async {
        let fixedMins = parsedFixedMinutes ?? (Int(serverState["interval_minutes"]?.intValue ?? 60))
        let smartMins = parsedSmartMinutes ?? (Int(serverState["min_interval_minutes"]?.intValue ?? 5))
        if isSmartMode && parsedSmartMinutes == nil {
            error = "智能模式最小间隔需为 5–30 分钟整数"
            return
        }
        if !isSmartMode && parsedFixedMinutes == nil {
            error = "固定模式检测间隔至少为 1 分钟整数"
            return
        }

        saving = true
        error = ""
        defer { saving = false }

        var body: [String: JSONValue] = [
            "expected_version": .string(version),
            "enabled": .bool(enabled),
            "schedule_mode": .string(isSmartMode ? "smart" : "fixed"),
            "interval_minutes": .int(Int64(max(1, fixedMins))),
            "min_interval_minutes": .int(Int64(min(30, max(5, smartMins)))),
        ]
        if isKeyAccount {
            let trimmedOverride = modelOverride.trimmingCharacters(in: .whitespacesAndNewlines)
            body["model_override"] = trimmedOverride.isEmpty ? .null : .string(trimmedOverride)
        }

        do {
            let _: JSONValue = try await RustBridge.shared.api(
                "PUT",
                "/accounts/\(account.id)/model-detection",
                body: .object(body)
            )
            store.refresh()
            store.showToast("已保存 \(account.name) 定时检测设置")
            onClose()
        } catch {
            self.error = error.localizedDescription
            store.report(error)
        }
    }
}
