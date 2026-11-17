import Foundation
import Observation

public struct DetectionReceipt: Codable, Sendable, Equatable {
    public var status: String
    public var basis: String?
    public var group_indexes: [Int]?
    public var marked: Bool?
    public var schedule_verified: Bool?
    public var mark_cleared: Bool?
    public var reason: String
    public var completed_at: String?

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        status = try c.decodeIfPresent(String.self, forKey: .status) ?? ""
        basis = try c.decodeIfPresent(String.self, forKey: .basis)
        group_indexes = try c.decodeIfPresent([Int].self, forKey: .group_indexes)
        marked = try c.decodeIfPresent(Bool.self, forKey: .marked)
        schedule_verified = try c.decodeIfPresent(Bool.self, forKey: .schedule_verified)
        mark_cleared = try c.decodeIfPresent(Bool.self, forKey: .mark_cleared)
        reason = try c.decodeIfPresent(String.self, forKey: .reason) ?? ""
        completed_at = try c.decodeIfPresent(String.self, forKey: .completed_at)
    }
}

public struct DetectionClassificationInfo: Codable, Sendable, Equatable {
    public var basis: String?
    public var group_indexes: [Int]?
}

public struct DetectionGroupPredictionItem: Codable, Sendable, Equatable {
    public var index: Int
    public var prediction: String?
}

public struct DetectionLogSummary: Codable, Sendable, Equatable {
    public var verdict_mode: String?
    public var degraded_model_id: String?
    public var engine: String?
    public var sample_count: Int?
    public var reason: String?
    public var prediction_name: String?
    public var decision: String?
    public var classification: DetectionClassificationInfo?
    public var bank_revision: String?
    public var group_predictions: [DetectionGroupPredictionItem]?
    public var outcome: String
    public var analysis_available: Bool
    public var first_prediction: String
    public var prediction: String
    public var probability: Double?
    public var first_probability: Double?
    public var used_outputs: Int?
    public var first_downgrade: Bool?
    public var aggregate_downgrade: Bool?
    public var disposition: DetectionReceipt?
    public var recovery: DetectionReceipt?
    public var verification: DetectionReceipt?
    public var restoration: DetectionReceipt?

    enum CodingKeys: String, CodingKey {
        case verdict_mode, degraded_model_id, engine, sample_count, reason
        case prediction_name, decision, classification, bank_version, group_predictions
        case outcome, analysis_available, first_prediction, prediction
        case probability, first_probability, used_outputs, first_downgrade, aggregate_downgrade
        case disposition, recovery, verification, restoration
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        verdict_mode = try c.decodeIfPresent(String.self, forKey: .verdict_mode)
        degraded_model_id = try c.decodeIfPresent(String.self, forKey: .degraded_model_id)
        engine = try c.decodeIfPresent(String.self, forKey: .engine)
        sample_count = try c.decodeIfPresent(Int.self, forKey: .sample_count)
        reason = try c.decodeIfPresent(String.self, forKey: .reason)
        prediction_name = try c.decodeIfPresent(String.self, forKey: .prediction_name)
        decision = try c.decodeIfPresent(String.self, forKey: .decision)
        classification = try c.decodeIfPresent(DetectionClassificationInfo.self, forKey: .classification)
        if let bv = try? c.decodeIfPresent([String: JSONValue].self, forKey: .bank_version) {
            bank_revision = bv["revision"]?.stringValue
        }
        group_predictions = try c.decodeIfPresent([DetectionGroupPredictionItem].self, forKey: .group_predictions)
        outcome = try c.decodeIfPresent(String.self, forKey: .outcome) ?? ""
        analysis_available = try c.decodeIfPresent(Bool.self, forKey: .analysis_available) ?? false
        first_prediction = try c.decodeIfPresent(String.self, forKey: .first_prediction) ?? ""
        prediction = try c.decodeIfPresent(String.self, forKey: .prediction) ?? ""
        probability = try c.decodeIfPresent(Double.self, forKey: .probability)
        first_probability = try c.decodeIfPresent(Double.self, forKey: .first_probability)
        used_outputs = try c.decodeIfPresent(Int.self, forKey: .used_outputs)
        first_downgrade = try c.decodeIfPresent(Bool.self, forKey: .first_downgrade)
        aggregate_downgrade = try c.decodeIfPresent(Bool.self, forKey: .aggregate_downgrade)
        disposition = try c.decodeIfPresent(DetectionReceipt.self, forKey: .disposition)
        recovery = try c.decodeIfPresent(DetectionReceipt.self, forKey: .recovery)
        verification = try c.decodeIfPresent(DetectionReceipt.self, forKey: .verification)
        restoration = try c.decodeIfPresent(DetectionReceipt.self, forKey: .restoration)
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encodeIfPresent(verdict_mode, forKey: .verdict_mode)
        try c.encodeIfPresent(degraded_model_id, forKey: .degraded_model_id)
        try c.encodeIfPresent(engine, forKey: .engine)
        try c.encodeIfPresent(sample_count, forKey: .sample_count)
        try c.encodeIfPresent(reason, forKey: .reason)
        try c.encodeIfPresent(prediction_name, forKey: .prediction_name)
        try c.encodeIfPresent(decision, forKey: .decision)
        try c.encodeIfPresent(classification, forKey: .classification)
        try c.encode(outcome, forKey: .outcome)
        try c.encode(analysis_available, forKey: .analysis_available)
        try c.encode(first_prediction, forKey: .first_prediction)
        try c.encode(prediction, forKey: .prediction)
        try c.encodeIfPresent(probability, forKey: .probability)
        try c.encodeIfPresent(first_probability, forKey: .first_probability)
    }
}

public struct DetectionLogNotification: Codable, Sendable, Equatable {
    public var status: String
    public var merged: Bool
    public var delivered_at: String?
    public var reason: String

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        status = try c.decodeIfPresent(String.self, forKey: .status) ?? ""
        merged = try c.decodeIfPresent(Bool.self, forKey: .merged) ?? false
        delivered_at = try c.decodeIfPresent(String.self, forKey: .delivered_at)
        reason = try c.decodeIfPresent(String.self, forKey: .reason) ?? ""
    }
}

public struct DetectionLog: Codable, Sendable, Equatable, Identifiable {
    public var job_id: String
    public var account_id: Int
    public var account_name: String
    public var model_id: String
    public var status: String
    public var created_at: String
    public var started_at: String?
    public var completed_at: String?
    public var triggers: [String]
    public var error: String
    public var details_available: Bool
    public var summary: DetectionLogSummary
    public var notification: DetectionLogNotification?

    public var id: String { job_id }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        job_id = try c.decode(String.self, forKey: .job_id)
        account_id = try c.decodeIfPresent(Int.self, forKey: .account_id) ?? 0
        account_name = try c.decodeIfPresent(String.self, forKey: .account_name) ?? ""
        model_id = try c.decodeIfPresent(String.self, forKey: .model_id) ?? ""
        status = try c.decodeIfPresent(String.self, forKey: .status) ?? ""
        created_at = try c.decodeIfPresent(String.self, forKey: .created_at) ?? ""
        started_at = try c.decodeIfPresent(String.self, forKey: .started_at)
        completed_at = try c.decodeIfPresent(String.self, forKey: .completed_at)
        triggers = try c.decodeIfPresent([String].self, forKey: .triggers) ?? []
        error = try c.decodeIfPresent(String.self, forKey: .error) ?? ""
        details_available = try c.decodeIfPresent(Bool.self, forKey: .details_available) ?? false
        summary = try c.decode(DetectionLogSummary.self, forKey: .summary)
        notification = try c.decodeIfPresent(DetectionLogNotification.self, forKey: .notification)
    }
}

private struct DetectionLogPageEnvelope: Decodable, Sendable {
    var items: [DetectionLog]
    var next_cursor: String?
}

@MainActor
@Observable
public final class DetectionLogStore {
    public private(set) var items: [DetectionLog] = []
    public private(set) var cursor: String? = nil
    public private(set) var loading: Bool = false
    public private(set) var error: String = ""

    private var connectionKey: String = ""
    private var generation: UInt64 = 0

    public init() {}

    public func bind(connectionKey key: String) {
        guard connectionKey != key else { return }
        connectionKey = key
        generation &+= 1
        items = []
        cursor = nil
        loading = false
        error = ""
    }

    public func load(replace: Bool = true, online: Bool) async {
        guard online else { return }
        let before = replace ? nil : cursor
        if !replace && before == nil { return }
        generation &+= 1
        let capturedGen = generation
        let capturedKey = connectionKey
        loading = true
        error = ""
        defer {
            if generation == capturedGen { loading = false }
        }
        let encodedBefore = before.flatMap { $0.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed) }
        let suffix = encodedBefore.map { "&before=\($0)" } ?? ""
        do {
            let res: DetectionLogPageEnvelope = try await RustBridge.shared.api("GET", "/detection-logs?limit=50\(suffix)")
            guard generation == capturedGen && connectionKey == capturedKey else { return }
            if replace {
                items = res.items
            } else {
                var seen = Set(items.map(\.job_id))
                for item in res.items where seen.insert(item.job_id).inserted {
                    items.append(item)
                }
            }
            cursor = res.next_cursor
        } catch {
            if generation == capturedGen {
                self.error = error.localizedDescription
            }
        }
    }
}
