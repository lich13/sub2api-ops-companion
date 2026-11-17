import SwiftUI

public struct KeyQuotaSettingsSheet: View {
    @Bindable public var store: AppStore
    public let targetAccount: Account?
    public let onClose: () -> Void

    @State private var autoRefresh: Bool = true
    @State private var secondsText: String = "60"
    @State private var globalRevision: String = ""
    @State private var accountRevision: String = ""
    @State private var hasToken: Bool = false
    @State private var tokenInput: String = ""
    @State private var revealedToken: String = ""
    @State private var loading: Bool = false
    @State private var saving: Bool = false
    @State private var error: String = ""

    public init(store: AppStore, targetAccount: Account?, onClose: @escaping () -> Void) {
        self.store = store
        self.targetAccount = targetAccount
        self.onClose = onClose
    }

    public var body: some View {
        LinearModalCard(
            title: targetAccount == nil ? "余额刷新设置" : "余额查询设置",
            subtitle: targetAccount.map { "\($0.name) #\($0.id)" },
            width: 460,
            closeDisabled: saving,
            onClose: onClose
        ) {
            if let account = targetAccount {
                VStack(alignment: .leading, spacing: 8) {
                    HStack {
                        Text("账户访问令牌（可选）")
                            .font(DS.fontCompactSemibold)
                            .foregroundStyle(DS.ink)
                        Spacer()
                        Text(hasToken ? "已保存令牌" : "未配置")
                            .font(DS.fontResetTime)
                            .foregroundStyle(hasToken ? DS.good : DS.muted)
                    }

                    SecureField(text: $tokenInput, prompt: Text(verbatim: hasToken ? "输入新令牌覆盖，留空保持不变" : "粘贴上游账户 Access Token")) { EmptyView() }
                        .textFieldStyle(.plain)
                        .font(DS.fontBody)
                        .padding(.horizontal, 10)
                        .frame(height: DS.controlHeightField)
                        .background(DS.surface)
                        .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                        .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))

                    if hasToken {
                        HStack(spacing: 8) {
                            Button(revealedToken.isEmpty ? "查看已保存令牌" : "隐藏令牌") {
                                if revealedToken.isEmpty {
                                    Task { await revealAccountToken(account.id) }
                                } else {
                                    revealedToken = ""
                                }
                            }
                            .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                            .disabled(!store.state.online || saving)

                            Button("清除令牌") {
                                Task { await saveAccountToken(accountId: account.id, token: "") }
                            }
                            .buttonStyle(LinearButtonStyle(variant: .danger, compact: true))
                            .disabled(!store.state.online || saving)
                        }
                    }

                    if !revealedToken.isEmpty {
                        DiagnosticTextView(text: revealedToken, monospaced: true)
                    }
                }
            }

            if targetAccount == nil {
                VStack(alignment: .leading, spacing: 8) {
                    LinearCheckbox(isChecked: autoRefresh, label: "余额自动刷新", disabled: saving || !store.state.online) {
                        autoRefresh.toggle()
                    }
                    if autoRefresh {
                        Text("间隔 / 秒")
                            .font(DS.fontCompactSemibold)
                            .foregroundStyle(DS.ink)
                        TextField(text: $secondsText, prompt: Text(verbatim: "10–86400")) { EmptyView() }
                            .textFieldStyle(.plain)
                            .font(DS.fontBody)
                            .monospacedDigit()
                            .padding(.horizontal, 10)
                            .frame(height: DS.controlHeightField)
                            .background(DS.surface)
                            .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                            .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
                    }
                    Text("适用于所有启用原生余额读取的按量 API Key 账号")
                        .font(DS.fontResetTime)
                        .foregroundStyle(DS.muted)
                }
            }

            if !error.isEmpty {
                Text(error)
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.bad)
            }
        } footer: {
            Spacer()
            Button("取消", action: onClose)
                .buttonStyle(LinearButtonStyle(variant: .secondary))
                .disabled(saving)
            Button(saving ? "保存中…" : "保存设置") {
                Task { await submit() }
            }
            .buttonStyle(LinearButtonStyle(variant: .primary))
            .disabled(!store.state.online || loading || saving || loadedRevision.isEmpty)
        }
        .sub2opsDialogFocus(onEscape: onClose)
        .task {
            await loadSettings()
        }
    }

    private func loadSettings() async {
        guard store.state.online else { return }
        loading = true
        defer { loading = false }
        do {
            if let acc = targetAccount {
                let accSettings: AccountKeyQuotaSettings = try await RustBridge.shared.api("GET", "/accounts/\(acc.id)/key-quota-settings")
                hasToken = accSettings.has_access_token
                accountRevision = accSettings.revision
            } else {
                let global: KeyQuotaGlobalConfig = try await RustBridge.shared.api("GET", "/config/key-quota")
                autoRefresh = global.seconds != 0
                secondsText = String(global.seconds == 0 ? 60 : global.seconds)
                globalRevision = global.revision
            }
            error = ""
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func revealAccountToken(_ accountId: Int) async {
        do {
            let res: AccountKeyQuotaTokenResponse = try await RustBridge.shared.api("GET", "/accounts/\(accountId)/key-quota-settings/access-token")
            revealedToken = res.access_token
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func saveAccountToken(accountId: Int, token: String) async {
        saving = true
        defer { saving = false }
        do {
            let updated: AccountKeyQuotaSettings = try await RustBridge.shared.api(
                "PUT",
                "/accounts/\(accountId)/key-quota-settings",
                body: .object([
                    "access_token": .string(token),
                    "expected_revision": .string(accountRevision),
                ])
            )
            hasToken = updated.has_access_token
            accountRevision = updated.revision
            tokenInput = ""
            revealedToken = ""
            store.showToast("令牌已更新")
        } catch {
            self.error = error.localizedDescription
        }
    }

    private var loadedRevision: String {
        targetAccount == nil ? globalRevision : accountRevision
    }

    private func submit() async {
        // Server contract: 10–86400 seconds, or 0 to turn automatic refresh off.
        let secs = autoRefresh ? Int(secondsText.trimmingCharacters(in: .whitespacesAndNewlines)) : 0
        if targetAccount == nil {
            guard let secs, !autoRefresh || (10...86400).contains(secs) else {
                error = "余额刷新间隔需为 10–86400 秒"
                return
            }
        }
        saving = true
        error = ""
        defer { saving = false }
        do {
            if let acc = targetAccount {
                let trimmedToken = tokenInput.trimmingCharacters(in: .whitespacesAndNewlines)
                var body: [String: JSONValue] = ["expected_revision": .string(accountRevision)]
                if !trimmedToken.isEmpty {
                    body["access_token"] = .string(trimmedToken)
                }
                let updatedAcc: AccountKeyQuotaSettings = try await RustBridge.shared.api(
                    "PUT",
                    "/accounts/\(acc.id)/key-quota-settings",
                    body: .object(body)
                )
                hasToken = updatedAcc.has_access_token
                accountRevision = updatedAcc.revision
            } else {
                let updatedGlobal: KeyQuotaGlobalConfig = try await RustBridge.shared.api(
                    "PUT",
                    "/config/key-quota",
                    body: .object([
                        "seconds": .int(Int64(secs ?? 0)),
                        "expected_revision": .string(globalRevision),
                    ])
                )
                globalRevision = updatedGlobal.revision
            }
            store.refresh()
            store.showToast(targetAccount == nil ? "余额刷新设置已保存" : "余额查询设置已保存")
            onClose()
        } catch {
            self.error = error.localizedDescription
        }
    }
}
