import SwiftUI

public struct DeleteAccountsSheet: View {
    public let accounts: [Account]
    @Bindable public var store: AppStore
    public let onDeleted: (Set<Int>) -> Void
    public let onClose: () -> Void

    @State private var busy: Bool = false
    @State private var error: String = ""
    @State private var results: [(name: String, ok: Bool, reason: String)] = []

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

    public var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack {
                Text("删除 \(accounts.count) 个账号？")
                    .font(DS.fontDialogTitle)
                    .monospacedDigit()
                    .foregroundStyle(DS.ink)
                Spacer()
                LinearIconButton(systemName: "xmark", label: "关闭删除确认", disabled: busy, action: onClose)
            }

            Text("删除后账号将从 Sub2API 调度池移除，请核对目标账号名称与 ID。")
                .font(DS.fontCompact)
                .foregroundStyle(DS.muted)

            ScrollView {
                VStack(alignment: .leading, spacing: 6) {
                    if results.isEmpty {
                        ForEach(accounts) { acc in
                            HStack(spacing: 8) {
                                Text(acc.name)
                                    .font(DS.fontCompactSemibold)
                                    .foregroundStyle(DS.ink)
                                    .lineLimit(1)
                                Text("#\(acc.id)")
                                    .font(DS.fontMono11)
                                    .foregroundStyle(DS.muted)
                                Spacer()
                                Text("\(PlatformMeta.displayName(acc.platform)) · \(PlatformMeta.typeLabel(acc.type))")
                                    .font(DS.fontResetTime)
                                    .foregroundStyle(DS.muted)
                            }
                            .padding(.horizontal, 10)
                            .padding(.vertical, 6)
                            .background(DS.surfaceSubtle)
                            .clipShape(RoundedRectangle(cornerRadius: 6))
                        }
                    } else {
                        ForEach(Array(results.enumerated()), id: \.offset) { _, row in
                            HStack(spacing: 8) {
                                StatusDot(row.ok ? .good : .bad)
                                Text(row.name)
                                    .font(DS.fontCompactMedium)
                                    .foregroundStyle(DS.ink)
                                Spacer()
                                Text(row.reason)
                                    .font(DS.fontResetTime)
                                    .foregroundStyle(row.ok ? DS.good : DS.bad)
                            }
                            .padding(.horizontal, 10)
                            .padding(.vertical, 6)
                            .background(DS.surfaceSubtle)
                            .clipShape(RoundedRectangle(cornerRadius: 6))
                        }
                    }
                }
            }
            .frame(maxHeight: 240)

            if !error.isEmpty {
                Text(error)
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.bad)
            }

            HStack(spacing: 10) {
                Spacer()
                Button(results.isEmpty ? "取消" : "完成", action: onClose)
                    .buttonStyle(LinearButtonStyle(variant: .secondary))
                    .disabled(busy)
                if results.isEmpty {
                    Button(busy ? "正在删除…" : "确认删除") {
                        Task { await confirmDelete() }
                    }
                    .buttonStyle(LinearButtonStyle(variant: .danger))
                    .disabled(!store.state.online || busy)
                }
            }
        }
        .padding(22)
        .frame(width: 440)
        .background(DS.surface)
        .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(DS.line, lineWidth: 1))
        .clipShape(RoundedRectangle(cornerRadius: 12))
        .sub2opsDialogFocus {
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
            var out: [(String, Bool, String)] = []
            for job in final.items {
                let ok = job.status == "completed"
                if ok { deleted.insert(job.account_id) }
                out.append((job.account_name, ok, ok ? "已删除" : (job.reason ?? "删除未完成")))
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
