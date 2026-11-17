import AppKit
import SwiftUI
import UniformTypeIdentifiers

public struct TestDialogSheet: View {
    public let account: Account
    @Bindable public var store: AppStore
    public let onClose: () -> Void

    public enum ScheduleMode: String, CaseIterable, Sendable {
        case immediate
        case scheduled
        case quotaCycle

        public var label: String {
            switch self {
            case .immediate: return "立即执行"
            case .scheduled: return "预约时间"
            case .quotaCycle: return "连续周期测试"
            }
        }
    }

    @State private var model: String = ""
    @State private var prompt: String = "Reply with OK only."
    @State private var scheduleMode: ScheduleMode = .immediate
    @State private var scheduledDate: Date = Date().addingTimeInterval(600)
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

    public init(account: Account, store: AppStore, onClose: @escaping () -> Void) {
        self.account = account
        self.store = store
        self.onClose = onClose
    }

    private var supportsQuotaCycle: Bool {
        account.platform == "openai" && account.type == "oauth"
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    Text("测试连接 · \(account.name)")
                        .font(DS.fontDialogTitle)
                        .foregroundStyle(DS.ink)
                    Text("#\(account.id) · \(PlatformMeta.displayName(account.platform)) · \(PlatformMeta.typeLabel(account.type))")
                        .font(DS.fontResetTime)
                        .monospacedDigit()
                        .foregroundStyle(DS.muted)
                }
                Spacer()
                LinearIconButton(systemName: "xmark", label: "关闭连接测试", action: handleClose)
            }

            HStack(spacing: 10) {
                VStack(alignment: .leading, spacing: 5) {
                    Text("指定模型（留空使用默认探活模型）")
                        .font(DS.fontCompactSemibold)
                        .foregroundStyle(DS.ink)
                    TextField(text: $model, prompt: Text(verbatim: "例如 gpt-5.6-luna")) { EmptyView() }
                        .textFieldStyle(.plain)
                        .font(DS.fontMono12)
                        .padding(.horizontal, 8)
                        .frame(height: DS.controlHeightCompact)
                        .background(DS.surface)
                        .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(DS.line, lineWidth: 1))
                        .clipShape(RoundedRectangle(cornerRadius: 6))
                        .disabled(running)
                }

                VStack(alignment: .leading, spacing: 5) {
                    Text("执行方式")
                        .font(DS.fontCompactSemibold)
                        .foregroundStyle(DS.ink)
                    Picker("", selection: $scheduleMode) {
                        Text(ScheduleMode.immediate.label).tag(ScheduleMode.immediate)
                        Text(ScheduleMode.scheduled.label).tag(ScheduleMode.scheduled)
                        if supportsQuotaCycle {
                            Text(ScheduleMode.quotaCycle.label).tag(ScheduleMode.quotaCycle)
                        }
                    }
                    .labelsHidden()
                    .frame(width: 170)
                    .disabled(running)
                }
            }

            if scheduleMode != .immediate {
                HStack(spacing: 10) {
                    Text("首次执行时间（北京时间）")
                        .font(DS.fontCompact)
                        .foregroundStyle(DS.muted)
                    DatePicker(
                        "",
                        selection: $scheduledDate,
                        displayedComponents: [.date, .hourAndMinute]
                    )
                    .labelsHidden()
                    .environment(\.timeZone, TimeZone(identifier: "Asia/Shanghai") ?? .current)
                    .disabled(running)
                    Spacer()
                }
            }

            VStack(alignment: .leading, spacing: 5) {
                HStack {
                    Text("测试提示词")
                        .font(DS.fontCompactSemibold)
                        .foregroundStyle(DS.ink)
                    Spacer()
                    if attachedFileName.isEmpty {
                        Button("附加媒体文件") {
                            pickAttachment()
                        }
                        .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                        .disabled(running)
                    } else {
                        HStack(spacing: 6) {
                            Text(verbatim: attachedFileName)
                                .font(DS.fontMono11)
                                .foregroundStyle(DS.ink)
                                .lineLimit(1)
                            Button("移除") {
                                attachedFileName = ""
                                attachedDataURL = ""
                            }
                            .buttonStyle(LinearButtonStyle(variant: .ghost, compact: true))
                            .disabled(running)
                        }
                    }
                }
                TextField(text: $prompt, prompt: Text(verbatim: "输入探活提示词")) { EmptyView() }
                    .textFieldStyle(.plain)
                    .font(DS.fontBody)
                    .padding(.horizontal, 8)
                    .frame(height: DS.controlHeightCompact)
                    .background(DS.surface)
                    .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(DS.line, lineWidth: 1))
                    .clipShape(RoundedRectangle(cornerRadius: 6))
                    .disabled(running)
            }

            if !statusLine.isEmpty || !outputText.isEmpty || !errorText.isEmpty || !mediaLinks.isEmpty {
                VStack(alignment: .leading, spacing: 8) {
                    HStack {
                        if running {
                            ProgressView().controlSize(.small)
                        } else {
                            StatusDot(errorText.isEmpty ? .good : .bad)
                        }
                        Text(statusLine.isEmpty ? (errorText.isEmpty ? "测试完成" : "测试未通过") : statusLine)
                            .font(DS.fontCompactSemibold)
                            .foregroundStyle(errorText.isEmpty ? DS.ink : DS.bad)
                        Spacer()
                        if let ms = durationMs {
                            Text(String(format: "%.2f s", ms / 1000.0))
                                .font(DS.fontMono11)
                                .monospacedDigit()
                                .foregroundStyle(DS.muted)
                        }
                    }

                    if !outputText.isEmpty {
                        DiagnosticTextView(text: outputText, monospaced: true)
                    }
                    if !errorText.isEmpty {
                        DiagnosticTextView(text: errorText)
                    }
                    ForEach(mediaLinks, id: \.self) { link in
                        Text(verbatim: link)
                            .font(DS.fontMono11)
                            .foregroundStyle(DS.accent)
                            .textSelection(.enabled)
                    }
                }
                .padding(12)
                .background(DS.surfaceSubtle)
                .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
            }

            HStack(spacing: 10) {
                Spacer()
                if running {
                    Button("取消") {
                        Task { await cancelRun() }
                    }
                    .buttonStyle(LinearButtonStyle(variant: .danger))
                } else {
                    Button("关闭", action: handleClose)
                        .buttonStyle(LinearButtonStyle(variant: .secondary))
                }
                Button(scheduleMode == .immediate ? (running ? "测试中…" : "开始测试") : "提交预约") {
                    Task { await startRun() }
                }
                .buttonStyle(LinearButtonStyle(variant: .primary))
                .disabled(!store.state.online || running)
            }
        }
        .padding(22)
        .frame(width: 540)
        .background(DS.surface)
        .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(DS.line, lineWidth: 1))
        .clipShape(RoundedRectangle(cornerRadius: 12))
        .sub2opsDialogFocus(onEscape: handleClose)
    }

    private func handleClose() {
        if running {
            Task {
                await cancelRun()
                onClose()
            }
        } else {
            onClose()
        }
    }

    private func pickAttachment() {
        let panel = NSOpenPanel()
        panel.allowsMultipleSelection = false
        panel.canChooseDirectories = false
        panel.allowedContentTypes = [.image, .audio, .pdf, .plainText, .data]
        guard panel.runModal() == .OK, let url = panel.url, let data = try? Data(contentsOf: url) else { return }
        guard data.count <= 6_000_000 else {
            errorText = "媒体附件不能超过 6 MB"
            return
        }
        let mime: String
        switch url.pathExtension.lowercased() {
        case "png": mime = "image/png"
        case "jpg", "jpeg": mime = "image/jpeg"
        case "webp": mime = "image/webp"
        case "wav": mime = "audio/wav"
        case "mp3": mime = "audio/mpeg"
        default: mime = "application/octet-stream"
        }
        attachedFileName = url.lastPathComponent
        attachedDataURL = "data:\(mime);base64,\(data.base64EncodedString())"
    }

    /// Implements the Stage 0 cancel-before-job-id fix & stale operationId reset.
    private func startRun() async {
        guard store.state.online, !running else { return }
        operationId = nil
        cancelled = false
        running = true
        outputText = ""
        mediaLinks = []
        errorText = ""
        durationMs = nil
        statusLine = "正在提交任务…"

        defer {
            operationId = nil
            running = false
        }

        var payload: [String: JSONValue] = [:]
        let trimmedModel = model.trimmingCharacters(in: .whitespacesAndNewlines)
        if !trimmedModel.isEmpty { payload["model"] = .string(trimmedModel) }
        let trimmedPrompt = prompt.trimmingCharacters(in: .whitespacesAndNewlines)
        if !trimmedPrompt.isEmpty { payload["prompt"] = .string(trimmedPrompt) }
        if !attachedDataURL.isEmpty {
            payload["attachment_data_url"] = .string(attachedDataURL)
            payload["attachment_name"] = .string(attachedFileName)
        }

        let isoFormatter = ISO8601DateFormatter()
        isoFormatter.formatOptions = [.withInternetDateTime]
        let scheduledISO: String? = (scheduleMode == .immediate) ? nil : isoFormatter.string(from: scheduledDate)
        let repeatMode: String? = (scheduleMode == .quotaCycle) ? "quota_cycle" : nil

        do {
            let job = try await store.operations.submitOperation(
                account: account,
                action: "test",
                payload: payload,
                scheduledAt: scheduledISO,
                repeatMode: repeatMode
            )
            operationId = job.id

            // Stage 0 fix: if user clicked Cancel before job.id arrived, immediately cancel on server!
            if cancelled {
                _ = try? await store.operations.cancelJobId(job.id)
                statusLine = "已取消，未重放请求"
                return
            }

            if scheduleMode != .immediate {
                statusLine = scheduleMode == .quotaCycle ? "已加入连续周期测试队列" : "已创建预约测活任务"
                store.showToast("\(account.name)：\(statusLine)")
                return
            }

            _ = try await store.operations.followOperation(
                initial: job,
                onEvent: { ev in
                    applyEvent(ev)
                }
            )
            if !cancelled && errorText.isEmpty {
                statusLine = "连接测试通过"
            }
            store.refresh()
        } catch {
            if !cancelled {
                errorText = error.localizedDescription
                statusLine = "测试失败"
            }
        }
    }

    private func cancelRun() async {
        cancelled = true
        statusLine = "已取消，未重放请求"
        if let activeId = operationId {
            _ = try? await store.operations.cancelJobId(activeId)
        }
    }

    private func applyEvent(_ event: TestEvent) {
        switch event.type {
        case "status":
            if let text = event.text, !text.isEmpty { statusLine = text }
        case "content", "delta", "text":
            if let chunk = event.text { outputText += chunk }
        case "error":
            if let err = event.error ?? event.text, !err.isEmpty { errorText = err }
        case "done", "result":
            if let ms = event.duration_ms { durationMs = ms }
            if event.success == false, let err = event.error, !err.isEmpty {
                errorText = err
            }
        default:
            break
        }
        for media in [event.image_url, event.audio_url, event.video_url].compactMap({ $0 }) where !media.isEmpty {
            if !mediaLinks.contains(media) {
                mediaLinks.append(media)
            }
        }
    }
}
