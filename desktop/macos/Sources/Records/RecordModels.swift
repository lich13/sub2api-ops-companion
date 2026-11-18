import Foundation

public struct UsageRecord: Codable, Sendable, Equatable, Identifiable {
    public var id: Int
    public var created_at: String
    public var user_id: Int
    public var user_name: String?
    public var user_email: String?
    public var api_key_id: Int
    public var api_key_name: String?
    public var account_id: Int
    public var account_name: String?
    public var group_id: Int?
    public var group_name: String?
    public var model: String?
    public var requested_model: String?
    public var upstream_model: String?
    public var upstream_response_model: String?
    public var model_mapping_chain: String?
    public var upstream_model_mismatch: Bool
    public var reasoning_effort: String?
    public var requested_reasoning_effort: String?
    public var request_type: String?
    public var native_request_type: String?
    public var stream: Bool?
    public var openai_ws_mode: Bool?
    public var native_compaction_v2: Bool?
    public var input_tokens: Int?
    public var output_tokens: Int?
    public var cache_creation_tokens: Int?
    public var cache_read_tokens: Int?
    public var cache_creation_5m_tokens: Int?
    public var cache_creation_1h_tokens: Int?
    public var input_cost: String?
    public var output_cost: String?
    public var cache_creation_cost: String?
    public var cache_read_cost: String?
    public var total_cost: String?
    public var actual_cost: String?
    public var account_cost: String?
    public var account_stats_cost: String?
    public var account_rate_multiplier: String?
    public var rate_multiplier: String?
    public var first_token_ms: Int?
    public var duration_ms: Int?
    public var user_agent: String?
    public var ip_address: String?
    public var inbound_endpoint: String?
    public var upstream_endpoint: String?
    public var request_id: String?
    public var upstream_request_id: String?
    public var billing_type: Int?
    public var billing_mode: String?
    public var service_tier: String?
    public var image_count: Int?
    public var image_output_tokens: Int?
    public var image_output_cost: String?
    public var image_input_tokens: Int?
    public var image_input_cost: String?
    public var video_count: Int?
    public var video_duration_seconds: Double?
    public var video_resolution: String?

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(Int.self, forKey: .id)
        created_at = try c.decodeIfPresent(String.self, forKey: .created_at) ?? ""
        user_id = try c.decodeIfPresent(Int.self, forKey: .user_id) ?? 0
        user_name = try c.decodeIfPresent(String.self, forKey: .user_name)
        user_email = try c.decodeIfPresent(String.self, forKey: .user_email)
        api_key_id = try c.decodeIfPresent(Int.self, forKey: .api_key_id) ?? 0
        api_key_name = try c.decodeIfPresent(String.self, forKey: .api_key_name)
        account_id = try c.decodeIfPresent(Int.self, forKey: .account_id) ?? 0
        account_name = try c.decodeIfPresent(String.self, forKey: .account_name)
        group_id = try c.decodeIfPresent(Int.self, forKey: .group_id)
        group_name = try c.decodeIfPresent(String.self, forKey: .group_name)
        model = try c.decodeIfPresent(String.self, forKey: .model)
        requested_model = try c.decodeIfPresent(String.self, forKey: .requested_model)
        upstream_model = try c.decodeIfPresent(String.self, forKey: .upstream_model)
        upstream_response_model = try c.decodeIfPresent(String.self, forKey: .upstream_response_model)
        model_mapping_chain = try c.decodeIfPresent(String.self, forKey: .model_mapping_chain)
        upstream_model_mismatch = try c.decodeIfPresent(Bool.self, forKey: .upstream_model_mismatch) ?? false
        reasoning_effort = try c.decodeIfPresent(String.self, forKey: .reasoning_effort)
        requested_reasoning_effort = try c.decodeIfPresent(String.self, forKey: .requested_reasoning_effort)
        request_type = try c.decodeIfPresent(String.self, forKey: .request_type)
        native_request_type = try c.decodeIfPresent(String.self, forKey: .native_request_type)
        stream = try c.decodeIfPresent(Bool.self, forKey: .stream)
        openai_ws_mode = try c.decodeIfPresent(Bool.self, forKey: .openai_ws_mode)
        native_compaction_v2 = try c.decodeIfPresent(Bool.self, forKey: .native_compaction_v2)
        input_tokens = try c.decodeIfPresent(Int.self, forKey: .input_tokens)
        output_tokens = try c.decodeIfPresent(Int.self, forKey: .output_tokens)
        cache_creation_tokens = try c.decodeIfPresent(Int.self, forKey: .cache_creation_tokens)
        cache_read_tokens = try c.decodeIfPresent(Int.self, forKey: .cache_read_tokens)
        cache_creation_5m_tokens = try c.decodeIfPresent(Int.self, forKey: .cache_creation_5m_tokens)
        cache_creation_1h_tokens = try c.decodeIfPresent(Int.self, forKey: .cache_creation_1h_tokens)
        input_cost = try c.decodeIfPresent(String.self, forKey: .input_cost)
        output_cost = try c.decodeIfPresent(String.self, forKey: .output_cost)
        cache_creation_cost = try c.decodeIfPresent(String.self, forKey: .cache_creation_cost)
        cache_read_cost = try c.decodeIfPresent(String.self, forKey: .cache_read_cost)
        total_cost = try c.decodeIfPresent(String.self, forKey: .total_cost)
        actual_cost = try c.decodeIfPresent(String.self, forKey: .actual_cost)
        account_cost = try c.decodeIfPresent(String.self, forKey: .account_cost)
        account_stats_cost = try c.decodeIfPresent(String.self, forKey: .account_stats_cost)
        account_rate_multiplier = try c.decodeIfPresent(String.self, forKey: .account_rate_multiplier)
        rate_multiplier = try c.decodeIfPresent(String.self, forKey: .rate_multiplier)
        first_token_ms = try c.decodeIfPresent(Int.self, forKey: .first_token_ms)
        duration_ms = try c.decodeIfPresent(Int.self, forKey: .duration_ms)
        user_agent = try c.decodeIfPresent(String.self, forKey: .user_agent)
        ip_address = try c.decodeIfPresent(String.self, forKey: .ip_address)
        inbound_endpoint = try c.decodeIfPresent(String.self, forKey: .inbound_endpoint)
        upstream_endpoint = try c.decodeIfPresent(String.self, forKey: .upstream_endpoint)
        request_id = try c.decodeIfPresent(String.self, forKey: .request_id)
        upstream_request_id = try c.decodeIfPresent(String.self, forKey: .upstream_request_id)
        billing_type = try c.decodeIfPresent(Int.self, forKey: .billing_type)
        billing_mode = try c.decodeIfPresent(String.self, forKey: .billing_mode)
        service_tier = try c.decodeIfPresent(String.self, forKey: .service_tier)
        image_count = try c.decodeIfPresent(Int.self, forKey: .image_count)
        image_output_tokens = try c.decodeIfPresent(Int.self, forKey: .image_output_tokens)
        image_output_cost = try c.decodeIfPresent(String.self, forKey: .image_output_cost)
        image_input_tokens = try c.decodeIfPresent(Int.self, forKey: .image_input_tokens)
        image_input_cost = try c.decodeIfPresent(String.self, forKey: .image_input_cost)
        video_count = try c.decodeIfPresent(Int.self, forKey: .video_count)
        video_duration_seconds = try c.decodeIfPresent(Double.self, forKey: .video_duration_seconds)
        video_resolution = try c.decodeIfPresent(String.self, forKey: .video_resolution)
    }
}

public struct RecordSummaryEnvelope: Codable, Sendable, Equatable {
    public var actual_cost: String
}

public struct RecordPageEnvelope: Codable, Sendable, Equatable {
    public var items: [UsageRecord]
    public var next_cursor: String?
    public var latest_id: Int
    public var observed_at: String
    public var summary: RecordSummaryEnvelope?
}

public struct RecordNewCountEnvelope: Codable, Sendable, Equatable {
    public var new_count: Int
}

public struct RecordOption: Codable, Sendable, Equatable, Identifiable {
    public var id: Int
    public var name: String?
    public var email: String?
    public var user_id: Int?
    public var user_name: String?
    public var user_email: String?
    public var status: String?
    public var platform: String?
    public var deleted: Bool

    public init(id: Int, name: String?, status: String? = nil, platform: String? = nil, deleted: Bool = false) {
        self.id = id
        self.name = name
        self.status = status
        self.platform = platform
        self.deleted = deleted
    }
}

public struct RecordOptionPage: Codable, Sendable, Equatable {
    public var items: [RecordOption]
    public var next_cursor: String?
}

public enum RecordColumnSpec: String, CaseIterable, Sendable {
    case api_key
    case account
    case model
    case type
    case reasoning
    case tokens
    case cost
    case latency
    case user_agent
    case ip
    case endpoint
    case group
    case billing
    case request_id
    case upstream_id

    public var label: String {
        switch self {
        case .api_key: return "API 密钥"
        case .account: return "账户"
        case .model: return "模型"
        case .type: return "类型"
        case .reasoning: return "推理强度"
        case .tokens: return "Token"
        case .cost: return "费用"
        case .latency: return "延迟"
        case .user_agent: return "User-Agent"
        case .ip: return "IP"
        case .endpoint: return "端点"
        case .group: return "分组"
        case .billing: return "计费模式"
        case .request_id: return "请求 ID"
        case .upstream_id: return "上游 ID"
        }
    }

    public var width: CGFloat {
        switch self {
        case .api_key: return 80
        case .account: return 80
        case .model: return 116
        case .type: return 64
        case .reasoning: return 52
        case .tokens: return 118
        case .cost: return 86
        case .latency: return 114
        case .user_agent: return 108
        case .ip: return 100
        case .endpoint: return 112
        case .group: return 76
        case .billing: return 48
        case .request_id: return 122
        case .upstream_id: return 122
        }
    }

    public static let userColumnWidth: CGFloat = 110
    public static let timeColumnWidth: CGFloat = 88
    public static let columnSpacing: CGFloat = 8
    public static let cellPadding: CGFloat = 4

    /// Minimum width of the horizontally scrolling pane: the time column plus `columns`.
    public static func dataWidth(columns: [RecordColumnSpec]) -> CGFloat {
        let widths = timeColumnWidth + columns.reduce(CGFloat(0)) { $0 + $1.width }
        return widths + CGFloat(columns.count) * columnSpacing + cellPadding * 2
    }

    public static let defaultKeys: [String] = [
        "api_key", "account", "model", "type", "reasoning",
        "tokens", "cost", "latency", "user_agent", "ip",
    ]

    public static func normalize(_ columns: [String]?) -> [RecordColumnSpec] {
        guard let columns else {
            return defaultKeys.compactMap(RecordColumnSpec.init(rawValue:))
        }
        return columns.compactMap(RecordColumnSpec.init(rawValue:))
    }
}

/// Scroll-to-load rule of the records table, matching the 200px root margin the web
/// front end gives its pagination observer.
public enum RecordPagination {
    public static let lookahead: CGFloat = 200

    /// `footerMinY` is the top of the pagination row measured from the top of the viewport.
    public static func footerNear(footerMinY: CGFloat, viewportHeight: CGFloat) -> Bool {
        footerMinY <= viewportHeight + lookahead
    }

    public static func autoLoads(footerNear: Bool, active: Bool, detailOpen: Bool) -> Bool {
        footerNear && active && !detailOpen
    }
}

public struct ModelRouteStep: Sendable, Equatable {
    public var model: String
    public var labels: [String]
}

public enum RecordFormatting {
    public static func requestTypeKey(_ row: UsageRecord) -> (key: String, label: String) {
        let map: [String: String] = [
            "sync": "同步", "stream": "流式", "ws_v2": "WebSocket",
            "cyber": "安全策略", "live": "Live", "unknown": "未知",
        ]
        for candidate in [row.native_request_type, row.request_type].compactMap({ $0 }) {
            if let text = map[candidate] { return (candidate, text) }
        }
        if row.openai_ws_mode == true { return ("ws_v2", "WebSocket") }
        if row.stream == true { return ("stream", "流式") }
        return ("sync", "同步")
    }

    public static func modelRoute(_ row: UsageRecord) -> [ModelRouteStep] {
        var steps: [ModelRouteStep] = []
        func add(_ raw: String?, label: String) {
            guard let trimmed = raw?.trimmingCharacters(in: .whitespacesAndNewlines), !trimmed.isEmpty else { return }
            if let lastIdx = steps.indices.last, steps[lastIdx].model == trimmed {
                if !steps[lastIdx].labels.contains(label) {
                    steps[lastIdx].labels.append(label)
                }
            } else {
                steps.append(ModelRouteStep(model: trimmed, labels: [label]))
            }
        }
        add(row.requested_model ?? row.model, label: "请求")
        if let chain = row.model_mapping_chain {
            let normalized = chain.replacingOccurrences(of: "->", with: "→")
            for part in normalized.split(separator: "→") {
                add(String(part), label: "映射")
            }
        }
        add(row.upstream_model ?? row.requested_model ?? row.model, label: "转发")
        add(row.upstream_response_model, label: "返回")
        return steps
    }

    /// Exact Decimal rounding matching `desktop/src/records.ts` `decimal()` (including scientific notation).
    public static func decimal(_ raw: String?, digits: Int) -> String {
        guard let raw = raw?.trimmingCharacters(in: .whitespacesAndNewlines), !raw.isEmpty,
              let dec = Decimal(string: raw, locale: Locale(identifier: "en_US_POSIX")),
              !dec.isNaN
        else {
            return "—"
        }
        var input = dec
        var rounded = Decimal()
        NSDecimalRound(&rounded, &input, digits, .plain)
        let formatter = NumberFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.numberStyle = .decimal
        formatter.usesGroupingSeparator = false
        formatter.minimumIntegerDigits = 1
        formatter.minimumFractionDigits = digits
        formatter.maximumFractionDigits = digits
        return formatter.string(from: rounded as NSDecimalNumber) ?? "—"
    }

    public static func money(_ raw: String?, digits: Int = 6) -> String {
        let formatted = decimal(raw, digits: digits)
        return formatted == "—" ? "—" : "$\(formatted)"
    }

    public static func tokenCount(_ val: Int?) -> String {
        guard let val, val >= 0 else { return "—" }
        let formatter = NumberFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.numberStyle = .decimal
        return formatter.string(from: NSNumber(value: val)) ?? "\(val)"
    }

    public static func cacheTokenCount(_ val: Int?) -> String {
        guard let val, val >= 0 else { return "—" }
        if val >= 1_000_000 { return String(format: "%.1fM", Double(val) / 1_000_000.0) }
        if val >= 1_000 { return String(format: "%.1fK", Double(val) / 1_000.0) }
        return tokenCount(val)
    }

    public static func latency(_ ms: Int?) -> String {
        guard let ms, ms >= 0 else { return "—" }
        return String(format: "%.2fs", Double(ms) / 1000.0)
    }

    public enum LatencyLevel: Sendable {
        case unknown, good, warn, slow, critical
    }

    public static func latencyTone(_ ms: Int?, firstToken: Bool) -> LatencyLevel {
        guard let ms, ms >= 0 else { return .unknown }
        let thresholds = firstToken ? (10_000, 30_000, 60_000) : (60_000, 180_000, 300_000)
        if ms >= thresholds.2 { return .critical }
        if ms >= thresholds.1 { return .slow }
        if ms >= thresholds.0 { return .warn }
        return .good
    }

    public static func outputRate(_ row: UsageRecord) -> String {
        let kind = row.native_request_type ?? row.request_type
        if (row.image_count ?? 0) > 0 || (row.image_output_tokens ?? 0) > 0 || row.billing_mode == "image" {
            return "—"
        }
        if let kind, !["sync", "stream", "ws_v2", "cyber"].contains(kind) {
            return "—"
        }
        guard let out = row.output_tokens, out > 0, let total = row.duration_ms, total > 0 else {
            return "—"
        }
        return String(format: "%.1f tok/s", Double(out) * 1000.0 / Double(total))
    }
}
