import AppKit
import Foundation
import SwiftUI

/// Matches `ReauthorizeAccountDialog.tsx` (using real `/account-creation/auth/openai/{authorize|exchange}` + `tokenIdentity`).
public struct ReauthorizeAccountSheet: View {
    public let account: Account
    @Bindable public var store: AppStore
    public let onClose: () -> Void

    private struct AuthSessionInfo: Sendable {
        var auth_url: String
        var session_id: String
        var state: String?
        var proxy_id: Int?
    }

    @State private var session: AuthSessionInfo? = nil
    @State private var rawInput: String = ""
    @State private var parsedCode: String = ""
    @State private var parsedState: String? = nil
    @State private var inputError: String = ""
    @State private var busy: Bool = false
    @State private var submitted: Bool = false
    @State private var error: String = ""
    @State private var copied: Bool = false

    public init(account: Account, store: AppStore, onClose: @escaping () -> Void) {
        self.account = account
        self.store = store
        self.onClose = onClose
    }

    public static func parseAuthorizationInput(_ raw: String) throws -> (code: String, state: String?) {
        let value = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !value.isEmpty else { return ("", nil) }
        let looksLikeQuery = value.range(of: #"(?:^|[?&])(?:code|state|error)="#, options: .regularExpression) != nil
        let looksLikeURL = value.range(of: #"^https?:"#, options: [.regularExpression, .caseInsensitive]) != nil
        if !looksLikeQuery && !looksLikeURL {
            return (value, nil)
        }
        let items: [URLQueryItem]
        if looksLikeURL {
            guard let comps = URLComponents(string: value) else {
                throw RustBridgeError("回调地址格式无效")
            }
            items = comps.queryItems ?? []
        } else {
            let stripped = value.hasPrefix("?") ? String(value.dropFirst()) : value
            guard let comps = URLComponents(string: "https://example.invalid/?\(stripped)") else {
                throw RustBridgeError("回调地址格式无效")
            }
            items = comps.queryItems ?? []
        }
        if items.contains(where: { $0.name == "error" }) {
            throw RustBridgeError("浏览器授权未完成，请重新授权")
        }
        let codes = items.filter { $0.name == "code" }.compactMap(\.value).filter { !$0.isEmpty }
        let states = items.filter { $0.name == "state" }.compactMap(\.value)
        guard codes.count == 1, states.count <= 1 else {
            throw RustBridgeError("回调地址缺少有效授权码")
        }
        return (codes[0], states.first?.isEmpty == false ? states.first : nil)
    }

    public static func buildTokenIdentity(platform: String, token: [String: JSONValue], fallbackRefresh: String? = nil) -> [String: JSONValue] {
        let credentialKeys = [
            "access_token", "refresh_token", "id_token", "token_type", "expires_at", "scope",
            "email", "email_address", "org_uuid", "account_uuid", "chatgpt_account_id", "chatgpt_user_id",
            "organization_id", "plan_type", "subscription_expires_at", "client_id", "project_id",
            "oauth_type", "tier_id", "sub", "team_id", "subscription_tier", "entitlement_status",
        ]
        var credentials: [String: JSONValue] = [:]
        for k in credentialKeys {
            if let v = token[k], v != .null, v.stringValue != "" {
                credentials[k] = v
            }
        }
        if let fallbackRefresh, !fallbackRefresh.isEmpty, credentials["refresh_token"] == nil {
            credentials["refresh_token"] = .string(fallbackRefresh)
        }
        if ["gemini", "antigravity"].contains(platform), let exp = credentials["expires_at"] {
            if let i = exp.intValue { credentials["expires_at"] = .string(String(i)) }
            else if let d = exp.doubleValue { credentials["expires_at"] = .string(String(d)) }
        }
        var extra: [String: JSONValue] = [:]
        if platform == "gemini", let nested = token["extra"]?.objectValue {
            for k in ["drive_storage_limit", "drive_storage_usage", "drive_tier_updated_at"] {
                if let v = nested[k] { extra[k] = v }
            }
        }
        for k in ["email", "email_address", "org_uuid", "account_uuid", "subscription_tier", "entitlement_status", "name", "privacy_mode"] {
            if let v = token[k], v != .null, v.stringValue != "" {
                extra[k] = v
            }
        }
        return [
            "credentials": .object(credentials),
            "extra": .object(extra),
        ]
    }

    public var body: some View {
        LinearModalCard(
            title: "重新授权",
            subtitle: "\(account.name) #\((account.id))",
            width: 520,
            closeDisabled: busy,
            onClose: onClose
        ) {
            HStack {
                Text("生成新的 OpenAI OAuth 授权链接而不改变账号 ID")
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.muted)
                Spacer()
                Button {
                    Task { await generateLink() }
                } label: {
                    HStack(spacing: 5) {
                        Image(systemName: "link")
                            .font(DS.iconFont(12, weight: .medium))
                        Text(session == nil ? "生成授权链接" : "重新生成")
                    }
                }
                .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                .disabled(!store.state.online || busy)
            }

            if let session {
                VStack(alignment: .leading, spacing: 8) {
                    Text("授权链接")
                        .font(DS.fontCompactMedium)
                        .foregroundStyle(DS.ink)

                    HStack(spacing: 8) {
                        Text(verbatim: session.auth_url)
                            .font(DS.fontMono11)
                            .foregroundStyle(DS.muted)
                            .lineLimit(1)
                            .truncationMode(.middle)
                            .padding(.horizontal, 10)
                            .frame(maxWidth: .infinity, minHeight: DS.controlHeightCompact, alignment: .leading)
                            .background(DS.surface)
                            .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(DS.line, lineWidth: 1))
                            .clipShape(RoundedRectangle(cornerRadius: 6))

                        Button(copied ? "已复制" : "复制链接") {
                            NSPasteboard.general.clearContents()
                            NSPasteboard.general.setString(session.auth_url, forType: .string)
                            copied = true
                        }
                        .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                        .disabled(busy)

                        Button("浏览器授权") {
                            Task {
                                try? await RustBridge.shared.commandVoid(
                                    "open_authorization_url",
                                    args: .object(["url": .string(session.auth_url)])
                                )
                            }
                        }
                        .buttonStyle(LinearButtonStyle(variant: .primary, compact: true))
                        .disabled(!store.state.online || busy)
                    }
                }
                .padding(12)
                .background(DS.surfaceSubtle)
                .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))

                VStack(alignment: .leading, spacing: 6) {
                    Text("授权码或回调地址")
                        .font(DS.fontCompactMedium)
                        .foregroundStyle(DS.ink)
                    TextEditor(text: $rawInput)
                        .font(DS.fontMono12)
                        .scrollContentBackground(.hidden)
                        .padding(6)
                        .frame(height: 68)
                        .background(DS.surface)
                        .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                        .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
                        .disabled(busy || submitted)
                        .onChange(of: rawInput) { _, next in
                            error = ""
                            do {
                                let parsed = try Self.parseAuthorizationInput(next)
                                parsedCode = parsed.code
                                parsedState = parsed.state
                                inputError = ""
                            } catch {
                                parsedCode = ""
                                parsedState = nil
                                inputError = error.localizedDescription
                            }
                        }
                }
            }

            if !error.isEmpty || !inputError.isEmpty {
                Text(error.isEmpty ? inputError : error)
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.bad)
            }
        } footer: {
            Spacer()
            Button("取消", action: onClose)
                .buttonStyle(LinearButtonStyle(variant: .secondary))
                .disabled(busy)
            Button(busy && submitted ? "正在验证…" : "完成授权") {
                Task { await submitExchange() }
            }
            .buttonStyle(LinearButtonStyle(variant: .primary))
            .disabled(!store.state.online || busy || submitted || session == nil || parsedCode.isEmpty || !inputError.isEmpty)
        }
        .sub2opsDialogFocus {
            if !busy { onClose() }
        }
    }

    private func generateLink() async {
        guard store.state.online, !busy else { return }
        busy = true
        error = ""
        inputError = ""
        rawInput = ""
        parsedCode = ""
        parsedState = nil
        submitted = false
        copied = false
        session = nil
        defer { busy = false }
        do {
            let settings: [String: JSONValue] = try await RustBridge.shared.api("GET", "/accounts/\(account.id)/settings")
            let pid = settings["proxy_id"]?.intValue.map(Int.init)
            var authBody: [String: JSONValue] = [:]
            if let pid, pid > 0 { authBody["proxy_id"] = .int(Int64(pid)) }
            let raw: [String: JSONValue] = try await RustBridge.shared.api(
                "POST",
                "/account-creation/auth/openai/authorize",
                body: .object(authBody)
            )
            let urlStr = raw["auth_url"]?.stringValue ?? ""
            let sid = raw["session_id"]?.stringValue ?? ""
            let st = raw["state"]?.stringValue ?? URLComponents(string: urlStr)?.queryItems?.first(where: { $0.name == "state" })?.value
            session = AuthSessionInfo(auth_url: urlStr, session_id: sid, state: st, proxy_id: pid)
        } catch {
            self.error = "无法生成授权链接，请重新尝试"
        }
    }

    private func submitExchange() async {
        guard let curSession = session, !parsedCode.isEmpty, inputError.isEmpty, store.state.online, !busy else { return }
        if let cbState = parsedState, let expectedState = curSession.state, cbState != expectedState {
            error = "回调地址不属于本次授权，请重新粘贴"
            return
        }
        busy = true
        submitted = true
        error = ""
        var applying = false
        defer {
            session = nil
            busy = false
        }
        do {
            var exchangeBody: [String: JSONValue] = [
                "session_id": .string(curSession.session_id),
                "code": .string(parsedCode),
            ]
            if let st = parsedState ?? curSession.state, !st.isEmpty {
                exchangeBody["state"] = .string(st)
            }
            if let pid = curSession.proxy_id, pid > 0 {
                exchangeBody["proxy_id"] = .int(Int64(pid))
            }
            let token: [String: JSONValue] = try await RustBridge.shared.api(
                "POST",
                "/account-creation/auth/openai/exchange",
                body: .object(exchangeBody)
            )
            let identity = Self.buildTokenIdentity(platform: "openai", token: token)
            guard identity["credentials"]?.objectValue?["access_token"]?.stringValue?.isEmpty == false else {
                throw RustBridgeError("missing_token")
            }
            applying = true
            let applied: [String: JSONValue] = try await RustBridge.shared.api(
                "POST",
                "/accounts/\(account.id)/apply-oauth-credentials",
                body: .object(identity)
            )
            guard applied["verified"]?.boolValue == true else {
                throw RustBridgeError("授权保存结果未确认，请刷新账号核对；未重放请求")
            }
            let status = applied["status"]?.stringValue ?? "active"
            store.refresh()
            store.showToast(status == "active" ? "\(account.name)：重新授权完成" : "\(account.name)：授权已保存，账号状态 \(status)")
            onClose()
        } catch {
            self.error = applying
                ? error.localizedDescription
                : "授权码交换失败，请重新生成链接后授权"
        }
    }
}
