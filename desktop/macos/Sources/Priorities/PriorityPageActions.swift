import SwiftUI

extension PrioritiesPageView {
    @discardableResult
    func moveItem(platform: String, id: Int, targetIndex: Int) -> Bool {
        guard let grp = platforms.first(where: { $0.platform == platform }) else { return false }
        let existing = drafts[platform]
        let editing = isEditingPlatform(grp, draft: existing)
        guard let nextDraft = PriorityDraftLogic.applyMove(
            existing: existing,
            group: grp,
            id: id,
            targetIndex: targetIndex,
            editing: editing
        ) else {
            return false
        }
        history[platform, default: []].append(existing)
        drafts[platform] = nextDraft
        notices[platform] = ""
        return true
    }

    func undoDraft(platform: String) {
        guard var stack = history[platform], !stack.isEmpty else { return }
        let prev = stack.removeLast()
        history[platform] = stack
        if let prev {
            drafts[platform] = prev
        } else {
            drafts.removeValue(forKey: platform)
        }
    }

    private func showTransientResult(platform: String, message: String) {
        results[platform] = message
        Task { @MainActor in
            try? await Task.sleep(for: .seconds(3))
            guard !Task.isCancelled else { return }
            if results[platform] == message {
                results.removeValue(forKey: platform)
            }
        }
    }

    func readPriorities(repreparePlatform: String? = nil) async {
        guard store.state.online else { return }
        let capturedKey = store.connectionKey
        loading = true
        error = ""
        defer { loading = false }
        do {
            let loaded: PriorityViewEnvelope = try await RustBridge.shared.api("GET", "/account-priorities?schema=2")
            guard store.connectionKey == capturedKey else { return }
            view = loaded
            if let rep = repreparePlatform {
                drafts.removeValue(forKey: rep)
                history[rep] = []
                notices[rep] = "已重新读取实际优先级，请核对后应用"
            }
        } catch {
            guard store.connectionKey == capturedKey else { return }
            self.error = error.localizedDescription
        }
    }

    func updateParallelism(group: PriorityPlatformGroup, next: Int) async {
        let clamped = max(1, min(8, next))
        settingParallelism = group.platform
        error = ""
        defer { settingParallelism = "" }
        do {
            let updated: PriorityViewEnvelope = try await RustBridge.shared.api(
                "PUT",
                "/account-priorities?schema=2",
                body: .object([
                    "platform": .string(group.platform),
                    "parallelism": .int(Int64(clamped)),
                    "expected_version": .string(group.version),
                ])
            )
            view = updated
        } catch {
            self.error = error.localizedDescription
        }
    }

    func applyOrder(group: PriorityPlatformGroup, ids: [Int], targets: [Int], parallelism: Int, expectedVersion: String) async {
        submittingPlatform = group.platform
        error = ""
        defer { submittingPlatform = "" }
        var body: [String: JSONValue] = [
            "platform": .string(group.platform),
            "ordered_account_ids": .array(ids.map { .int(Int64($0)) }),
            "parallelism": .int(Int64(parallelism)),
            "expected_version": .string(expectedVersion),
            "request_id": .string(UUID().uuidString.lowercased()),
            "client_id": .string(store.operations.clientId()),
        ]
        if parallelism > 1 {
            body["target_priorities"] = .array(targets.map { .int(Int64($0)) })
        }
        do {
            let task: PriorityTask = try await RustBridge.shared.api("POST", "/account-priority-orders?schema=2", body: .object(body))
            drafts.removeValue(forKey: group.platform)
            history[group.platform] = []
            if task.isPending {
                view = view?.withActiveTask(task)
            } else {
                await readPriorities()
                let label = Self.taskLabels[task.status] ?? "已完成"
                showTransientResult(platform: group.platform, message: "\(label) · \(task.verified_count)/\(task.total) 已确认")
                store.refresh()
            }
        } catch {
            self.error = error.localizedDescription
        }
    }

    func pollActiveTask(id: String) async {
        let capturedKey = store.connectionKey
        var failures = 0
        var step = 0
        let delays = OperationStore.adaptivePollDelaysMs
        while !Task.isCancelled
            && store.state.online
            && store.connectionKey == capturedKey
            && isPollingVisible
        {
            let delayMs: Int
            if failures > 0 {
                delayMs = min(4, 1 << min(failures, 2)) * 1000
            } else {
                delayMs = delays[min(step, delays.count - 1)]
                step += 1
            }
            try? await Task.sleep(for: .milliseconds(delayMs))
            guard !Task.isCancelled,
                  store.state.online,
                  store.connectionKey == capturedKey,
                  isPollingVisible
            else { return }
            do {
                let next: PriorityTask = try await RustBridge.shared.api("GET", "/account-priority-orders/\(id)?schema=2&details=0")
                guard !Task.isCancelled, store.connectionKey == capturedKey else { return }
                failures = 0
                self.error = ""
                if !next.isPending {
                    await readPriorities()
                    let label = Self.taskLabels[next.status] ?? "已完成"
                    showTransientResult(platform: next.platform, message: "\(label) · \(next.verified_count)/\(next.total) 已确认")
                    store.refresh()
                    return
                }
                view = view?.withActiveTask(next)
            } catch is CancellationError {
                return
            } catch {
                guard !Task.isCancelled, store.connectionKey == capturedKey else { return }
                failures += 1
                self.error = error.localizedDescription
            }
        }
    }

    func cancelTask(_ task: PriorityTask) async {
        do {
            let updated: PriorityTask = try await RustBridge.shared.api("POST", "/account-priority-orders/\(task.id)/cancel?schema=2", body: .object([:]))
            view = view?.withActiveTask(updated.isPending ? updated : nil)
            if !updated.isPending {
                await readPriorities()
            }
        } catch {
            self.error = error.localizedDescription
        }
    }
}
