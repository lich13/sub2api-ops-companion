import Foundation

// MARK: - Beijing Time Formatting (`MM-DD HH:mm:ss`)

public enum BeijingTime {
    private static let shanghaiOffsetSeconds: Int64 = 8 * 3600

    private enum FastParseOutcome {
        case parsed(Int64, Double)
        case invalid
        case unsupported
    }

    public static func parseISO(_ raw: String?) -> Date? {
        guard let raw else { return nil }
        switch parseFastEpoch(raw) {
        case .parsed(let secs, let frac):
            return Date(timeIntervalSince1970: Double(secs) + frac)
        case .invalid:
            return nil
        case .unsupported:
            break
        }
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, trimmed.contains(":") else {
            return nil
        }
        let iso = trimmed.contains("T")
            ? trimmed
            : trimmed.replacingOccurrences(of: " ", with: "T", options: [], range: trimmed.range(of: " "))
        return (try? Date.ISO8601FormatStyle(includingFractionalSeconds: true).parse(iso))
            ?? (try? Date.ISO8601FormatStyle().parse(iso))
    }

    public static func fullTime(_ raw: String?) -> String {
        guard let raw, !raw.isEmpty else {
            return "暂无记录"
        }
        switch parseFastEpoch(raw) {
        case .parsed(let secs, _):
            return unixSeconds(secs)
        case .invalid:
            return "时间未知"
        case .unsupported:
            guard let date = parseISO(raw) else { return "时间未知" }
            return format(date)
        }
    }

    private static let minSupportedEpochSeconds: Int64 = -62_135_596_800 // 0001-01-01T00:00:00Z
    private static let maxSupportedEpochSeconds: Int64 = 253_402_300_799 // 9999-12-31T23:59:59Z

    @inline(__always)
    private static func clampedWholeSeconds(_ seconds: Double) -> Int64 {
        guard seconds.isFinite else { return 0 }
        let floored = floor(seconds)
        if floored <= Double(minSupportedEpochSeconds) { return minSupportedEpochSeconds }
        if floored >= Double(maxSupportedEpochSeconds) { return maxSupportedEpochSeconds }
        return Int64(floored)
    }

    public static func format(_ date: Date) -> String {
        unixSeconds(clampedWholeSeconds(date.timeIntervalSince1970))
    }

    public static func unixSeconds(_ wholeSeconds: Int64) -> String {
        let (_, month, day, hour, minute, second) = decomposeUTC(wholeSeconds + shanghaiOffsetSeconds)
        let buf: [UInt8] = [
            twoDigitTens(month), twoDigitOnes(month), 0x2D,
            twoDigitTens(day), twoDigitOnes(day), 0x20,
            twoDigitTens(hour), twoDigitOnes(hour), 0x3A,
            twoDigitTens(minute), twoDigitOnes(minute), 0x3A,
            twoDigitTens(second), twoDigitOnes(second),
        ]
        return buf.withUnsafeBufferPointer { ptr in
            String(decoding: ptr, as: UTF8.self)
        }
    }

    public static func callTimestamp(_ raw: String?) -> Double {
        guard let raw, !raw.isEmpty else { return -.infinity }
        switch parseFastEpoch(raw) {
        case .parsed(let secs, let frac):
            return Double(secs) + frac
        case .invalid:
            return -.infinity
        case .unsupported:
            guard let date = parseISO(raw) else { return -.infinity }
            return date.timeIntervalSince1970
        }
    }

    public static func isoUTCString(from date: Date) -> String {
        isoUTCString(fromEpochSeconds: date.timeIntervalSince1970)
    }

    public static func isoUTCString(fromEpochSeconds seconds: Double) -> String {
        let whole = clampedWholeSeconds(seconds)
        let (year, month, day, hour, minute, second) = decomposeUTC(whole)
        let y0 = UInt8(0x30 + ((year / 1000) % 10))
        let y1 = UInt8(0x30 + ((year / 100) % 10))
        let y2 = UInt8(0x30 + ((year / 10) % 10))
        let y3 = UInt8(0x30 + (year % 10))
        let buf: [UInt8] = [
            y0, y1, y2, y3, 0x2D,
            twoDigitTens(month), twoDigitOnes(month), 0x2D,
            twoDigitTens(day), twoDigitOnes(day), 0x54,
            twoDigitTens(hour), twoDigitOnes(hour), 0x3A,
            twoDigitTens(minute), twoDigitOnes(minute), 0x3A,
            twoDigitTens(second), twoDigitOnes(second), 0x5A,
        ]
        return buf.withUnsafeBufferPointer { String(decoding: $0, as: UTF8.self) }
    }

    public static func formatBeijingDateTime(_ date: Date, includeSeconds: Bool = true) -> String {
        let whole = clampedWholeSeconds(date.timeIntervalSince1970) + shanghaiOffsetSeconds
        let (year, month, day, hour, minute, second) = decomposeUTC(whole)
        let y0 = UInt8(0x30 + ((year / 1000) % 10))
        let y1 = UInt8(0x30 + ((year / 100) % 10))
        let y2 = UInt8(0x30 + ((year / 10) % 10))
        let y3 = UInt8(0x30 + (year % 10))
        if includeSeconds {
            let buf: [UInt8] = [
                y0, y1, y2, y3, 0x2D,
                twoDigitTens(month), twoDigitOnes(month), 0x2D,
                twoDigitTens(day), twoDigitOnes(day), 0x20,
                twoDigitTens(hour), twoDigitOnes(hour), 0x3A,
                twoDigitTens(minute), twoDigitOnes(minute), 0x3A,
                twoDigitTens(second), twoDigitOnes(second),
            ]
            return buf.withUnsafeBufferPointer { String(decoding: $0, as: UTF8.self) }
        }
        let buf: [UInt8] = [
            y0, y1, y2, y3, 0x2D,
            twoDigitTens(month), twoDigitOnes(month), 0x2D,
            twoDigitTens(day), twoDigitOnes(day), 0x20,
            twoDigitTens(hour), twoDigitOnes(hour), 0x3A,
            twoDigitTens(minute), twoDigitOnes(minute),
        ]
        return buf.withUnsafeBufferPointer { String(decoding: $0, as: UTF8.self) }
    }

    /// Parses `YYYY-MM-DD HH:mm[:ss]` or `YYYY-MM-DDTHH:mm[:ss]` in `Asia/Shanghai` (UTC+8).
    public static func parseBeijingLocal(_ raw: String, requireSeconds: Bool = false) -> Date? {
        var utf8 = raw
        return utf8.withUTF8 { ptr in
            var start = 0
            var end = ptr.count
            while start < end && isASCIIWhitespace(ptr[start]) { start += 1 }
            while end > start && isASCIIWhitespace(ptr[end - 1]) { end -= 1 }
            let len = end - start
            guard len == 19 || (!requireSeconds && len == 16) else { return nil }
            guard let year = parse4Digits(ptr, start),
                  year >= 1970,
                  ptr[start + 4] == 0x2D,
                  let month = parse2Digits(ptr, start + 5),
                  ptr[start + 7] == 0x2D,
                  let day = parse2Digits(ptr, start + 8),
                  isValidDate(year: year, month: month, day: day),
                  (ptr[start + 10] == 0x20 || ptr[start + 10] == 0x54 || ptr[start + 10] == 0x74),
                  let hour = parse2Digits(ptr, start + 11), (0...23).contains(hour),
                  ptr[start + 13] == 0x3A,
                  let minute = parse2Digits(ptr, start + 14), (0...59).contains(minute)
            else { return nil }
            let second: Int
            if len == 19 {
                guard ptr[start + 16] == 0x3A,
                      let sec = parse2Digits(ptr, start + 17), (0...59).contains(sec)
                else { return nil }
                second = sec
            } else {
                second = 0
            }
            let days = daysFromCivil(year: year, month: month, day: day)
            let localSeconds = days * 86_400 + Int64(hour * 3600 + minute * 60 + second)
            return Date(timeIntervalSince1970: Double(localSeconds - shanghaiOffsetSeconds))
        }
    }

    @inline(__always)
    private static func twoDigitTens(_ v: Int) -> UInt8 { UInt8(0x30 + ((v / 10) % 10)) }
    @inline(__always)
    private static func twoDigitOnes(_ v: Int) -> UInt8 { UInt8(0x30 + (v % 10)) }
    @inline(__always)
    private static func isASCIIWhitespace(_ b: UInt8) -> Bool {
        b == 0x20 || b == 0x09 || b == 0x0A || b == 0x0D
    }

    @inline(__always)
    private static func parse2Digits(_ ptr: UnsafeBufferPointer<UInt8>, _ offset: Int) -> Int? {
        let d0 = Int(ptr[offset]) &- 0x30
        let d1 = Int(ptr[offset + 1]) &- 0x30
        guard (0...9).contains(d0), (0...9).contains(d1) else { return nil }
        return d0 * 10 + d1
    }

    @inline(__always)
    private static func parse4Digits(_ ptr: UnsafeBufferPointer<UInt8>, _ offset: Int) -> Int? {
        guard let hi = parse2Digits(ptr, offset), let lo = parse2Digits(ptr, offset + 2) else { return nil }
        return hi * 100 + lo
    }

    @inline(__always)
    private static func isValidDate(year: Int, month: Int, day: Int) -> Bool {
        guard (1...12).contains(month), day >= 1 else { return false }
        let maxDay: Int
        switch month {
        case 1, 3, 5, 7, 8, 10, 12: maxDay = 31
        case 4, 6, 9, 11: maxDay = 30
        case 2:
            let leap = (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
            maxDay = leap ? 29 : 28
        default: return false
        }
        return day <= maxDay
    }

    @inline(__always)
    private static func daysFromCivil(year: Int, month: Int, day: Int) -> Int64 {
        let y = year - (month <= 2 ? 1 : 0)
        let era = (y >= 0 ? y : y - 399) / 400
        let yoe = y - era * 400
        let doy = (153 * (month + (month > 2 ? -3 : 9)) + 2) / 5 + day - 1
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy
        return Int64(era) * 146_097 + Int64(doe) - 719_468
    }

    @inline(__always)
    private static func decomposeUTC(_ epochSeconds: Int64) -> (Int, Int, Int, Int, Int, Int) {
        var days = epochSeconds / 86_400
        var rem = epochSeconds % 86_400
        if rem < 0 {
            rem += 86_400
            days -= 1
        }
        let z = days + 719_468
        let era = (z >= 0 ? z : z - 146_096) / 146_097
        let doe = z - era * 146_097
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365
        let y = yoe + era * 400
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100)
        let mp = (5 * doy + 2) / 153
        let day = Int(doy - (153 * mp + 2) / 5 + 1)
        let month = Int(mp + (mp < 10 ? 3 : -9))
        let year = Int(y) + (month <= 2 ? 1 : 0)
        let hour = Int(rem / 3600)
        let minute = Int((rem % 3600) / 60)
        let second = Int(rem % 60)
        return (year, month, day, hour, minute, second)
    }

    private static func parseFastEpoch(_ raw: String) -> FastParseOutcome {
        var str = raw
        return str.withUTF8 { ptr in
            var start = 0
            var end = ptr.count
            while start < end && isASCIIWhitespace(ptr[start]) { start += 1 }
            while end > start && isASCIIWhitespace(ptr[end - 1]) { end -= 1 }
            guard end - start >= 20 else {
                return (end - start > 0 && ptr[start] >= 0x30 && ptr[start] <= 0x39) ? .unsupported : .invalid
            }
            guard let year = parse4Digits(ptr, start),
                  ptr[start + 4] == 0x2D,
                  let month = parse2Digits(ptr, start + 5),
                  ptr[start + 7] == 0x2D,
                  let day = parse2Digits(ptr, start + 8)
            else { return .unsupported }

            guard (1...9999).contains(year),
                  isValidDate(year: year, month: month, day: day),
                  (ptr[start + 10] == 0x54 || ptr[start + 10] == 0x74 || ptr[start + 10] == 0x20),
                  let hour = parse2Digits(ptr, start + 11), (0...23).contains(hour),
                  ptr[start + 13] == 0x3A,
                  let minute = parse2Digits(ptr, start + 14), (0...59).contains(minute),
                  ptr[start + 16] == 0x3A,
                  let second = parse2Digits(ptr, start + 17), (0...60).contains(second)
            else { return .invalid }

            var idx = start + 19
            var frac = 0.0
            if idx < end && (ptr[idx] == 0x2E || ptr[idx] == 0x2C) {
                idx += 1
                let fracStart = idx
                var nanos: Int64 = 0
                var scale: Double = 1.0
                var digits = 0
                while idx < end {
                    let d = Int(ptr[idx]) &- 0x30
                    guard (0...9).contains(d) else { break }
                    if digits < 9 {
                        nanos = nanos * 10 + Int64(d)
                        scale *= 0.1
                        digits += 1
                    }
                    idx += 1
                }
                guard idx > fracStart else { return .invalid }
                frac = Double(nanos) * scale
            }

            guard idx < end else { return .invalid }
            let tzSignByte = ptr[idx]
            let tzOffsetSeconds: Int64
            if tzSignByte == 0x5A || tzSignByte == 0x7A {
                guard idx + 1 == end else { return .invalid }
                tzOffsetSeconds = 0
            } else if tzSignByte == 0x2B || tzSignByte == 0x2D {
                let sign: Int64 = (tzSignByte == 0x2D) ? -1 : 1
                let rem = end - (idx + 1)
                let tzHour: Int
                let tzMin: Int
                if rem == 5 {
                    guard ptr[idx + 3] == 0x3A,
                          let h = parse2Digits(ptr, idx + 1),
                          let m = parse2Digits(ptr, idx + 4)
                    else { return .invalid }
                    tzHour = h
                    tzMin = m
                } else if rem == 4 {
                    guard let h = parse2Digits(ptr, idx + 1),
                          let m = parse2Digits(ptr, idx + 3)
                    else { return .invalid }
                    tzHour = h
                    tzMin = m
                } else if rem == 2 {
                    guard let h = parse2Digits(ptr, idx + 1) else { return .invalid }
                    tzHour = h
                    tzMin = 0
                } else {
                    return .invalid
                }
                guard (0...23).contains(tzHour), (0...59).contains(tzMin) else { return .invalid }
                tzOffsetSeconds = sign * Int64(tzHour * 3600 + tzMin * 60)
            } else {
                return .unsupported
            }

            let clampedSec = min(second, 59)
            let days = daysFromCivil(year: year, month: month, day: day)
            let utcSeconds = days * 86_400 + Int64(hour * 3600 + minute * 60 + clampedSec) - tzOffsetSeconds
            return .parsed(utcSeconds, frac)
        }
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
