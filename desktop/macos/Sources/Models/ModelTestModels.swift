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
        guard secs > 0 else { return nil }
        let iso = ISO8601DateFormatter()
        iso.formatOptions = [.withInternetDateTime]
        return iso.string(from: Date(timeIntervalSince1970: secs))
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
        if try (!c.contains(.error) || c.decodeNil(forKey: .error)) {
            error = nil
        } else if let s = try? c.decode(String.self, forKey: .error) {
            error = s
        } else {
            let obj = try c.decode([String: JSONValue].self, forKey: .error)
            error = obj["message"]?.stringValue
        }
    }
}

public struct ModelFingerprintCandidateScore: Codable, Sendable, Equatable {
    public var model: String
    public var display_name: String?
    public var probability: Double?
    public var score: Double?
}

public struct ModelFingerprintReport: Codable, Sendable, Equatable {
    public var engine: String?
    public var prediction: String?
    public var prediction_name: String?
    public var first_prediction: String?
    public var probability: Double?
    public var first_probability: Double?
    public var decision: String?
    public var merged_class: Bool?
    public var ranking_score: Double?
    public var used_outputs: Int?
    public var sample_count: Int?
    public var results: [ModelFingerprintCandidateScore]?
    public var bank_version: FingerprintBankVersion?
}

public struct ModelTestClassification: Codable, Sendable, Equatable {
    public var outcome: String
    public var reason: String?
    public var basis: String?
    public var group_indexes: [Int]?
}

public struct ModelTestGroupStatus: Codable, Sendable, Equatable, Identifiable {
    public var index: Int
    public var status: String
    public var attempts: Int?
    public var ttft_ms: Double?
    public var duration_ms: Double?
    public var error: String?

    public var id: Int { index }
}

public struct ModelTestJob: Codable, Sendable, Equatable, Identifiable {
    public var id: String
    public var account_id: Int?
    public var account_name: String?
    public var model_id: String?
    public var requested_model: String?
    public var forwarded_model: String?
    public var returned_models: [String]?
    public var engine: String?
    public var sample_count: Int?
    public var planned_groups: Int?
    public var completed_groups: Int?
    public var valid_groups: Int?
    public var concurrency: Int?
    public var attempts: Int?
    public var duration_ms: Double?
    public var phase: String?
    public var completion_reason: String?
    public var verdict_mode: String?
    public var degraded_model_id: String?
    public var can_retry: Bool?
    public var status: String
    public var reason: String?
    public var error: String?
    public var created_at: String?
    public var completed_at: String?
    public var groups: [ModelTestGroupStatus]?
    public var report: ModelFingerprintReport?
    public var classification: ModelTestClassification?
    public var bank_version: FingerprintBankVersion?

    public var isTerminal: Bool {
        ["completed", "failed", "cancelled", "interrupted", "needs_confirmation"].contains(status)
    }
}
