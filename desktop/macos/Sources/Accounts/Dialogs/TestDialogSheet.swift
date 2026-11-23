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

    static let defaultPrompts: [String: String] = [
        "image": "Generate a cute orange cat astronaut sticker on a clean pastel background.",
        "video": "A red ball bouncing once on a white floor, short simple motion.",
        "search": "xAI Grok",
        "tts": "Hello from Sub2API account connectivity test.",
    ]

    @State var execution: ExecutionKind = .now
    @State var mode: String
    @State var model: String = ""
    @State var candidateModels: [ModelOption] = []
    @State var loadingModels: Bool = false
    @State var modelError: String = ""
    @State var prompt: String = ""
    @State var scheduledText: String = ""
    @State var cycleBusy: Bool = false
    @State var attachedFileName: String = ""
    @State var attachedDataURL: String = ""
    @State var running: Bool = false
    @State var cancelled: Bool = false
    @State var operationId: String? = nil
    @State var statusLine: String = ""
    @State var outputText: String = ""
    @State var mediaLinks: [String] = []
    @State var errorText: String = ""
    @State var durationMs: Double? = nil
    @State var completedAt: String? = nil

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

    var isStandaloneMode: Bool {
        ["search", "tts", "stt", "realtime"].contains(mode)
    }

    var isOpenAIImageModel: Bool {
        account.platform == "openai" && mode == "default" && model.hasPrefix("gpt-image-")
    }

    var hasPromptField: Bool {
        ["image", "video", "search", "tts"].contains(mode) || isOpenAIImageModel
    }

    private var hasMediaUpload: Bool {
        (account.platform == "grok" && ["image", "video", "stt"].contains(mode)) || isOpenAIImageModel
    }

    private var availableModels: [ModelOption] {
        Self.modelsForMode(candidateModels, platform: account.platform, mode: mode)
    }

    private var scheduledOperations: [Operation] {
        store.operations.rows.filter { job in
            job.account_id == account.id
                && (job.scheduled_at != nil || job.repeat_mode != nil)
                && OperationStore.isPending(job)
        }
    }

    var hasExistingCycle: Bool {
        scheduledOperations.contains(where: OperationStore.isRepeating)
    }

    public static func modelsForMode(_ models: [ModelOption], platform: String, mode: String) -> [ModelOption] {
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

    public static func preferredModel(_ models: [ModelOption], platform: String, mode: String) -> String {
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

    static func formatBeijingTimestamp(_ date: Date) -> String {
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
                return found.titleText
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
                                            title: item.titleText,
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
                            .buttonStyle(.linearPlain)
                            .disabled(running)
                        }
                    }
                }

                if !scheduledOperations.isEmpty {
                    VStack(spacing: 6) {
                        ForEach(scheduledOperations) { job in
                            let isRep = OperationStore.isRepeating(job)
                            let reqModel = job.requested.model_id ?? ""
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
}
