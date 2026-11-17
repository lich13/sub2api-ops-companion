import AppKit
import SwiftUI

public struct ReauthorizeAccountSheet: View {
    public let account: Account
    @Bindable public var store: AppStore
    public let onClose: () -> Void

    @State private var session: OAuthAuthorizationSession?
    @State private var callbackInput: String = ""
    @State private var busy: Bool = false
    @State private var error: String = ""
    @State private var copied: Bool = false

    public init(account: Account, store: AppStore, onClose: @escaping () -> Void) {
        self.account = account
        self.store = store
        self.onClose = onClose
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    Text("重新授权 · \(account.name)")
                        .font(DS.fontDialogTitle)
                        .foregroundStyle(DS.ink)
                    Text("#\(account.id) · 刷新 OpenAI OAuth 令牌而不改变账号 ID")
                        .font(DS.fontResetTime)
                        .monospacedDigit()
                        .foregroundStyle(DS.muted)
                }
                Spacer()
                LinearIconButton(systemName: "xmark", label: "关闭重新授权", disabled: busy, action: onClose)
            }

            VStack(alignment: .leading, spacing: 8) {
                HStack {
                    Text("步骤 1：生成并打开授权链接")
                        .font(DS.fontCompactSemibold)
                        .foregroundStyle(DS.ink)
                    Spacer()
                    Button(session == nil ? "生成授权链接" : "重新生成") {
                        Task { await requestAuthURL() }
                    }
                    .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                    .disabled(!store.state.online || busy)
                }

                if let session {
                    HStack(spacing: 8) {
                        Text(verbatim: session.auth_url)
                            .font(DS.fontMono11)
                            .foregroundStyle(DS.muted)
                            .lineLimit(1)
                        Button(copied ? "已复制" : "复制") {
                            NSPasteboard.general.clearContents()
                            NSPasteboard.general.setString(session.auth_url, forType: .string)
                            copied = true
                        }
                        .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))

                        Button("浏览器打开") {
                            Task {
                                try? await RustBridge.shared.commandVoid(
                                    "open_authorization_url",
                                    args: .object(["url": .string(session.auth_url)])
                                )
                            }
                        }
                        .buttonStyle(LinearButtonStyle(variant: .primary, compact: true))
                    }
                }
            }
            .padding(12)
            .background(DS.surfaceSubtle)
            .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
            .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))

            VStack(alignment: .leading, spacing: 6) {
                Text("步骤 2：粘贴浏览器回调地址或授权码")
                    .font(DS.fontCompactSemibold)
                    .foregroundStyle(DS.ink)
                TextField(text: $callbackInput, prompt: Text(verbatim: "http://localhost:1455/auth/callback?code=...")) { EmptyView() }
                    .textFieldStyle(.plain)
                    .font(DS.fontMono12)
                    .padding(.horizontal, 10)
                    .frame(height: DS.controlHeightField)
                    .background(DS.surface)
                    .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                    .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
                    .disabled(busy || session == nil)
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
                    .disabled(busy)
                Button(busy ? "正在验证…" : "交换并应用凭据") {
                    Task { await exchangeAndApply() }
                }
                .buttonStyle(LinearButtonStyle(variant: .primary))
                .disabled(!store.state.online || busy || session == nil || callbackInput.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
        .padding(22)
        .frame(width: 520)
        .background(DS.surface)
        .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(DS.line, lineWidth: 1))
        .clipShape(RoundedRectangle(cornerRadius: 12))
        .sub2opsDialogFocus {
            if !busy { onClose() }
        }
        .task {
            await requestAuthURL()
        }
    }

    private func requestAuthURL() async {
        guard store.state.online else { return }
        busy = true
        error = ""
        copied = false
        defer { busy = false }
        do {
            let created: OAuthAuthorizationSession = try await RustBridge.shared.api(
                "POST",
                "/account-creation/oauth/authorize",
                body: .object([
                    "platform": .string(account.platform),
                    "account_id": .int(Int64(account.id)),
                ])
            )
            session = created
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func exchangeAndApply() async {
        guard let session else { return }
        let trimmed = callbackInput.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        busy = true
        error = ""
        defer { busy = false }
        do {
            let exchanged: [String: JSONValue] = try await RustBridge.shared.api(
                "POST",
                "/account-creation/oauth/exchange",
                body: .object([
                    "platform": .string(account.platform),
                    "session_id": .string(session.session_id),
                    "callback_url": .string(trimmed),
                    "code": .string(trimmed),
                ])
            )
            let creds = exchanged["credentials"]?.objectValue ?? exchanged
            let _: JSONValue = try await RustBridge.shared.api(
                "POST",
                "/accounts/\(account.id)/apply-oauth-credentials",
                body: .object([
                    "credentials": .object(creds),
                    "expected_version": .string(account.version),
                ])
            )
            store.refresh()
            store.showToast("\(account.name)：已完成重新授权")
            onClose()
        } catch {
            self.error = error.localizedDescription
        }
    }
}
