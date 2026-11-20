import SwiftUI

/// Matches `ModelTestDialog` in `desktop/src/ModelTestDialog.tsx` (`react/dialog_test_light.png`).
public struct ModelTestSheet: View {
    public let account: Account
    @Bindable public var store: AppStore
    public let onClose: () -> Void

    @State private var engine: ModelTestEngine = .modeltrace
    @State private var sampleCount: Int = 1
    @State private var concurrency: Int = 1
    @State private var tokenizerProbe: Bool = false
    @State private var selectedModel: String = ""
    @State private var models: [AccountModelItem] = []
    @State private var loadingModels: Bool = false
    @State private var modelError: String = ""

    @State private var verdictMode: String = "strict"
    @State private var degradedTarget: String = "gpt-5.6-luna"
    @State private var savedVerdictMode: String = "strict"
    @State private var savedDegradedTarget: String = "gpt-5.6-luna"
    @State private var policyVersion: String = ""
    @State private var policySaving: Bool = false
    @State private var policyError: String = ""
    @FocusState private var degradedFocused: Bool

    @State private var job: ModelTestJob? = nil
    @State private var submitting: Bool = false
    @State private var error: String = ""

    public init(account: Account, store: AppStore, onClose: @escaping () -> Void) {
        self.account = account
        self.store = store
        self.onClose = onClose
    }

    private var supportsModelTrace: Bool {
        account.platform == "openai"
    }

    private var preferenceKey: String {
        "\(store.state.preferences.base_url):\(account.id)"
    }

    private var isRunning: Bool {
        submitting || (job != nil && job?.isTerminal == false)
    }

    private var policyDirty: Bool {
        verdictMode != savedVerdictMode
            || (verdictMode == "relaxed" && degradedTarget.trimmingCharacters(in: .whitespacesAndNewlines) != savedDegradedTarget)
    }

    public var body: some View {
        LinearModalCard(
            title: "模型测试",
            subtitle: "\(account.name) #\((account.id))",
            width: 620,
            onClose: onClose
        ) {
            LinearBoundedScrollView(maxHeight: 460) {
                VStack(alignment: .leading, spacing: 14) {
                    // Row 1: Model selector + Refresh button
                    VStack(alignment: .leading, spacing: 6) {
                        Text("模型")
                            .font(DS.fontBody)
                            .foregroundStyle(DS.muted)

                        HStack(spacing: 8) {
                            LinearSelect(
                                label: selectedModelLabel,
                                accessibilityTitle: "测试模型",
                                height: DS.controlHeightField,
                                expandWidth: true,
                                disabled: loadingModels || models.isEmpty || isRunning || !store.state.online,
                                entries: models.map { item in
                                    LinearMenuEntry(
                                        id: item.id,
                                        title: item.display_name.isEmpty ? item.id : item.display_name,
                                        checked: selectedModel == item.id
                                    ) {
                                        selectedModel = item.id
                                    }
                                }
                            )

                            LinearIconButton(
                                systemName: "arrow.clockwise",
                                label: "刷新模型列表",
                                size: 14,
                                boxSize: DS.controlHeightField,
                                bordered: true,
                                disabled: loadingModels || isRunning || !store.state.online
                            ) {
                                Task { await loadModels(refresh: true) }
                            }
                        }

                        if !modelError.isEmpty {
                            HStack(spacing: 6) {
                                Text(modelError)
                                    .font(DS.fontCompact)
                                    .foregroundStyle(DS.bad)
                                Button("重试") {
                                    Task { await loadModels(refresh: true) }
                                }
                                .font(DS.fontCompactMedium)
                                .foregroundStyle(DS.accent)
                                .buttonStyle(.linearPlain)
                            }
                        } else if !loadingModels && models.isEmpty {
                            Text("暂无可用模型")
                                .font(DS.fontCompact)
                                .foregroundStyle(DS.muted)
                        }
                    }

                    // Row 2: Engine | Samples | Concurrency (3-column grid matching .model-test-controls)
                    HStack(alignment: .top, spacing: 12) {
                        VStack(alignment: .leading, spacing: 6) {
                            Text("检测源")
                                .font(DS.fontBody)
                                .foregroundStyle(DS.muted)
                            LinearSelect(
                                label: engine == .modeltrace ? "ModelTrace" : "LM Detector",
                                accessibilityTitle: "检测源",
                                height: DS.controlHeightField,
                                expandWidth: true,
                                disabled: isRunning || !store.state.online || !supportsModelTrace,
                                entries: (supportsModelTrace ? [ModelTestEngine.modeltrace, .lm_detector] : [.lm_detector]).map { eng in
                                    LinearMenuEntry(
                                        id: eng.rawValue,
                                        title: eng == .modeltrace ? "ModelTrace" : "LM Detector",
                                        checked: engine == eng
                                    ) {
                                        engine = eng
                                        persistPreference()
                                    }
                                }
                            )
                        }
                        .frame(maxWidth: .infinity)

                        VStack(alignment: .leading, spacing: 6) {
                            Text("组数")
                                .font(DS.fontBody)
                                .foregroundStyle(DS.muted)
                            let effectiveSamples = engine == .lm_detector ? sampleCount : 3
                            LinearSelect(
                                label: "\(effectiveSamples) 组",
                                accessibilityTitle: "采样组数",
                                height: DS.controlHeightField,
                                width: 100,
                                disabled: engine != .lm_detector || isRunning || !store.state.online,
                                entries: [1, 3].map { count in
                                    LinearMenuEntry(id: "\(count)", title: "\(count) 组", checked: effectiveSamples == count) {
                                        sampleCount = count
                                        persistPreference()
                                    }
                                }
                            )
                        }

                        VStack(alignment: .leading, spacing: 6) {
                            Text("并发")
                                .font(DS.fontBody)
                                .foregroundStyle(DS.muted)
                            LinearSelect(
                                label: "\(concurrency)",
                                accessibilityTitle: "并发数",
                                height: DS.controlHeightField,
                                width: 92,
                                disabled: isRunning || !store.state.online,
                                entries: [1, 2, 3].map { c in
                                    LinearMenuEntry(id: "\(c)", title: "\(c)", checked: concurrency == c) {
                                        concurrency = c
                                        Task { await store.updatePreferences(modelTestConcurrency: c) }
                                    }
                                }
                            )
                        }
                    }

                    if engine == .lm_detector {
                        LinearCheckbox(
                            isChecked: tokenizerProbe,
                            label: "分词器探测",
                            disabled: isRunning
                        ) {
                            tokenizerProbe.toggle()
                        }
                    }

                    // Row 3: Verdict Mode & Degraded Target (equal-width 2-column grid per DESIGN.md line 61)
                    HStack(alignment: .top, spacing: 12) {
                        VStack(alignment: .leading, spacing: 6) {
                            Text("判定模式")
                                .font(DS.fontBody)
                                .foregroundStyle(DS.muted)
                            LinearSelect(
                                label: verdictMode == "relaxed" ? "宽松模式" : "严格模式",
                                accessibilityTitle: "判定模式",
                                height: DS.controlHeightField,
                                expandWidth: true,
                                disabled: isRunning || policySaving || !store.state.online,
                                entries: [
                                    LinearMenuEntry(id: "strict", title: "严格模式", checked: verdictMode == "strict") {
                                        verdictMode = "strict"
                                        Task { await savePolicy() }
                                    },
                                    LinearMenuEntry(id: "relaxed", title: "宽松模式", checked: verdictMode == "relaxed") {
                                        verdictMode = "relaxed"
                                        Task { await savePolicy() }
                                    },
                                ]
                            )
                        }
                        .frame(maxWidth: .infinity)

                        if verdictMode == "relaxed" {
                            VStack(alignment: .leading, spacing: 6) {
                                Text("降智模型")
                                    .font(DS.fontBody)
                                    .foregroundStyle(DS.muted)
                                TextField(text: $degradedTarget, prompt: Text(verbatim: "gpt-5.6-luna")) { EmptyView() }
                                    .textFieldStyle(.plain)
                                    .font(DS.fontBody)
                                    .focused($degradedFocused)
                                    .padding(.horizontal, 10)
                                    .frame(height: DS.controlHeightField)
                                    .background(DS.surface)
                                    .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                                    .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
                                    .disabled(isRunning || policySaving || !store.state.online)
                                    .onSubmit {
                                        Task { await savePolicy() }
                                    }
                                    .onChange(of: degradedFocused) { _, focused in
                                        if !focused && policyDirty {
                                            Task { await savePolicy() }
                                        }
                                    }
                            }
                            .frame(maxWidth: .infinity)
                        } else {
                            DS.clear.frame(maxWidth: .infinity, maxHeight: 1)
                        }
                    }

                    if policySaving {
                        Text("正在保存判定规则")
                            .font(DS.fontBody)
                            .foregroundStyle(DS.muted)
                    } else if !policyError.isEmpty {
                        HStack(spacing: 6) {
                            Text(policyError)
                                .font(DS.fontBody)
                                .foregroundStyle(DS.bad)
                            Button("重试") {
                                Task { await savePolicy(refreshVersion: true) }
                            }
                            .font(DS.fontBodyMedium)
                            .foregroundStyle(DS.accent)
                            .buttonStyle(.linearPlain)
                            .disabled(policySaving || !store.state.online)
                        }
                    }

                    if let job {
                        ModelFingerprintResultView(job: job)
                    }

                    FingerprintBankStatusView(
                        online: store.state.online,
                        connectionKey: store.connectionKey,
                        engines: [engine],
                        taskVersion: (job?.engine ?? "modeltrace") == engine.rawValue ? job?.bank_version : nil
                    )

                    if !error.isEmpty {
                        Text(error)
                            .font(DS.fontCompact)
                            .foregroundStyle(DS.bad)
                    }
                }
            }
        } footer: {
            Spacer()
            if let currentJob = job, currentJob.can_retry == true, currentJob.isTerminal {
                Button("重试失败组") {
                    Task { await retryFailedGroups(currentJob.id) }
                }
                .buttonStyle(LinearButtonStyle(variant: .secondary))
                .disabled(submitting || policySaving || !store.state.online)
            }
            if let currentJob = job, currentJob.status == "needs_confirmation" {
                Button("取消任务") {
                    Task { await cancelJob(currentJob.id) }
                }
                .buttonStyle(LinearButtonStyle(variant: .secondary))
                .disabled(submitting || !store.state.online)
            }
            if let currentJob = job, !currentJob.isTerminal {
                Button {
                    Task { await cancelJob(currentJob.id) }
                } label: {
                    HStack(spacing: 5) {
                        Image(systemName: "stop")
                            .font(DS.iconFont(11, weight: .semibold))
                        Text("停止")
                    }
                }
                .buttonStyle(LinearButtonStyle(variant: .danger))
                .disabled(submitting || !store.state.online)
            } else {
                Button {
                    Task { await startTest() }
                } label: {
                    HStack(spacing: 6) {
                        Image(systemName: job != nil ? "arrow.counterclockwise" : "play")
                            .font(DS.iconFont(11, weight: .semibold))
                        Text(submitting ? "测试中…" : (job != nil ? "重新测试" : "开始测试"))
                    }
                }
                .buttonStyle(LinearButtonStyle(variant: .secondary))
                .disabled(
                    !store.state.online
                        || loadingModels
                        || isRunning
                        || policySaving
                        || policyDirty
                        || !policyError.isEmpty
                        || selectedModel.isEmpty
                        || !models.contains(where: { $0.id == selectedModel })
                )
            }
        }
        .sub2opsDialogFocus(onEscape: onClose)
        .task {
            initializeFromPreferences()
            await loadModels(refresh: false)
            await loadDefaultsAndPolicy()
        }
    }

    private var selectedModelLabel: String {
        if let found = models.first(where: { $0.id == selectedModel }) {
            return found.display_name.isEmpty ? found.id : found.display_name
        }
        return selectedModel.isEmpty ? (loadingModels ? "正在读取模型…" : "暂无可用模型") : selectedModel
    }

    private func initializeFromPreferences() {
        let savedConc = store.state.preferences.model_test_concurrency ?? 1
        concurrency = [1, 2, 3].contains(savedConc) ? savedConc : 1
        if let saved = store.state.preferences.model_test_preferences?[preferenceKey] {
            if supportsModelTrace, let parsed = ModelTestEngine(rawValue: saved.engine) {
                engine = parsed
            } else {
                engine = supportsModelTrace ? .modeltrace : .lm_detector
            }
            sampleCount = saved.sample_count == 3 ? 3 : 1
        } else {
            engine = supportsModelTrace ? .modeltrace : .lm_detector
            sampleCount = 1
        }
    }

    private func persistPreference() {
        var map = store.state.preferences.model_test_preferences ?? [:]
        map[preferenceKey] = ModelTestPreference(engine: engine.rawValue, sample_count: sampleCount)
        Task { await store.updatePreferences(modelTestPreferences: map) }
    }

    private func loadModels(refresh: Bool) async {
        guard store.state.online else { return }
        ModelDirectoryStore.shared.bindConnection(store.connectionKey)
        loadingModels = true
        modelError = ""
        defer { loadingModels = false }
        do {
            let list = try await ModelDirectoryStore.shared.readModels(accountId: account.id, refresh: refresh)
            models = list
            if selectedModel.isEmpty || !list.contains(where: { $0.id == selectedModel }), let first = list.first {
                selectedModel = first.id
            }
        } catch {
            if ModelDirectoryStore.isRevoked(error) {
                models = []
            }
            modelError = error.localizedDescription
        }
    }

    private func loadDefaultsAndPolicy() async {
        guard store.state.online else { return }
        if let policy: [String: JSONValue] = try? await RustBridge.shared.api("GET", "/accounts/\(account.id)/model-detection") {
            policyVersion = policy["version"]?.stringValue ?? ""
            let loadedMode = policy["verdict_mode"]?.stringValue ?? "strict"
            let loadedTarget = policy["degraded_model_id"]?.stringValue ?? "gpt-5.6-luna"
            verdictMode = loadedMode
            degradedTarget = loadedTarget
            savedVerdictMode = loadedMode
            savedDegradedTarget = loadedTarget
            if let preferred = policy["model_override"]?.stringValue, !preferred.isEmpty, models.contains(where: { $0.id == preferred }) {
                selectedModel = preferred
            }
        }
        if let latest: ModelTestJob = try? await RustBridge.shared.api("GET", "/accounts/\(account.id)/model-tests/latest") {
            job = latest
            if let reqModel = latest.requested_model ?? latest.model_id, !reqModel.isEmpty, models.contains(where: { $0.id == reqModel }) {
                selectedModel = reqModel
            }
            if !latest.isTerminal {
                await pollJob(id: latest.id)
            }
        }
    }

    private func savePolicy(refreshVersion: Bool = false) async {
        guard store.state.online, !policySaving else { return }
        policySaving = true
        policyError = ""
        defer { policySaving = false }
        do {
            if refreshVersion || policyVersion.isEmpty {
                let cur: [String: JSONValue] = try await RustBridge.shared.api("GET", "/accounts/\(account.id)/model-detection")
                policyVersion = cur["version"]?.stringValue ?? ""
            }
            let trimmedTarget = degradedTarget.trimmingCharacters(in: .whitespacesAndNewlines)
            var body: [String: JSONValue] = [
                "expected_version": .string(policyVersion),
                "verdict_mode": .string(verdictMode),
            ]
            if verdictMode == "relaxed" {
                body["degraded_model_id"] = .string(trimmedTarget)
            }
            let updated: [String: JSONValue] = try await RustBridge.shared.api(
                "PUT",
                "/accounts/\(account.id)/model-detection",
                body: .object(body)
            )
            policyVersion = updated["version"]?.stringValue ?? policyVersion
            savedVerdictMode = updated["verdict_mode"]?.stringValue ?? verdictMode
            savedDegradedTarget = updated["degraded_model_id"]?.stringValue ?? trimmedTarget
        } catch {
            policyError = error.localizedDescription
        }
    }

    private func retryFailedGroups(_ id: String) async {
        guard store.state.online, !submitting, !policySaving else { return }
        submitting = true
        error = ""
        defer { submitting = false }
        do {
            let next: ModelTestJob = try await RustBridge.shared.api(
                "POST",
                "/model-tests/\(id)/retry",
                body: .object(["request_id": .string(UUID().uuidString.lowercased())])
            )
            job = next
            if !next.isTerminal {
                await pollJob(id: next.id)
            }
            store.refresh()
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func startTest() async {
        guard store.state.online, !isRunning else { return }
        submitting = true
        error = ""
        var payload: [String: JSONValue] = [
            "model_id": .string(selectedModel.trimmingCharacters(in: .whitespacesAndNewlines)),
            "expected_version": .string(account.version),
            "concurrency": .int(Int64(concurrency)),
            "engine": .string(engine.rawValue),
            "request_id": .string(UUID().uuidString.replacingOccurrences(of: "-", with: "").lowercased()),
        ]
        if let opVer = account.operation_versions["model_test"] {
            payload["expected_operation_version"] = .string(opVer)
        }
        if engine == .lm_detector {
            payload["sample_count"] = .int(Int64(sampleCount))
            payload["tokenizer_probe"] = .bool(tokenizerProbe)
        }
        do {
            let created: ModelTestJob = try await RustBridge.shared.api(
                "POST",
                "/accounts/\(account.id)/model-tests",
                body: .object(payload)
            )
            submitting = false
            job = created
            if !created.isTerminal {
                await pollJob(id: created.id)
            }
            store.refresh()
        } catch {
            submitting = false
            self.error = error.localizedDescription
        }
    }

    private func pollJob(id: String) async {
        while !Task.isCancelled {
            try? await Task.sleep(for: .milliseconds(700))
            guard !Task.isCancelled, store.state.online else { return }
            do {
                let updated: ModelTestJob = try await RustBridge.shared.api("GET", "/model-tests/\(id)")
                job = updated
                if updated.isTerminal {
                    store.refresh()
                    return
                }
            } catch {
                return
            }
        }
    }

    private func cancelJob(_ id: String) async {
        do {
            let updated: ModelTestJob = try await RustBridge.shared.api("POST", "/model-tests/\(id)/cancel", body: .object([:]))
            job = updated
            store.refresh()
        } catch {
            self.error = error.localizedDescription
        }
    }
}
