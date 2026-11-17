import SwiftUI

/// Matches `ConfigForm(section: "oauth")` in `desktop/src/main.tsx`.
public struct RecoveryDetectionTabView: View {
    @Bindable public var store: AppStore

    @State private var draft: [String: JSONValue] = [:]
    @State private var changes: [String: JSONValue] = [:]
    @State private var busy: Bool = false
    @State private var conflict: Bool = false
    @State private var seededRevision: String = ""

    public init(store: AppStore) {
        self.store = store
    }

    private var serverOAuth: [String: JSONValue] {
        store.config?["oauth"]?.objectValue ?? [:]
    }

    private var serverRevision: String {
        serverOAuth["revision"]?.stringValue ?? "\(serverOAuth["revision"]?.intValue ?? 0)"
    }

    public var body: some View {
        CardSection {
            VStack(alignment: .leading, spacing: 14) {
                HStack {
                    Text("恢复、测活与模型检测")
                        .font(DS.fontSectionTitle)
                        .foregroundStyle(DS.ink)
                    Spacer()
                    Button {
                        Task { await save() }
                    } label: {
                        HStack(spacing: 5) {
                            Image(systemName: busy ? "hourglass" : "checkmark")
                                .font(DS.iconFont(12, weight: .semibold))
                            Text("保存")
                        }
                    }
                    .buttonStyle(LinearButtonStyle(variant: .primary, compact: true))
                    .disabled(!store.state.online || busy || changes.isEmpty || conflict)
                }

                Divider().overlay(DS.line)

                VStack(alignment: .leading, spacing: 10) {
                    boolRow(key: "oauth_recovery_monitor_enabled", label: "额度恢复监控")
                    boolRow(key: "oauth_early_reset_enabled", label: "提前重置检测")
                    intRow(
                        key: "oauth_early_reset_interval_seconds",
                        label: "检测间隔（秒）",
                        min: 2,
                        disabled: draft["oauth_early_reset_enabled"]?.boolValue != true
                    )
                    boolRow(key: "oauth_auto_reset_credit_enabled", label: "7d 100% 且 429 时自动用卡")
                    boolRow(key: "oauth_daily_test_enabled", label: "每日定时测活")
                    textRow(key: "oauth_daily_test_time", label: "测活时间（北京时间）", placeholder: "08:00")
                    intRow(key: "oauth_usage_refresh_concurrency", label: "额度查询并发", min: 1)
                    intRow(key: "oauth_recovery_test_concurrency", label: "测活并发", min: 1)
                    intRow(key: "connection_test_retry_interval_seconds", label: "连接测试重试间隔（秒）", min: 1)
                    intRow(key: "connection_test_max_retries", label: "连接测试最大重试次数", min: 0)

                    AccountModelFieldView(
                        label: "测活模型",
                        value: draft["oauth_recovery_test_model_id"]?.stringValue ?? "",
                        accounts: store.accounts,
                        online: store.state.online,
                        connectionKey: store.connectionKey
                    ) { v in
                        setField("oauth_recovery_test_model_id", .string(v))
                    }

                    AccountModelFieldView(
                        label: "自动检测模型",
                        value: draft["model_detection_model_id"]?.stringValue ?? "",
                        accounts: store.accounts,
                        online: store.state.online,
                        connectionKey: store.connectionKey
                    ) { v in
                        setField("model_detection_model_id", .string(v))
                    }

                    Divider().overlay(DS.line)

                    DetectionPlatformSettingsView(
                        value: draft["model_detection_platforms"],
                        online: store.state.online,
                        connectionKey: store.connectionKey
                    ) { nextVal in
                        setField("model_detection_platforms", nextVal)
                    }

                    Divider().overlay(DS.line)

                    RecoveryAccountsPickerView(
                        accounts: store.accounts,
                        connectionIds: intArray(draft["oauth_recovery_connection_account_ids"]),
                        modelIds: intArray(draft["oauth_recovery_model_account_ids"])
                    ) { conn, models in
                        setField("oauth_recovery_connection_account_ids", .array(conn.map { .int(Int64($0)) }))
                        setField("oauth_recovery_model_account_ids", .array(models.map { .int(Int64($0)) }))
                    }
                }
                .disabled(!store.state.online || busy)

                if conflict {
                    HStack(spacing: 10) {
                        Text("云端配置已变化。")
                            .font(DS.fontCompactMedium)
                            .foregroundStyle(DS.warn)
                        Spacer()
                        Button("放弃本地修改并刷新") {
                            Task {
                                await store.reloadConfig()
                                syncFromServer(force: true)
                            }
                        }
                        .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                    }
                    .padding(10)
                    .background(DS.warnBg)
                    .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.warn, lineWidth: 1))
                    .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
                }
            }
        }
        .frame(maxWidth: 860)
        .onAppear { syncFromServer(force: false) }
        .onChange(of: serverRevision) { _, _ in syncFromServer(force: false) }
    }

    private func syncFromServer(force: Bool) {
        let current = serverOAuth
        guard !current.isEmpty else { return }
        if force || changes.isEmpty {
            draft = current
            changes = [:]
            conflict = false
            seededRevision = serverRevision
        } else if seededRevision != serverRevision {
            conflict = true
        }
    }

    private func setField(_ key: String, _ value: JSONValue) {
        draft[key] = value
        changes[key] = value
    }

    private func intArray(_ val: JSONValue?) -> [Int] {
        (val?.arrayValue ?? []).compactMap { $0.intValue.map(Int.init) }
    }

    private func boolRow(key: String, label: String) -> some View {
        let isOn = draft[key]?.boolValue ?? false
        return HStack {
            Text(label)
                .font(DS.fontBody)
                .foregroundStyle(DS.ink)
            Spacer()
            LinearSwitch(isOn: isOn, label: label) {
                setField(key, .bool(!isOn))
            }
        }
        .frame(minHeight: DS.controlHeightDense)
    }

    private func textRow(key: String, label: String, placeholder: String) -> some View {
        let cur = draft[key]?.stringValue ?? ""
        return HStack {
            Text(label)
                .font(DS.fontBody)
                .foregroundStyle(DS.ink)
            Spacer()
            TextField(
                text: Binding(get: { cur }, set: { setField(key, .string($0)) }),
                prompt: Text(verbatim: placeholder)
            ) { EmptyView() }
            .textFieldStyle(.plain)
            .font(DS.fontMono12)
            .monospacedDigit()
            .padding(.horizontal, 10)
            .frame(width: 140, height: DS.controlHeightDense)
            .background(DS.surface)
            .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
            .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
        }
    }

    private func intRow(key: String, label: String, min: Int, disabled: Bool = false) -> some View {
        let curInt = Int(draft[key]?.intValue ?? Int64(min))
        return HStack {
            Text(label)
                .font(DS.fontBody)
                .foregroundStyle(disabled ? DS.muted : DS.ink)
            Spacer()
            HStack(spacing: 6) {
                TextField(
                    text: Binding(
                        get: { "\(curInt)" },
                        set: { raw in
                            if let parsed = Int(raw.trimmingCharacters(in: .whitespaces)), parsed >= min {
                                setField(key, .int(Int64(parsed)))
                            }
                        }
                    ),
                    prompt: Text(verbatim: "\(min)")
                ) { EmptyView() }
                .textFieldStyle(.plain)
                .font(DS.fontMono12)
                .monospacedDigit()
                .multilineTextAlignment(.trailing)
                .padding(.horizontal, 10)
                .frame(width: 96, height: DS.controlHeightDense)
                .background(DS.surface)
                .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
                .disabled(disabled)
                .opacity(disabled ? 0.5 : 1.0)
            }
        }
    }

    private func save() async {
        guard store.state.online, !busy, !changes.isEmpty, !conflict else { return }
        busy = true
        defer { busy = false }
        let revVal = draft["revision"] ?? .int(0)
        do {
            let updated: [String: JSONValue] = try await RustBridge.shared.api(
                "PUT",
                "/config/oauth",
                body: .object([
                    "expected_revision": revVal,
                    "changes": .object(changes),
                ])
            )
            draft = updated
            changes = [:]
            conflict = false
            var nextCfg = store.config ?? [:]
            nextCfg["oauth"] = .object(updated)
            store.config = nextCfg
            seededRevision = serverRevision
            store.showToast("设置已保存")
        } catch {
            store.report(error)
            if error.localizedDescription.contains("409") {
                conflict = true
            }
        }
    }
}
