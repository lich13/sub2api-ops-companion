import SwiftUI

@MainActor
@Observable
public final class ModelTestLifecycle {
    public private(set) var isOpen: Bool = true
    public private(set) var boundConnectionKey: String = ""
    public private(set) var activeJobId: String? = nil
    public private(set) var generation: UInt64 = 0
    public private(set) var dialogSeq: UInt64 = 0
    public private(set) var modelLoadSeq: UInt64 = 0
    public var models: [ModelOption] = []
    public private(set) var loadingModels: Bool = false
    public var modelError: String = ""
    @ObservationIgnored nonisolated(unsafe) private var pollTask: Task<Void, Never>?

    deinit {
        pollTask?.cancel()
    }

    public init() {}

    public func open(connectionKey: String) {
        isOpen = true
        if boundConnectionKey != connectionKey {
            boundConnectionKey = connectionKey
            generation &+= 1
            dialogSeq &+= 1
            modelLoadSeq &+= 1
            loadingModels = false
            modelError = ""
            models = []
            activeJobId = nil
            pollTask?.cancel()
            pollTask = nil
        }
    }

    public func close() {
        isOpen = false
        generation &+= 1
        dialogSeq &+= 1
        modelLoadSeq &+= 1
        loadingModels = false
        activeJobId = nil
        pollTask?.cancel()
        pollTask = nil
    }

    public func isDialogActive(connectionKey: String, capturedDialogSeq: UInt64) -> Bool {
        isOpen && boundConnectionKey == connectionKey && dialogSeq == capturedDialogSeq
    }

    @discardableResult
    public func loadModels(
        accountId: Int,
        refresh: Bool,
        online: Bool,
        connectionKey: String
    ) async -> [ModelOption]? {
        guard online, isOpen, boundConnectionKey == connectionKey else { return nil }
        let capturedDialogSeq = dialogSeq
        modelLoadSeq &+= 1
        let capturedModelSeq = modelLoadSeq
        ModelDirectoryStore.shared.bindConnection(connectionKey)
        loadingModels = true
        modelError = ""
        defer {
            if isDialogActive(connectionKey: connectionKey, capturedDialogSeq: capturedDialogSeq),
               modelLoadSeq == capturedModelSeq
            {
                loadingModels = false
            }
        }
        do {
            let list = try await ModelDirectoryStore.shared.readModels(accountId: accountId, refresh: refresh)
            guard !Task.isCancelled,
                  isDialogActive(connectionKey: connectionKey, capturedDialogSeq: capturedDialogSeq),
                  modelLoadSeq == capturedModelSeq
            else { return nil }
            models = list
            return list
        } catch is CancellationError {
            return nil
        } catch {
            guard !Task.isCancelled,
                  isDialogActive(connectionKey: connectionKey, capturedDialogSeq: capturedDialogSeq),
                  modelLoadSeq == capturedModelSeq
            else { return nil }
            if ModelDirectoryStore.isRevoked(error) {
                models = []
            }
            modelError = error.localizedDescription
            return nil
        }
    }

    public func canAccept(connectionKey: String, capturedGeneration: UInt64) -> Bool {
        isOpen && boundConnectionKey == connectionKey && generation == capturedGeneration
    }

    public func startPolling(
        jobId: String,
        connectionKey: String,
        operation: @escaping @MainActor (UInt64) async -> Void
    ) {
        guard isOpen, boundConnectionKey == connectionKey else { return }
        pollTask?.cancel()
        generation &+= 1
        activeJobId = jobId
        let capturedGen = generation
        pollTask = Task { @MainActor [weak self] in
            guard let self, self.canPoll(jobId: jobId, generation: capturedGen, connectionKey: connectionKey) else { return }
            await operation(capturedGen)
        }
    }

    public func canPoll(jobId: String, generation capturedGen: UInt64, connectionKey: String) -> Bool {
        isOpen && activeJobId == jobId && generation == capturedGen && boundConnectionKey == connectionKey
    }

    public func stopPolling(jobId: String, connectionKey: String) {
        guard isOpen, boundConnectionKey == connectionKey, activeJobId == jobId else { return }
        generation &+= 1
        activeJobId = nil
        pollTask?.cancel()
        pollTask = nil
    }
}

/// Matches `ModelTestDialog` in `desktop/src/ModelTestDialog.tsx` (`react/dialog_test_light.png`).
public struct ModelTestSheet: View {
    public let account: Account
    @Bindable public var store: AppStore
    public let onClose: () -> Void

    @State private var lifecycle = ModelTestLifecycle()
    @State private var engine: ModelTestEngine = .modeltrace
    @State private var sampleCount: Int = 1
    @State private var concurrency: Int = 1
    @State private var tokenizerProbe: Bool = false
    @State private var selectedModel: String = ""

    private var models: [ModelOption] { lifecycle.models }
    private var loadingModels: Bool { lifecycle.loadingModels }
    private var modelError: String { lifecycle.modelError }

    @State private var verdictMode: String = "strict"
    @State private var degradedTarget: String = DefaultModelId.legacyDegraded.rawValue
    @State private var savedVerdictMode: String = "strict"
    @State private var savedDegradedTarget: String = DefaultModelId.legacyDegraded.rawValue
    @State private var policyVersion: String = ""
    @State private var policyLoading: Bool = true
    @State private var policySaving: Bool = false
    @State private var policyError: String = ""
    @FocusState private var degradedFocused: Bool

    @State private var job: ModelTestJob? = nil
    @State private var submitting: Bool = false
    @State private var error: String = ""

    nonisolated public static func canStartTest(
        online: Bool,
        loadingModels: Bool,
        isRunning: Bool,
        policyLoading: Bool,
        policySaving: Bool,
        policyVersion: String,
        policyDirty: Bool,
        policyError: String,
        selectedModel: String,
        models: [ModelOption]
    ) -> Bool {
        online
            && !loadingModels
            && !isRunning
            && !policyLoading
            && !policySaving
            && !policyVersion.isEmpty
            && !policyDirty
            && policyError.isEmpty
            && !selectedModel.isEmpty
            && models.contains(where: { $0.id == selectedModel })
    }

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

    private func handleClose() {
        lifecycle.close()
        onClose()
    }

    public var body: some View {
        LinearModalCard(
            title: "模型测试",
            subtitle: "\(account.name) #\((account.id))",
            width: 620,
            onClose: handleClose
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
                                        title: item.titleText,
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
                                disabled: isRunning || policyLoading || policySaving || !store.state.online,
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
                                TextField(text: $degradedTarget, prompt: Text(verbatim: DefaultModelId.legacyDegraded.rawValue)) { EmptyView() }
                                    .textFieldStyle(.plain)
                                    .font(DS.fontBody)
                                    .focused($degradedFocused)
                                    .padding(.horizontal, 10)
                                    .frame(height: DS.controlHeightField)
                                    .background(DS.surface)
                                    .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                                    .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
                                    .disabled(isRunning || policyLoading || policySaving || !store.state.online)
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
                                Task {
                                    if policyVersion.isEmpty {
                                        await loadDefaultsAndPolicy()
                                    } else {
                                        await savePolicy(refreshVersion: true)
                                    }
                                }
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
                    !Self.canStartTest(
                        online: store.state.online,
                        loadingModels: loadingModels,
                        isRunning: isRunning,
                        policyLoading: policyLoading,
                        policySaving: policySaving,
                        policyVersion: policyVersion,
                        policyDirty: policyDirty,
                        policyError: policyError,
                        selectedModel: selectedModel,
                        models: models
                    )
                )
            }
        }
        .sub2opsDialogFocus(
            canBackdropDismiss: !isRunning && !policySaving && !policyDirty,
            onEscape: { if !policySaving { handleClose() } }
        )
        .onChange(of: store.connectionKey) { _, next in
            if lifecycle.boundConnectionKey != next {
                lifecycle.close()
                submitting = false
            }
        }
        .onDisappear {
            lifecycle.close()
        }
        .task {
            lifecycle.open(connectionKey: store.connectionKey)
            initializeFromPreferences()
            await loadModels(refresh: false)
            await loadDefaultsAndPolicy()
        }
    }

    private var selectedModelLabel: String {
        if let found = models.first(where: { $0.id == selectedModel }) {
            return found.titleText
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
        guard let list = await lifecycle.loadModels(
            accountId: account.id,
            refresh: refresh,
            online: store.state.online,
            connectionKey: store.connectionKey
        ) else { return }
        if selectedModel.isEmpty || !list.contains(where: { $0.id == selectedModel }), let first = list.first {
            selectedModel = first.id
        }
    }

    private func loadDefaultsAndPolicy() async {
        guard lifecycle.isOpen else { return }
        let capturedKey = store.connectionKey
        let capturedDialogSeq = lifecycle.dialogSeq
        guard store.state.online else {
            policyLoading = false
            return
        }
        policyLoading = true
        policyError = ""
        do {
            defer {
                if lifecycle.isDialogActive(connectionKey: store.connectionKey, capturedDialogSeq: capturedDialogSeq) {
                    policyLoading = false
                }
            }
            let policy: ModelDetectionReply = try await RustBridge.shared.api("GET", "/accounts/\(account.id)/model-detection")
            guard !Task.isCancelled,
                  lifecycle.isDialogActive(connectionKey: store.connectionKey, capturedDialogSeq: capturedDialogSeq)
            else { return }
            policyVersion = policy.version
            let loadedMode = policy.verdict_mode ?? "strict"
            let loadedTarget = policy.degraded_model_id ?? DefaultModelId.legacyDegraded.rawValue
            verdictMode = loadedMode
            degradedTarget = loadedTarget
            savedVerdictMode = loadedMode
            savedDegradedTarget = loadedTarget
            if let preferred = policy.model_override, !preferred.isEmpty, models.contains(where: { $0.id == preferred }) {
                selectedModel = preferred
            }
        } catch {
            guard !Task.isCancelled,
                  lifecycle.isDialogActive(connectionKey: store.connectionKey, capturedDialogSeq: capturedDialogSeq)
            else { return }
            policyError = error.localizedDescription
        }
        do {
            let latest: ModelTestJob? = try await RustBridge.shared.api("GET", "/accounts/\(account.id)/model-tests/latest")
            guard !Task.isCancelled,
                  lifecycle.isDialogActive(connectionKey: store.connectionKey, capturedDialogSeq: capturedDialogSeq)
            else { return }
            if let latest {
                job = latest
                if let reqModel = latest.requested_model, !reqModel.isEmpty, models.contains(where: { $0.id == reqModel }) {
                    selectedModel = reqModel
                }
                if !latest.isTerminal {
                    startPollingJob(id: latest.id, connectionKey: capturedKey)
                }
            }
        } catch {
            guard !Task.isCancelled,
                  lifecycle.isDialogActive(connectionKey: store.connectionKey, capturedDialogSeq: capturedDialogSeq)
            else { return }
            self.error = error.localizedDescription
        }
    }

    private func startPollingJob(id: String, connectionKey: String) {
        lifecycle.startPolling(jobId: id, connectionKey: connectionKey) { capturedGen in
            await pollJob(id: id, capturedGen: capturedGen, capturedKey: connectionKey)
        }
    }

    private func savePolicy(refreshVersion: Bool = false) async {
        guard store.state.online, !policySaving, lifecycle.isOpen else { return }
        let capturedDialogSeq = lifecycle.dialogSeq
        policySaving = true
        policyError = ""
        defer {
            if lifecycle.isDialogActive(connectionKey: store.connectionKey, capturedDialogSeq: capturedDialogSeq) {
                policySaving = false
            }
        }
        do {
            if refreshVersion || policyVersion.isEmpty {
                let cur: ModelDetectionReply = try await RustBridge.shared.api("GET", "/accounts/\(account.id)/model-detection")
                guard lifecycle.isDialogActive(connectionKey: store.connectionKey, capturedDialogSeq: capturedDialogSeq) else { return }
                policyVersion = cur.version
            }
            let trimmedTarget = degradedTarget.trimmingCharacters(in: .whitespacesAndNewlines)
            var body: [String: JSONValue] = [
                "expected_version": .string(policyVersion),
                "verdict_mode": .string(verdictMode),
            ]
            if verdictMode == "relaxed" {
                body["degraded_model_id"] = .string(trimmedTarget)
            }
            let updated: ModelDetectionReply = try await RustBridge.shared.api(
                "PUT",
                "/accounts/\(account.id)/model-detection",
                body: .object(body)
            )
            guard lifecycle.isDialogActive(connectionKey: store.connectionKey, capturedDialogSeq: capturedDialogSeq) else { return }
            policyVersion = updated.version
            savedVerdictMode = updated.verdict_mode ?? verdictMode
            savedDegradedTarget = updated.degraded_model_id ?? trimmedTarget
        } catch {
            guard lifecycle.isDialogActive(connectionKey: store.connectionKey, capturedDialogSeq: capturedDialogSeq) else { return }
            policyError = error.localizedDescription
        }
    }

    private func retryFailedGroups(_ id: String) async {
        guard store.state.online, !submitting, !policySaving, lifecycle.isOpen else { return }
        let capturedKey = store.connectionKey
        let capturedGen = lifecycle.generation
        submitting = true
        error = ""
        do {
            let next: ModelTestJob = try await RustBridge.shared.api(
                "POST",
                "/model-tests/\(id)/retry",
                body: .object(["request_id": .string(UUID().uuidString.lowercased())])
            )
            guard lifecycle.canAccept(connectionKey: store.connectionKey, capturedGeneration: capturedGen) else {
                submitting = false
                return
            }
            submitting = false
            job = next
            if !next.isTerminal {
                startPollingJob(id: next.id, connectionKey: capturedKey)
            } else {
                store.refresh()
            }
        } catch {
            submitting = false
            if lifecycle.canAccept(connectionKey: store.connectionKey, capturedGeneration: capturedGen) {
                self.error = error.localizedDescription
            }
        }
    }

    private func startTest() async {
        guard store.state.online, !isRunning, lifecycle.isOpen else { return }
        let capturedKey = store.connectionKey
        let capturedGen = lifecycle.generation
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
            guard lifecycle.canAccept(connectionKey: store.connectionKey, capturedGeneration: capturedGen) else {
                submitting = false
                return
            }
            submitting = false
            job = created
            if !created.isTerminal {
                startPollingJob(id: created.id, connectionKey: capturedKey)
            } else {
                store.refresh()
            }
        } catch {
            submitting = false
            if lifecycle.canAccept(connectionKey: store.connectionKey, capturedGeneration: capturedGen) {
                self.error = error.localizedDescription
            }
        }
    }

    private func pollJob(id: String, capturedGen: UInt64, capturedKey: String) async {
        var failures = 0
        while !Task.isCancelled
            && store.state.online
            && lifecycle.canPoll(jobId: id, generation: capturedGen, connectionKey: store.connectionKey)
            && capturedKey == store.connectionKey
        {
            let delayMs = min(3000, 700 * max(1, 1 << min(failures, 2)))
            try? await Task.sleep(for: .milliseconds(delayMs))
            guard !Task.isCancelled,
                  store.state.online,
                  lifecycle.canPoll(jobId: id, generation: capturedGen, connectionKey: store.connectionKey),
                  capturedKey == store.connectionKey
            else { return }
            do {
                let updated: ModelTestJob = try await RustBridge.shared.api("GET", "/model-tests/\(id)")
                guard !Task.isCancelled,
                      lifecycle.canPoll(jobId: id, generation: capturedGen, connectionKey: store.connectionKey),
                      capturedKey == store.connectionKey
                else { return }
                failures = 0
                self.error = ""
                job = updated
                if updated.isTerminal {
                    store.refresh()
                    return
                }
            } catch is CancellationError {
                return
            } catch {
                guard !Task.isCancelled,
                      lifecycle.canPoll(jobId: id, generation: capturedGen, connectionKey: store.connectionKey),
                      capturedKey == store.connectionKey
                else { return }
                failures += 1
                self.error = error.localizedDescription
            }
        }
    }

    private func cancelJob(_ id: String) async {
        let capturedKey = store.connectionKey
        do {
            let updated: ModelTestJob = try await RustBridge.shared.api("POST", "/model-tests/\(id)/cancel", body: .object([:]))
            guard store.connectionKey == capturedKey else { return }
            if lifecycle.isOpen && lifecycle.boundConnectionKey == capturedKey {
                if lifecycle.activeJobId == nil || lifecycle.activeJobId == id {
                    job = updated
                }
                if updated.isTerminal {
                    lifecycle.stopPolling(jobId: id, connectionKey: capturedKey)
                }
            }
            store.refresh()
        } catch {
            if lifecycle.isOpen && lifecycle.boundConnectionKey == capturedKey && store.connectionKey == capturedKey {
                self.error = error.localizedDescription
            }
        }
    }
}
