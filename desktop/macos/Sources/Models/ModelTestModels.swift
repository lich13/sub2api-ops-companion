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
        revision = (try? c.decodeIfPresent(String.self, forKey: .revision)) ?? ""
        sha256 = try? c.decodeIfPresent(String.self, forKey: .sha256)
        built_at = try? c.decodeIfPresent(String.self, forKey: .built_at)
        if let s = try? c.decodeIfPresent(String.self, forKey: .analyzer_version) {
            analyzer_version = s
        } else if let n = try? c.decodeIfPresent(Int.self, forKey: .analyzer_version) {
            analyzer_version = String(n)
        } else {
            analyzer_version = nil
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
    public var available: Bool

    enum CodingKeys: String, CodingKey {
        case version, source, status, result, checked_at, synced_at, cooldown_until, error
        case available, revision, updated_at
    }

    private static func decodeFlexibleTimestamp<K: CodingKey>(_ c: KeyedDecodingContainer<K>, key: K) -> String? {
        if let s = try? c.decodeIfPresent(String.self, forKey: key), !s.isEmpty {
            return s
        }
        if let secs = try? c.decodeIfPresent(Double.self, forKey: key), secs > 0 {
            let iso = ISO8601DateFormatter()
            iso.formatOptions = [.withInternetDateTime]
            return iso.string(from: Date(timeIntervalSince1970: secs))
        }
        return nil
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        if let ver = try? c.decodeIfPresent(FingerprintBankVersion.self, forKey: .version) {
            version = ver
            available = !ver.revision.isEmpty
        } else if let legacyRev = try? c.decodeIfPresent(String.self, forKey: .revision) {
            version = FingerprintBankVersion(revision: legacyRev)
            available = (try? c.decodeIfPresent(Bool.self, forKey: .available)) ?? !legacyRev.isEmpty
        } else {
            version = nil
            available = (try? c.decodeIfPresent(Bool.self, forKey: .available)) ?? false
        }
        source = try? c.decodeIfPresent(String.self, forKey: .source)
        status = try? c.decodeIfPresent(String.self, forKey: .status)
        result = try? c.decodeIfPresent(String.self, forKey: .result)
        checked_at = Self.decodeFlexibleTimestamp(c, key: .checked_at)
        synced_at = Self.decodeFlexibleTimestamp(c, key: .synced_at) ?? Self.decodeFlexibleTimestamp(c, key: .updated_at)
        cooldown_until = Self.decodeFlexibleTimestamp(c, key: .cooldown_until)
        error = try? c.decodeIfPresent(String.self, forKey: .error)
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encodeIfPresent(version, forKey: .version)
        try c.encodeIfPresent(source, forKey: .source)
        try c.encodeIfPresent(status, forKey: .status)
        try c.encodeIfPresent(result, forKey: .result)
        try c.encodeIfPresent(checked_at, forKey: .checked_at)
        try c.encodeIfPresent(synced_at, forKey: .synced_at)
        try c.encodeIfPresent(cooldown_until, forKey: .cooldown_until)
        try c.encodeIfPresent(error, forKey: .error)
    }
}

public struct ModelFingerprintCandidateScore: Codable, Sendable, Equatable {
    public var model: String
    public var display_name: String?
    public var probability: Double?
    public var score: Double?
    public var rank: Int?

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        model = (try? c.decode(String.self, forKey: .model)) ?? ""
        display_name = try? c.decodeIfPresent(String.self, forKey: .display_name)
        probability = try? c.decodeIfPresent(Double.self, forKey: .probability)
        score = try? c.decodeIfPresent(Double.self, forKey: .score)
        rank = try? c.decodeIfPresent(Int.self, forKey: .rank)
    }
}

public struct ModelFingerprintGroupPrediction: Codable, Sendable, Equatable {
    public var index: Int
    public var prediction: String?
    public var probability: Double?
    public var status: String?
}

public struct ModelFingerprintReport: Codable, Sendable, Equatable {
    public var engine: String?
    public var prediction: String?
    public var prediction_name: String?
    public var first_prediction: String?
    public var probability: Double?
    public var first_probability: Double?
    public var decision: String?
    public var outcome: String?
    public var reason: String?
    public var used_outputs: Int?
    public var sample_count: Int?
    public var candidates: [ModelFingerprintCandidateScore]?
    public var group_predictions: [ModelFingerprintGroupPrediction]?
    public var bank_revision: String?

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        engine = try? c.decodeIfPresent(String.self, forKey: .engine)
        prediction = try? c.decodeIfPresent(String.self, forKey: .prediction)
        prediction_name = try? c.decodeIfPresent(String.self, forKey: .prediction_name)
        first_prediction = try? c.decodeIfPresent(String.self, forKey: .first_prediction)
        probability = try? c.decodeIfPresent(Double.self, forKey: .probability)
        first_probability = try? c.decodeIfPresent(Double.self, forKey: .first_probability)
        decision = try? c.decodeIfPresent(String.self, forKey: .decision)
        outcome = try? c.decodeIfPresent(String.self, forKey: .outcome)
        reason = try? c.decodeIfPresent(String.self, forKey: .reason)
        used_outputs = try? c.decodeIfPresent(Int.self, forKey: .used_outputs)
        sample_count = try? c.decodeIfPresent(Int.self, forKey: .sample_count)
        candidates = try? c.decodeIfPresent([ModelFingerprintCandidateScore].self, forKey: .candidates)
        group_predictions = try? c.decodeIfPresent([ModelFingerprintGroupPrediction].self, forKey: .group_predictions)
        if let directRev = try? c.decodeIfPresent(String.self, forKey: .bank_revision) {
            bank_revision = directRev
        } else if let bankObj = try? c.decodeIfPresent([String: JSONValue].self, forKey: .bank_version) {
            bank_revision = bankObj["revision"]?.stringValue
        }
    }

    enum CodingKeys: String, CodingKey {
        case engine, prediction, prediction_name, first_prediction, probability, first_probability
        case decision, outcome, reason, used_outputs, sample_count, candidates, group_predictions
        case bank_revision, bank_version
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encodeIfPresent(engine, forKey: .engine)
        try c.encodeIfPresent(prediction, forKey: .prediction)
        try c.encodeIfPresent(prediction_name, forKey: .prediction_name)
        try c.encodeIfPresent(first_prediction, forKey: .first_prediction)
        try c.encodeIfPresent(probability, forKey: .probability)
        try c.encodeIfPresent(first_probability, forKey: .first_probability)
        try c.encodeIfPresent(decision, forKey: .decision)
        try c.encodeIfPresent(outcome, forKey: .outcome)
        try c.encodeIfPresent(reason, forKey: .reason)
        try c.encodeIfPresent(used_outputs, forKey: .used_outputs)
        try c.encodeIfPresent(sample_count, forKey: .sample_count)
        try c.encodeIfPresent(candidates, forKey: .candidates)
        try c.encodeIfPresent(group_predictions, forKey: .group_predictions)
        try c.encodeIfPresent(bank_revision, forKey: .bank_revision)
    }
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
    public var bank_version: FingerprintBankVersion?

    public var isTerminal: Bool {
        ["completed", "failed", "cancelled", "interrupted", "needs_confirmation"].contains(status)
    }
}

public struct ModelDetectionDetailConfig: Codable, Sendable, Equatable {
    public var account_id: Int
    public var enabled: Bool
    public var schedule_mode: String?
    public var interval_minutes: Int
    public var min_interval_minutes: Int?
    public var effective_interval_minutes: Int?
    public var model_id: String
    public var verdict_mode: String?
    public var degraded_model_id: String?
    public var version: String
    public var status: String?
    public var reason: String?
    public var next_at: String?
}
