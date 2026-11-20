import Foundation

// MARK: - Beijing Time Formatting (`MM-DD HH:mm:ss`)

public enum BeijingTime {
    private static let shanghaiZone = TimeZone(identifier: "Asia/Shanghai") ?? TimeZone(secondsFromGMT: 8 * 3600)!

    public static func parseISO(_ raw: String?) -> Date? {
        guard let raw = raw?.trimmingCharacters(in: .whitespacesAndNewlines), !raw.isEmpty else {
            return nil
        }
        let iso = raw.contains("T") ? raw : raw.replacingOccurrences(of: " ", with: "T", options: [], range: raw.range(of: " "))
        return (try? Date.ISO8601FormatStyle(includingFractionalSeconds: true).parse(iso))
            ?? (try? Date.ISO8601FormatStyle().parse(iso))
    }

    public static func fullTime(_ raw: String?) -> String {
        guard let raw = raw, !raw.isEmpty else {
            return "暂无记录"
        }
        guard let date = parseISO(raw) else {
            return "时间未知"
        }
        return format(date)
    }

    public static func format(_ date: Date) -> String {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = shanghaiZone
        let comps = calendar.dateComponents([.month, .day, .hour, .minute, .second], from: date)
        return String(
            format: "%02d-%02d %02d:%02d:%02d",
            comps.month ?? 1,
            comps.day ?? 1,
            comps.hour ?? 0,
            comps.minute ?? 0,
            comps.second ?? 0
        )
    }

    public static func callTimestamp(_ raw: String?) -> Double {
        guard let date = parseISO(raw) else { return -.infinity }
        return date.timeIntervalSince1970
    }
}

// MARK: - Core Domain DTOs (`desktop/src/types.ts`)

public struct DegradationMark: Codable, Sendable, Equatable {
    public var marked: Bool?
    public var marked_at: String?
    public var version: String?
    public var error: String?

    public init(marked: Bool? = nil, marked_at: String? = nil, version: String? = nil, error: String? = nil) {
        self.marked = marked
        self.marked_at = marked_at
        self.version = version
        self.error = error
    }
}

public struct AccountKeepalive: Codable, Sendable, Equatable {
    public var account_id: Int
    public var enabled: Bool
    public var generation: Int
    public var version: String
    public var status: String
    public var reason: String
    public var paused_reason: String
    public var last_restore_at: String?
    public var restore_count: Int
    public var budget_used: Int
    public var next_allowed_at: String?
}

public struct ModelDetectionLastReport: Codable, Sendable, Equatable {
    public var prediction_name: String?
}

public struct ModelDetectionLastResult: Codable, Sendable, Equatable {
    public var status: String?
    public var report: ModelDetectionLastReport?
    public var completed_at: String?
}

public struct ModelDetectionSummary: Codable, Sendable, Equatable {
    public var schedule_mode: String?
    public var min_interval_minutes: Int?
    public var effective_interval_minutes: Int?
    public var recent_request_count: Int?
    public var last_automatic_started_at: String?
    public var enabled: Bool
    public var interval_minutes: Int
    public var model_id: String
    public var version: String
    public var next_at: String?
    public var status: String
    public var reason: String?
    public var last_result: ModelDetectionLastResult?
}

public struct AccountBlocker: Codable, Sendable, Equatable {
    public var code: String
    public var label: String
    public var until: String?

    public init(code: String, label: String, until: String? = nil) {
        self.code = code
        self.label = label
        self.until = until
    }
}

public struct SettingCapability: Codable, Sendable, Equatable {
    public var editable: Bool
    public var reason: String

    public init(editable: Bool, reason: String) {
        self.editable = editable
        self.reason = reason
    }
}

public struct RecoveryMonitorState: Codable, Sendable, Equatable {
    public var status: String
    public var label: String
    public var reason: String
    public var next_at: String?
    public var attempt_count: Int?
    public var next_retry_at: String?
}

public struct AutoResetCreditState: Codable, Sendable, Equatable {
    public var trigger_at: String?
    public var consume_deadline_at: String?
    public var verification_deadline_at: String?
    public var completed_at: String?
    public var outcome: String?
    public var notification_status: String?
    public var notification_attempts: Int?
    public var evidence_source: String?
    public var evidence_at: String?
    public var stage: String
    public var label: String
    public var error: String
    public var next_at: String?
    public var attempt_at: String?
    public var test_completed_at: String?
    public var recovered_at: String?
}
