import SwiftUI

public struct KeyQuotaSettingsSheet: View {
    @Bindable public var store: AppStore
    public let targetAccount: Account?
    public let onClose: () -> Void

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
        VStack(alignment: .leading, spacing: 16) {
            HStack {
                Text(targetAccount == nil ? "余额刷新设置" : "\(targetAccount!.name) · 余额令牌设置")
                    .font(DS.fontDialogTitle)
                    .foregroundStyle(DS.ink)
                Spacer()
                LinearIconButton(systemName: "xmark", label: "关闭余额设置", action: onClose)
            }

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

            VStack(alignment: .leading, spacing: 6) {
                Text("后台余额轮询间隔（秒）")
                    .font(DS.fontCompactSemibold)
                    .foregroundStyle(DS.ink)
                TextField(text: $secondsText, prompt: Text(verbatim: "15–86400")) { EmptyView() }
                    .textFieldStyle(.plain)
                    .font(DS.fontBody)
                    .monospacedDigit()
                    .padding(.horizontal, 10)
                    .frame(height: DS.controlHeightField)
                    .background(DS.surface)
                    .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                    .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
                Text("适用于所有启用原生余额读取的按量 API Key 账号")
                    .font(DS.fontResetTime)
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
                Button(saving ? "保存中…" : "保存设置") {
                    Task { await submit() }
                }
                .buttonStyle(LinearButtonStyle(variant: .primary))
                .disabled(!store.state.online || loading || saving)
            }
        }
        .padding(22)
        .frame(width: 440)
        .background(DS.surface)
        .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(DS.line, lineWidth: 1))
        .clipShape(RoundedRectangle(cornerRadius: 12))
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
            let global: KeyQuotaGlobalConfig = try await RustBridge.shared.api("GET", "/config/key-quota")
            secondsText = String(global.seconds)
            globalRevision = global.revision
            if let acc = targetAccount {
                let accSettings: AccountKeyQuotaSettings = try await RustBridge.shared.api("GET", "/accounts/\(acc.id)/key-quota-settings")
                hasToken = accSettings.has_access_token
                accountRevision = accSettings.revision
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

    private func submit() async {
        guard let secs = Int(secondsText.trimmingCharacters(in: .whitespacesAndNewlines)), secs >= 15 else {
            error = "请输入不少于 15 秒的整数间隔"
            return
        }
        saving = true
        defer { saving = false }
        do {
            let updatedGlobal: KeyQuotaGlobalConfig = try await RustBridge.shared.api(
                "PUT",
                "/config/key-quota",
                body: .object([
                    "seconds": .int(Int64(secs)),
                    "expected_revision": .string(globalRevision),
                ])
            )
            globalRevision = updatedGlobal.revision

            let trimmedToken = tokenInput.trimmingCharacters(in: .whitespacesAndNewlines)
            if let acc = targetAccount, !trimmedToken.isEmpty {
                let updatedAcc: AccountKeyQuotaSettings = try await RustBridge.shared.api(
                    "PUT",
                    "/accounts/\(acc.id)/key-quota-settings",
                    body: .object([
                        "access_token": .string(trimmedToken),
                        "expected_revision": .string(accountRevision),
                    ])
                )
                hasToken = updatedAcc.has_access_token
                accountRevision = updatedAcc.revision
            }
            store.refresh()
            store.showToast("余额刷新设置已保存")
            onClose()
        } catch {
            self.error = error.localizedDescription
        }
    }
}
