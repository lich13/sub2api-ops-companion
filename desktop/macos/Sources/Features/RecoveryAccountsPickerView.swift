import SwiftUI

/// Matches `RecoveryAccounts` in `desktop/src/RecoveryAccounts.tsx`.
public struct RecoveryAccountsPickerView: View {
    public let accounts: [Account]
    public let connectionIds: [Int]
    public let modelIds: [Int]
    public let onChange: (_ connection: [Int], _ models: [Int]) -> Void

    @State private var search: String = ""

    public init(
        accounts: [Account],
        connectionIds: [Int],
        modelIds: [Int],
        onChange: @escaping (_ connection: [Int], _ models: [Int]) -> Void
    ) {
        self.accounts = accounts
        self.connectionIds = connectionIds
        self.modelIds = modelIds
        self.onChange = onChange
    }

    private var eligibleAccounts: [Account] {
        accounts.filter {
            $0.platform == "openai" && $0.type == "oauth" && $0.recovery_selectable != false
        }
    }

    private var filteredAccounts: [Account] {
        let q = search.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard !q.isEmpty else { return eligibleAccounts }
        return eligibleAccounts.filter {
            $0.name.lowercased().contains(q) || "\($0.id)".contains(q)
        }
    }

    private func toggleConnection(_ id: Int) {
        var nextConn = Set(connectionIds)
        var nextModel = Set(modelIds)
        if nextConn.contains(id) {
            nextConn.remove(id)
        } else {
            nextConn.insert(id)
            nextModel.remove(id)
        }
        onChange(nextConn.sorted(), nextModel.sorted())
    }

    private func toggleModel(_ id: Int) {
        var nextConn = Set(connectionIds)
        var nextModel = Set(modelIds)
        if nextModel.contains(id) {
            nextModel.remove(id)
        } else {
            nextModel.insert(id)
            nextConn.remove(id)
        }
        onChange(nextConn.sorted(), nextModel.sorted())
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    Text("恢复测活方式分配（OpenAI OAuth）")
                        .font(DS.fontBodySemibold)
                        .foregroundStyle(DS.ink)
                    Text("已选连接测活 \(connectionIds.count) 个 · 模型测活 \(modelIds.count) 个（同一账号互斥）")
                        .font(DS.fontResetTime)
                        .monospacedDigit()
                        .foregroundStyle(DS.muted)
                }
                Spacer()
                HStack(spacing: 6) {
                    Image(systemName: "magnifyingglass")
                        .font(DS.iconFont(11))
                        .foregroundStyle(DS.muted)
                    TextField(text: $search, prompt: Text("搜索账号")) { EmptyView() }
                        .textFieldStyle(.plain)
                        .font(DS.fontCompact)
                        .frame(width: 150)
                }
                .padding(.horizontal, 8)
                .frame(height: DS.controlHeightDense)
                .background(DS.surface)
                .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
            }

            if filteredAccounts.isEmpty {
                Text(eligibleAccounts.isEmpty ? "暂无可分配的 OpenAI OAuth 账号" : "未匹配到账号")
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.muted)
                    .padding(12)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(DS.surfaceSubtle)
                    .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
            } else {
                VStack(spacing: 0) {
                    HStack {
                        Text("账号")
                            .font(DS.fontTableHeader)
                            .foregroundStyle(DS.muted)
                        Spacer()
                        Text("测试连接")
                            .font(DS.fontTableHeader)
                            .foregroundStyle(DS.muted)
                            .frame(width: 84, alignment: .center)
                        Text("模型测试")
                            .font(DS.fontTableHeader)
                            .foregroundStyle(DS.muted)
                            .frame(width: 84, alignment: .center)
                    }
                    .padding(.horizontal, 12)
                    .frame(height: 30)
                    .background(DS.surfaceSubtle)

                    Rectangle().fill(DS.line).frame(height: 1)

                    ScrollView {
                        VStack(spacing: 0) {
                            ForEach(Array(filteredAccounts.enumerated()), id: \.element.id) { idx, acc in
                                let isConn = connectionIds.contains(acc.id)
                                let isModel = modelIds.contains(acc.id)
                                HStack {
                                    HStack(spacing: 6) {
                                        Text(acc.name)
                                            .font(DS.fontCompactMedium)
                                            .foregroundStyle(DS.ink)
                                            .lineLimit(1)
                                        Text("#\(acc.id)")
                                            .font(DS.fontMono11)
                                            .foregroundStyle(DS.muted)
                                    }
                                    Spacer()
                                    Button {
                                        toggleConnection(acc.id)
                                    } label: {
                                        Image(systemName: isConn ? "checkmark.square.fill" : "square")
                                            .font(DS.iconFont(14))
                                            .foregroundStyle(isConn ? DS.accent : DS.muted)
                                            .frame(width: 84, height: 28)
                                    }
                                    .buttonStyle(.plain)

                                    Button {
                                        toggleModel(acc.id)
                                    } label: {
                                        Image(systemName: isModel ? "checkmark.square.fill" : "square")
                                            .font(DS.iconFont(14))
                                            .foregroundStyle(isModel ? DS.accent : DS.muted)
                                            .frame(width: 84, height: 28)
                                    }
                                    .buttonStyle(.plain)
                                }
                                .padding(.horizontal, 12)
                                .frame(height: 32)
                                if idx < filteredAccounts.count - 1 {
                                    Rectangle().fill(DS.line).frame(height: 1)
                                }
                            }
                        }
                    }
                    .frame(maxHeight: 220)
                }
                .background(DS.surface)
                .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
            }
        }
        .padding(.top, 4)
    }
}
