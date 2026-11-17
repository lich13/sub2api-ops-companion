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

    public init(online: Bool, connectionKey: String, initialItems: [Recovery] = []) {
        self.online = online
        self.connectionKey = connectionKey
        self.initialItems = initialItems
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text("自动恢复与定时测活记录 · \(displayItems.count) 条")
                    .font(DS.fontCompactMedium)
                    .monospacedDigit()
                    .foregroundStyle(DS.muted)
                Spacer()
                Button("刷新记录") {
                    Task { await load(replace: true) }
                }
                .buttonStyle(LinearButtonStyle(variant: .ghost, compact: true))
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
                    .padding(.vertical, 14)
            } else {
                VStack(spacing: 8) {
                    ForEach(displayItems) { row in
                        rowCard(row)
                    }
                }
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

    private var displayItems: [Recovery] {
        items.isEmpty ? initialItems : items
    }

    private func rowCard(_ row: Recovery) -> some View {
        HStack(alignment: .center, spacing: 10) {
            StatusDot(row.recovered_at != nil || row.result_status == "completed" ? .good : .warning)
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 6) {
                    Text(row.account_name.isEmpty ? "账号 #\(row.account_id)" : row.account_name)
                        .font(DS.fontBodySemibold)
                        .foregroundStyle(DS.ink)
                    Text("#\(row.account_id)")
                        .font(DS.fontMono11)
                        .foregroundStyle(DS.muted)
                    kindBadge(row)
                    if !row.model_id.isEmpty {
                        Text(verbatim: row.model_id)
                            .font(DS.fontMono11)
                            .foregroundStyle(DS.muted)
                    }
                }
                HStack(spacing: 10) {
                    if let attempts = row.attempt_count, attempts > 0 {
                        Text("尝试 \(attempts) 次")
                    }
                    if let tested = row.test_completed_at, !tested.isEmpty {
                        Text("验证 \(BeijingTime.fullTime(tested))")
                    }
                    if let rec = row.recovered_at, !rec.isEmpty {
                        Text("恢复 \(BeijingTime.fullTime(rec))")
                    }
                }
                .font(DS.fontResetTime)
                .monospacedDigit()
                .foregroundStyle(DS.muted)
            }
            Spacer()
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 10)
        .background(DS.surface)
        .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
        .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
    }

    private func kindBadge(_ row: Recovery) -> some View {
        let label: String
        let fg: Color
        let bg: Color
        if row.kind == "reset_credit" || row.reset_credit != nil {
            label = "自动用卡恢复"
            fg = DS.accent
            bg = DS.accentBg
        } else if row.kind == "connection_test" {
            label = "定时测活"
            fg = DS.windowEmerald
            bg = DS.windowEmeraldBg
        } else {
            label = "额度恢复"
            fg = DS.good
            bg = DS.goodBg
        }
        return Text(label)
            .font(DS.fontResetTimeSemibold)
            .foregroundStyle(fg)
            .padding(.horizontal, 6)
            .padding(.vertical, 1)
            .background(bg)
            .clipShape(RoundedRectangle(cornerRadius: 4))
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
                let existing = Set(items.map(\.id))
                items.append(contentsOf: env.items.filter { !existing.contains($0.id) })
            }
            nextCursor = env.next_cursor
            error = ""
        } catch {
            self.error = error.localizedDescription
        }
    }
}
