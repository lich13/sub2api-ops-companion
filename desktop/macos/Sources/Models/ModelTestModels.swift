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

public struct FingerprintBankStatusInfo: Codable, Sendable, Equatable {
    public var available: Bool
    public var revision: String?
    public var model_count: Int?
    public var updated_at: String?
    public var syncing: Bool?
    public var message: String?
    public var error: String?

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        available = try c.decodeIfPresent(Bool.self, forKey: .available) ?? false
        revision = try c.decodeIfPresent(String.self, forKey: .revision)
        model_count = try c.decodeIfPresent(Int.self, forKey: .model_count)
        updated_at = try c.decodeIfPresent(String.self, forKey: .updated_at)
        syncing = try c.decodeIfPresent(Bool.self, forKey: .syncing)
        message = try c.decodeIfPresent(String.self, forKey: .message)
        error = try c.decodeIfPresent(String.self, forKey: .error)
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
    public var duration_ms: Double?
    public var error: String?

    public var id: Int { index }
}

public struct ModelTestJob: Codable, Sendable, Equatable, Identifiable {
    public var id: String
    public var account_id: Int?
    public var account_name: String?
    public var model_id: String?
    public var engine: String?
    public var sample_count: Int?
    public var status: String
    public var reason: String?
    public var error: String?
    public var created_at: String?
    public var completed_at: String?
    public var groups: [ModelTestGroupStatus]?
    public var report: ModelFingerprintReport?

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
