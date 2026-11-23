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

// MARK: - Core Domain DTOs (`Protocol.swift` Single Source of Truth)

public typealias DegradationMark = DegradationMarkRecord
public typealias AccountKeepalive = KeepaliveReply
public typealias ModelDetectionLastReport = ModelDetectionLastReportRecord
public typealias ModelDetectionLastResult = ModelDetectionLastResultRecord
public typealias ModelDetectionSummary = ModelDetectionSummaryRecord
public typealias AccountBlocker = AccountBlockerRecord
public typealias SettingCapability = SettingCapabilityRecord
public typealias RecoveryMonitorState = RecoveryMonitorStateRecord
public typealias AutoResetCreditState = AutoResetCreditStateRecord

extension DegradationMarkRecord {
    public init(marked: Bool? = nil, marked_at: String? = nil, version: String? = nil) {
        self.init(marked: marked, marked_at: marked_at, version: version, error: nil)
    }
}

extension AccountBlockerRecord {
    public init(code: String, label: String) {
        self.init(code: code, label: label, until: nil)
    }
}
