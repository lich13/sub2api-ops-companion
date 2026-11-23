import SwiftUI

public struct RecoveryHistoryEnvelope: Decodable, Sendable {
    public var items: [Recovery]
    public var next_cursor: Int?
}

public struct RecoveryHistoryView: View {
    public let online: Bool
    public let connectionKey: String
    public var initialItems: [Recovery]

    @State private var items: [Recovery] = []
    @State private var nextCursor: Int?
    @State private var loading: Bool = false
    @State private var error: String = ""
    @State private var viewportWidth: CGFloat = 1100
    @State private var hoveredRowId: Int? = nil

    public init(online: Bool, connectionKey: String, initialItems: [Recovery] = []) {
        self.online = online
        self.connectionKey = connectionKey
        self.initialItems = initialItems
    }

    private var displayItems: [Recovery] {
        items.isEmpty ? initialItems : items
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text("\(displayItems.count) 条记录")
                    .font(DS.fontCompact)
                    .monospacedDigit()
                    .foregroundStyle(DS.muted)
                Spacer()
                Button("刷新记录") {
                    Task { await load(replace: true) }
                }
                .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                .disabled(!online || loading)
            }

            if !error.isEmpty {
                HStack(spacing: 8) {
                    Text(error)
                        .font(DS.fontCompact)
                        .foregroundStyle(DS.bad)
                    Button("重试") {
                        Task { await load(replace: true) }
                    }
                    .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                    .disabled(!online || loading)
                }
            }

            if displayItems.isEmpty {
                Text(loading ? "正在读取恢复记录…" : "暂无自动恢复记录")
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.muted)
                    .frame(maxWidth: .infinity, alignment: .center)
                    .padding(.vertical, 28)
                    .background(DS.surface)
                    .overlay(RoundedRectangle(cornerRadius: DS.cardRadius).strokeBorder(DS.line, lineWidth: 1))
                    .clipShape(RoundedRectangle(cornerRadius: DS.cardRadius))
            } else {
                ScrollView(.horizontal, showsIndicators: false) {
                    VStack(spacing: 0) {
                        headerRow
                        Rectangle().fill(DS.line).frame(height: 1)
                        ForEach(Array(displayItems.enumerated()), id: \.element.recordId) { idx, row in
                            tableRow(row)
                            if idx < displayItems.count - 1 {
                                Rectangle().fill(DS.line).frame(height: 1)
                            }
                        }
                    }
                    .frame(minWidth: max(960, viewportWidth), maxWidth: .infinity, alignment: .leading)
                }
                .focusEffectDisabled()
                .background(LinearWidthObserver { viewportWidth = $0 })
                .background(DS.surface)
                .overlay(RoundedRectangle(cornerRadius: DS.cardRadius).strokeBorder(DS.line, lineWidth: 1))
                .clipShape(RoundedRectangle(cornerRadius: DS.cardRadius))
            }

            if nextCursor != nil {
                Button(loading ? "加载中…" : "加载更早记录") {
                    Task { await load(replace: false) }
                }
                .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                .disabled(!online || loading)
            }
        }
        .task(id: "\(connectionKey):\(online)") {
            guard online else { return }
            await load(replace: true)
        }
    }

    private var headerRow: some View {
        HStack(spacing: 12) {
            Text("账号")
                .frame(minWidth: 180, maxWidth: .infinity, alignment: .leading)
            Text("恢复类型")
                .frame(width: 84, alignment: .leading)
            Text("验证方式")
                .frame(width: 84, alignment: .leading)
            Text("测试模型")
                .frame(width: 132, alignment: .leading)
            Text("尝试次数")
                .frame(width: 68, alignment: .trailing)
                .padding(.trailing, 14)
            Text("用卡时间")
                .frame(width: 124, alignment: .leading)
            Text("测试完成时间")
                .frame(width: 124, alignment: .leading)
            Text("结果 / 恢复时间")
                .frame(width: 124, alignment: .leading)
        }
        .font(DS.fontTableHeader)
        .foregroundStyle(DS.muted)
        .padding(.horizontal, 14)
        .frame(height: 36)
        .background(DS.surfaceSubtle)
    }

    private func tableRow(_ row: Recovery) -> some View {
        let kindLabel: String = {
            if row.kind == "connection_test" { return "预约测试" }
            if row.kind == "reset_credit" || row.reset_credit != nil { return "用卡恢复" }
            return "额度恢复"
        }()

        let verificationLabel: String = {
            if row.kind == "connection_test" { return "测试连接" }
            if let method = row.reset_credit?.verification_method {
                if method == "connection_test" { return "测试连接" }
                if method == "model_test" { return "模型测试" }
                return method
            }
            return "—"
        }()

        let cardTime: String = {
            guard let completed = row.reset_credit?.completed_at, !completed.isEmpty else { return "—" }
            return BeijingTime.fullTime(completed)
        }()

        let testTime: String = {
            guard let completed = row.test_completed_at, !completed.isEmpty else { return "—" }
            return BeijingTime.fullTime(completed)
        }()

        let resultOrRecovered: String = {
            if let rec = row.recovered_at, !rec.isEmpty {
                return BeijingTime.fullTime(rec)
            }
            switch row.result_status {
            case "completed": return "已完成"
            case "failed": return "失败"
            case "cancelled": return "已取消"
            default: return "—"
            }
        }()

        return HStack(alignment: .center, spacing: 12) {
            Text(row.accountNameText.isEmpty ? "账号 #\(row.account_id ?? 0)" : row.accountNameText)
                .font(DS.fontCompactMedium)
                .foregroundStyle(DS.ink)
                .lineLimit(2)
                .frame(minWidth: 180, maxWidth: .infinity, alignment: .leading)

            Text(kindLabel)
                .font(DS.fontCompact)
                .foregroundStyle(DS.ink)
                .frame(width: 84, alignment: .leading)

            Text(verificationLabel)
                .font(DS.fontCompact)
                .foregroundStyle(verificationLabel == "—" ? DS.muted : DS.ink)
                .frame(width: 84, alignment: .leading)

            Text(verbatim: row.modelIdText.isEmpty ? "—" : row.modelIdText)
                .font(DS.fontCompact)
                .foregroundStyle(row.modelIdText.isEmpty ? DS.muted : DS.ink)
                .lineLimit(1)
                .frame(width: 132, alignment: .leading)

            Text(row.attempt_count.map(String.init) ?? "—")
                .font(DS.fontCompact)
                .monospacedDigit()
                .foregroundStyle(row.attempt_count == nil ? DS.muted : DS.ink)
                .frame(width: 68, alignment: .trailing)
                .padding(.trailing, 14)

            Text(cardTime)
                .font(DS.fontCompact)
                .monospacedDigit()
                .foregroundStyle(DS.muted)
                .frame(width: 124, alignment: .leading)

            Text(testTime)
                .font(DS.fontCompact)
                .monospacedDigit()
                .foregroundStyle(DS.muted)
                .frame(width: 124, alignment: .leading)

            Text(resultOrRecovered)
                .font(DS.fontCompact)
                .monospacedDigit()
                .foregroundStyle(DS.muted)
                .frame(width: 124, alignment: .leading)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 12)
        .background(row.id != nil && hoveredRowId == row.id ? DS.rowHover : DS.clear)
        .onHover { hovering in
            if hovering {
                hoveredRowId = row.id
            } else if hoveredRowId == row.id {
                hoveredRowId = nil
            }
        }
    }

    private func load(replace: Bool) async {
        guard online else { return }
        if !replace && nextCursor == nil { return }
        loading = true
        defer { loading = false }
        let suffix = (!replace && nextCursor != nil) ? "?before_id=\(nextCursor!)" : ""
        do {
            let env: RecoveryHistoryEnvelope = try await RustBridge.shared.api("GET", "/recoveries\(suffix)")
            if replace {
                items = env.items
            } else {
                let existing = Set(items.compactMap(\.id))
                items.append(contentsOf: env.items.filter { $0.id.map { !existing.contains($0) } ?? true })
            }
            nextCursor = env.next_cursor
            error = ""
        } catch {
            self.error = error.localizedDescription
        }
    }
}
