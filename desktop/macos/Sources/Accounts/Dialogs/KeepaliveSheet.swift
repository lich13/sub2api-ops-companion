import OSLog
import SwiftUI

/// Matches `desktop/src/KeepaliveDialog.tsx` (`status` labels, `restore_count/5`, 2s live polling, `重新读取`).
public struct KeepaliveSheet: View {
    private static let logger = Logger(subsystem: "Sub2Ops", category: "keepalive")
    public let account: Account
    @Bindable public var store: AppStore
    public let onClose: () -> Void

    private static let statusLabels: [String: String] = [
        "disabled": "未启用",
        "waiting": "等待检测",
        "paused": "已暂停",
        "running": "恢复中",
        "needs_confirmation": "待核对",
    ]

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
        LinearModalCard(
            title: "账号保活",
            subtitle: "\(account.name) #\(account.id)",
            width: 480,
            closeDisabled: saving,
            onClose: onClose
        ) {
            Text("检测到符合条件的临时不可调度时尝试恢复。原生恢复也会清除模型级限制；不会打开已关闭的调度或解除降智标记。")
                .font(DS.fontCompact)
                .foregroundStyle(DS.muted)

            if !store.state.online {
                Text("连接离线，恢复连接后可查看和修改。")
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.muted)
            }

            HStack {
                Text("启用账号保活")
                    .font(DS.fontBodyMedium)
                    .foregroundStyle(DS.ink)
                Spacer()
                LinearSwitch(
                    isOn: enabled,
                    disabled: !store.state.online || loading || saving || state == nil,
                    label: "启用账号保活"
                ) {
                    enabled.toggle()
                }
            }
            .padding(12)
            .background(DS.surfaceSubtle)
            .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
            .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))

            if let state {
                VStack(alignment: .leading, spacing: 7) {
                    fact("状态", value: Self.statusLabels[state.status] ?? state.status)
                    fact("最近恢复", value: state.last_restore_at.flatMap { $0.isEmpty ? nil : BeijingTime.fullTime($0) } ?? "暂无记录")
                    fact("近一小时", value: "\(state.restore_count)/5 次已确认恢复")
                    let reason = !state.reason.isEmpty ? state.reason : state.paused_reason
                    if !reason.isEmpty {
                        fact("原因", value: reason)
                    }
                    if let next = state.next_allowed_at, !next.isEmpty {
                        fact("等待至", value: BeijingTime.fullTime(next))
                    }
                }
                .padding(12)
                .background(DS.surfaceSubtle)
                .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))

                Text("两次恢复至少间隔 60 秒，一小时最多 5 次。达到上限后暂停，需检查账号并手动重新开启。")
                    .font(DS.fontResetTime)
                    .foregroundStyle(DS.muted)
            } else if loading {
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                    Text("正在读取保活设置…")
                        .font(DS.fontCompact)
                        .foregroundStyle(DS.muted)
                }
            }

            if !error.isEmpty {
                Text(error)
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.bad)
            }
        } footer: {
            Button("重新读取") {
                Task { await load(replaceDraft: true) }
            }
            .buttonStyle(LinearButtonStyle(variant: .secondary))
            .disabled(!store.state.online || loading || saving)

            Spacer()

            Button("关闭", action: onClose)
                .buttonStyle(LinearButtonStyle(variant: .secondary))
                .disabled(saving)

            Button(saving ? "保存中…" : "保存") {
                Task { await save() }
            }
            .buttonStyle(LinearButtonStyle(variant: .primary))
            .disabled(!store.state.online || loading || saving || state == nil)
        }
        .sub2opsDialogFocus(canBackdropDismiss: !saving && (state == nil || state?.enabled == enabled)) {
            if !saving { onClose() }
        }
        .task(id: "\(store.connectionKey):\(account.id):\(store.state.online)") {
            guard store.state.online else { return }
            await load(replaceDraft: true)
            var hadPollError = false
            while !Task.isCancelled && store.state.online {
                try? await Task.sleep(for: .seconds(2))
                guard !Task.isCancelled, store.state.online, !saving else { continue }
                do {
                    let polled: AccountKeepalive = try await RustBridge.shared.api("GET", "/accounts/\(account.id)/keepalive")
                    state = polled
                    if hadPollError {
                        hadPollError = false
                        error = ""
                    }
                } catch {
                    if !Task.isCancelled && (error as? RustBridgeError)?.ignored != true {
                        hadPollError = true
                        Self.logger.warning("Keepalive poll failed: \(error.localizedDescription, privacy: .public)")
                        self.error = error.localizedDescription
                    }
                }
            }
        }
    }

    private func fact(_ key: String, value: String) -> some View {
        HStack(alignment: .firstTextBaseline) {
            Text(key)
                .font(DS.fontCompact)
                .foregroundStyle(DS.muted)
                .frame(width: 76, alignment: .leading)
            Text(value)
                .font(DS.fontCompactMedium)
                .monospacedDigit()
                .foregroundStyle(DS.ink)
            Spacer()
        }
    }

    private func load(replaceDraft: Bool) async {
        guard store.state.online, !saving else { return }
        loading = true
        defer { loading = false }
        do {
            let loaded: AccountKeepalive = try await RustBridge.shared.api("GET", "/accounts/\(account.id)/keepalive")
            state = loaded
            if replaceDraft {
                enabled = loaded.enabled
            }
            error = ""
        } catch {
            self.error = error.localizedDescription
            state = nil
        }
    }

    private func save() async {
        guard let current = state, store.state.online, !saving else { return }
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
            enabled = updated.enabled
            store.refresh()
            store.showToast("\(account.name)：已\(updated.enabled ? "开启" : "关闭")账号保活")
            onClose()
        } catch {
            self.error = error.localizedDescription
            store.report(error)
        }
    }
}
