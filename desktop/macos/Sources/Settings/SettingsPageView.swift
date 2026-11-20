import Foundation
import SwiftUI

/// Matches `SettingsPage(page: "settings")` in `desktop/src/main.tsx` (`settings_light.png`).
public struct SettingsPageView: View {
    @Bindable public var store: AppStore

    @State private var removingUrl: String? = nil
    @State private var barkDraft: [String: JSONValue] = [:]
    @State private var barkDeviceKey: String = ""
    @State private var barkChanges: [String: JSONValue] = [:]
    @State private var barkBusy: Bool = false
    @State private var barkConflict: Bool = false
    @State private var seededBarkRev: String = ""

    public init(store: AppStore) {
        self.store = store
    }

    private var serverBark: [String: JSONValue] {
        store.config?["bark"]?.objectValue ?? [:]
    }

    private var serverBarkRevision: String {
        serverBark["revision"]?.stringValue ?? "\(serverBark["revision"]?.intValue ?? 0)"
    }

    private var clientVersionString: String {
        (Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String) ?? "未知"
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack(alignment: .firstTextBaseline, spacing: 12) {
                Text("设置")
                    .font(DS.fontPageTitle)
                    .foregroundStyle(DS.ink)

                Spacer()

                if let observed = store.state.snapshot?.observed_at, !observed.isEmpty {
                    HStack(spacing: 5) {
                        Image(systemName: "clock")
                            .font(DS.iconFont(11))
                        Text("更新于 \(BeijingTime.fullTime(observed))")
                            .font(DS.fontPriorityMeta)
                            .monospacedDigit()
                    }
                    .foregroundStyle(DS.muted)
                }
            }

            VStack(alignment: .leading, spacing: 16) {
                ConnectionStatusCardView(connectionKey: store.connectionKey)

                instanceManagementCard

                clientPreferencesCard

                barkConfigCard

                connectionManagementCard
            }
            .frame(maxWidth: 820)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .task(id: "\(store.connectionKey):\(store.state.online)") {
            if store.state.online && store.config == nil {
                await store.reloadConfig()
            }
            syncBarkFromServer(force: false)
        }
        .onChange(of: serverBarkRevision) { _, _ in
            syncBarkFromServer(force: false)
        }
    }

    private var instanceManagementCard: some View {
        CardSection(padding: 20) {
            VStack(alignment: .leading, spacing: 14) {
                HStack {
                    Text("Sub2API 实例管理")
                        .font(DS.fontSectionTitle)
                        .foregroundStyle(DS.ink)
                    Spacer()
                    Button {
                        store.connectionModal = ConnectionModalTarget(baseUrl: "", name: "", requireKey: true)
                    } label: {
                        HStack(spacing: 4) {
                            Image(systemName: "plus")
                                .font(DS.iconFont(11, weight: .semibold))
                            Text("新增实例")
                        }
                    }
                    .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                }

                Rectangle().fill(DS.line).frame(height: 1)

                let connections = store.savedConnections
                if connections.isEmpty {
                    Text("尚未保存任何 Sub2API 实例。")
                        .font(DS.fontCompact)
                        .foregroundStyle(DS.muted)
                } else {
                    VStack(spacing: 8) {
                        ForEach(connections, id: \.base_url) { item in
                            let isCurrent = store.state.connected && item.base_url == store.activeUrl
                            let isSwitching = store.switchingUrl == item.base_url
                            let isRemoving = removingUrl == item.base_url

                            HStack(spacing: 10) {
                                StatusDot(isCurrent ? (store.state.online ? .good : .warning) : .muted)
                                VStack(alignment: .leading, spacing: 3) {
                                    HStack(spacing: 8) {
                                        Text(item.name)
                                            .font(DS.fontBodySemibold)
                                            .foregroundStyle(DS.ink)
                                        Text(isCurrent ? "当前连接" : "已保存")
                                            .font(DS.fontResetTimeSemibold)
                                            .foregroundStyle(isCurrent ? DS.accent : DS.muted)
                                            .padding(.horizontal, 8)
                                            .padding(.vertical, 2)
                                            .background(isCurrent ? DS.accentBg : DS.surfaceSubtle)
                                            .clipShape(Capsule())
                                    }
                                    Text(verbatim: item.base_url)
                                        .font(DS.fontPriorityMeta).monospacedDigit()
                                        .foregroundStyle(DS.muted)
                                        .lineLimit(1)
                                }
                                Spacer()
                                HStack(spacing: 6) {
                                    if !isCurrent {
                                        Button(isSwitching ? "切换中…" : "切换连接") {
                                            Task { await store.switchConnection(item) }
                                        }
                                        .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                                        .disabled(store.switchingUrl != nil || isRemoving)
                                    }

                                    Button("更新 Key / 重命名") {
                                        store.connectionModal = ConnectionModalTarget(
                                            baseUrl: item.base_url,
                                            name: item.name,
                                            requireKey: false
                                        )
                                    }
                                    .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))

                                    Button {
                                        Task { await removeInstance(item) }
                                    } label: {
                                        HStack(spacing: 4) {
                                            Image(systemName: "trash")
                                                .font(DS.iconFont(11))
                                            Text("删除实例")
                                        }
                                    }
                                    .buttonStyle(LinearButtonStyle(variant: .danger, compact: true))
                                    .disabled(isCurrent || store.switchingUrl != nil || isRemoving)
                                    .help(isCurrent ? "当前实例正在使用中，请先切换到其他实例或断开连接后再删除" : "删除此实例并清除对应的本机 Key")
                                }
                            }
                            .padding(.horizontal, 14)
                            .padding(.vertical, 12)
                            .background(DS.surface)
                            .overlay(
                                RoundedRectangle(cornerRadius: 8)
                                    .strokeBorder(isCurrent ? DS.accent.opacity(0.45) : DS.line, lineWidth: 1)
                            )
                            .clipShape(RoundedRectangle(cornerRadius: 8))
                        }
                    }
                }
            }
        }
    }

    private var clientPreferencesCard: some View {
        CardSection(padding: 20) {
            VStack(alignment: .leading, spacing: 0) {
                Text("客户端")
                    .font(DS.fontSectionTitle)
                    .foregroundStyle(DS.ink)
                    .padding(.bottom, 12)

                Rectangle().fill(DS.line).frame(height: 1)

                LinearSettingRow("开机启动") {
                    LinearSwitch(
                        isOn: store.state.preferences.launch_at_login,
                        label: "开机启动"
                    ) {
                        Task {
                            await store.updatePreferences(launchAtLogin: !store.state.preferences.launch_at_login)
                        }
                    }
                }

                LinearSettingRow("当前版本", showDivider: false) {
                    Button {
                        Task { await store.checkUpdatesAndNotify() }
                    } label: {
                        HStack(spacing: 5) {
                            Text("\(clientVersionString) · 检查更新")
                                .monospacedDigit()
                            Image(systemName: "arrow.up.right.square")
                                .font(DS.iconFont(11))
                        }
                    }
                    .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                }
            }
        }
    }

    private var barkConfigCard: some View {
        CardSection(padding: 20) {
            VStack(alignment: .leading, spacing: 0) {
                HStack {
                    Text("Bark 事件推送")
                        .font(DS.fontSectionTitle)
                        .foregroundStyle(DS.ink)
                    Spacer()
                    Button {
                        Task { await saveBark() }
                    } label: {
                        HStack(spacing: 4) {
                            Image(systemName: "checkmark")
                                .font(DS.iconFont(11, weight: .semibold))
                            Text("保存")
                        }
                    }
                    .buttonStyle(LinearButtonStyle(variant: .primary, compact: true))
                    .disabled(!store.state.online || barkBusy || barkChanges.isEmpty || barkConflict)
                }
                .padding(.bottom, 12)

                Rectangle().fill(DS.line).frame(height: 1)

                VStack(alignment: .leading, spacing: 0) {
                    let isEnabled = barkDraft["enabled"]?.boolValue ?? false
                    LinearSettingRow("启用推送") {
                        LinearSwitch(isOn: isEnabled, label: "启用推送") {
                            setBarkField("enabled", .bool(!isEnabled))
                        }
                    }

                    let keySet = serverBark["device_key_set"]?.boolValue == true
                    LinearSettingRow("Device Key\(keySet ? "（已保存，留空保留）" : "")") {
                        SecureField(
                            text: $barkDeviceKey,
                            prompt: Text(verbatim: keySet ? "留空保留现有 Device Key" : "输入 Bark Device Key").foregroundStyle(DS.placeholder)
                        ) { EmptyView() }
                        .textFieldStyle(.plain)
                        .font(DS.fontBody)
                        .foregroundStyle(DS.ink)
                        .padding(.horizontal, 12)
                        .frame(width: 260, height: DS.controlHeight)
                        .background(DS.surface)
                        .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                        .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
                        .onChange(of: barkDeviceKey) { _, next in
                            if (barkDraft["device_key"]?.stringValue ?? "") != next {
                                setBarkField("device_key", .string(next))
                            }
                        }
                    }

                    HStack {
                        Button("发送测试消息") {
                            Task { await sendBarkTest() }
                        }
                        .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                        .disabled(!store.state.online || barkBusy)
                        Spacer()
                    }
                    .padding(.top, 12)
                }
                .disabled(!store.state.online || barkBusy)

                if barkConflict {
                    HStack {
                        Text("云端配置已变化。")
                            .font(DS.fontCompactMedium)
                            .foregroundStyle(DS.warn)
                        Spacer()
                        Button("放弃本地修改并刷新") {
                            Task {
                                await store.reloadConfig()
                                syncBarkFromServer(force: true)
                            }
                        }
                        .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                    }
                    .padding(10)
                    .padding(.top, 10)
                    .background(DS.warnBg)
                    .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
                }
            }
        }
    }

    private var connectionManagementCard: some View {
        CardSection(padding: 20) {
            VStack(alignment: .leading, spacing: 14) {
                ConnectionOnboardingView(store: store, embeddedInSettings: true)

                Rectangle().fill(DS.line).frame(height: 1)

                HStack {
                    Button {
                        Task {
                            guard await store.beforeConnectionChange() else { return }
                            do {
                                try await RustBridge.shared.commandVoid("disconnect")
                                store.showToast("已断开当前连接并删除本机 Key")
                            } catch {
                                store.report(error)
                            }
                        }
                    } label: {
                        HStack(spacing: 5) {
                            Image(systemName: "bolt.slash")
                                .font(DS.iconFont(12))
                            Text("断开连接并删除本机 Key")
                        }
                    }
                    .buttonStyle(LinearButtonStyle(variant: .danger, compact: true))
                    Spacer()
                }
            }
        }
    }

    private func syncBarkFromServer(force: Bool) {
        let cur = serverBark
        guard !cur.isEmpty else { return }
        if force || barkChanges.isEmpty {
            barkDraft = cur
            barkDeviceKey = cur["device_key"]?.stringValue ?? ""
            barkChanges = [:]
            barkConflict = false
            seededBarkRev = serverBarkRevision
        } else if seededBarkRev != serverBarkRevision {
            barkConflict = true
        }
    }

    private func setBarkField(_ key: String, _ val: JSONValue) {
        barkDraft[key] = val
        barkChanges[key] = val
    }

    private func saveBark() async {
        guard store.state.online, !barkBusy, !barkChanges.isEmpty, !barkConflict else { return }
        barkBusy = true
        defer { barkBusy = false }
        let revVal = barkDraft["revision"] ?? .int(0)
        do {
            let updated: [String: JSONValue] = try await RustBridge.shared.api(
                "PUT",
                "/config/bark",
                body: .object([
                    "expected_revision": revVal,
                    "changes": .object(barkChanges),
                ])
            )
            barkDraft = updated
            barkChanges = [:]
            barkConflict = false
            var nextCfg = store.config ?? [:]
            nextCfg["bark"] = .object(updated)
            store.config = nextCfg
            seededBarkRev = serverBarkRevision
            store.showToast("设置已保存")
        } catch {
            store.report(error)
            if (error as? RustBridgeError)?.failure.status == 409 {
                barkConflict = true
            }
        }
    }

    private func sendBarkTest() async {
        struct ActionReply: Decodable, Sendable { var message: String? }
        do {
            let res: ActionReply = try await RustBridge.shared.api("POST", "/actions/bark-test")
            store.showToast(res.message ?? "操作完成")
        } catch {
            store.report(error)
        }
    }

    private func removeInstance(_ item: SavedConnection) async {
        guard removingUrl == nil else { return }
        removingUrl = item.base_url
        defer { removingUrl = nil }
        do {
            let next: ViewState = try await RustBridge.shared.command(
                "remove_connection",
                args: .object(["baseUrl": .string(item.base_url)])
            )
            store.applyViewState(next)
            store.showToast("已删除实例 \(item.name)")
        } catch {
            store.report(error)
        }
    }
}
