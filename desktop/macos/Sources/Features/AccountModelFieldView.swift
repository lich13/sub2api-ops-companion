import SwiftUI

/// Matches `AccountModelField` in `desktop/src/AccountModelField.tsx`.
public struct AccountModelFieldView: View {
    public let label: String
    public let value: String
    public let accounts: [Account]
    public let online: Bool
    public let connectionKey: String
    public let onChange: @MainActor (String) -> Void

    @State private var selectedAccountId: Int = 0
    @State private var models: [AccountModelItem] = []
    @State private var loading: Bool = false
    @State private var errorText: String = ""

    public init(
        label: String,
        value: String,
        accounts: [Account],
        online: Bool,
        connectionKey: String,
        onChange: @escaping @MainActor (String) -> Void
    ) {
        self.label = label
        self.value = value
        self.accounts = accounts
        self.online = online
        self.connectionKey = connectionKey
        self.onChange = onChange
    }

    private var candidateAccounts: [Account] {
        accounts.filter { $0.status == "active" && !$0.name.isEmpty }
    }

    private var resolvedAccountId: Int {
        if candidateAccounts.contains(where: { $0.id == selectedAccountId }) {
            return selectedAccountId
        }
        if let openaiActive = candidateAccounts.first(where: { $0.platform == "openai" && $0.schedulable }) {
            return openaiActive.id
        }
        return candidateAccounts.first?.id ?? 0
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .center, spacing: 10) {
                Text(label)
                    .font(DS.fontBody)
                    .foregroundStyle(DS.ink)
                Spacer()
                HStack(spacing: 6) {
                    TextField(
                        text: Binding(get: { value }, set: onChange),
                        prompt: Text(verbatim: "输入或选择模型 ID")
                    ) { EmptyView() }
                    .textFieldStyle(.plain)
                    .font(DS.fontMono12)
                    .padding(.horizontal, 10)
                    .frame(width: 220, height: DS.controlHeightDense)
                    .background(DS.surface)
                    .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                    .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))

                    if !models.isEmpty {
                        Menu {
                            ForEach(models) { item in
                                Button {
                                    onChange(item.id)
                                } label: {
                                    Text(verbatim: item.display_name == item.id ? item.id : "\(item.id) (\(item.display_name))")
                                }
                            }
                        } label: {
                            HStack(spacing: 4) {
                                Text("候选 (\(models.count))")
                                    .monospacedDigit()
                                Image(systemName: "chevron.down")
                                    .font(DS.iconFont(10))
                            }
                            .font(DS.fontCompactMedium)
                            .foregroundStyle(DS.ink)
                            .padding(.horizontal, 8)
                            .frame(height: DS.controlHeightDense)
                            .background(DS.surface)
                            .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                            .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
                        }
                        .menuStyle(.borderlessButton)
                        .menuIndicator(.hidden)
                        .fixedSize()
                    }

                    if !candidateAccounts.isEmpty {
                        Menu {
                            ForEach(candidateAccounts) { acc in
                                Button {
                                    selectedAccountId = acc.id
                                    Task { await loadModels(accountId: acc.id, refresh: false) }
                                } label: {
                                    Text("\(acc.name) (#\(acc.id))")
                                }
                            }
                        } label: {
                            let activeName = candidateAccounts.first(where: { $0.id == resolvedAccountId })?.name ?? "账号"
                            HStack(spacing: 4) {
                                Text(activeName)
                                    .lineLimit(1)
                                Image(systemName: "chevron.down")
                                    .font(DS.iconFont(10))
                            }
                            .font(DS.fontCompact)
                            .foregroundStyle(DS.muted)
                            .padding(.horizontal, 8)
                            .frame(maxWidth: 130, minHeight: DS.controlHeightDense)
                            .background(DS.surfaceSubtle)
                            .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                            .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
                        }
                        .menuStyle(.borderlessButton)
                        .menuIndicator(.hidden)
                        .fixedSize()

                        LinearIconButton(
                            systemName: "arrow.clockwise",
                            label: "刷新模型列表",
                            size: 12,
                            disabled: !online || loading || resolvedAccountId == 0
                        ) {
                            Task { await loadModels(accountId: resolvedAccountId, refresh: true) }
                        }
                    }
                }
            }

            if candidateAccounts.isEmpty {
                Text("当前没有可读取模型列表的在线账号，可直接输入模型 ID。")
                    .font(DS.fontResetTime)
                    .foregroundStyle(DS.muted)
            } else if !errorText.isEmpty {
                Text(errorText)
                    .font(DS.fontResetTime)
                    .foregroundStyle(DS.bad)
            }
        }
        .task(id: "\(connectionKey):\(online):\(resolvedAccountId)") {
            ModelDirectoryStore.shared.bindConnection(connectionKey)
            guard online, resolvedAccountId != 0 else { return }
            await loadModels(accountId: resolvedAccountId, refresh: false)
        }
    }

    private func loadModels(accountId: Int, refresh: Bool) async {
        guard online, accountId > 0 else { return }
        loading = true
        errorText = ""
        defer { loading = false }
        do {
            models = try await ModelDirectoryStore.shared.readModels(accountId: accountId, refresh: refresh)
        } catch {
            if ModelDirectoryStore.isRevoked(error) {
                errorText = "所选账号认证已失效，请切换其他账号或直接输入模型 ID"
            } else {
                errorText = error.localizedDescription
            }
        }
    }
}
