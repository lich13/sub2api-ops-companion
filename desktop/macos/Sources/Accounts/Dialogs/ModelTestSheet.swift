import SwiftUI

public struct ModelTestSheet: View {
    public let account: Account
    @Bindable public var store: AppStore
    public let onClose: () -> Void

    @State private var engine: ModelTestEngine = .modeltrace
    @State private var sampleCount: Int = 1
    @State private var selectedModel: String = ""
    @State private var models: [AccountModelItem] = []
    @State private var loadingModels: Bool = false
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

    public var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    Text("模型测试与分词器指纹 · \(account.name)")
                        .font(DS.fontDialogTitle)
                        .foregroundStyle(DS.ink)
                    Text("#\(account.id) · \(PlatformMeta.displayName(account.platform))")
                        .font(DS.fontResetTime)
                        .monospacedDigit()
                        .foregroundStyle(DS.muted)
                }
                Spacer()
                LinearIconButton(systemName: "xmark", label: "关闭模型测试", action: onClose)
            }

            FingerprintBankStatusView(
                online: store.state.online,
                connectionKey: store.connectionKey,
                engines: supportsModelTrace ? [.modeltrace, .lm_detector] : [.lm_detector]
            )

            HStack(spacing: 10) {
                VStack(alignment: .leading, spacing: 4) {
                    Text("检测引擎")
                        .font(DS.fontCompactSemibold)
                        .foregroundStyle(DS.ink)
                    Picker("", selection: $engine) {
                        if supportsModelTrace {
                            Text("ModelTrace").tag(ModelTestEngine.modeltrace)
                        }
                        Text("LM Detector").tag(ModelTestEngine.lm_detector)
                    }
                    .labelsHidden()
                    .disabled(isRunning || !supportsModelTrace)
                    .onChange(of: engine) { _, _ in persistPreference() }
                }

                if engine == .lm_detector {
                    VStack(alignment: .leading, spacing: 4) {
                        Text("采样组数")
                            .font(DS.fontCompactSemibold)
                            .foregroundStyle(DS.ink)
                        Picker("", selection: $sampleCount) {
                            Text("1 组").tag(1)
                            Text("3 组").tag(3)
                        }
                        .labelsHidden()
                        .frame(width: 96)
                        .disabled(isRunning)
                        .onChange(of: sampleCount) { _, _ in persistPreference() }
                    }
                }

                VStack(alignment: .leading, spacing: 4) {
                    HStack {
                        Text("目标模型")
                            .font(DS.fontCompactSemibold)
                            .foregroundStyle(DS.ink)
                        Spacer()
                        Button(loadingModels ? "刷新中" : "刷新目录") {
                            Task { await loadModels(refresh: true) }
                        }
                        .buttonStyle(LinearButtonStyle(variant: .ghost, compact: true))
                        .disabled(!store.state.online || loadingModels || isRunning)
                    }
                    HStack(spacing: 6) {
                        TextField(text: $selectedModel, prompt: Text(verbatim: "选择或输入模型 ID")) { EmptyView() }
                            .textFieldStyle(.plain)
                            .font(DS.fontMono12)
                            .padding(.horizontal, 8)
                            .frame(height: DS.controlHeightCompact)
                            .background(DS.surface)
                            .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(DS.line, lineWidth: 1))
                            .clipShape(RoundedRectangle(cornerRadius: 6))
                            .disabled(isRunning)

                        if !models.isEmpty {
                            Menu {
                                ForEach(models) { item in
                                    Button(item.display_name) {
                                        selectedModel = item.id
                                    }
                                }
                            } label: {
                                Image(systemName: "chevron.down")
                                    .font(DS.iconFont(11, weight: .semibold))
                                    .foregroundStyle(DS.ink)
                                    .frame(width: 28, height: DS.controlHeightCompact)
                                    .background(DS.surface)
                                    .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(DS.line, lineWidth: 1))
                                    .clipShape(RoundedRectangle(cornerRadius: 6))
                            }
                            .menuStyle(.borderlessButton)
                            .frame(width: 30)
                            .disabled(isRunning)
                        }
                    }
                }
            }

            if let job {
                ModelFingerprintResultView(job: job)
            }

            if !error.isEmpty {
                Text(error)
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.bad)
            }

            HStack(spacing: 10) {
                Spacer()
                if let currentJob = job, !currentJob.isTerminal {
                    Button("停止测试") {
                        Task { await cancelJob(currentJob.id) }
                    }
                    .buttonStyle(LinearButtonStyle(variant: .danger))
                }
                Button("关闭", action: onClose)
                    .buttonStyle(LinearButtonStyle(variant: .secondary))
                Button(isRunning ? "测试中…" : "开始检测") {
                    Task { await startTest() }
                }
                .buttonStyle(LinearButtonStyle(variant: .primary))
                .disabled(!store.state.online || isRunning || selectedModel.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
        .padding(22)
        .frame(width: 580)
        .background(DS.surface)
        .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(DS.line, lineWidth: 1))
        .clipShape(RoundedRectangle(cornerRadius: 12))
        .sub2opsDialogFocus(onEscape: onClose)
        .task {
            initializeFromPreferences()
            await loadModels(refresh: false)
            await loadLatestJob()
        }
    }

    private func initializeFromPreferences() {
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
        defer { loadingModels = false }
        do {
            let list = try await ModelDirectoryStore.shared.readModels(accountId: account.id, refresh: refresh)
            models = list
            if selectedModel.isEmpty, let first = list.first {
                selectedModel = first.id
            }
            error = ""
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func loadLatestJob() async {
        guard store.state.online else { return }
        if let latest: ModelTestJob = try? await RustBridge.shared.api("GET", "/accounts/\(account.id)/model-tests/latest") {
            job = latest
            if !latest.isTerminal {
                await pollJob(id: latest.id)
            }
        }
    }

    private func startTest() async {
        guard store.state.online, !isRunning else { return }
        submitting = true
        error = ""
        let ver = account.operation_versions["model_test"] ?? account.version
        do {
            let created: ModelTestJob = try await RustBridge.shared.api(
                "POST",
                "/accounts/\(account.id)/model-tests",
                body: .object([
                    "model_id": .string(selectedModel.trimmingCharacters(in: .whitespacesAndNewlines)),
                    "engine": .string(engine.rawValue),
                    "sample_count": .int(Int64(engine == .lm_detector ? sampleCount : 3)),
                    "expected_version": .string(ver),
                ])
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
                self.error = error.localizedDescription
                return
            }
        }
    }

    private func cancelJob(_ id: String) async {
        do {
            let updated: ModelTestJob = try await RustBridge.shared.api("POST", "/model-tests/\(id)/cancel", body: .object([:]))
            job = updated
        } catch {
            self.error = error.localizedDescription
        }
    }
}
