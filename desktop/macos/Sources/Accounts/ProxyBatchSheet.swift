import SwiftUI

public struct ProxyBatchSheet: View {
    public let accounts: [Account]
    @Bindable public var store: AppStore
    public let onClose: () -> Void

    @State private var selectedProxyId: Int? = nil
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
                Text("批量修改代理（\(accounts.count) 个账号）")
                    .font(DS.fontDialogTitle)
                    .monospacedDigit()
                    .foregroundStyle(DS.ink)
                Spacer()
                LinearIconButton(systemName: "xmark", label: "关闭批量代理窗口", disabled: busy, action: onClose)
            }

            ProxyOptionsPickerView(
                selectedProxyId: $selectedProxyId,
                online: store.state.online,
                connectionKey: store.connectionKey
            )

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
                Button(busy ? "正在应用…" : "应用到 \(accounts.count) 个账号") {
                    Task { await submit() }
                }
                .buttonStyle(LinearButtonStyle(variant: .primary))
                .disabled(!store.state.online || busy)
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
    }

    private func submit() async {
        guard store.state.online, !busy else { return }
        busy = true
        error = ""
        defer { busy = false }
        let proxyValue: JSONValue = selectedProxyId.map { .int(Int64($0)) } ?? .null
        let targets: [[String: JSONValue]] = accounts.map { acc in
            let ver = acc.operation_versions["proxy"] ?? acc.version
            return [
                "account_id": .int(Int64(acc.id)),
                "expected_version": .string(ver),
                "payload": .object(["proxy_id": proxyValue]),
            ]
        }
        do {
            let initial = try await store.operations.submitBatch(
                action: "proxy",
                accounts: targets,
                requestId: UUID().uuidString.lowercased(),
                customPayload: ["proxy_id": proxyValue]
            )
            let final = try await store.operations.followBatch(initial: initial)
            let okCount = final.items.filter { $0.status == "completed" }.count
            store.refresh()
            store.showToast("已更新 \(okCount)/\(final.items.count) 个账号代理")
            onClose()
        } catch {
            self.error = error.localizedDescription
        }
    }
}
