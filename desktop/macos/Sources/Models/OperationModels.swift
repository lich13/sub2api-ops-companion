import Foundation

public struct TestEvent: Codable, Sendable, Equatable {
    public var type: String
    public var text: String?
    public var model: String?
    public var error: String?
    public var success: Bool?
    public var image_url: String?
    public var audio_url: String?
    public var video_url: String?
    public var duration_ms: Double?
    public var completed_at: String?

    public init(
        type: String,
        text: String? = nil,
        model: String? = nil,
        error: String? = nil,
        success: Bool? = nil,
        image_url: String? = nil,
        audio_url: String? = nil,
        video_url: String? = nil,
        duration_ms: Double? = nil,
        completed_at: String? = nil
    ) {
        self.type = type
        self.text = text
        self.model = model
        self.error = error
        self.success = success
        self.image_url = image_url
        self.audio_url = audio_url
        self.video_url = video_url
        self.duration_ms = duration_ms
        self.completed_at = completed_at
    }
}

public struct Operation: Codable, Sendable, Equatable, Identifiable {
    public var id: String
    public var account_id: Int
    public var account_name: String
    public var action: String
    public var status: String
    public var batch_id: String?
    public var reason: String?
    public var requested: [String: JSONValue]
    public var current: [String: JSONValue]?
    public var result: [String: JSONValue]?
    public var events: [TestEvent]?
    public var next_event: Int?
    public var started_at: String?
    public var scheduled_at: String?
    public var attempt_count: Int?
    public var next_retry_at: String?
    public var repeat_mode: String?
    public var repeat_series_id: String?
    public var repeat_state: String?
    public var repeat_reason: String?
    public var next_operation_id: String?

    enum CodingKeys: String, CodingKey {
        case id, account_id, account_name, action, status, batch_id, reason
        case requested, current, result, events, next_event
        case started_at, scheduled_at, attempt_count, next_retry_at
        case repeat_mode = "repeat"
        case repeat_series_id, repeat_state, repeat_reason, next_operation_id
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        account_id = try c.decodeIfPresent(Int.self, forKey: .account_id) ?? 0
        account_name = try c.decodeIfPresent(String.self, forKey: .account_name) ?? ""
        action = try c.decodeIfPresent(String.self, forKey: .action) ?? ""
        status = try c.decodeIfPresent(String.self, forKey: .status) ?? ""
        batch_id = try c.decodeIfPresent(String.self, forKey: .batch_id)
        reason = try c.decodeIfPresent(String.self, forKey: .reason)
        requested = try c.decodeIfPresent([String: JSONValue].self, forKey: .requested) ?? [:]
        current = try c.decodeIfPresent([String: JSONValue].self, forKey: .current)
        result = try c.decodeIfPresent([String: JSONValue].self, forKey: .result)
        events = try c.decodeIfPresent([TestEvent].self, forKey: .events)
        next_event = try c.decodeIfPresent(Int.self, forKey: .next_event)
        started_at = try c.decodeIfPresent(String.self, forKey: .started_at)
        scheduled_at = try c.decodeIfPresent(String.self, forKey: .scheduled_at)
        attempt_count = try c.decodeIfPresent(Int.self, forKey: .attempt_count)
        next_retry_at = try c.decodeIfPresent(String.self, forKey: .next_retry_at)
        repeat_mode = try c.decodeIfPresent(String.self, forKey: .repeat_mode)
        repeat_series_id = try c.decodeIfPresent(String.self, forKey: .repeat_series_id)
        repeat_state = try c.decodeIfPresent(String.self, forKey: .repeat_state)
        repeat_reason = try c.decodeIfPresent(String.self, forKey: .repeat_reason)
        next_operation_id = try c.decodeIfPresent(String.self, forKey: .next_operation_id)
    }

    public init(
        id: String,
        account_id: Int,
        account_name: String,
        action: String,
        status: String,
        reason: String? = nil,
        requested: [String: JSONValue] = [:],
        current: [String: JSONValue]? = nil,
        scheduled_at: String? = nil,
        repeat_mode: String? = nil,
        repeat_state: String? = nil
    ) {
        self.id = id
        self.account_id = account_id
        self.account_name = account_name
        self.action = action
        self.status = status
        self.reason = reason
        self.requested = requested
        self.current = current
        self.scheduled_at = scheduled_at
        self.repeat_mode = repeat_mode
        self.repeat_state = repeat_state
    }
}

public struct OperationBatch: Codable, Sendable, Equatable {
    public var batch_id: String
    public var items: [Operation]
    public var pending: Int

    public init(batch_id: String, items: [Operation], pending: Int) {
        self.batch_id = batch_id
        self.items = items
        self.pending = pending
    }
}
