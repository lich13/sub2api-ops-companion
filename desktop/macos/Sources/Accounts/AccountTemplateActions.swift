import SwiftUI

extension AccountTemplatesSheet {
    func reloadAll() async {
        await loadConfig()
        await readAccountPreviews()
    }

    private func loadConfig() async {
        guard store.state.online else { return }
        busy = true
        error = ""
        defer { busy = false }
        do {
            let loaded: TemplatesHeader = try await RustBridge.shared.api("GET", "/account-templates")
            saved = loaded
            resetDrafts(from: loaded)
        } catch {
            self.error = error.localizedDescription
        }
    }

    func readAccountPreviews() async {
        guard store.state.online else { return }
        var nextConfigs: [Int: AccountTemplateTargetPreview.AccountEntry] = [:]
        var nextErrors: [Int: String] = [:]
        for id in targetIds {
            do {
                let res: AccountTemplateTargetPreview = try await RustBridge.shared.api("GET", "/account-templates?account_id=\(id)")
                if let entry = res.account {
                    nextConfigs[id] = entry
                } else {
                    nextErrors[id] = "账号配置读取失败"
                }
            } catch {
                nextErrors[id] = error.localizedDescription
            }
        }
        accountConfigs = nextConfigs
        accountErrors = nextErrors
    }

    func mutateTemplate(method: String, id: String?) async {
        guard let currentSaved = saved, !busy, store.state.online else { return }
        let draftValue = id.flatMap { drafts[$0] } ?? creating
        if method != "DELETE" {
            let trimmedName = draftValue?.name.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
            if trimmedName.isEmpty || trimmedName.count > 40 {
                error = "模板名称需为 1–40 个字符"
                return
            }
            if let prof = draftValue?.profile {
                let invalid = ModelProfileReply.validate(prof)
                if !invalid.isEmpty {
                    error = invalid
                    return
                }
            }
        }
        busy = true
        error = ""
        defer { busy = false }
        var payload: [String: JSONValue] = [
            "expected_version": .string(currentSaved.version),
        ]
        if method != "DELETE", let draftValue {
            payload["name"] = .string(draftValue.name.trimmingCharacters(in: .whitespacesAndNewlines))
            payload["whitelist"] = .array(draftValue.profile.whitelist.map(JSONValue.string))
            payload["mappings"] = .array(draftValue.profile.mappings.map {
                .object(["source": .string($0.source), "target": .string($0.target)])
            })
        }
        do {
            let path = id.map { "/account-templates/\($0)" } ?? "/account-templates"
            let updated: TemplatesHeader = try await RustBridge.shared.api(method, path, body: .object(payload))
            let previousIds = Set(currentSaved.orderList)
            saved = updated
            resetDrafts(from: updated)
            if method == "POST" {
                if let createdId = updated.orderList.first(where: { !previousIds.contains($0) }) {
                    selectedTemplateId = createdId
                }
                creating = nil
            }
            if method == "DELETE" {
                deleteId = nil
            }
            store.showToast(method == "DELETE" ? "模板已删除" : "模板已保存")
        } catch {
            self.error = error.localizedDescription
        }
    }

    func applySelectedTemplate(saved: TemplatesHeader) async {
        guard !busy, store.state.online, readyToApply, !selectedTemplateId.isEmpty else { return }
        busy = true
        error = ""
        batchError = ""
        defer { busy = false }

        let tplVer = saved.template_versions?[selectedTemplateId] ?? saved.version
        let accountsPayload: [JSONValue] = targetIds.compactMap { id in
            guard let cfg = accountConfigs[id] else { return nil }
            return .object([
                "account_id": .int(Int64(id)),
                "expected_version": .string(cfg.version),
            ])
        }
        let clientClean = store.operations.clientId().replacingOccurrences(of: "-", with: "")
        let sig = "\(selectedTemplateId):\(tplVer):\(targetKey)"
        if pendingSignature != sig || pendingRequestId.isEmpty {
            pendingSignature = sig
            pendingRequestId = UUID().uuidString.replacingOccurrences(of: "-", with: "").lowercased()
        }

        let body: JSONValue = .object([
            "template_id": .string(selectedTemplateId),
            "template_version": .string(tplVer),
            "accounts": .array(accountsPayload),
            "client_id": .string(clientClean),
            "request_id": .string(pendingRequestId),
        ])

        do {
            let initialBatch: OperationBatch = try await RustBridge.shared.api("POST", "/account-templates/apply", body: body)
            batch = initialBatch
            let finalBatch = try await store.operations.followBatch(initial: initialBatch) { updated in
                batch = updated
            }
            store.refresh()
            if !finalBatch.items.isEmpty && finalBatch.items.allSatisfy({ $0.status == "completed" }) {
                store.showToast("模板已应用")
                onClose()
            }
        } catch {
            self.error = error.localizedDescription
        }
    }
}
