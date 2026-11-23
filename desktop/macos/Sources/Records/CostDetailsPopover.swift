import SwiftUI

public enum CostDetailsBuilder {
    private static func numeric(_ raw: String?) -> Double? {
        guard let raw, !raw.isEmpty, let v = Double(raw), v.isFinite else { return nil }
        return v
    }

    private static func amount(_ raw: String?) -> String {
        guard let raw else { return "—" }
        return RecordFormatting.money(raw, digits: 8)
    }

    private static func multiplier(_ raw: String?) -> String {
        let d = RecordFormatting.decimal(raw, digits: 2)
        return d == "—" ? "—" : "\(d)x"
    }

    public static func items(for row: UsageRecord) -> [(label: String, value: String)] {
        var rows: [(String, String)] = [
            ("输入费用", amount(row.input_cost)),
            ("输出费用", amount(row.output_cost)),
        ]
        func appendUnitPrice(_ label: String, cost: String?, tokens: Int?) {
            guard let tokens, tokens > 0 else { return }
            if let c = numeric(cost) {
                rows.append((label, String(format: "$%.4f / 1M Token", c / Double(tokens) * 1_000_000.0)))
            } else {
                rows.append((label, "—"))
            }
        }

        let imageBilling = (row.image_count ?? 0) > 0 && row.billing_mode != "token" && row.billing_mode != "video"
        if !imageBilling && (row.billing_mode == nil || row.billing_mode == "token") {
            let textIn = row.input_tokens.map { max(0, $0 - (row.image_input_tokens ?? 0)) }
            let textOut = row.output_tokens.map { max(0, $0 - (row.image_output_tokens ?? 0)) }
            appendUnitPrice("输入单价", cost: row.input_cost, tokens: textIn)
            appendUnitPrice("输出单价", cost: row.output_cost, tokens: textOut)
            if (row.image_input_tokens ?? 0) > 0 {
                rows.append(("图片输入费用", amount(row.image_input_cost)))
                appendUnitPrice("图片输入单价", cost: row.image_input_cost, tokens: row.image_input_tokens)
            }
            if (row.image_output_tokens ?? 0) > 0 {
                rows.append(("图片输出费用", amount(row.image_output_cost)))
                appendUnitPrice("图片输出单价", cost: row.image_output_cost, tokens: row.image_output_tokens)
            }
        } else if imageBilling || row.billing_mode == "image" {
            rows.append(("图片数量", row.image_count.map(String.init) ?? "—"))
            if let cnt = row.image_count, cnt > 0, let tot = numeric(row.total_cost) {
                rows.append(("图片单价", RecordFormatting.money(String(tot / Double(cnt)), digits: 8)))
            } else {
                rows.append(("图片单价", "—"))
            }
        } else if row.billing_mode == "per_request" {
            rows.append(("每次费用", amount(row.total_cost)))
        }

        if row.cache_read_cost != nil {
            rows.append(("缓存读取费用", amount(row.cache_read_cost)))
        }
        if row.cache_creation_cost != nil {
            rows.append(("缓存写入费用", amount(row.cache_creation_cost)))
        }

        let tierRaw = (row.service_tier ?? "").trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        let tierKey = tierRaw.isEmpty ? "default" : tierRaw
        let tierMap: [String: String] = [
            "default": "Standard", "standard": "Standard",
            "fast": "Priority", "priority": "Priority",
            "ultrafast": "Ultrafast", "flex": "Flex", "scale": "Scale",
        ]
        rows.append(("服务档位", tierMap[tierKey] ?? tierKey))
        rows.append(("倍率", multiplier(row.rate_multiplier)))
        rows.append(("原始", amount(row.total_cost)))
        rows.append(("用户扣费", amount(row.actual_cost)))
        rows.append(("账号倍率", multiplier(row.account_rate_multiplier)))
        rows.append(("账号计费", amount(row.account_cost)))
        return rows
    }
}

public struct CostDetailsPopover: View {
    public let row: UsageRecord
    @State private var isOpen: Bool = false

    public init(row: UsageRecord) {
        self.row = row
    }

    public var body: some View {
        Button {
            isOpen.toggle()
        } label: {
            VStack(alignment: .trailing, spacing: 2) {
                HStack(spacing: 3) {
                    Text(RecordFormatting.money(row.actual_cost))
                        .font(DS.fontCompactMedium)
                        .monospacedDigit()
                        .lineLimit(1)
                        .foregroundStyle(DS.good)
                    Image(systemName: "info.circle")
                        .font(DS.iconFont(10))
                        .foregroundStyle(DS.muted)
                }
                Text("A \(RecordFormatting.money(row.account_cost))")
                    .font(DS.fontResetTime)
                    .monospacedDigit()
                    .lineLimit(1)
                    .foregroundStyle(DS.costOrange)
            }
        }
        .buttonStyle(.linearPlain)
        .accessibilityLabel("查看费用明细")
        .popover(isPresented: $isOpen, arrowEdge: .trailing) {
            VStack(alignment: .leading, spacing: 8) {
                Text("费用明细")
                    .font(DS.fontCompactSemibold)
                    .foregroundStyle(DS.ink)
                ForEach(Array(CostDetailsBuilder.items(for: row).enumerated()), id: \.offset) { _, pair in
                    if pair.label == "服务档位" || pair.label == "账号倍率" {
                        Rectangle().fill(DS.line).frame(height: 1)
                    }
                    HStack {
                        Text(pair.label)
                            .font(DS.fontResetTime)
                            .foregroundStyle(DS.muted)
                        Spacer(minLength: 12)
                        Text(verbatim: pair.value)
                            .font(DS.fontPriorityMeta).monospacedDigit()
                            .foregroundStyle(pair.label == "用户扣费" || pair.label == "账号计费" ? DS.good : DS.ink)
                    }
                }
            }
            .padding(12)
            .frame(width: 260)
            .background(DS.surface)
        }
    }
}
