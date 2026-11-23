import SwiftUI

/// Matches `AccountModelField` in `desktop/src/AccountModelField.tsx` (2-column grid: model input + refresh | source account select).
public struct AccountModelFieldView: View {
    public let label: String
    public let value: String
    public let accounts: [Account]
    public let online: Bool
    public let connectionKey: String
    public let onChange: @MainActor (String) -> Void

    @State private var textValue: String
    @State private var selectedAccountId: Int = 0
    @State private var models: [ModelOption] = []
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
        self._textValue = State(initialValue: value)
    }

    private var candidateAccounts: [Account] {
        accounts.filter { !$0.name.isEmpty }
    }

    private var selectedAccountName: String {
        guard let found = candidateAccounts.first(where: { $0.id == selectedAccountId }) else {
            return "选择账号"
        }
        return "\(found.name) · \(PlatformMeta.priorityName(found.platform)) #\(found.id)"
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .top, spacing: 18) {
                VStack(alignment: .leading, spacing: 6) {
                    Text(label)
                        .font(DS.fontCompactMedium)
                        .foregroundStyle(DS.muted)

                    HStack(spacing: 6) {
                        TextField(
                            text: $textValue,
                            prompt: Text(verbatim: "输入或选择模型 ID")
                        ) { EmptyView() }
                        .textFieldStyle(.plain)
                        .font(DS.fontBody)
                        .foregroundStyle(DS.ink)
                        .padding(.horizontal, 12)
                        .frame(maxWidth: .infinity)
                        .frame(height: DS.controlHeight)
                        .background(DS.surface)
                        .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                        .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
                        .onChange(of: value) { _, next in
                            if textValue != next { textValue = next }
                        }
                        .onChange(of: textValue) { _, next in
                            if next != value { onChange(next) }
                        }

                        if !models.isEmpty {
                            LinearSelect(
                                label: "候选 (\(models.count))",
                                accessibilityTitle: "\(label)候选",
                                height: DS.controlHeight,
                                compactFont: true,
                                entries: models.map { item in
                                    let display = item.titleText
                                    let title = display == item.id ? item.id : "\(item.id) (\(display))"
                                    return LinearMenuEntry(id: item.id, title: title, checked: item.id == textValue) {
                                        onChange(item.id)
                                    }
                                }
                            )
                        }

                        LinearIconButton(
                            systemName: "arrow.clockwise",
                            label: "刷新模型列表",
                            size: 13,
                            boxSize: DS.controlHeight,
                            bordered: true,
                            disabled: !online || loading || selectedAccountId == 0
                        ) {
                            Task { await loadModels(accountId: selectedAccountId, refresh: true) }
                        }
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)

                VStack(alignment: .leading, spacing: 6) {
                    Text("来源账号")
                        .font(DS.fontCompactMedium)
                        .foregroundStyle(DS.muted)

                    LinearSelect(
                        label: selectedAccountName,
                        accessibilityTitle: "\(label)来源账号",
                        height: DS.controlHeight,
                        expandWidth: true,
                        mutedLabel: selectedAccountId == 0,
                        disabled: !online || candidateAccounts.isEmpty,
                        entries: [
                            LinearMenuEntry(id: "0", title: "选择账号", checked: selectedAccountId == 0) {
                                selectedAccountId = 0
                                models = []
                            },
                        ] + candidateAccounts.map { acc in
                            LinearMenuEntry(id: "\(acc.id)", title: "\(acc.name) · \(PlatformMeta.priorityName(acc.platform)) #\(acc.id)", checked: selectedAccountId == acc.id) {
                                selectedAccountId = acc.id
                                Task { await loadModels(accountId: acc.id, refresh: false) }
                            }
                        }
                    )
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }

            if !errorText.isEmpty {
                Text(errorText)
                    .font(DS.fontResetTime)
                    .foregroundStyle(DS.bad)
            }
        }
        .task(id: "\(connectionKey):\(online):\(selectedAccountId)") {
            ModelDirectoryStore.shared.bindConnection(connectionKey)
            guard online, selectedAccountId > 0 else { return }
            await loadModels(accountId: selectedAccountId, refresh: false)
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
