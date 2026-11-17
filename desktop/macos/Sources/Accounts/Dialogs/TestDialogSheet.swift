import AppKit
import SwiftUI
import UniformTypeIdentifiers

/// Matches `desktop/src/TestDialog.tsx` (`react_test_dialog_light.png`) in both visual layout and server JSON contract.
public struct TestDialogSheet: View {
    public let account: Account
    @Bindable public var store: AppStore
    public let onClose: () -> Void

    public enum ExecutionKind: String, Sendable {
        case now
        case scheduled
        case cycle
    }

    private static let defaultPrompts: [String: String] = [
        "image": "Generate a cute orange cat astronaut sticker on a clean pastel background.",
        "video": "A red ball bouncing once on a white floor, short simple motion.",
        "search": "xAI Grok",
        "tts": "Hello from Sub2API account connectivity test.",
    ]

    @State private var execution: ExecutionKind = .now
    @State private var mode: String
    @State private var model: String = ""
    @State private var candidateModels: [AccountModelItem] = []
    @State private var loadingModels: Bool = false
    @State private var modelError: String = ""
    @State private var prompt: String = ""
    @State private var scheduledText: String = ""
    @State private var cycleBusy: Bool = false
    @State private var attachedFileName: String = ""
    @State private var attachedDataURL: String = ""
    @State private var running: Bool = false
    @State private var cancelled: Bool = false
    @State private var operationId: String? = nil
    @State private var statusLine: String = ""
    @State private var outputText: String = ""
    @State private var mediaLinks: [String] = []
    @State private var errorText: String = ""
    @State private var durationMs: Double? = nil
    @State private var completedAt: String? = nil

    public init(account: Account, store: AppStore, onClose: @escaping () -> Void) {
        self.account = account
        self.store = store
        self.onClose = onClose
        self._mode = State(initialValue: account.platform == "grok" ? "text" : "default")
    }

    private var supportsQuotaCycle: Bool {
        account.platform == "openai" && account.type == "oauth"
    }

    private var modeChoices: [(id: String, label: String)] {
        if account.platform == "grok" {
            return [
                ("text", "文本"), ("image", "图像"), ("video", "视频"),
                ("search", "搜索"), ("tts", "TTS"), ("stt", "STT"), ("realtime", "Realtime"),
            ]
        }
        if account.platform == "openai" {
            return [("default", "普通"), ("compact", "Compact")]
        }
        return [("default", "普通")]
    }

    private var isStandaloneMode: Bool {
        ["search", "tts", "stt", "realtime"].contains(mode)
    }

    private var isOpenAIImageModel: Bool {
        account.platform == "openai" && mode == "default" && model.hasPrefix("gpt-image-")
    }

    private var hasPromptField: Bool {
        ["image", "video", "search", "tts"].contains(mode) || isOpenAIImageModel
    }

    private var hasMediaUpload: Bool {
        (account.platform == "grok" && ["image", "video", "stt"].contains(mode)) || isOpenAIImageModel
    }

    private var availableModels: [AccountModelItem] {
        Self.modelsForMode(candidateModels, platform: account.platform, mode: mode)
    }

    private var scheduledOperations: [Operation] {
        store.operations.rows.filter { job in
            job.account_id == account.id
                && (job.scheduled_at != nil || job.repeat_mode != nil)
                && OperationStore.isPending(job)
        }
    }

    private var hasExistingCycle: Bool {
        scheduledOperations.contains(where: OperationStore.isRepeating)
    }

    public static func modelsForMode(_ models: [AccountModelItem], platform: String, mode: String) -> [AccountModelItem] {
        guard platform == "grok" else { return models }
        if ["search", "tts", "stt", "realtime"].contains(mode) { return [] }
        let isImage: (String) -> Bool = { id in
            id == "grok-imagine" || id == "grok-imagine-edit" || id.hasPrefix("grok-imagine-image")
        }
        let isVideo: (String) -> Bool = { id in
            id.hasPrefix("grok-imagine-video") || id.hasPrefix("grok-video")
        }
        return models.filter { m in
            if mode == "image" { return isImage(m.id) }
            if mode == "video" { return isVideo(m.id) }
            return !isImage(m.id) && !isVideo(m.id)
        }
    }

    public static func preferredModel(_ models: [AccountModelItem], platform: String, mode: String) -> String {
        let opts = modelsForMode(models, platform: platform, mode: mode)
        if platform == "grok" {
            return opts.first(where: { $0.id.contains("grok-4.5") })?.id
                ?? opts.first(where: { $0.id == "grok" })?.id
                ?? opts.first?.id ?? ""
        }
        return opts.first(where: { $0.id.contains("sonnet") })?.id ?? opts.first?.id ?? ""
    }

    private static func nextBeijingMinuteString(now: Date = Date()) -> String {
        let nextMin = Date(timeIntervalSince1970: (floor(now.timeIntervalSince1970 / 60.0) + 1.0) * 60.0)
        let df = DateFormatter()
        df.locale = Locale(identifier: "en_US_POSIX")
        df.timeZone = TimeZone(identifier: "Asia/Shanghai") ?? .current
        df.dateFormat = "yyyy-MM-dd HH:mm:ss"
        return df.string(from: nextMin)
    }

    private static func formatBeijingTimestamp(_ date: Date) -> String {
        let df = DateFormatter()
        df.locale = Locale(identifier: "en_US_POSIX")
        df.timeZone = TimeZone(identifier: "Asia/Shanghai") ?? .current
        df.dateFormat = "yyyy-MM-dd HH:mm:ss"
        return df.string(from: date)
    }

    public static func parseBeijingAppointment(_ raw: String) -> Date? {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        let df = DateFormatter()
        df.locale = Locale(identifier: "en_US_POSIX")
        df.timeZone = TimeZone(identifier: "Asia/Shanghai") ?? .current
        for fmt in ["yyyy-MM-dd HH:mm:ss", "yyyy-MM-dd HH:mm", "yyyy-MM-dd'T'HH:mm:ss", "yyyy-MM-dd'T'HH:mm"] {
            df.dateFormat = fmt
            if let d = df.date(from: trimmed) {
                return d
            }
        }
        return nil
    }

    public static func nextCycleAppointmentDate(usage: UsageSummary?, windows: [UsageWindow], now: Date = Date()) -> Date? {
        let quota = usage?.key_quota?.state == "ok" ? usage?.key_quota : nil
        var rawResets: [String] = (usage?.windows ?? windows)
            .filter { $0.status != "error" && $0.status != "unknown" }
            .compactMap(\.reset_at)
        if let plans = quota?.plans {
            rawResets.append(contentsOf: plans.compactMap(\.reset_at))
        }
        if let subs = quota?.subscriptions {
            for sub in subs where sub.active != false {
                if let exp = sub.expires_at, let expDate = BeijingTime.parseISO(exp), expDate <= now {
                    continue
                }
                rawResets.append(contentsOf: sub.plans.compactMap(\.reset_at))
            }
        }
        let candidates = rawResets.compactMap { str -> Date? in
            guard let d = BeijingTime.parseISO(str) else { return nil }
            let target = Date(timeIntervalSince1970: ceil(d.timeIntervalSince1970) + 3.0)
            return target > now ? target : nil
        }
        return candidates.min()
    }

    public var body: some View {
        let execChoices: [(ExecutionKind, String)] = supportsQuotaCycle
            ? [(.now, "立即"), (.scheduled, "指定时间"), (.cycle, "持续预约 · 最近周期 +3 秒")]
            : [(.now, "立即"), (.scheduled, "指定时间")]
        let currentExecLabel = execChoices.first(where: { $0.0 == execution })?.1 ?? "立即"
        let currentModeLabel = modeChoices.first(where: { $0.id == mode })?.label ?? "普通"
        let currentModelLabel: String = {
            if model.isEmpty { return "默认模型" }
            if let found = availableModels.first(where: { $0.id == model }) {
                return found.display_name.isEmpty ? found.id : found.display_name
            }
            return "\(model)（当前不可用）"
        }()

        LinearModalCard(
            title: "测试连接",
            subtitle: account.name,
            width: 560,
            onClose: handleClose
        ) {
            VStack(alignment: .leading, spacing: 12) {
                HStack(alignment: .top, spacing: 10) {
                    VStack(alignment: .leading, spacing: 6) {
                        Text("执行")
                            .font(DS.fontCompactMedium)
                            .foregroundStyle(DS.muted)
                        LinearSelect(
                            label: currentExecLabel,
                            accessibilityTitle: "执行方式",
                            height: DS.controlHeight,
                            width: execution == .cycle ? 196 : 112,
                            disabled: running || cycleBusy,
                            entries: execChoices.map { item in
                                LinearMenuEntry(id: item.0.rawValue, title: item.1, checked: execution == item.0) {
                                    execution = item.0
                                    if item.0 != .now {
                                        if scheduledText.isEmpty {
                                            scheduledText = Self.nextBeijingMinuteString()
                                        }
                                        let baseMode = account.platform == "grok" ? "text" : "default"
                                        if mode != baseMode {
                                            switchMode(baseMode)
                                        }
                                    }
                                }
                            }
                        )
                    }

                    VStack(alignment: .leading, spacing: 6) {
                        Text("模式")
                            .font(DS.fontCompactMedium)
                            .foregroundStyle(DS.muted)
                        LinearSelect(
                            label: currentModeLabel,
                            accessibilityTitle: "测试模式",
                            height: DS.controlHeight,
                            width: 102,
                            disabled: running || execution != .now || modeChoices.count <= 1,
                            entries: modeChoices.map { item in
                                LinearMenuEntry(id: item.id, title: item.label, checked: mode == item.id) {
                                    switchMode(item.id)
                                }
                            }
                        )
                    }

                    if !isStandaloneMode {
                        VStack(alignment: .leading, spacing: 6) {
                            Text("模型")
                                .font(DS.fontCompactMedium)
                                .foregroundStyle(DS.muted)
                            HStack(spacing: 6) {
                                LinearSelect(
                                    label: currentModelLabel,
                                    accessibilityTitle: "测试模型",
                                    height: DS.controlHeight,
                                    expandWidth: true,
                                    disabled: running || loadingModels,
                                    entries: [
                                        LinearMenuEntry(id: "__default__", title: "默认模型", checked: model.isEmpty) {
                                            clearMediaIfNeeded(nextModel: "")
                                            model = ""
                                        },
                                    ] + (availableModels.isEmpty ? [] : [.separator()]) + availableModels.map { item in
                                        LinearMenuEntry(
                                            id: item.id,
                                            title: item.display_name.isEmpty ? item.id : item.display_name,
                                            checked: model == item.id
                                        ) {
                                            clearMediaIfNeeded(nextModel: item.id)
                                            model = item.id
                                        }
                                    }
                                )

                                LinearIconButton(
                                    systemName: "arrow.clockwise",
                                    label: "刷新模型列表",
                                    size: 13,
                                    boxSize: DS.controlHeight,
                                    bordered: true,
                                    disabled: !store.state.online || loadingModels || running
                                ) {
                                    Task { await loadCandidateModels(refresh: true) }
                                }
                            }
                        }
                        .frame(maxWidth: .infinity)
                    } else {
                        Spacer(minLength: 0)
                    }
                }

                if execution == .scheduled {
                    HStack(alignment: .bottom, spacing: 8) {
                        VStack(alignment: .leading, spacing: 6) {
                            Text("预约时间（北京时间）")
                                .font(DS.fontCompactMedium)
                                .foregroundStyle(DS.muted)
                            TextField(text: $scheduledText, prompt: Text(verbatim: "YYYY-MM-DD HH:mm:ss").foregroundStyle(DS.placeholder)) { EmptyView() }
                                .textFieldStyle(.plain)
                                .font(DS.fontBody)
                                .monospacedDigit()
                                .padding(.horizontal, 10)
                                .frame(height: DS.controlHeight)
                                .background(DS.surface)
                                .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                                .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
                                .disabled(running || cycleBusy)
                        }
                        .frame(maxWidth: .infinity)

                        Button(cycleBusy ? "读取中…" : "最近周期 +3 秒") {
                            Task { await fillCycleTime() }
                        }
                        .buttonStyle(LinearButtonStyle(variant: .secondary))
                        .disabled(running || cycleBusy || !store.state.online)
                    }
                } else if execution == .cycle {
                    Text("按最近额度周期重置后 3 秒持续测试，7d 额度满 100% 且上游 429 时停止。可在操作待办停止后续预约。")
                        .font(DS.fontCompact)
                        .foregroundStyle(DS.muted)
                }

                if hasPromptField && execution == .now {
                    VStack(alignment: .leading, spacing: 6) {
                        Text("提示词")
                            .font(DS.fontCompactMedium)
                            .foregroundStyle(DS.muted)
                        let placeholderText = isOpenAIImageModel ? (Self.defaultPrompts["image"] ?? "") : (Self.defaultPrompts[mode] ?? "")
                        ZStack(alignment: .topLeading) {
                            if prompt.isEmpty && !placeholderText.isEmpty {
                                Text(verbatim: placeholderText)
                                    .font(DS.fontBody)
                                    .foregroundStyle(DS.placeholder)
                                    .padding(.horizontal, 11)
                                    .padding(.vertical, 9)
                                    .allowsHitTesting(false)
                            }
                            TextEditor(text: $prompt)
                                .font(DS.fontBody)
                                .scrollContentBackground(.hidden)
                                .padding(6)
                        }
                        .frame(height: 68)
                        .background(DS.surface)
                        .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                        .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
                        .disabled(running)
                    }
                }

                if hasMediaUpload && execution == .now {
                    HStack(spacing: 8) {
                        Text(mode == "stt" ? "音频素材（可选）" : "图片素材（可选）")
                            .font(DS.fontCompactMedium)
                            .foregroundStyle(DS.muted)
                        Spacer()
                        if attachedFileName.isEmpty {
                            Button("选择文件") {
                                pickAttachment()
                            }
                            .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                            .disabled(running)
                        } else {
                            Text(verbatim: attachedFileName)
                                .font(DS.fontPriorityMeta)
                                .foregroundStyle(DS.ink)
                                .lineLimit(1)
                            Button("移除") {
                                clearMedia()
                            }
                            .font(DS.fontPriorityMeta)
                            .foregroundStyle(DS.bad)
                            .buttonStyle(.plain)
                            .disabled(running)
                        }
                    }
                }

                if !scheduledOperations.isEmpty {
                    VStack(spacing: 6) {
                        ForEach(scheduledOperations) { job in
                            let isRep = OperationStore.isRepeating(job)
                            let reqModel = job.requested["model_id"]?.stringValue ?? ""
                            let stateMap = [
                                "queued": "待执行", "running": "测试中", "checking": "核对中",
                                "needs_confirmation": "待核对", "completed": "准备下一次预约",
                            ]
                            HStack(spacing: 8) {
                                Text(BeijingTime.fullTime(job.scheduled_at))
                                    .font(DS.fontResetTime)
                                    .monospacedDigit()
                                    .foregroundStyle(DS.ink)
                                Text("\(isRep ? "持续预约 · " : "")\(reqModel.isEmpty ? "默认模型" : reqModel)")
                                    .font(DS.fontResetTime)
                                    .foregroundStyle(DS.muted)
                                    .lineLimit(1)
                                Spacer()
                                Text(job.repeat_reason ?? stateMap[job.status] ?? job.status)
                                    .font(DS.fontResetTime)
                                    .foregroundStyle(DS.muted)
                                if job.status == "queued" || isRep {
                                    LinearIconButton(
                                        systemName: "xmark",
                                        label: isRep ? "停止连续预约" : "取消预约",
                                        size: 10,
                                        boxSize: 22,
                                        disabled: running || !store.state.online
                                    ) {
                                        Task {
                                            _ = try? await store.operations.cancelJob(job)
                                        }
                                    }
                                }
                            }
                            .padding(.horizontal, 10)
                            .padding(.vertical, 6)
                            .background(DS.surfaceSubtle)
                            .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(DS.line, lineWidth: 1))
                            .clipShape(RoundedRectangle(cornerRadius: 6))
                        }
                    }
                }

                let hasResultContent = !store.state.online || !modelError.isEmpty || !statusLine.isEmpty
                    || !outputText.isEmpty || !mediaLinks.isEmpty || !errorText.isEmpty
                if hasResultContent {
                    VStack(alignment: .leading, spacing: 8) {
                        if !store.state.online {
                            Text("当前离线，请恢复连接后操作。")
                                .font(DS.fontCompact)
                                .foregroundStyle(DS.bad)
                        }
                        if !modelError.isEmpty {
                            Text(modelError)
                                .font(DS.fontCompact)
                                .foregroundStyle(DS.bad)
                        }
                        if !statusLine.isEmpty {
                            HStack(spacing: 6) {
                                if running {
                                    ProgressView().controlSize(.small)
                                } else {
                                    StatusDot(errorText.isEmpty ? .good : .bad)
                                }
                                Text(statusLine)
                                    .font(DS.fontCompactSemibold)
                                    .foregroundStyle(errorText.isEmpty ? DS.ink : DS.bad)
                                Spacer()
                                if let ms = durationMs {
                                    let suffix = completedAt.map { " · \(BeijingTime.fullTime($0))" } ?? ""
                                    Text(String(format: "%.2f s%@", ms / 1000.0, suffix))
                                        .font(DS.fontResetTime)
                                        .monospacedDigit()
                                        .foregroundStyle(DS.muted)
                                }
                            }
                        }
                        if !outputText.isEmpty {
                            DiagnosticTextView(text: outputText, monospaced: false)
                        }
                        ForEach(mediaLinks, id: \.self) { link in
                            Text(verbatim: link)
                                .font(DS.fontPriorityMeta)
                                .foregroundStyle(DS.accent)
                                .textSelection(.enabled)
                        }
                        if !errorText.isEmpty {
                            DiagnosticTextView(text: errorText)
                        }
                    }
                    .frame(maxWidth: .infinity, minHeight: 36, alignment: .topLeading)
                }
            }
        } footer: {
            if running {
                if execution != .now {
                    Button("正在提交预约") {}
                        .buttonStyle(LinearButtonStyle(variant: .secondary))
                        .disabled(true)
                } else {
                    Button {
                        Task { await cancelRun() }
                    } label: {
                        HStack(spacing: 5) {
                            Image(systemName: "stop")
                                .font(DS.iconFont(11, weight: .semibold))
                            Text("取消")
                        }
                    }
                    .buttonStyle(LinearButtonStyle(variant: .secondary))
                }
            } else {
                Button {
                    Task { await startRun() }
                } label: {
                    HStack(spacing: 5) {
                        Image(systemName: "play")
                            .font(DS.iconFont(11, weight: .semibold))
                        Text(execution == .now ? "开始测试" : "预约测试")
                    }
                }
                .buttonStyle(LinearButtonStyle(variant: .secondary))
                .disabled(!store.state.online || cycleBusy || (execution == .cycle && hasExistingCycle))
            }
            Spacer()
        }
        .sub2opsDialogFocus(onEscape: handleClose)
        .task {
            if store.state.online {
                await store.operations.pollOnce()
                await loadCandidateModels(refresh: false)
            }
        }
    }

    private func clearMedia() {
        attachedFileName = ""
        attachedDataURL = ""
    }

    private func clearMediaIfNeeded(nextModel: String) {
        let nextIsImg = account.platform == "openai" && mode == "default" && nextModel.hasPrefix("gpt-image-")
        if nextIsImg != isOpenAIImageModel {
            clearMedia()
        }
    }

    private func switchMode(_ next: String) {
        clearMedia()
        mode = next
        model = Self.preferredModel(candidateModels, platform: account.platform, mode: next)
        prompt = Self.defaultPrompts[next] ?? ""
    }

    private func loadCandidateModels(refresh: Bool) async {
        guard store.state.online else { return }
        ModelDirectoryStore.shared.bindConnection(store.connectionKey)
        loadingModels = true
        modelError = ""
        defer { loadingModels = false }
        do {
            candidateModels = try await ModelDirectoryStore.shared.readModels(accountId: account.id, refresh: refresh)
        } catch {
            if ModelDirectoryStore.isRevoked(error) {
                candidateModels = []
            }
            modelError = "模型列表读取失败：\(error.localizedDescription)"
        }
    }

    private func fillCycleTime() async {
        guard store.state.online, !cycleBusy, !running else { return }
        cycleBusy = true
        errorText = ""
        defer { cycleBusy = false }
        do {
            let usage: UsageSummary?
            if account.usage?.native_read == true {
                usage = try await RustBridge.shared.api("GET", "/accounts/\(account.id)/usage")
            } else {
                usage = account.usage
            }
            guard let targetDate = Self.nextCycleAppointmentDate(usage: usage, windows: account.usage_windows) else {
                errorText = "暂无可用的周期重置时间，请手动设置预约时间。"
                return
            }
            scheduledText = Self.formatBeijingTimestamp(targetDate)
        } catch {
            errorText = "周期时间读取失败：\(error.localizedDescription)"
        }
    }

    private func handleClose() {
        clearMedia()
        onClose()
    }

    private func pickAttachment() {
        clearMedia()
        errorText = ""
        let isAudio = (mode == "stt")
        let panel = NSOpenPanel()
        panel.allowsMultipleSelection = false
        panel.canChooseDirectories = false
        panel.allowedContentTypes = isAudio ? [.audio] : [.png, .jpeg, .webP, .gif, .image]
        guard panel.runModal() == .OK, let url = panel.url, let data = try? Data(contentsOf: url) else { return }
        let maxBytes = (isAudio ? 6 : 4) * 1024 * 1024
        guard data.count <= maxBytes else {
            errorText = isAudio ? "音频不得超过 6 MiB" : "图片不得超过 4 MiB"
            return
        }
        let ext = url.pathExtension.lowercased()
        let mime: String
        if isAudio {
            switch ext {
            case "wav": mime = "audio/wav"
            case "mp3": mime = "audio/mpeg"
            case "m4a": mime = "audio/mp4"
            case "ogg": mime = "audio/ogg"
            case "webm": mime = "audio/webm"
            default: mime = "audio/mpeg"
            }
        } else {
            switch ext {
            case "png": mime = "image/png"
            case "jpg", "jpeg": mime = "image/jpeg"
            case "webp": mime = "image/webp"
            case "gif": mime = "image/gif"
            default: mime = "image/png"
            }
        }
        attachedFileName = url.lastPathComponent
        attachedDataURL = "data:\(mime);base64,\(data.base64EncodedString())"
    }

    /// Implements exact `TestDialog.tsx` submission + Stage 0 cancel-before-job-id fix & stale operationId reset.
    private func startRun() async {
        guard store.state.online, !running, !cycleBusy else { return }

        if execution != .now {
            let continuous = (execution == .cycle)
            var scheduledISO: String? = nil
            if !continuous {
                if scheduledText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                    errorText = "请填写预约时间。"
                    return
                }
                guard let date = Self.parseBeijingAppointment(scheduledText) else {
                    errorText = "预约时间无效，请选择有效日期并精确到秒。"
                    return
                }
                guard date > Date() else {
                    errorText = "预约时间须晚于当前北京时间。"
                    return
                }
                let iso = ISO8601DateFormatter()
                iso.formatOptions = [.withInternetDateTime]
                scheduledISO = iso.string(from: date)
            } else if hasExistingCycle {
                errorText = "此账号已有连续预约，请先停止现有预约。"
                return
            }

            running = true
            errorText = ""
            defer { running = false }
            do {
                let job = try await store.operations.submitOperation(
                    account: account,
                    action: "test",
                    payload: [
                        "confirmed": .bool(true),
                        "mode": .string(account.platform == "grok" ? "text" : "default"),
                        "model_id": .string(model),
                        "prompt": .string(""),
                    ],
                    scheduledAt: scheduledISO,
                    repeatMode: continuous ? "quota_cycle" : nil
                )
                if job.status == "needs_confirmation" || job.repeat_state == "needs_confirmation" || job.status == "failed" {
                    errorText = job.repeat_reason ?? job.reason ?? "预约需确认，请核对后重新提交。"
                } else {
                    store.refresh()
                    handleClose()
                }
            } catch {
                errorText = error.localizedDescription
            }
            return
        }

        operationId = nil
        cancelled = false
        running = true
        outputText = ""
        mediaLinks = []
        errorText = ""
        durationMs = nil
        completedAt = nil
        statusLine = "正在连接"
        var terminalReceived = false

        defer {
            operationId = nil
            running = false
            if cancelled {
                statusLine = "已取消，未重放请求"
            }
            store.refresh()
        }

        let effectivePrompt: String
        if hasPromptField {
            let fallback = isOpenAIImageModel ? (Self.defaultPrompts["image"] ?? "") : (Self.defaultPrompts[mode] ?? "")
            effectivePrompt = prompt.isEmpty ? fallback : prompt
        } else {
            effectivePrompt = ""
        }

        var payload: [String: JSONValue] = [
            "confirmed": .bool(true),
            "mode": .string(mode),
            "model_id": .string(isStandaloneMode ? "" : model),
            "prompt": .string(effectivePrompt),
        ]
        if !attachedDataURL.isEmpty {
            payload[mode == "stt" ? "audio_data_url" : "image_data_url"] = .string(attachedDataURL)
        }

        do {
            let job = try await store.operations.submitOperation(
                account: account,
                action: "test",
                payload: payload
            )

            // Stage 0 fix: if user clicked Cancel before job.id arrived, immediately cancel active job on server!
            if cancelled {
                if !["completed", "failed", "cancelled", "needs_confirmation"].contains(job.status) {
                    _ = try? await store.operations.cancelJobId(job.id)
                }
                return
            }

            operationId = job.id
            _ = try await store.operations.followOperation(
                initial: job,
                onEvent: { event in
                    guard !cancelled else { return }
                    switch event.type {
                    case "content":
                        if let chunk = event.text {
                            outputText = String((outputText + chunk).suffix(500_000))
                        }
                    case "status":
                        statusLine = event.text ?? ""
                    case "test_start":
                        statusLine = event.model ?? "测试中"
                    case "image", "audio", "video":
                        for u in [event.image_url, event.audio_url, event.video_url].compactMap({ $0 }) where !u.isEmpty {
                            if !mediaLinks.contains(u) { mediaLinks.append(u) }
                        }
                    case "error", "test_complete":
                        terminalReceived = true
                        durationMs = event.duration_ms
                        completedAt = event.completed_at
                        statusLine = (event.type == "test_complete" && event.success == true) ? "测试通过" : "测试失败"
                        if let err = event.error, !err.isEmpty {
                            errorText = err
                        }
                    default:
                        break
                    }
                },
                onUpdate: { updated in
                    if let ms = updated.result?["duration_ms"]?.doubleValue {
                        durationMs = ms
                    }
                }
            )
            if !terminalReceived && !cancelled && errorText.isEmpty {
                statusLine = "测试通过"
            }
        } catch {
            if !cancelled {
                errorText = error.localizedDescription
                statusLine = "测试失败"
            }
        }
    }

    private func cancelRun() async {
        cancelled = true
        if let id = operationId {
            operationId = nil
            _ = try? await store.operations.cancelJobId(id)
        }
    }
}
