import SwiftUI

/// Matches `desktop/src/ProxyBatchDialog.tsx` (reads `/accounts/{id}/settings`, previews per-account diff & capabilities, submits `settings[id].versions.proxy`).
public struct ProxyBatchSheet: View {
    public let accounts: [Account]
    @Bindable public var store: AppStore
    public let onClose: () -> Void

    private struct AccountProxyPreview: Sendable {
        var proxyId: Int?
        var editable: Bool
        var reason: String
        var expectedVersion: String
    }

    @State private var hasChosenTarget: Bool = false
    @State private var selectedProxyId: Int? = nil
    @State private var proxies: [ProxyOptionItem] = []
    @State private var previews: [Int: AccountProxyPreview] = [:]
    @State private var accountErrors: [Int: String] = [:]
    @State private var batch: OperationBatch? = nil
    @State private var loading: Bool = true
    @State private var busy: Bool = false
    @State private var error: String = ""
    @State private var refreshNonce: Int = 0

    public init(accounts: [Account], store: AppStore, onClose: @escaping () -> Void) {
        self.accounts = accounts
        self.store = store
        self.onClose = onClose
    }

    private var editableAccounts: [Account] {
        accounts.filter { previews[$0.id]?.editable == true }
    }

    private var isBatchPending: Bool {
        batch?.items.contains { ["queued", "running", "checking"].contains($0.status) } ?? false
    }

    private func proxyLabel(_ id: Int?, chosen: Bool = true) -> String {
        guard chosen else { return "请选择代理" }
        guard let id else { return "不使用代理" }
        if let p = proxies.first(where: { $0.id == id }) {
            return "\(p.name) #\(p.id)"
        }
        return "代理 #\(id)"
    }

    public var body: some View {
        LinearModalCard(
            title: "选择代理",
            subtitle: "已选择 \(accounts.count) 个账号",
            width: 540,
            closeDisabled: busy,
            onClose: onClose
        ) {
            HStack {
                Text("母账号的代理变更同时影响继承其代理的子账号。")
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.muted)
                Spacer()
                Button {
                    Task { await readPreviews() }
                } label: {
                    Label("刷新预览", systemImage: "arrow.clockwise")
                }
                .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                .disabled(!store.state.online || busy || loading || isBatchPending)
            }

            if loading {
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                    Text("正在读取账号代理配置…")
                        .font(DS.fontCompact)
                        .foregroundStyle(DS.muted)
                }
                .padding(.vertical, 14)
            } else {
                ProxyOptionsPickerView(
                    selectedProxyId: $selectedProxyId,
                    online: store.state.online,
                    connectionKey: "\(store.connectionKey):\(refreshNonce)"
                )
                .disabled(busy || batch != nil)
                .onChange(of: selectedProxyId) { _, _ in
                    hasChosenTarget = true
                }

                LinearBoundedScrollView(maxHeight: 200) {
                    VStack(spacing: 8) {
                        ForEach(accounts) { acc in
                            let prev = previews[acc.id]
                            let job = batch?.items.first(where: { $0.account_id == acc.id })

                            VStack(alignment: .leading, spacing: 4) {
                                HStack {
                                    Text("\(acc.name) #\(acc.id)")
                                        .font(DS.fontCompactSemibold)
                                        .monospacedDigit()
                                        .foregroundStyle(DS.ink)
                                        .lineLimit(1)
                                    Spacer()
                                    if let job {
                                        Text(job.status)
                                            .font(DS.fontPriorityMeta)
                                            .foregroundStyle(job.status == "failed" ? DS.bad : DS.muted)
                                    }
                                }

                                if let prev {
                                    let sameSuffix = (hasChosenTarget && prev.proxyId == selectedProxyId) ? " · 无变化" : ""
                                    Text("当前：\(proxyLabel(prev.proxyId))   目标：\(proxyLabel(selectedProxyId, chosen: hasChosenTarget))\(sameSuffix)")
                                        .font(DS.fontResetTime)
                                        .monospacedDigit()
                                        .foregroundStyle(DS.muted)
                                    if !prev.editable && !prev.reason.isEmpty {
                                        Text(prev.reason)
                                            .font(DS.fontResetTime)
                                            .foregroundStyle(DS.warn)
                                    }
                                } else {
                                    Text(accountErrors[acc.id] ?? "账号读取失败")
                                        .font(DS.fontResetTime)
                                        .foregroundStyle(DS.bad)
                                }

                                if let reason = job?.reason, !reason.isEmpty {
                                    Text(reason)
                                        .font(DS.fontResetTime)
                                        .foregroundStyle(job?.status == "failed" || job?.status == "needs_confirmation" ? DS.bad : DS.muted)
                                }
                            }
                            .padding(10)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .background(DS.surfaceSubtle)
                            .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(DS.line, lineWidth: 1))
                            .clipShape(RoundedRectangle(cornerRadius: 6))
                        }
                    }
                }
            }

            if !error.isEmpty {
                Text(error)
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.bad)
            }
        } footer: {
            Spacer()
            Button("关闭", action: onClose)
                .buttonStyle(LinearButtonStyle(variant: .secondary))
                .disabled(busy)

            if batch != nil {
                Button("重新预览") {
                    Task { await readPreviews() }
                }
                .buttonStyle(LinearButtonStyle(variant: .secondary))
                .disabled(busy || isBatchPending || !store.state.online)
            } else {
                Button(busy ? "正在应用…" : "确认应用") {
                    Task { await submit() }
                }
                .buttonStyle(LinearButtonStyle(variant: .primary))
                .disabled(!store.state.online || busy || loading || editableAccounts.isEmpty || !accountErrors.isEmpty)
            }
        }
        .sub2opsDialogFocus {
            if !busy { onClose() }
        }
        .task(id: "\(store.connectionKey):\(accounts.map(\.id).description)") {
            await readPreviews()
        }
    }

    private func readPreviews() async {
        guard store.state.online else {
            loading = false
            return
        }
        loading = true
        error = ""
        batch = nil
        previews = [:]
        accountErrors = [:]
        hasChosenTarget = true
        refreshNonce &+= 1
        defer { loading = false }

        do {
            proxies = try await RustBridge.shared.api("GET", "/proxies")
        } catch {
            proxies = []
        }

        var nextPreviews: [Int: AccountProxyPreview] = [:]
        var nextErrors: [Int: String] = [:]
        for acc in accounts {
            do {
                let loaded: AccountSettingsReply = try await RustBridge.shared.api("GET", "/accounts/\(acc.id)/settings")
                let cap = loaded.capabilities["proxy"]
                let ver = loaded.versions["proxy"] ?? acc.operation_versions["proxy"] ?? acc.version
                nextPreviews[acc.id] = AccountProxyPreview(
                    proxyId: loaded.proxy_id,
                    editable: cap?.editable == true,
                    reason: cap?.reason ?? "",
                    expectedVersion: ver
                )
            } catch {
                nextErrors[acc.id] = error.localizedDescription
            }
        }
        previews = nextPreviews
        accountErrors = nextErrors
    }

    private func submit() async {
        guard store.state.online, !busy, !loading, !editableAccounts.isEmpty else { return }
        busy = true
        error = ""
        defer { busy = false }
        let proxyValue: JSONValue = selectedProxyId.map { .int(Int64($0)) } ?? .null
        let targets: [[String: JSONValue]] = accounts.compactMap { acc in
            guard let prev = previews[acc.id] else { return nil }
            return [
                "account_id": .int(Int64(acc.id)),
                "expected_version": .string(prev.expectedVersion),
            ]
        }
        do {
            let initial = try await store.operations.submitBatch(
                action: "proxy",
                accounts: targets,
                requestId: UUID().uuidString.lowercased(),
                customPayload: ["proxy_id": proxyValue]
            )
            batch = initial
            let final = try await store.operations.followBatch(initial: initial) { updated in
                batch = updated
            }
            let okCount = final.items.filter { $0.status == "completed" }.count
            store.refresh()
            if !final.items.isEmpty && final.items.allSatisfy({ $0.status == "completed" }) {
                store.showToast("已更新 \(okCount) 个账号代理")
                onClose()
            }
        } catch {
            self.error = error.localizedDescription
            store.report(error)
        }
    }
}
