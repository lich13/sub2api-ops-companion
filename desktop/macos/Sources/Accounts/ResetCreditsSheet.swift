import SwiftUI

public struct ResetCreditsSheet: View {
    public let accounts: [Account]
    @Bindable public var store: AppStore
    public let onClose: () -> Void

    @State private var batch: OperationBatch?
    @State private var busy: Bool = false
    @State private var error: String = ""

    public init(accounts: [Account], store: AppStore, onClose: @escaping () -> Void) {
        self.accounts = accounts
        self.store = store
        self.onClose = onClose
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack {
                Text("批量查询用卡次数（\(accounts.count)）")
                    .font(DS.fontDialogTitle)
                    .monospacedDigit()
                    .foregroundStyle(DS.ink)
                Spacer()
                LinearIconButton(systemName: "xmark", label: "关闭用卡结果", disabled: busy, action: onClose)
            }

            if !error.isEmpty {
                Text(error)
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.bad)
            }

            ScrollView {
                VStack(alignment: .leading, spacing: 6) {
                    if let items = batch?.items, !items.isEmpty {
                        ForEach(items) { item in
                            HStack(spacing: 8) {
                                StatusDot(item.status == "completed" ? .good : (OperationStore.isTerminal(item.status) ? .bad : .warning))
                                Text(item.account_name)
                                    .font(DS.fontCompactMedium)
                                    .foregroundStyle(DS.ink)
                                    .lineLimit(1)
                                Text("#\(item.account_id)")
                                    .font(DS.fontMono11)
                                    .foregroundStyle(DS.muted)
                                Spacer()
                                Text(statusSummary(item))
                                    .font(DS.fontCompact)
                                    .monospacedDigit()
                                    .foregroundStyle(item.status == "completed" ? DS.good : DS.muted)
                            }
                            .padding(.horizontal, 10)
                            .padding(.vertical, 7)
                            .background(DS.surfaceSubtle)
                            .clipShape(RoundedRectangle(cornerRadius: 6))
                        }
                    } else {
                        Text(busy ? "正在提交批量查询…" : "等待开始")
                            .font(DS.fontCompact)
                            .foregroundStyle(DS.muted)
                    }
                }
            }
            .frame(maxHeight: 300)

            HStack {
                Spacer()
                Button(busy ? "正在查询…" : "关闭", action: onClose)
                    .buttonStyle(LinearButtonStyle(variant: .secondary))
                    .disabled(busy)
            }
        }
        .padding(22)
        .frame(width: 480)
        .background(DS.surface)
        .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(DS.line, lineWidth: 1))
        .clipShape(RoundedRectangle(cornerRadius: 12))
        .sub2opsDialogFocus {
            if !busy { onClose() }
        }
        .task {
            await runBatch()
        }
    }

    private func statusSummary(_ item: Operation) -> String {
        if item.status == "completed" {
            if let avail = item.result?["available"]?.intValue {
                return "剩余 \(avail) 次"
            }
            return "已更新"
        }
        return item.reason ?? item.status
    }

    private func runBatch() async {
        guard store.state.online, !accounts.isEmpty else { return }
        busy = true
        defer { busy = false }
        let targets: [[String: JSONValue]] = accounts.compactMap { acc in
            guard let ver = acc.operation_versions["usage"] ?? Optional(acc.version), !ver.isEmpty else { return nil }
            return [
                "account_id": .int(Int64(acc.id)),
                "expected_version": .string(ver),
                "payload": .object(["include_reset_credits": .bool(true)]),
            ]
        }
        do {
            let initial = try await store.operations.submitBatch(
                action: "usage",
                accounts: targets,
                requestId: UUID().uuidString.lowercased(),
                customPayload: ["include_reset_credits": .bool(true)]
            )
            batch = initial
            _ = try await store.operations.followBatch(initial: initial) { updated in
                batch = updated
            }
            store.refresh()
        } catch {
            self.error = error.localizedDescription
        }
    }
}
