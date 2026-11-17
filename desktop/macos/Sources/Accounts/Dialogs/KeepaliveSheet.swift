import SwiftUI

public struct KeepaliveSheet: View {
    public let account: Account
    @Bindable public var store: AppStore
    public let onClose: () -> Void

    @State private var state: AccountKeepalive?
    @State private var enabled: Bool = false
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
                    Text("账号保活 · \(account.name)")
                        .font(DS.fontDialogTitle)
                        .foregroundStyle(DS.ink)
                    Text("#\(account.id) · 自动刷新或探活防掉线守护")
                        .font(DS.fontResetTime)
                        .monospacedDigit()
                        .foregroundStyle(DS.muted)
                }
                Spacer()
                LinearIconButton(systemName: "xmark", label: "关闭保活设置", disabled: saving, action: onClose)
            }

            HStack {
                Text("启用后台自动保活")
                    .font(DS.fontBodyMedium)
                    .foregroundStyle(DS.ink)
                Spacer()
                LinearSwitch(isOn: enabled, disabled: !store.state.online || loading || saving, label: "启用账号保活") {
                    enabled.toggle()
                }
            }
            .padding(12)
            .background(DS.surfaceSubtle)
            .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))

            if let state {
                VStack(alignment: .leading, spacing: 6) {
                    fact("当前状态", value: state.status)
                    if !state.reason.isEmpty {
                        fact("说明", value: state.reason)
                    }
                    if !state.paused_reason.isEmpty {
                        fact("暂停原因", value: state.paused_reason)
                    }
                    fact("累计恢复", value: "\(state.restore_count) 次（预算已用 \(state.budget_used)）")
                    if let last = state.last_restore_at, !last.isEmpty {
                        fact("最近恢复", value: BeijingTime.fullTime(last))
                    }
                    if let next = state.next_allowed_at, !next.isEmpty {
                        fact("下次允许", value: BeijingTime.fullTime(next))
                    }
                }
                .padding(12)
                .background(DS.surfaceSubtle)
                .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
            } else if loading {
                Text("正在读取保活状态…")
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.muted)
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
                Button(saving ? "保存中…" : "保存") {
                    Task { await save() }
                }
                .buttonStyle(LinearButtonStyle(variant: .primary))
                .disabled(!store.state.online || loading || saving || state == nil)
            }
        }
        .padding(22)
        .frame(width: 440)
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

    private func fact(_ key: String, value: String) -> some View {
        HStack {
            Text(key)
                .font(DS.fontCompact)
                .foregroundStyle(DS.muted)
                .frame(width: 72, alignment: .leading)
            Text(value)
                .font(DS.fontCompactMedium)
                .monospacedDigit()
                .foregroundStyle(DS.ink)
            Spacer()
        }
    }

    private func load() async {
        guard store.state.online else { return }
        loading = true
        defer { loading = false }
        do {
            let loaded: AccountKeepalive = try await RustBridge.shared.api("GET", "/accounts/\(account.id)/keepalive")
            state = loaded
            enabled = loaded.enabled
            error = ""
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func save() async {
        guard let current = state else { return }
        saving = true
        error = ""
        defer { saving = false }
        do {
            let updated: AccountKeepalive = try await RustBridge.shared.api(
                "PUT",
                "/accounts/\(account.id)/keepalive",
                body: .object([
                    "enabled": .bool(enabled),
                    "expected_version": .string(current.version),
                ])
            )
            state = updated
            store.refresh()
            store.showToast("\(account.name)：已\(updated.enabled ? "开启" : "关闭")账号保活")
            onClose()
        } catch {
            self.error = error.localizedDescription
        }
    }
}
