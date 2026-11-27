import AppKit
import SwiftUI
import UniformTypeIdentifiers

extension TestDialogSheet {
    func clearMedia() {
        attachedFileName = ""
        attachedDataURL = ""
    }

    func clearMediaIfNeeded(nextModel: String) {
        let nextIsImg = account.platform == "openai" && mode == "default" && nextModel.hasPrefix("gpt-image-")
        if nextIsImg != isOpenAIImageModel {
            clearMedia()
        }
    }

    func switchMode(_ next: String) {
        clearMedia()
        mode = next
        model = Self.preferredModel(candidateModels, platform: account.platform, mode: next)
        prompt = Self.defaultPrompts[next] ?? ""
    }

    func loadCandidateModels(refresh: Bool) async {
        await lifecycle.loadCandidateModels(
            accountId: account.id,
            refresh: refresh,
            online: store.state.online,
            connectionKey: store.connectionKey
        )
    }

    func fillCycleTime() async {
        guard store.state.online, !cycleBusy, !running, lifecycle.isOpen else { return }
        let capturedDialogSeq = lifecycle.dialogSeq
        cycleBusy = true
        errorText = ""
        defer {
            if lifecycle.isDialogActive(dialogSeq: capturedDialogSeq, connectionKey: store.connectionKey) {
                cycleBusy = false
            }
        }
        do {
            let usage: UsageSummary?
            if account.usage?.native_read == true {
                usage = try await RustBridge.shared.api("GET", "/accounts/\(account.id)/usage")
            } else {
                usage = account.usage
            }
            guard !Task.isCancelled,
                  lifecycle.isDialogActive(dialogSeq: capturedDialogSeq, connectionKey: store.connectionKey)
            else { return }
            guard let targetDate = Self.nextCycleAppointmentDate(usage: usage, windows: account.usage_windows) else {
                errorText = "暂无可用的周期重置时间，请手动设置预约时间。"
                return
            }
            scheduledText = Self.formatBeijingTimestamp(targetDate)
        } catch {
            if lifecycle.isDialogActive(dialogSeq: capturedDialogSeq, connectionKey: store.connectionKey) {
                errorText = "周期时间读取失败：\(error.localizedDescription)"
            }
        }
    }

    func handleClose() {
        lifecycle.close()
        clearMedia()
        onClose()
    }

    func pickAttachment() {
        clearMedia()
        errorText = ""
        let isAudio = (mode == "stt")
        let panel = NSOpenPanel()
        panel.allowsMultipleSelection = false
        panel.canChooseDirectories = false
        panel.allowedContentTypes = isAudio ? [.audio] : [.png, .jpeg, .webP, .gif, .image]
        guard panel.runModal() == .OK, let url = panel.url else { return }
        let data: Data
        do {
            data = try Data(contentsOf: url)
        } catch {
            errorText = error.localizedDescription
            return
        }
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
    func startRun(capturedGen: UInt64? = nil) async {
        let capturedKey = store.connectionKey
        let activeGen = capturedGen ?? lifecycle.generation
        guard store.state.online, !running, !cycleBusy, lifecycle.isOpen else { return }

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
                scheduledISO = BeijingTime.isoUTCString(from: date)
            } else if hasExistingCycle {
                errorText = "此账号已有连续预约，请先停止现有预约。"
                return
            }

            running = true
            errorText = ""
            defer {
                if lifecycle.canContinue(generation: activeGen, connectionKey: store.connectionKey) {
                    running = false
                }
            }
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
                guard lifecycle.canContinue(generation: activeGen, connectionKey: store.connectionKey) else { return }
                if job.status == "needs_confirmation" || job.repeat_state == "needs_confirmation" || job.status == "failed" {
                    errorText = job.repeat_reason ?? job.reason ?? "预约需确认，请核对后重新提交。"
                } else {
                    store.refresh()
                    handleClose()
                }
            } catch {
                if lifecycle.canContinue(generation: activeGen, connectionKey: store.connectionKey) {
                    errorText = error.localizedDescription
                }
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
            if lifecycle.canContinue(generation: activeGen, connectionKey: store.connectionKey) {
                lifecycle.bindJob(nil, generation: activeGen)
                operationId = nil
                running = false
                if cancelled {
                    statusLine = "已取消，未重放请求"
                }
            }
            if store.connectionKey == capturedKey {
                store.refresh()
            }
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
                    do {
                        _ = try await store.operations.cancelJobId(job.id)
                    } catch {
                        if store.connectionKey == capturedKey {
                            store.report(error)
                        }
                    }
                }
                return
            }

            // Do not start polling if the dialog was closed or connection switched while submitOperation was in flight.
            guard !Task.isCancelled,
                  lifecycle.canContinue(generation: activeGen, connectionKey: store.connectionKey)
            else { return }

            lifecycle.bindJob(job.id, generation: activeGen)
            operationId = job.id
            _ = try await store.operations.followOperation(
                initial: job,
                onEvent: { event in
                    guard !cancelled,
                          lifecycle.canContinue(generation: activeGen, connectionKey: store.connectionKey, jobId: job.id)
                    else { return }
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
                    guard lifecycle.canContinue(generation: activeGen, connectionKey: store.connectionKey, jobId: job.id) else { return }
                    if let ms = updated.result?.duration_ms {
                        durationMs = Double(ms)
                    }
                }
            )
            if !terminalReceived && !cancelled && errorText.isEmpty
                && lifecycle.canContinue(generation: activeGen, connectionKey: store.connectionKey, jobId: job.id)
            {
                statusLine = "测试通过"
            }
        } catch is CancellationError {
            return
        } catch {
            if !cancelled
                && lifecycle.canContinue(generation: activeGen, connectionKey: store.connectionKey)
            {
                errorText = error.localizedDescription
                statusLine = "测试失败"
            }
        }
    }

    func cancelRun() async {
        let capturedKey = store.connectionKey
        cancelled = true
        if let id = operationId {
            operationId = nil
            do {
                _ = try await store.operations.cancelJobId(id)
            } catch {
                if store.connectionKey == capturedKey {
                    store.report(error)
                }
            }
        }
    }
}
