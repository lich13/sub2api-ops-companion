import Foundation

public enum DatePresetKey: String, CaseIterable, Sendable {
    case today = "today"
    case yesterday = "yesterday"
    case hours24 = "24h"
    case days7 = "7d"
    case days14 = "14d"
    case days30 = "30d"
    case month = "month"
    case lastMonth = "lastMonth"

    public var label: String {
        switch self {
        case .today: return "今天"
        case .yesterday: return "昨天"
        case .hours24: return "近24小时"
        case .days7: return "近7天"
        case .days14: return "近14天"
        case .days30: return "近30天"
        case .month: return "本月"
        case .lastMonth: return "上月"
        }
    }
}

public struct RecordDateRange: Sendable, Equatable {
    public var preset: DatePresetKey?
    public var start: String
    public var end: String

    mutating func refreshPreset(at now: Date, holding: Bool) -> Bool {
        guard let preset else { return false }
        let current = BeijingDateLogic.presetRange(preset, now: now)
        guard current != self else { return false }
        if holding { return true }
        self = current
        return false
    }

    public init(preset: DatePresetKey?, start: String, end: String) {
        self.preset = preset
        self.start = start
        self.end = end
    }
}

public enum BeijingDateLogic {
    private static let utcZone = TimeZone(secondsFromGMT: 0)!

    public static func beijingDate(now: Date = Date()) -> String {
        let shifted = now.addingTimeInterval(8 * 3600)
        var cal = Calendar(identifier: .gregorian)
        cal.timeZone = utcZone
        let c = cal.dateComponents([.year, .month, .day], from: shifted)
        return String(format: "%04d-%02d-%02d", c.year ?? 2026, c.month ?? 1, c.day ?? 1)
    }

    public static func shiftDate(_ value: String, days: Int) -> String {
        guard let base = parseYMD(value) else { return value }
        let next = base.addingTimeInterval(Double(days) * 86_400)
        var cal = Calendar(identifier: .gregorian)
        cal.timeZone = utcZone
        let c = cal.dateComponents([.year, .month, .day], from: next)
        return String(format: "%04d-%02d-%02d", c.year ?? 2026, c.month ?? 1, c.day ?? 1)
    }

    public static func parseYMD(_ raw: String) -> Date? {
        let parts = raw.split(separator: "-")
        guard parts.count == 3,
              parts[0].count == 4, parts[1].count == 2, parts[2].count == 2,
              let y = Int(parts[0]), let m = Int(parts[1]), let d = Int(parts[2])
        else { return nil }
        var cal = Calendar(identifier: .gregorian)
        cal.timeZone = utcZone
        var comps = DateComponents()
        comps.year = y
        comps.month = m
        comps.day = d
        guard let date = cal.date(from: comps) else { return nil }
        let check = cal.dateComponents([.year, .month, .day], from: date)
        guard check.year == y && check.month == m && check.day == d else { return nil }
        return date
    }

    public static func isValidRange(start: String, end: String) -> Bool {
        guard parseYMD(start) != nil, parseYMD(end) != nil else { return false }
        return start <= end && end < "9999-12-31"
    }

    public static func presetRange(_ preset: DatePresetKey, now: Date = Date()) -> RecordDateRange {
        let today = beijingDate(now: now)
        var start = today
        var end = today
        switch preset {
        case .today:
            break
        case .yesterday:
            let y = shiftDate(today, days: -1)
            start = y
            end = y
        case .hours24:
            start = shiftDate(today, days: -1)
        case .days7:
            start = shiftDate(today, days: -6)
        case .days14:
            start = shiftDate(today, days: -13)
        case .days30:
            start = shiftDate(today, days: -29)
        case .month:
            start = "\(today.prefix(7))-01"
        case .lastMonth:
            let prevEnd = shiftDate("\(today.prefix(7))-01", days: -1)
            end = prevEnd
            start = "\(prevEnd.prefix(7))-01"
        }
        return RecordDateRange(preset: preset, start: start, end: end)
    }

    public static func rangeLabel(_ value: RecordDateRange) -> String {
        if let preset = value.preset {
            return preset.label
        }
        return value.start == value.end ? value.start : "\(value.start) — \(value.end)"
    }
}
