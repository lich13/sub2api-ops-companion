import AppKit
import SwiftUI

private struct ReusableEnvelope: Decodable, Sendable {
    var items: [ReusableAccountCandidate]?
    var accounts: [ReusableAccountCandidate]?
}

public struct AccountCreationSheet: View {
    @Bindable public var store: AppStore
    public let onClose: () -> Void

    public enum Mode: String, CaseIterable, Sendable {
        case create
        case reuse
    }

    @State private var mode: Mode = .create
    @State private var draft = AccountCreationFormDraft()
    @State private var oauthSession: OAuthAuthorizationSession?
    @State private var oauthCallback: String = ""
    @State private var reusableQuery: String = ""
    @State private var reusableItems: [ReusableAccountCandidate] = []
    @State private var loadingReusable: Bool = false
    @State private var busy: Bool = false
    @State private var error: String = ""

    public init(store: AppStore, onClose: @escaping () -> Void) {
        self.store = store
        self.onClose = onClose
    }

    private var catalog: AccountCreationCatalog {
        AccountCreationCatalog.shared
    }

    private var currentPlatformSpec: CreationPlatformSpec? {
        catalog.platforms.first(where: { $0.id == draft.platform })
    }

    private var platformGroups: [Group] {
        store.groups.filter { $0.platform == draft.platform }
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack {
                Text("添加账号")
                    .font(DS.fontDialogTitle)
                    .foregroundStyle(DS.ink)
                HStack(spacing: 4) {
                    Button("新建账号") { mode = .create }
                        .buttonStyle(LinearButtonStyle(variant: mode == .create ? .selectedGhost : .ghost, compact: true))
                    Button("复用已有账号") { mode = .reuse }
                        .buttonStyle(LinearButtonStyle(variant: mode == .reuse ? .selectedGhost : .ghost, compact: true))
                }
                Spacer()
                LinearIconButton(systemName: "xmark", label: "关闭添加账号", disabled: busy, action: onClose)
            }

            if mode == .create {
                createFormBody
            } else {
                reuseBody
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
                if mode == .create {
                    Button(busy ? "正在创建…" : "创建账号") {
                        Task { await submitCreate() }
                    }
                    .buttonStyle(LinearButtonStyle(variant: .primary))
                    .disabled(!store.state.online || busy)
                }
            }
        }
        .padding(22)
        .frame(width: 640)
        .background(DS.surface)
        .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(DS.line, lineWidth: 1))
        .clipShape(RoundedRectangle(cornerRadius: 12))
        .sub2opsDialogFocus {
            if !busy { onClose() }
        }
        .task(id: mode) {
            if mode == .reuse {
                await loadReusable()
            }
        }
    }

    private var createFormBody: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                HStack(spacing: 10) {
                    VStack(alignment: .leading, spacing: 4) {
                        Text("账号名称")
                            .font(DS.fontCompactSemibold)
                            .foregroundStyle(DS.ink)
                        TextField(text: $draft.name, prompt: Text(verbatim: "例如：Codex 主力账号")) { EmptyView() }
                            .textFieldStyle(.plain)
                            .font(DS.fontBody)
                            .padding(.horizontal, 8)
                            .frame(height: DS.controlHeightCompact)
                            .background(DS.surface)
                            .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(DS.line, lineWidth: 1))
                    }

                    VStack(alignment: .leading, spacing: 4) {
                        Text("平台")
                            .font(DS.fontCompactSemibold)
                            .foregroundStyle(DS.ink)
                        Menu {
                            ForEach(catalog.platforms) { p in
                                Button(p.label) {
                                    oauthSession = nil
                                    oauthCallback = ""
                                    draft.selectPlatform(p.id)
                                }
                            }
                        } label: {
                            let pLabel = catalog.platforms.first(where: { $0.id == draft.platform })?.label ?? draft.platform
                            HStack {
                                Text(pLabel).font(DS.fontCompact).foregroundStyle(DS.ink)
                                Spacer()
                            }
                            .padding(.horizontal, 8)
                            .frame(width: 150, height: DS.controlHeightDense)
                            .background(DS.surface)
                            .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(DS.line, lineWidth: 1))
                        }
                        .menuStyle(.borderlessButton)
                        .menuIndicator(.hidden)
                    }

                    VStack(alignment: .leading, spacing: 4) {
                        Text("认证方式")
                            .font(DS.fontCompactSemibold)
                            .foregroundStyle(DS.ink)
                        Menu {
                            ForEach(currentPlatformSpec?.types ?? ["apikey"], id: \.self) { t in
                                Button(PlatformMeta.typeLabel(t)) {
                                    oauthSession = nil
                                    oauthCallback = ""
                                    draft.selectType(t)
                                }
                            }
                        } label: {
                            HStack {
                                Text(PlatformMeta.typeLabel(draft.type)).font(DS.fontCompact).foregroundStyle(DS.ink)
                                Spacer()
                            }
                            .padding(.horizontal, 8)
                            .frame(width: 140, height: DS.controlHeightDense)
                            .background(DS.surface)
                            .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(DS.line, lineWidth: 1))
                        }
                        .menuStyle(.borderlessButton)
                        .menuIndicator(.hidden)
                    }
                }

                authSection

                HStack(spacing: 10) {
                    smallNumberField("并发", text: $draft.concurrency)
                    smallNumberField("优先级", text: $draft.priority)
                    smallNumberField("计费倍率", text: $draft.rateMultiplier)
                    HStack(spacing: 6) {
                        Text("创建后允许调度")
                            .font(DS.fontCompact)
                            .foregroundStyle(DS.ink)
                        LinearSwitch(isOn: draft.schedulable, label: "允许调度") {
                            draft.schedulable.toggle()
                        }
                    }
                    .padding(.top, 18)
                }

                if !platformGroups.isEmpty {
                    VStack(alignment: .leading, spacing: 6) {
                        Text("加入分组")
                            .font(DS.fontCompactSemibold)
                            .foregroundStyle(DS.ink)
                        HStack(spacing: 8) {
                            ForEach(platformGroups) { g in
                                let selected = draft.groupIds.contains(g.id)
                                Button("\(g.name) #\(g.id)") {
                                    if selected { draft.groupIds.remove(g.id) } else { draft.groupIds.insert(g.id) }
                                }
                                .buttonStyle(LinearButtonStyle(variant: selected ? .selectedGhost : .secondary, compact: true))
                            }
                        }
                    }
                }

                AccountCreationFieldsView(draft: $draft)
            }
            .padding(.trailing, 4)
        }
        .frame(maxHeight: 420)
    }

    @ViewBuilder
    private var authSection: some View {
        switch draft.type {
        case "oauth":
            VStack(alignment: .leading, spacing: 8) {
                HStack {
                    Text(draft.oauthCredentials == nil ? "OAuth 浏览器授权" : "已完成 OAuth 令牌交换")
                        .font(DS.fontCompactSemibold)
                        .foregroundStyle(draft.oauthCredentials == nil ? DS.ink : DS.good)
                    Spacer()
                    Button(oauthSession == nil ? "生成授权链接" : "重新生成") {
                        Task { await startOAuth() }
                    }
                    .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                    .disabled(!store.state.online || busy)
                }
                if let session = oauthSession {
                    HStack(spacing: 8) {
                        Text(verbatim: session.auth_url)
                            .font(DS.fontMono11)
                            .foregroundStyle(DS.muted)
                            .lineLimit(1)
                        Button("打开授权页") {
                            Task {
                                try? await RustBridge.shared.commandVoid(
                                    "open_authorization_url",
                                    args: .object(["url": .string(session.auth_url)])
                                )
                            }
                        }
                        .buttonStyle(LinearButtonStyle(variant: .primary, compact: true))
                    }
                    HStack(spacing: 8) {
                        TextField(text: $oauthCallback, prompt: Text(verbatim: "粘贴授权回调 URL 或 code")) { EmptyView() }
                            .textFieldStyle(.plain)
                            .font(DS.fontMono11)
                            .padding(.horizontal, 8)
                            .frame(height: DS.controlHeightCompact)
                            .background(DS.surface)
                            .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(DS.line, lineWidth: 1))
                        Button("交换令牌") {
                            Task { await exchangeOAuth(session) }
                        }
                        .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                        .disabled(busy || oauthCallback.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    }
                }
            }
            .padding(12)
            .background(DS.surfaceSubtle)
            .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
            .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))

        case "service_account":
            VStack(alignment: .leading, spacing: 4) {
                Text("Service Account JSON")
                    .font(DS.fontCompactSemibold)
                    .foregroundStyle(DS.ink)
                TextEditor(text: $draft.serviceAccountJSON)
                    .font(DS.fontMono11)
                    .frame(height: 76)
                    .padding(4)
                    .background(DS.surface)
                    .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(DS.line, lineWidth: 1))
            }

        case "bedrock":
            HStack(spacing: 8) {
                TextField(text: $draft.awsAccessKeyId, prompt: Text(verbatim: "AWS Access Key ID")) { EmptyView() }
                    .textFieldStyle(.plain)
                    .font(DS.fontMono11)
                    .padding(.horizontal, 8)
                    .frame(height: DS.controlHeightCompact)
                    .background(DS.surface)
                    .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(DS.line, lineWidth: 1))
                SecureField(text: $draft.awsSecretAccessKey, prompt: Text(verbatim: "AWS Secret Access Key")) { EmptyView() }
                    .textFieldStyle(.plain)
                    .font(DS.fontMono11)
                    .padding(.horizontal, 8)
                    .frame(height: DS.controlHeightCompact)
                    .background(DS.surface)
                    .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(DS.line, lineWidth: 1))
            }

        default:
            VStack(alignment: .leading, spacing: 4) {
                Text(draft.type == "setup-token" ? "Setup Token" : "API Key")
                    .font(DS.fontCompactSemibold)
                    .foregroundStyle(DS.ink)
                SecureField(text: $draft.apiKey, prompt: Text(verbatim: "粘贴上游密钥")) { EmptyView() }
                    .textFieldStyle(.plain)
                    .font(DS.fontMono12)
                    .padding(.horizontal, 8)
                    .frame(height: DS.controlHeightCompact)
                    .background(DS.surface)
                    .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(DS.line, lineWidth: 1))
            }
        }
    }

    private func smallNumberField(_ label: String, text: Binding<String>) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(label)
                .font(DS.fontCompactSemibold)
                .foregroundStyle(DS.ink)
            TextField(text: text, prompt: Text(verbatim: "1")) { EmptyView() }
                .textFieldStyle(.plain)
                .font(DS.fontMono12)
                .monospacedDigit()
                .padding(.horizontal, 8)
                .frame(width: 90, height: DS.controlHeightCompact)
                .background(DS.surface)
                .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(DS.line, lineWidth: 1))
        }
    }

    private var reuseBody: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                Image(systemName: "magnifyingglass")
                    .font(DS.iconFont(12))
                    .foregroundStyle(DS.muted)
                TextField(text: $reusableQuery, prompt: Text(verbatim: "搜索可复用的上游账号")) { EmptyView() }
                    .textFieldStyle(.plain)
                    .font(DS.fontCompact)
                Button("刷新") {
                    Task { await loadReusable() }
                }
                .buttonStyle(LinearButtonStyle(variant: .ghost, compact: true))
                .disabled(!store.state.online || loadingReusable)
            }
            .padding(.horizontal, 10)
            .frame(height: DS.controlHeightCompact)
            .background(DS.surface)
            .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
            .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))

            ScrollView {
                let filtered = reusableItems.filter { item in
                    let q = reusableQuery.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
                    return q.isEmpty || "\(item.name) \(item.id) \(item.platform)".lowercased().contains(q)
                }
                if filtered.isEmpty {
                    Text(loadingReusable ? "正在读取可复用账号…" : "暂无可复用的历史或未绑定账号")
                        .font(DS.fontCompact)
                        .foregroundStyle(DS.muted)
                        .padding(.vertical, 22)
                } else {
                    VStack(spacing: 6) {
                        ForEach(filtered) { item in
                            HStack {
                                VStack(alignment: .leading, spacing: 2) {
                                    Text("\(item.name) #\(item.id)")
                                        .font(DS.fontCompactSemibold)
                                        .foregroundStyle(DS.ink)
                                    Text("\(PlatformMeta.displayName(item.platform)) · \(PlatformMeta.typeLabel(item.type))")
                                        .font(DS.fontResetTime)
                                        .foregroundStyle(DS.muted)
                                }
                                Spacer()
                                Button("复用") {
                                    Task { await reuseAccount(item) }
                                }
                                .buttonStyle(LinearButtonStyle(variant: .primary, compact: true))
                                .disabled(!store.state.online || busy)
                            }
                            .padding(.horizontal, 10)
                            .padding(.vertical, 8)
                            .background(DS.surfaceSubtle)
                            .clipShape(RoundedRectangle(cornerRadius: 6))
                        }
                    }
                }
            }
            .frame(height: 300)
        }
    }

    private func startOAuth() async {
        busy = true
        error = ""
        defer { busy = false }
        do {
            let s: OAuthAuthorizationSession = try await RustBridge.shared.api(
                "POST",
                "/account-creation/oauth/authorize",
                body: .object(["platform": .string(draft.platform)])
            )
            oauthSession = s
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func exchangeOAuth(_ session: OAuthAuthorizationSession) async {
        let cb = oauthCallback.trimmingCharacters(in: .whitespacesAndNewlines)
        busy = true
        error = ""
        defer { busy = false }
        do {
            let res: [String: JSONValue] = try await RustBridge.shared.api(
                "POST",
                "/account-creation/oauth/exchange",
                body: .object([
                    "platform": .string(draft.platform),
                    "session_id": .string(session.session_id),
                    "callback_url": .string(cb),
                    "code": .string(cb),
                ])
            )
            draft.oauthCredentials = res["credentials"]?.objectValue ?? res
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func submitCreate() async {
        busy = true
        error = ""
        defer { busy = false }
        do {
            let body = try draft.buildPayload()
            let _: JSONValue = try await RustBridge.shared.api("POST", "/account-creation/accounts", body: .object(body))
            store.refresh()
            store.showToast("已创建账号 \(draft.name)")
            onClose()
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func loadReusable() async {
        guard store.state.online else { return }
        loadingReusable = true
        defer { loadingReusable = false }
        do {
            let raw: JSONValue = try await RustBridge.shared.api("GET", "/account-creation/reusable-accounts")
            let data = try JSONEncoder().encode(raw)
            if let direct = try? JSONDecoder().decode([ReusableAccountCandidate].self, from: data) {
                reusableItems = direct
            } else if let env = try? JSONDecoder().decode(ReusableEnvelope.self, from: data) {
                reusableItems = env.items ?? env.accounts ?? []
            } else {
                reusableItems = []
            }
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func reuseAccount(_ item: ReusableAccountCandidate) async {
        busy = true
        error = ""
        defer { busy = false }
        do {
            let _: JSONValue = try await RustBridge.shared.api(
                "POST",
                "/account-creation/reuse",
                body: .object(["account_id": .int(Int64(item.id))])
            )
            store.refresh()
            store.showToast("已复用账号 \(item.name)")
            onClose()
        } catch {
            self.error = error.localizedDescription
        }
    }
}
