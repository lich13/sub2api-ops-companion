import Foundation
import SwiftUI

/// Matches `SettingsPage(page: "settings")` in `desktop/src/main.tsx`.
public struct SettingsPageView: View {
    @Bindable public var store: AppStore

    @State private var removingUrl: String? = nil
    @State private var barkDraft: [String: JSONValue] = [:]
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
        (Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String) ?? "0.1.94"
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            ConnectionStatusCardView(connectionKey: store.connectionKey)

            instanceManagementCard

            clientPreferencesCard

            barkConfigCard

            connectionManagementCard
        }
        .frame(maxWidth: 820)
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
        CardSection {
            VStack(alignment: .leading, spacing: 12) {
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

                Divider().overlay(DS.line)

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
                                VStack(alignment: .leading, spacing: 2) {
                                    HStack(spacing: 6) {
                                        Text(item.name)
                                            .font(DS.fontBodySemibold)
                                            .foregroundStyle(DS.ink)
                                        Text(isCurrent ? "当前连接" : "已保存")
                                            .font(DS.fontResetTimeSemibold)
                                            .foregroundStyle(isCurrent ? DS.accent : DS.muted)
                                            .padding(.horizontal, 6)
                                            .padding(.vertical, 2)
                                            .background(isCurrent ? DS.accentBg : DS.surfaceSubtle)
                                            .clipShape(RoundedRectangle(cornerRadius: 4))
                                    }
                                    Text(verbatim: item.base_url)
                                        .font(DS.fontMono11)
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

                                    Button("删除实例") {
                                        Task { await removeInstance(item) }
                                    }
                                    .buttonStyle(LinearButtonStyle(variant: .danger, compact: true))
                                    .disabled(isCurrent || store.switchingUrl != nil || isRemoving)
                                    .help(isCurrent ? "当前实例正在使用中，请先切换到其他实例或断开连接后再删除" : "删除此实例并清除对应的本机 Key")
                                }
                            }
                            .padding(10)
                            .background(isCurrent ? DS.surfaceSubtle : DS.surface)
                            .overlay(
                                RoundedRectangle(cornerRadius: DS.cornerRadius)
                                    .strokeBorder(isCurrent ? DS.accent : DS.line, lineWidth: 1)
                            )
                            .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
                        }
                    }
                }
            }
        }
    }

    private var clientPreferencesCard: some View {
        CardSection {
            VStack(alignment: .leading, spacing: 12) {
                Text("客户端")
                    .font(DS.fontSectionTitle)
                    .foregroundStyle(DS.ink)

                Divider().overlay(DS.line)

                HStack {
                    Text("开机启动")
                        .font(DS.fontBody)
                        .foregroundStyle(DS.ink)
                    Spacer()
                    LinearSwitch(
                        isOn: store.state.preferences.launch_at_login,
                        label: "开机启动"
                    ) {
                        Task {
                            await store.updatePreferences(launchAtLogin: !store.state.preferences.launch_at_login)
                        }
                    }
                }

                HStack {
                    Text("当前版本")
                        .font(DS.fontBody)
                        .foregroundStyle(DS.ink)
                    Spacer()
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
        CardSection {
            VStack(alignment: .leading, spacing: 12) {
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

                Divider().overlay(DS.line)

                VStack(alignment: .leading, spacing: 10) {
                    let isEnabled = barkDraft["enabled"]?.boolValue ?? false
                    HStack {
                        Text("启用推送")
                            .font(DS.fontBody)
                            .foregroundStyle(DS.ink)
                        Spacer()
                        LinearSwitch(isOn: isEnabled, label: "启用推送") {
                            setBarkField("enabled", .bool(!isEnabled))
                        }
                    }

                    let keySet = serverBark["device_key_set"]?.boolValue == true
                    HStack {
                        Text("Device Key\(keySet ? "（已保存，留空保留）" : "")")
                            .font(DS.fontBody)
                            .foregroundStyle(DS.ink)
                        Spacer()
                        SecureField(
                            text: Binding(
                                get: { barkDraft["device_key"]?.stringValue ?? "" },
                                set: { setBarkField("device_key", .string($0)) }
                            ),
                            prompt: Text(verbatim: keySet ? "留空保留现有 Device Key" : "输入 Bark Device Key")
                        ) { EmptyView() }
                        .textFieldStyle(.plain)
                        .font(DS.fontMono12)
                        .padding(.horizontal, 10)
                        .frame(width: 260, height: DS.controlHeightDense)
                        .background(DS.surface)
                        .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                        .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
                    }

                    HStack {
                        Button("发送测试消息") {
                            Task { await sendBarkTest() }
                        }
                        .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                        .disabled(!store.state.online || barkBusy)
                        Spacer()
                    }
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
                    .background(DS.warnBg)
                    .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
                }
            }
        }
    }

    private var connectionManagementCard: some View {
        CardSection {
            VStack(alignment: .leading, spacing: 14) {
                ConnectionOnboardingView(store: store, embeddedInSettings: true)

                Divider().overlay(DS.line)

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
            if error.localizedDescription.contains("409") {
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
