import SwiftUI

public struct ModelDetectionSheet: View {
    public let account: Account
    @Bindable public var store: AppStore
    public let onClose: () -> Void

    @State private var enabled: Bool = false
    @State private var scheduleMode: String = "fixed"
    @State private var intervalText: String = "60"
    @State private var modelId: String = ""
    @State private var verdictMode: String = "strict"
    @State private var degradedModelId: String = "gpt-5.6-luna"
    @State private var version: String = ""
    @State private var statusLine: String = ""
    @State private var nextAt: String? = nil
    @State private var loading: Bool = false
    @State private var saving: Bool = false
    @State private var error: String = ""

    public init(account: Account, store: AppStore, onClose: @escaping () -> Void) {
        self.account = account
        self.store = store
        self.onClose = onClose
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    Text("定时降智检测 · \(account.name)")
                        .font(DS.fontDialogTitle)
                        .foregroundStyle(DS.ink)
                    Text("#\(account.id) · \(PlatformMeta.displayName(account.platform))")
                        .font(DS.fontResetTime)
                        .monospacedDigit()
                        .foregroundStyle(DS.muted)
                }
                Spacer()
                LinearIconButton(systemName: "xmark", label: "关闭定时检测设置", disabled: saving, action: onClose)
            }

            HStack {
                Text("启用账号级定时检测")
                    .font(DS.fontBodyMedium)
                    .foregroundStyle(DS.ink)
                Spacer()
                LinearSwitch(isOn: $enabled.wrappedValue, disabled: !store.state.online || loading || saving, label: "启用定时检测") {
                    enabled.toggle()
                }
            }
            .padding(12)
            .background(DS.surfaceSubtle)
            .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))

            HStack(spacing: 10) {
                VStack(alignment: .leading, spacing: 5) {
                    Text("调度模式")
                        .font(DS.fontCompactSemibold)
                        .foregroundStyle(DS.ink)
                    Picker("", selection: $scheduleMode) {
                        Text("固定间隔").tag("fixed")
                        Text("流量自适应").tag("adaptive")
                    }
                    .labelsHidden()
                    .disabled(!enabled || saving)
                }

                VStack(alignment: .leading, spacing: 5) {
                    Text(scheduleMode == "adaptive" ? "最小间隔（分钟）" : "检测间隔（分钟）")
                        .font(DS.fontCompactSemibold)
                        .foregroundStyle(DS.ink)
                    TextField(text: $intervalText, prompt: Text(verbatim: "例如 60")) { EmptyView() }
                        .textFieldStyle(.plain)
                        .font(DS.fontBody)
                        .monospacedDigit()
                        .padding(.horizontal, 8)
                        .frame(height: DS.controlHeightCompact)
                        .background(DS.surface)
                        .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(DS.line, lineWidth: 1))
                        .clipShape(RoundedRectangle(cornerRadius: 6))
                        .disabled(!enabled || saving)
                }
            }

            VStack(alignment: .leading, spacing: 5) {
                Text("检测模型 ID")
                    .font(DS.fontCompactSemibold)
                    .foregroundStyle(DS.ink)
                TextField(text: $modelId, prompt: Text(verbatim: "例如 gpt-6-luna")) { EmptyView() }
                    .textFieldStyle(.plain)
                    .font(DS.fontMono12)
                    .padding(.horizontal, 8)
                    .frame(height: DS.controlHeightCompact)
                    .background(DS.surface)
                    .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(DS.line, lineWidth: 1))
                    .clipShape(RoundedRectangle(cornerRadius: 6))
                    .disabled(!enabled || saving)
            }

            if account.platform == "openai" {
                HStack(spacing: 10) {
                    VStack(alignment: .leading, spacing: 5) {
                        Text("判定模式")
                            .font(DS.fontCompactSemibold)
                            .foregroundStyle(DS.ink)
                        Picker("", selection: $verdictMode) {
                            Text("严格模式").tag("strict")
                            Text("宽松模式").tag("relaxed")
                        }
                        .labelsHidden()
                        .disabled(!enabled || saving)
                    }
                    if verdictMode == "relaxed" {
                        VStack(alignment: .leading, spacing: 5) {
                            Text("参考降智模型")
                                .font(DS.fontCompactSemibold)
                                .foregroundStyle(DS.ink)
                            TextField(text: $degradedModelId, prompt: Text(verbatim: "gpt-5.6-luna")) { EmptyView() }
                                .textFieldStyle(.plain)
                                .font(DS.fontMono12)
                                .padding(.horizontal, 8)
                                .frame(height: DS.controlHeightCompact)
                                .background(DS.surface)
                                .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(DS.line, lineWidth: 1))
                                .clipShape(RoundedRectangle(cornerRadius: 6))
                                .disabled(!enabled || saving)
                        }
                    }
                }
            }

            if !statusLine.isEmpty || nextAt != nil {
                HStack(spacing: 8) {
                    if !statusLine.isEmpty {
                        Text(statusLine)
                            .font(DS.fontResetTime)
                            .foregroundStyle(DS.muted)
                    }
                    Spacer()
                    if let nextAt, !nextAt.isEmpty {
                        Text("下次检测 \(BeijingTime.fullTime(nextAt))")
                            .font(DS.fontResetTime)
                            .monospacedDigit()
                            .foregroundStyle(DS.muted)
                    }
                }
            }

            if !error.isEmpty {
                Text(error)
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.bad)
            }

            HStack(spacing: 10) {
                Spacer()
                Button("取消", action: onClose)
                    .buttonStyle(LinearButtonStyle(variant: .secondary))
                    .disabled(saving)
                Button(saving ? "保存中…" : "保存配置") {
                    Task { await save() }
                }
                .buttonStyle(LinearButtonStyle(variant: .primary))
                .disabled(!store.state.online || loading || saving)
            }
        }
        .padding(22)
        .frame(width: 480)
        .background(DS.surface)
        .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(DS.line, lineWidth: 1))
        .clipShape(RoundedRectangle(cornerRadius: 12))
        .sub2opsDialogFocus {
            if !saving { onClose() }
        }
        .task {
            await load()
        }
    }

    private func load() async {
        guard store.state.online else { return }
        loading = true
        defer { loading = false }
        do {
            let detail: ModelDetectionDetailConfig = try await RustBridge.shared.api("GET", "/accounts/\(account.id)/model-detection")
            enabled = detail.enabled
            scheduleMode = detail.schedule_mode ?? "fixed"
            intervalText = String(detail.min_interval_minutes ?? detail.interval_minutes)
            modelId = detail.model_id
            verdictMode = detail.verdict_mode ?? "strict"
            degradedModelId = detail.degraded_model_id ?? "gpt-5.6-luna"
            version = detail.version
            statusLine = detail.reason ?? detail.status ?? ""
            nextAt = detail.next_at
            error = ""
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func save() async {
        guard let mins = Int(intervalText.trimmingCharacters(in: .whitespacesAndNewlines)), mins >= 5 else {
            error = "检测间隔至少为 5 分钟"
            return
        }
        let trimmedModel = modelId.trimmingCharacters(in: .whitespacesAndNewlines)
        if enabled && trimmedModel.isEmpty {
            error = "启用定时检测时请填写目标模型 ID"
            return
        }
        saving = true
        error = ""
        defer { saving = false }
        var body: [String: JSONValue] = [
            "enabled": .bool(enabled),
            "schedule_mode": .string(scheduleMode),
            "interval_minutes": .int(Int64(mins)),
            "min_interval_minutes": .int(Int64(mins)),
            "model_id": .string(trimmedModel),
            "expected_version": .string(version),
        ]
        if account.platform == "openai" {
            body["verdict_mode"] = .string(verdictMode)
            if verdictMode == "relaxed" {
                body["degraded_model_id"] = .string(degradedModelId.trimmingCharacters(in: .whitespacesAndNewlines))
            }
        }
        do {
            let _: ModelDetectionDetailConfig = try await RustBridge.shared.api(
                "PUT",
                "/accounts/\(account.id)/model-detection",
                body: .object(body)
            )
            store.refresh()
            store.showToast("已保存 \(account.name) 定时检测设置")
            onClose()
        } catch {
            self.error = error.localizedDescription
        }
    }
}
