import SwiftUI

public struct DeleteAccountsSheet: View {
    public let accounts: [Account]
    @Bindable public var store: AppStore
    public let onDeleted: (Set<Int>) -> Void
    public let onClose: () -> Void

    @State private var busy: Bool = false
    @State private var error: String = ""
    @State private var results: [(id: Int, name: String, ok: Bool, reason: String)] = []

    public init(
        accounts: [Account],
        store: AppStore,
        onDeleted: @escaping (Set<Int>) -> Void,
        onClose: @escaping () -> Void
    ) {
        self.accounts = accounts
        self.store = store
        self.onDeleted = onDeleted
        self.onClose = onClose
    }

    private var affectedChildrenCount: Int {
        let parentIds = Set(accounts.map(\.id))
        return store.accounts.filter { acc in
            guard let pid = acc.parent_account_id else { return false }
            return parentIds.contains(pid) && !parentIds.contains(acc.id)
        }.count
    }

    public var body: some View {
        LinearModalCard(
            title: results.isEmpty ? (busy ? "正在删除账号" : "删除 \(accounts.count) 个账号？") : "删除结果",
            width: 480,
            closeDisabled: busy,
            onClose: onClose
        ) {
            if results.isEmpty {
                Text("删除后无法撤销。\(affectedChildrenCount > 0 ? "母账号关联的 \(affectedChildrenCount) 个子账号也会被删除。" : "")")
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.muted)
            }

            LinearBoundedScrollView(maxHeight: 260) {
                VStack(spacing: 0) {
                    if results.isEmpty {
                        ForEach(Array(accounts.enumerated()), id: \.element.id) { idx, acc in
                            HStack(spacing: 12) {
                                Text(acc.name)
                                    .font(DS.fontCompactSemibold)
                                    .foregroundStyle(DS.ink)
                                    .lineLimit(1)
                                Spacer(minLength: 8)
                                Text("#\(acc.id)")
                                    .font(DS.fontPriorityMeta)
                                    .monospacedDigit()
                                    .foregroundStyle(DS.muted)
                            }
                            .padding(.vertical, 8)
                            if idx < accounts.count - 1 {
                                Rectangle().fill(DS.line).frame(height: 1)
                            }
                        }
                    } else {
                        ForEach(Array(results.enumerated()), id: \.offset) { idx, row in
                            HStack(spacing: 10) {
                                StatusDot(row.ok ? .good : .bad)
                                Text(row.name)
                                    .font(DS.fontCompactMedium)
                                    .foregroundStyle(DS.ink)
                                    .lineLimit(1)
                                Text("#\(row.id)")
                                    .font(DS.fontPriorityMeta)
                                    .monospacedDigit()
                                    .foregroundStyle(DS.muted)
                                Spacer()
                                Text(row.reason)
                                    .font(DS.fontPriorityMeta)
                                    .foregroundStyle(row.ok ? DS.good : DS.bad)
                            }
                            .padding(.vertical, 8)
                            if idx < results.count - 1 {
                                Rectangle().fill(DS.line).frame(height: 1)
                            }
                        }
                    }
                }
            }

            if !results.isEmpty {
                let okCount = results.filter(\.ok).count
                let failCount = results.count - okCount
                Text("\(okCount) 个已删除，\(failCount) 个失败")
                    .font(DS.fontCompactMedium)
                    .monospacedDigit()
                    .foregroundStyle(DS.ink)
            }

            if !error.isEmpty {
                Text(error)
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.bad)
            }
        } footer: {
            Spacer()
            Button(results.isEmpty ? "取消" : "关闭", action: onClose)
                .buttonStyle(LinearButtonStyle(variant: .secondary))
                .disabled(busy)
            if results.isEmpty {
                Button(busy ? "删除中…" : "确认删除") {
                    Task { await confirmDelete() }
                }
                .buttonStyle(LinearButtonStyle(variant: .solidDanger))
                .disabled(!store.state.online || busy)
            }
        }
        .sub2opsDialogFocus(canBackdropDismiss: !busy) {
            if !busy { onClose() }
        }
    }

    private func confirmDelete() async {
        guard store.state.online, !busy else { return }
        busy = true
        error = ""
        defer { busy = false }
        let targets: [[String: JSONValue]] = accounts.map { acc in
            let ver = acc.operation_versions["delete"] ?? acc.version
            return [
                "account_id": .int(Int64(acc.id)),
                "expected_version": .string(ver),
                "payload": .object([:]),
            ]
        }
        do {
            let initial = try await store.operations.submitBatch(
                action: "delete",
                accounts: targets,
                requestId: UUID().uuidString.lowercased()
            )
            let final = try await store.operations.followBatch(initial: initial)
            var deleted = Set<Int>()
            var out: [(Int, String, Bool, String)] = []
            for acc in accounts {
                let job = final.items.first(where: { $0.account_id == acc.id })
                let ok = job?.status == "completed" && (job?.result?.deleted ?? true)
                if ok { deleted.insert(acc.id) }
                out.append((acc.id, acc.name, ok, ok ? "已删除" : (job?.reason ?? "删除未完成")))
            }
            onDeleted(deleted)
            store.refresh()
            if deleted.count == accounts.count {
                store.showToast("已删除 \(deleted.count) 个账号")
                onClose()
            } else {
                results = out
            }
        } catch {
            self.error = error.localizedDescription
        }
    }
}
