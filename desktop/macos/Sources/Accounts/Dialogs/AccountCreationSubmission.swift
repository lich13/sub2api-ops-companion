import AppKit
import SwiftUI
import UniformTypeIdentifiers

extension AccountCreationSheet {
    func clearAuth() {
        oauthSession = nil
        identity = nil
    }

    func readCredentialFiles(multiple: Bool) -> String? {
        let panel = NSOpenPanel()
        panel.allowedContentTypes = multiple ? [.json, .plainText] : [.json]
        panel.allowsMultipleSelection = multiple
        guard panel.runModal() == .OK else { return nil }
        var total = 0
        var texts: [String] = []
        for url in panel.urls {
            guard let data = try? Data(contentsOf: url), let text = String(data: data, encoding: .utf8) else {
                error = "凭据文件读取失败"
                return nil
            }
            total += data.count
            if total > 1_000_000 {
                error = "凭据文件过大"
                return nil
            }
            texts.append(text)
        }
        error = ""
        return texts.joined(separator: "\n")
    }

    func switchKind(platform: String, type: String?) {
        let savedName = draft.name
        let savedNotes = draft.notes
        scope &+= 1
        if let type {
            draft.applyPlatformDefaults(for: platform, authType: type)
        } else {
            draft.selectPlatform(platform)
        }
        draft.name = savedName
        draft.notes = savedNotes
        selectedSourceId = reuseCandidates.count == 1 ? reuseCandidates[0].id : nil
        sourceQuery = ""
        reuseNotice = ""
        reusing = false
        busy = false
        authMethod = "manual"
        authInput = ""
        clearAuth()
        outcomes = []
        ledger = [:]
        error = ""
        mixedRisk = false
        mixedConfirmed = false
        errorCodesConfirmed = ""
        previewModels = []
        modelsPreviewed = false
        step = 1
    }

    func loadOptions() async {
        optionError = ""
        var partial = false
        do {
            let opt: CreationOptionsReply = try await RustBridge.shared.api("GET", "/account-creation/options")
            options = opt
        } catch {
            optionError = "账号选项读取失败，请重试"
        }
        do {
            let tls: [ChoiceRecord] = try await RustBridge.shared.api("GET", "/account-creation/catalog/tls")
            tlsProfiles = tls
        } catch { partial = true }
        do {
            let gemini: GeminiCatalogResponse = try await RustBridge.shared.api("GET", "/account-creation/catalog/gemini")
            aiStudioEnabled = gemini.ai_studio_oauth_enabled
        } catch { partial = true }
        do {
            let settingsObj: [String: JSONValue] = try await RustBridge.shared.api("GET", "/account-creation/catalog/settings")
            quotaNotifyEnabled = settingsObj["account_quota_notify_enabled"]?.boolValue ?? false
        } catch { partial = true }
        do {
            let wsObj: [String: JSONValue] = try await RustBridge.shared.api("GET", "/account-creation/catalog/web-search")
            webSearchEnabled = wsObj["enabled"]?.boolValue ?? false
        } catch { partial = true }
        if partial && optionError.isEmpty {
            optionError = "部分高级选项读取失败，请重试以读取完整选项"
        }
        if selectedSourceId == nil, reuseCandidates.count == 1 {
            selectedSourceId = reuseCandidates[0].id
        }
    }

    /// Antigravity ships its default model mapping from the server; fill it while the
    /// mapping is still empty for the platform/type that requested it.
    func loadAntigravityDefaults() async {
        guard draft.platform == "antigravity", store.state.online, draft.mappings.isEmpty else { return }
        let current = scope
        do {
            let map: [String: String] = try await RustBridge.shared.api("GET", "/account-creation/catalog/antigravity-models")
            guard current == scope, draft.platform == "antigravity", draft.mappings.isEmpty else { return }
            draft.mappings = map.keys.sorted().map { KeyValuePairDraft(key: $0, value: map[$0] ?? "") }
        } catch {
            if current == scope {
                self.error = "默认模型映射读取失败，可手动填写或切换平台后重试"
            }
        }
    }

    func reuseConfiguration() async {
        guard let sourceId = selectedSourceId, store.state.online, !reusing, !busy else { return }
        let current = scope
        reusing = true
        error = ""
        reuseNotice = ""
        defer { if current == scope { reusing = false } }
        do {
            let sourceObj: [String: JSONValue] = try await RustBridge.shared.api("GET", "/account-creation/source/\(sourceId)")
            guard current == scope, selectedSourceId == sourceId else { return }
            try draft.applyReusedSource(sourceObj)
            mixedRisk = false
            mixedConfirmed = false
            errorCodesConfirmed = ""
            modelsPreviewed = false
            reuseNotice = "已复用配置，可继续修改；账号名称与新填凭据已保留"
        } catch {
            if current == scope { self.error = error.localizedDescription }
        }
    }

    func generateAuthorizationURL() async {
        guard store.state.online, !busy else { return }
        let current = scope
        busy = true
        error = ""
        clearAuth()
        defer { if current == scope { busy = false } }
        do {
            let res: TokenAuthorization = try await RustBridge.shared.api(
                "POST",
                "/account-creation/auth/\(draft.authProvider)/authorize",
                body: .object(draft.authConfigBody)
            )
            guard current == scope else { return }
            var resolvedState = res.state
            if (resolvedState ?? "").isEmpty,
               let comps = URLComponents(string: res.auth_url),
               let st = comps.queryItems?.first(where: { $0.name == "state" })?.value
            {
                resolvedState = st
            }
            oauthSession = TokenAuthorization(auth_url: res.auth_url, session_id: res.session_id, state: resolvedState)
        } catch {
            if current == scope { self.error = error.localizedDescription }
        }
    }

    func previewUpstreamModels() async {
        guard store.state.online, !busy else { return }
        let current = scope
        busy = true
        error = ""
        defer { if current == scope { busy = false } }
        do {
            let payload = try draft.buildPayload(requireCredentials: true, requireName: false)
            let creds = payload["credentials"]?.objectValue ?? [:]
            var body: [String: JSONValue] = [
                "platform": .string(draft.platform),
                "type": .string(draft.type),
            ]
            for key in ["base_url", "api_key", "model_mapping"] {
                if let value = creds[key], value != .null { body[key] = value }
            }
            let res: CreationModelsReply = try await RustBridge.shared.api(
                "POST",
                "/account-creation/models",
                body: .object(body)
            )
            guard current == scope else { return }
            modelsPreviewed = true
            previewModels = res.models
            previewSelected = Set(res.models)
            if let warnings = res.warnings, !warnings.isEmpty {
                error = warnings.joined(separator: "；")
            }
        } catch {
            if current == scope { self.error = error.localizedDescription }
        }
    }

    func syncUpstreamGroup() async {
        guard store.state.online, !busy else { return }
        let current = scope
        busy = true
        error = ""
        defer { if current == scope { busy = false } }
        do {
            let payload = try draft.buildPayload(requireCredentials: true, requireName: false)
            let creds = payload["credentials"]?.objectValue ?? [:]
            let baseUrl = (creds["base_url"]?.stringValue ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            guard !baseUrl.isEmpty else {
                throw RustBridgeError("请先填写 API 地址")
            }
            var body: [String: JSONValue] = [
                "platform": .string(draft.platform),
                "base_url": .string(baseUrl),
                "api_key": creds["api_key"] ?? .string(""),
            ]
            if let proxyId = draft.proxyId {
                body["proxy_id"] = .int(Int64(proxyId))
            }
            let res: CreationGroupPreviewReply = try await RustBridge.shared.api(
                "POST",
                "/account-creation/upstream-group",
                body: .object(body)
            )
            guard current == scope else { return }
            let nextName = res.group_name.trimmingCharacters(in: .whitespacesAndNewlines)
            let nextNotes = res.group_description.trimmingCharacters(in: .whitespacesAndNewlines)
            if nextName.isEmpty && nextNotes.isEmpty {
                error = "上游没有返回可同步的分组名称或描述"
            } else {
                if !nextName.isEmpty { draft.name = nextName }
                if !nextNotes.isEmpty { draft.notes = nextNotes }
                store.showToast("已同步上游分组信息")
            }
        } catch {
            if current == scope { self.error = error.localizedDescription }
        }
    }

    private func authorizedIdentity(item: String) async throws -> ResolvedIdentity {
        if authMethod == "manual", let identity {
            return identity
        }
        var body = draft.authConfigBody
        body.removeValue(forKey: "project_id")
        let action: String
        if authMethod == "manual" {
            guard let oauthSession else {
                throw RustBridgeError("请先生成授权链接")
            }
            var code = item.trimmingCharacters(in: .whitespacesAndNewlines)
            var state = oauthSession.state
            if let comps = URLComponents(string: code), comps.scheme != nil, comps.host != nil {
                code = comps.queryItems?.first(where: { $0.name == "code" })?.value ?? ""
                if let s = comps.queryItems?.first(where: { $0.name == "state" })?.value, !s.isEmpty {
                    state = s
                }
            }
            guard !code.isEmpty else {
                throw RustBridgeError("请填写授权码或完整回调地址")
            }
            body["session_id"] = .string(oauthSession.session_id)
            body["code"] = .string(code)
            if let state, !state.isEmpty { body["state"] = .string(state) }
            action = "exchange"
        } else if authMethod == "cookie" {
            body["code"] = .string(item)
            action = "cookie"
        } else {
            body["refresh_token"] = .string(item)
            if authMethod == "mobile-refresh" {
                body["client_id"] = .string("app_LlGpXReQgckcGGUo2JrYvtJK")
            }
            action = "refresh"
        }

        let tokenObj: [String: JSONValue] = try await RustBridge.shared.api(
            "POST",
            "/account-creation/auth/\(draft.authProvider)/\(action)",
            body: .object(body)
        )
        let resolved = AccountCreationFormDraft.extractTokenIdentity(
            platform: draft.platform,
            token: tokenObj,
            fallbackRefresh: action == "refresh" ? item : nil
        )
        guard resolved.credentials["access_token"] != nil else {
            throw RustBridgeError("授权未返回可用令牌，请重新授权")
        }
        let result = ResolvedIdentity(credentials: resolved.credentials, extra: resolved.extra)
        if authMethod == "manual" {
            identity = result
        }
        return result
    }

    private func checkMixedChannel() async throws -> Bool {
        if mixedConfirmed || !["anthropic", "antigravity"].contains(draft.platform) { return true }
        let check: CreationCheckReply = try await RustBridge.shared.api(
            "POST",
            "/account-creation/check-mixed-channel",
            body: .object([
                "platform": .string(draft.platform),
                "group_ids": .array(draft.groupIds.sorted().map { .int(Int64($0)) }),
            ])
        )
        if check.has_risk {
            mixedRisk = true
            return false
        }
        return true
    }

    func submitAction() async {
        guard store.state.online, !busy, !reusing, options != nil else { return }
        let problem = draft.validate(requireCredentials: step == 2 && !draft.isOAuthFlow, requireName: requireName)
        if !problem.isEmpty {
            error = problem
            return
        }
        if !errorRiskKey.isEmpty && errorCodesConfirmed != errorRiskKey {
            error = "请先确认自定义错误码对调度的影响"
            return
        }

        let current = scope
        busy = true
        error = ""
        defer { if current == scope { busy = false } }

        do {
            guard try await checkMixedChannel() else { return }
            guard current == scope else { return }
            if step == 1 {
                step = 2
                return
            }

            let isNativeImport = draft.isOAuthFlow && ["codex-session", "agent-identity", "codex-pat", "grok-sso"].contains(authMethod)
            let items: [String]
            if !draft.isOAuthFlow {
                items = ["direct"]
            } else if ["manual", "codex-session", "agent-identity", "codex-pat"].contains(authMethod) {
                items = [authInput.trimmingCharacters(in: .whitespacesAndNewlines)]
            } else {
                items = authInput
                    .components(separatedBy: .newlines)
                    .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
                    .filter { !$0.isEmpty }
            }
            guard !items.isEmpty, !items.contains(where: \.isEmpty) else {
                error = "请填写凭据"
                return
            }

            let baseName = draft.name.trimmingCharacters(in: .whitespacesAndNewlines)
            var nextOutcomes: [CreationOutcome] = []
            for (idx, item) in items.enumerated() {
                guard current == scope else { break }
                let key = "\(draft.isOAuthFlow ? authMethod : "direct"):\(item)"
                if let previous = ledger[key], previous.status != "failed" {
                    nextOutcomes.append(CreationOutcome(index: idx + 1, status: previous.status, message: previous.message))
                    continue
                }
                var sent = false
                let outcome: CreationOutcome
                do {
                    var itemDraft = draft
                    if items.count > 1 && !baseName.isEmpty {
                        itemDraft.name = "\(draft.name) #\(idx + 1)"
                    }
                    var detail = "已创建"
                    var partial = false
                    if isNativeImport {
                        var body = try itemDraft.importParameters()
                        sent = true
                        if authMethod == "grok-sso" {
                            body["credentials"] = body.removeValue(forKey: "credential_extras") ?? .object([:])
                            body["sso_tokens"] = .array([.string(item)])
                            let res: GrokImportReply = try await RustBridge.shared.api("POST", "/account-creation/import/grok-sso", body: .object(body))
                            if !res.failed.isEmpty {
                                sent = false
                                throw RustBridgeError("SSO 导入失败，请检查凭据后重试")
                            }
                            guard let first = res.created.first else {
                                throw RustBridgeError("SSO 导入未返回创建结果")
                            }
                            detail = "已创建 #\(first.account.id)"
                        } else if authMethod == "codex-pat" {
                            body["access_token"] = .string(item)
                            let res: CreatedAccountReply = try await RustBridge.shared.api("POST", "/account-creation/import/codex-pat", body: .object(body))
                            detail = "已创建 #\(res.account.id)"
                        } else {
                            body["content"] = .string(item)
                            body["update_existing"] = .bool(true)
                            let res: SessionImportReply = try await RustBridge.shared.api("POST", "/account-creation/import/codex-session", body: .object(body))
                            let failed = res.failed
                            detail = "创建 \(res.created)，更新 \(res.updated)，跳过 \(res.skipped)，失败 \(failed)"
                            let failedIndexes = res.errors.map(\.index)
                            if !failedIndexes.isEmpty {
                                detail += "；失败项 \(failedIndexes.map { String($0) }.joined(separator: "、"))，请仅填入失败项重试"
                            }
                            let warnings = res.warnings.map {
                                "#\($0.index) \($0.message)"
                            }
                            if !warnings.isEmpty {
                                detail += "；\(warnings.joined(separator: "；"))"
                            }
                            partial = failed > 0
                        }
                    } else {
                        let auth = draft.isOAuthFlow ? try await authorizedIdentity(item: item) : nil
                        guard current == scope else { break }
                        var payload = try itemDraft.buildPayload(
                            identityCredentials: auth?.credentials,
                            identityExtra: auth?.extra,
                            requireCredentials: true,
                            requireName: true
                        )
                        if mixedConfirmed {
                            payload["confirm_mixed_channel_risk"] = .bool(true)
                        }
                        sent = true
                        let path = modelsPreviewed ? "/account-creation?sync_models=true" : "/account-creation"
                        let created: CreatedAccountReply = try await RustBridge.shared.api("POST", path, body: .object(payload))
                        let warnings = created.warnings
                        detail = "已创建 #\(created.account.id)\(warnings.isEmpty ? "" : "；\(warnings.joined(separator: "；"))")"
                    }
                    outcome = CreationOutcome(index: idx + 1, status: partial ? "unknown" : "success", message: detail)
                } catch {
                    let text = error.localizedDescription
                    // A request that left the client without a definite 4xx answer may have created the account.
                    let unknown = sent && text.range(of: "\\[4\\d\\d\\]", options: .regularExpression) == nil
                    if text.contains("mixed_channel_warning") {
                        mixedRisk = true
                        mixedConfirmed = false
                    }
                    outcome = CreationOutcome(
                        index: idx + 1,
                        status: unknown ? "unknown" : "failed",
                        message: unknown ? "结果未知，请刷新账号列表核对。此项不会重复提交。" : text
                    )
                }
                ledger[key] = outcome
                nextOutcomes.append(outcome)
                if current == scope { outcomes = nextOutcomes }
            }

            guard current == scope else { return }
            outcomes = nextOutcomes
            if nextOutcomes.contains(where: { $0.status != "failed" }) {
                store.refresh()
            }
            if !nextOutcomes.isEmpty && nextOutcomes.allSatisfy({ $0.status == "success" }) {
                store.showToast(nextOutcomes.map(\.message).joined(separator: "；"))
                onClose()
            }
        } catch {
            if current == scope { self.error = error.localizedDescription }
        }
    }
}
