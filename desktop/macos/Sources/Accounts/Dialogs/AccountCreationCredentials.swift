import AppKit
import SwiftUI
import UniformTypeIdentifiers

extension AccountCreationSheet {
    @ViewBuilder
    var stepTwoBody: some View {
        let pLabel = currentPlatformSpec?.label ?? draft.platform
        let trimmedName = draft.name.trimmingCharacters(in: .whitespacesAndNewlines)
        Text("\(trimmedName.isEmpty ? "未命名账号" : trimmedName) · \(pLabel) · \(typeDisplayName(draft.type))")
            .font(DS.fontCompactSemibold)
            .foregroundStyle(DS.ink)
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(DS.surfaceSubtle)
            .clipShape(RoundedRectangle(cornerRadius: 6))

        if draft.isOAuthFlow {
            let methods = draft.authMethods
            let currentMethodLabel = methods.first(where: { $0.id == authMethod })?.label ?? "授权码"
            VStack(alignment: .leading, spacing: 10) {
                VStack(alignment: .leading, spacing: 5) {
                    Text("添加方式").font(DS.fontCompactMedium).foregroundStyle(DS.muted)
                    LinearSelect(
                        label: currentMethodLabel,
                        accessibilityTitle: "添加方式",
                        height: DS.controlHeight,
                        expandWidth: true,
                        disabled: busy,
                        entries: methods.map { m in
                            LinearMenuEntry(id: m.id, title: m.label, checked: authMethod == m.id) {
                                authMethod = m.id
                                authInput = ""
                                clearAuth()
                                error = ""
                                outcomes = []
                            }
                        }
                    )
                }

                if authMethod == "manual" {
                    VStack(alignment: .leading, spacing: 8) {
                        Button(oauthSession == nil ? "生成授权链接" : "重新生成授权链接") {
                            Task { await generateAuthorizationURL() }
                        }
                        .buttonStyle(LinearButtonStyle(variant: .secondary))
                        .disabled(busy || !store.state.online)

                        if let session = oauthSession {
                            Text(verbatim: session.auth_url)
                                .font(DS.fontMono11)
                                .foregroundStyle(DS.muted)
                                .lineLimit(2)
                                .textSelection(.enabled)
                                .padding(8)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .background(DS.surface)
                                .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(DS.line, lineWidth: 1))

                            HStack(spacing: 8) {
                                Button {
                                    Task {
                                        do {
                                            try await RustBridge.shared.commandVoid(
                                                "open_authorization_url",
                                                args: .object(["url": .string(session.auth_url)])
                                            )
                                        } catch {
                                            self.error = error.localizedDescription
                                        }
                                    }
                                } label: {
                                    Label("系统浏览器授权", systemImage: "safari")
                                }
                                .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))

                                Button {
                                    NSPasteboard.general.clearContents()
                                    NSPasteboard.general.setString(session.auth_url, forType: .string)
                                    store.showToast("已复制授权链接")
                                } label: {
                                    Label("复制链接", systemImage: "doc.on.doc")
                                }
                                .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                            }
                        }
                    }
                    .padding(12)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(DS.surfaceSubtle)
                    .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                    .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
                }

                VStack(alignment: .leading, spacing: 5) {
                    Text(authMethod == "manual" ? "授权码或回调地址" : currentMethodLabel)
                        .font(DS.fontCompactMedium)
                        .foregroundStyle(DS.muted)
                    TextEditor(text: $authInput)
                        .font(DS.fontMono12)
                        .foregroundStyle(DS.ink)
                        .scrollContentBackground(.hidden)
                        .frame(height: 92)
                        .padding(6)
                        .background(DS.surface)
                        .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                        .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
                }

                Text(
                    ["refresh", "mobile-refresh", "cookie", "grok-sso"].contains(authMethod)
                        ? "支持批量，每行一个凭据。"
                        : (["codex-session", "agent-identity"].contains(authMethod)
                            ? "粘贴原生导出的 JSON；支持批量导入，匹配的已有账号将更新。"
                            : "授权完成后粘贴凭据并提交。")
                )
                .font(DS.fontResetTime)
                .foregroundStyle(DS.muted)

                if ["codex-session", "agent-identity"].contains(authMethod) {
                    Button {
                        if let text = readCredentialFiles(multiple: true) { authInput = text }
                    } label: {
                        Label("导入凭据文件", systemImage: "doc.badge.plus")
                    }
                    .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                }
            }
            .onChange(of: authInput) { _, _ in identity = nil }
        } else if draft.type == "service_account" && !draft.isVertexApiKey {
            VStack(alignment: .leading, spacing: 6) {
                HStack {
                    Text("Service Account JSON")
                        .font(DS.fontCompactMedium)
                        .foregroundStyle(DS.muted)
                    Spacer()
                    Button {
                        if let text = readCredentialFiles(multiple: false) { draft.serviceAccountJSON = text }
                    } label: {
                        Label("选择 JSON 文件", systemImage: "doc.badge.plus")
                    }
                    .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                }
                TextEditor(text: $draft.serviceAccountJSON)
                    .font(DS.fontMono11)
                    .foregroundStyle(DS.ink)
                    .scrollContentBackground(.hidden)
                    .frame(height: 120)
                    .padding(6)
                    .background(DS.surface)
                    .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                    .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
            }
        } else if draft.type == "bedrock" && (draft.values["credentials.auth_mode"]?.stringValue ?? "sigv4") == "sigv4" {
            VStack(alignment: .leading, spacing: 10) {
                labeledSecureField("AWS Access Key ID", text: $draft.awsAccessKeyId)
                labeledSecureField("AWS Secret Access Key", text: $draft.awsSecretAccessKey)
                labeledSecureField("AWS Session Token（可选）", text: $draft.awsSessionToken)
            }
        } else {
            VStack(alignment: .leading, spacing: 10) {
                labeledSecureField("API Key", text: $draft.apiKey)

                if draft.type == "apikey" {
                    HStack(spacing: 8) {
                        Button("读取上游模型") {
                            Task { await previewUpstreamModels() }
                        }
                        .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                        .disabled(busy || draft.apiKey.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)

                        Button("同步上游分组名称与备注") {
                            Task { await syncUpstreamGroup() }
                        }
                        .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                        .disabled(busy || draft.apiKey.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)

                        Spacer()
                    }

                    if !previewModels.isEmpty {
                        VStack(alignment: .leading, spacing: 8) {
                            HStack {
                                Text("上游模型候选（\(previewModels.count)）")
                                    .font(DS.fontCompactSemibold)
                                    .foregroundStyle(DS.ink)
                                Spacer()
                                Button("添加所选模型") {
                                    for m in previewModels where previewSelected.contains(m) {
                                        if !draft.mappings.contains(where: { $0.key == m }) {
                                            draft.mappings.append(KeyValuePairDraft(key: m, value: m))
                                        }
                                    }
                                    previewModels = []
                                }
                                .buttonStyle(LinearButtonStyle(variant: .primary, compact: true))
                            }
                            LinearBoundedScrollView(maxHeight: 120) {
                                VStack(alignment: .leading, spacing: 4) {
                                    ForEach(previewModels, id: \.self) { m in
                                        LinearCheckbox(isChecked: previewSelected.contains(m), label: m) {
                                            if previewSelected.contains(m) {
                                                previewSelected.remove(m)
                                            } else {
                                                previewSelected.insert(m)
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        .padding(10)
                        .background(DS.surfaceSubtle)
                        .clipShape(RoundedRectangle(cornerRadius: 6))
                    }
                }
            }
        }
    }

    func labeledTextField(_ title: String, text: Binding<String>, prompt: String, monospaced: Bool = false) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            Text(title).font(DS.fontCompactMedium).foregroundStyle(DS.muted)
            TextField(text: text, prompt: Text(verbatim: prompt).foregroundStyle(DS.placeholder)) { EmptyView() }
                .textFieldStyle(.plain)
                .font(monospaced ? DS.fontMono12 : DS.fontBody)
                .monospacedDigit()
                .foregroundStyle(DS.ink)
                .disabled(busy)
                .padding(.horizontal, 10)
                .frame(height: DS.controlHeight)
                .background(DS.surface)
                .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
        }
    }

    private func labeledSecureField(_ title: String, text: Binding<String>) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            Text(title).font(DS.fontCompactMedium).foregroundStyle(DS.muted)
            SecureField(text: text, prompt: Text(verbatim: "粘贴密钥").foregroundStyle(DS.placeholder)) { EmptyView() }
                .textFieldStyle(.plain)
                .font(DS.fontBody)
                .foregroundStyle(DS.ink)
                .disabled(busy)
                .padding(.horizontal, 10)
                .frame(height: DS.controlHeight)
                .background(DS.surface)
                .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
        }
    }
}
