import SwiftUI

/// Matches `ResetCreditsDialog` in `desktop/src/AccountManagement.tsx` (0.1.92 batch reset-card execution with confirmation).
public struct ResetCreditsSheet: View {
    public let accounts: [Account]
    @Bindable public var store: AppStore
    public let onClose: () -> Void

    private enum Phase: Sendable {
        case confirm
        case running
        case done
    }

    private struct RowOutcome: Sendable {
        var status: String = "waiting" // waiting | running | done | failed | uncertain
        var message: String = ""
    }

    private let targets: [Account]
    private let skippedCount: Int
    @State private var phase: Phase = .confirm
    @State private var outcomes: [RowOutcome]
    @State private var activeTask: Task<Void, Never>? = nil

    public init(accounts: [Account], store: AppStore, onClose: @escaping () -> Void) {
        self.accounts = accounts
        self.store = store
        self.onClose = onClose
        let eligible = accounts.filter(BatchToolbarView.isResetCreditEligible)
        self.targets = eligible
        self.skippedCount = accounts.count - eligible.count
        self._outcomes = State(initialValue: Array(repeating: RowOutcome(), count: eligible.count))
    }

    private var modalTitle: String {
        switch phase {
        case .confirm: return "对 \(targets.count) 个账号使用重置卡？"
        case .running: return "正在使用重置卡"
        case .done: return "用卡结果"
        }
    }

    private func weeklyPercent(_ account: Account) -> Double? {
        let wins = account.usage?.windows ?? account.usage_windows
        return wins.first(where: { $0.key == "codex_7d" || $0.key == "7d" || $0.label == "7d" })?.used_percent
    }

    public var body: some View {
        LinearModalCard(
            title: modalTitle,
            width: 500,
            onClose: handleDismiss
        ) {
            if phase == .confirm {
                Text("每个账号消耗一次重置次数并执行 Sub2API 的额度重置，无法撤销。\(skippedCount > 0 ? "已跳过 \(skippedCount) 个没有可用重置卡的账号。" : "")")
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.muted)
            } else if phase == .running {
                Text("逐个执行中；关闭后不再继续后面的账号。")
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.muted)
            }

            if targets.isEmpty {
                Text("所选账号中没有可用重置卡的账号。")
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.muted)
                    .padding(.vertical, 8)
            } else {
                LinearBoundedScrollView(maxHeight: 280) {
                    VStack(spacing: 0) {
                        ForEach(Array(targets.enumerated()), id: \.element.id) { index, account in
                            let outcome = outcomes[index]
                            let pct = weeklyPercent(account)
                            let avail = account.usage?.reset_credits?.available ?? 0

                            VStack(alignment: .leading, spacing: 4) {
                                HStack(spacing: 12) {
                                    Text(account.name)
                                        .font(DS.fontCompactSemibold)
                                        .foregroundStyle(DS.ink)
                                        .lineLimit(1)
                                    Spacer(minLength: 8)
                                    Text("#\(account.id) · 7d \(pct.map { "\(Int($0.rounded()))%" } ?? "未知") · 可用 \(avail) 张")
                                        .font(DS.fontPriorityMeta)
                                        .monospacedDigit()
                                        .foregroundStyle(DS.muted)
                                }

                                if phase != .confirm {
                                    HStack(spacing: 5) {
                                        if outcome.status == "running" {
                                            ProgressView().controlSize(.mini)
                                            Text("重置中")
                                        } else {
                                            Text(outcome.message.isEmpty ? (phase == .done ? "未执行" : "等待中") : outcome.message)
                                        }
                                    }
                                    .font(DS.fontPriorityMeta)
                                    .foregroundStyle(outcomeColor(outcome.status))
                                }
                            }
                            .padding(.vertical, 8)

                            if index < targets.count - 1 {
                                Rectangle().fill(DS.line).frame(height: 1)
                            }
                        }
                    }
                }
            }

            if phase == .done {
                let doneCount = outcomes.filter { $0.status == "done" }.count
                let failedCount = outcomes.filter { $0.status == "failed" }.count
                let uncertainCount = outcomes.filter { $0.status == "uncertain" }.count
                Text("\(doneCount) 个已重置，\(failedCount) 个失败，\(uncertainCount) 个待核对")
                    .font(DS.fontCompactMedium)
                    .monospacedDigit()
                    .foregroundStyle(DS.ink)
            }
        } footer: {
            Spacer()
            Button(phase == .confirm ? "取消" : "关闭", action: handleDismiss)
                .buttonStyle(LinearButtonStyle(variant: .secondary))
            if phase != .done {
                Button(phase == .running ? "使用中…" : "确认使用") {
                    startSequentialReset()
                }
                .buttonStyle(LinearButtonStyle(variant: .primary))
                .disabled(!store.state.online || phase == .running || targets.isEmpty)
            }
        }
        .sub2opsDialogFocus(
            canBackdropDismiss: phase != .running,
            onBackdrop: { if phase != .running { handleDismiss() } },
            onEscape: handleDismiss
        )
        .onDisappear {
            activeTask?.cancel()
            activeTask = nil
        }
    }

    private func outcomeColor(_ status: String) -> Color {
        switch status {
        case "done": return DS.good
        case "failed", "uncertain": return DS.bad
        default: return DS.muted
        }
    }

    private func handleDismiss() {
        activeTask?.cancel()
        activeTask = nil
        onClose()
    }

    private func startSequentialReset() {
        guard store.state.online, phase == .confirm, !targets.isEmpty else { return }
        phase = .running
        let capturedEpoch = store.operations.epoch
        activeTask = Task { @MainActor in
            var used = 0
            var failed = 0
            var uncertain = 0
            var attempted = 0

            for (idx, account) in targets.enumerated() {
                if Task.isCancelled || !store.state.online || store.operations.epoch != capturedEpoch {
                    break
                }
                attempted += 1
                outcomes[idx] = RowOutcome(status: "running", message: "")
                var latestJobStatus = ""
                do {
                    let initial = try await store.operations.submitOperation(
                        account: account,
                        action: "reset_quota",
                        payload: ["action": .string("reset_quota"), "confirmed": .bool(true)]
                    )
                    latestJobStatus = initial.status
                    let resultObj = try await store.operations.followOperation(
                        initial: initial,
                        onUpdate: { updated in
                            latestJobStatus = updated.status
                        }
                    )
                    latestJobStatus = "completed"
                    used += 1
                    let msg = resultObj?.message ?? "重置已执行"
                    outcomes[idx] = RowOutcome(status: "done", message: msg)
                } catch {
                    let msg = error.localizedDescription.replacingOccurrences(of: "^Error: ", with: "", options: .regularExpression)
                    if !latestJobStatus.isEmpty && !["failed", "cancelled", "superseded"].contains(latestJobStatus) {
                        uncertain += 1
                        outcomes[idx] = RowOutcome(status: "uncertain", message: "待核对：\(msg)")
                    } else {
                        failed += 1
                        outcomes[idx] = RowOutcome(status: "failed", message: msg)
                    }
                }
            }

            guard store.operations.epoch == capturedEpoch else { return }
            store.refresh()
            let pending = targets.count - attempted
            store.showToast("已使用 \(used) 张重置卡 · 失败 \(failed) 个 · 待核对 \(uncertain) 个 · 跳过 \(skippedCount) 个\(pending > 0 ? " · 未执行 \(pending) 个" : "")")
            if !Task.isCancelled {
                phase = .done
            }
        }
    }
}
