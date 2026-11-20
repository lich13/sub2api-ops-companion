import AppKit
import SwiftUI

public struct RecordDetailDrawerView: View {
    public let row: UsageRecord?
    public let loading: Bool
    public let error: String
    public let onClose: () -> Void

    public init(row: UsageRecord?, loading: Bool, error: String, onClose: @escaping () -> Void) {
        self.row = row
        self.loading = loading
        self.error = error
        self.onClose = onClose
    }

    public var body: some View {
        ZStack(alignment: .trailing) {
            DS.backdrop.ignoresSafeArea().onTapGesture(perform: onClose)
            VStack(alignment: .leading, spacing: 0) {
                HStack {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("调用记录")
                            .font(DS.fontDialogTitle)
                            .foregroundStyle(DS.ink)
                        if let row {
                            Text("#\(row.id) · \(BeijingTime.fullTime(row.created_at))")
                                .font(DS.fontPriorityMeta)
                                .monospacedDigit()
                                .foregroundStyle(DS.muted)
                        }
                    }
                    Spacer()
                    LinearIconButton(systemName: "xmark", label: "关闭详情", action: onClose)
                }
                .padding(.horizontal, 18)
                .padding(.vertical, 14)
                .background(DS.topbar)

                Rectangle().fill(DS.line).frame(height: 1)

                ScrollView {
                    if !error.isEmpty {
                        Text(error)
                            .font(DS.fontCompact)
                            .foregroundStyle(DS.bad)
                            .padding(18)
                    } else if let row {
                        VStack(alignment: .leading, spacing: 14) {
                            section("模型路由") {
                                pair("请求", row.requested_model ?? row.model ?? "—", copyable: true)
                                if let chain = row.model_mapping_chain, !chain.isEmpty {
                                    pair("映射", chain, copyable: true)
                                }
                                pair("转发", row.upstream_model ?? row.requested_model ?? row.model ?? "—", copyable: true)
                                pair("返回", row.upstream_response_model ?? "—", copyable: true)
                                if row.upstream_model_mismatch {
                                    Label("返回差异", systemImage: "exclamationmark.circle").font(DS.fontCompactMedium).foregroundStyle(DS.warn)
                                }
                                pair("请求强度", row.requested_reasoning_effort ?? "—")
                                pair("实际强度", row.reasoning_effort ?? "—")
                            }
                            section("请求") {
                                pair("用户", "\(row.user_name ?? row.user_email ?? "用户") #\(row.user_id)")
                                pair("API 密钥", "\(row.api_key_name ?? "密钥") #\(row.api_key_id)")
                                pair("账户", "\(row.account_name ?? "账户") #\(row.account_id)")
                                pair("分组", row.group_name ?? row.group_id.map { "#\($0)" } ?? "—")
                                field("类型") { RecordTypeBadges(row: row) }
                                pair("服务等级", row.service_tier ?? "—")
                                pair("入站端点", row.inbound_endpoint ?? "—", copyable: true)
                                pair("上游端点", row.upstream_endpoint ?? "—", copyable: true)
                                pair("请求 ID", row.request_id ?? "—", copyable: true)
                                pair("上游 ID", row.upstream_request_id ?? "—", copyable: true)
                                pair("User-Agent", row.user_agent ?? "—", copyable: true)
                                pair("IP", row.ip_address ?? "—", copyable: true)
                                pair("首字延迟", RecordFormatting.latency(row.first_token_ms))
                                pair("总耗时", RecordFormatting.latency(row.duration_ms))
                                pair("输出 TPS", RecordFormatting.outputRate(row))
                            }
                            section("Token 与费用") {
                                pair("输入 Token", RecordFormatting.tokenCount(row.input_tokens))
                                pair("输出 Token", RecordFormatting.tokenCount(row.output_tokens))
                                pair("缓存读取", RecordFormatting.tokenCount(row.cache_read_tokens))
                                pair("缓存写入", RecordFormatting.tokenCount(row.cache_creation_tokens))
                                if let tokens = row.cache_creation_5m_tokens, tokens > 0 { pair("5m 缓存", String(tokens)) }
                                if let tokens = row.cache_creation_1h_tokens, tokens > 0 { pair("1h 缓存", String(tokens)) }
                                pair("输入费用", RecordFormatting.money(row.input_cost, digits: 8))
                                pair("输出费用", RecordFormatting.money(row.output_cost, digits: 8))
                                pair("缓存读取费用", RecordFormatting.money(row.cache_read_cost, digits: 8))
                                pair("缓存写入费用", RecordFormatting.money(row.cache_creation_cost, digits: 8))
                                pair("标准费用", RecordFormatting.money(row.total_cost, digits: 8))
                                pair("用户实扣", RecordFormatting.money(row.actual_cost, digits: 8))
                                pair("账户费用", RecordFormatting.money(row.account_cost, digits: 8))
                                pair("用户倍率", RecordFormatting.decimal(row.rate_multiplier, digits: 2))
                                pair("账户倍率", RecordFormatting.decimal(row.account_rate_multiplier ?? "1", digits: 2))
                                pair("计费模式", RecordFormatting.billingMode(row.billing_mode))
                                if let count = row.image_count, count > 0 { pair("图片", String(count)) }
                                if let tokens = row.image_input_tokens, tokens > 0 { pair("图片输入 Token", String(tokens)) }
                                if let tokens = row.image_output_tokens, tokens > 0 { pair("图片输出 Token", String(tokens)) }
                                if let cost = row.image_input_cost, Decimal(string: cost).map({ $0 != 0 }) == true {
                                    pair("图片输入费用", RecordFormatting.money(cost, digits: 8))
                                }
                                if let cost = row.image_output_cost, Decimal(string: cost).map({ $0 != 0 }) == true {
                                    pair("图片输出费用", RecordFormatting.money(cost, digits: 8))
                                }
                                if let count = row.video_count, count > 0 {
                                    let duration = row.video_duration_seconds.map { $0.formatted(.number.grouping(.never)) } ?? "—"
                                    pair("视频", "\(count) · \(row.video_resolution ?? "—") · \(duration)s")
                                }
                            }
                        }
                        .padding(18)
                    } else if loading {
                        Text("正在读取记录…")
                            .font(DS.fontCompact)
                            .foregroundStyle(DS.muted)
                            .padding(18)
                    }
                }
            }
            .frame(width: 460)
            .frame(maxHeight: .infinity)
            .background(DS.surface)
            .overlay(alignment: .leading) { Rectangle().fill(DS.line).frame(width: 1) }
            .sub2opsDialogFocus(onEscape: onClose)
        }
    }

    private func section<C: View>(_ title: String, @ViewBuilder content: () -> C) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(title)
                .font(DS.fontCompactSemibold)
                .foregroundStyle(DS.ink)
            VStack(alignment: .leading, spacing: 5) {
                content()
            }
            .padding(12)
            .background(DS.surfaceSubtle)
            .clipShape(RoundedRectangle(cornerRadius: 8))
        }
    }

    private func field<C: View>(_ key: String, @ViewBuilder value: () -> C) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Text(key)
                .font(DS.fontCompact)
                .foregroundStyle(DS.muted)
                .frame(width: 84, alignment: .leading)
            value().frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private func pair(_ key: String, _ val: String, copyable: Bool = false) -> some View {
        field(key) {
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Text(verbatim: val)
                    .font(copyable ? DS.fontMono12 : DS.fontCompactMedium)
                    .monospacedDigit()
                    .foregroundStyle(DS.ink)
                    .fixedSize(horizontal: false, vertical: true)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                if copyable && val != "—" {
                    Button {
                        NSPasteboard.general.clearContents()
                        NSPasteboard.general.setString(val, forType: .string)
                    } label: {
                        Image(systemName: "doc.on.doc")
                            .font(DS.iconFont(10))
                            .foregroundStyle(DS.muted)
                    }
                    .buttonStyle(.linearPlain)
                    .accessibilityLabel("复制\(key)")
                }
            }
        }
    }
}
