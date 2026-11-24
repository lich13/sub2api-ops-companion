import Foundation

public enum ModelTestEngine: String, CaseIterable, Codable, Sendable {
    case modeltrace
    case lm_detector

    public var displayName: String {
        Self.name(for: rawValue)
    }

    public static func name(for raw: String?) -> String {
        switch raw {
        case "lm_detector": return "LM Detector"
        default: return "ModelTrace"
        }
    }
}

public struct FingerprintBankVersion: Codable, Sendable, Equatable {
    public var revision: String
    public var sha256: String?
    public var built_at: String?
    public var analyzer_version: String?

    enum CodingKeys: String, CodingKey {
        case revision, sha256, built_at, analyzer_version
    }

    public init(revision: String = "", sha256: String? = nil, built_at: String? = nil, analyzer_version: String? = nil) {
        self.revision = revision
        self.sha256 = sha256
        self.built_at = built_at
        self.analyzer_version = analyzer_version
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        revision = try c.decodeIfPresent(String.self, forKey: .revision) ?? ""
        sha256 = try c.decodeIfPresent(String.self, forKey: .sha256)
        built_at = try c.decodeIfPresent(String.self, forKey: .built_at)
        if try (!c.contains(.analyzer_version) || c.decodeNil(forKey: .analyzer_version)) {
            analyzer_version = nil
        } else if let s = try? c.decode(String.self, forKey: .analyzer_version) {
            analyzer_version = s
        } else {
            analyzer_version = String(try c.decode(Int.self, forKey: .analyzer_version))
        }
    }
}

public struct FingerprintBankStatusInfo: Codable, Sendable, Equatable {
    public var version: FingerprintBankVersion?
    public var source: String?
    public var status: String?
    public var result: String?
    public var checked_at: String?
    public var synced_at: String?
    public var cooldown_until: String?
    public var error: String?

    public var available: Bool {
        !(version?.revision.isEmpty ?? true)
    }

    enum CodingKeys: String, CodingKey {
        case version, source, status, result, checked_at, synced_at, cooldown_until, error
    }

    private static func decodeFlexibleTimestamp(_ c: KeyedDecodingContainer<CodingKeys>, key: CodingKeys) throws -> String? {
        if try (!c.contains(key) || c.decodeNil(forKey: key)) {
            return nil
        }
        if let s = try? c.decode(String.self, forKey: key) {
            return s.isEmpty ? nil : s
        }
        let secs = try c.decode(Double.self, forKey: key)
        guard secs.isFinite, secs > 0, secs <= 253_402_300_799 else { return nil }
        return BeijingTime.isoUTCString(fromEpochSeconds: secs)
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        version = try c.decodeIfPresent(FingerprintBankVersion.self, forKey: .version)
        source = try c.decodeIfPresent(String.self, forKey: .source)
        status = try c.decodeIfPresent(String.self, forKey: .status)
        result = try c.decodeIfPresent(String.self, forKey: .result)
        checked_at = try Self.decodeFlexibleTimestamp(c, key: .checked_at)
        synced_at = try Self.decodeFlexibleTimestamp(c, key: .synced_at)
        cooldown_until = try Self.decodeFlexibleTimestamp(c, key: .cooldown_until)
        error = try c.decodeIfPresent(String.self, forKey: .error)
    }
}

extension ModelTestGroupStatus: Identifiable {
    public var id: Int { index }
}

extension ModelTestJob: Identifiable {
    public var isTerminal: Bool {
        ["completed", "failed", "cancelled", "interrupted", "needs_confirmation"].contains(status)
    }
}
