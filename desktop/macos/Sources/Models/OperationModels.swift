import Foundation

public typealias TestEvent = TestEventRecord
public typealias Operation = OperationHeader
public typealias OperationBatch = BatchHeader

extension TestEventRecord {
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
        self.init(
            type: type,
            text: text,
            model: model,
            error: error,
            success: success,
            image_url: image_url,
            audio_url: audio_url,
            video_url: video_url,
            duration_ms: duration_ms,
            completed_at: completed_at,
            mime_type: nil,
            audio_format: nil
        )
    }
}

extension OperationRequestFields {
    public static let empty = OperationRequestFields(
        priority: nil,
        concurrency: nil,
        load_factor: nil,
        proxy_id: nil,
        schedulable: nil,
        marked: nil,
        confirmed: nil,
        model_id: nil,
        mode: nil,
        action: nil,
        template_id: nil,
        template_version: nil,
        group_ids: nil,
        scope_group_ids: nil,
        whitelist: nil,
        mappings: nil
    )

    public func asPayload(for actionName: String) -> [String: JSONValue] {
        var obj: [String: JSONValue] = [:]
        if let priority { obj["priority"] = .int(Int64(priority)) }
        if let concurrency { obj["concurrency"] = .int(Int64(concurrency)) }
        if actionName == "load_factor" || load_factor != nil {
            obj["load_factor"] = load_factor.map { .int(Int64($0)) } ?? .null
        }
        if actionName == "proxy" || proxy_id != nil {
            obj["proxy_id"] = proxy_id.map { .int(Int64($0)) } ?? .null
        }
        if let schedulable { obj["schedulable"] = .bool(schedulable) }
        if let marked { obj["marked"] = .bool(marked) }
        if let confirmed { obj["confirmed"] = .bool(confirmed) }
        if let model_id { obj["model_id"] = .string(model_id) }
        if let mode { obj["mode"] = .string(mode) }
        if let action { obj["action"] = .string(action) }
        if let template_id { obj["template_id"] = .string(template_id) }
        if let template_version { obj["template_version"] = .string(template_version) }
        if let group_ids { obj["group_ids"] = .array(group_ids.map { .int(Int64($0)) }) }
        if let scope_group_ids { obj["scope_group_ids"] = .array(scope_group_ids.map { .int(Int64($0)) }) }
        if let whitelist { obj["whitelist"] = .array(whitelist.map(JSONValue.string)) }
        if let mappings {
            obj["mappings"] = .array(mappings.map {
                .object(["source": .string($0.source), "target": .string($0.target)])
            })
        }
        return obj
    }
}

extension OperationCurrentFields {
    public func asPayload(for actionName: String) -> [String: JSONValue] {
        var obj: [String: JSONValue] = [:]
        if let id { obj["id"] = .int(Int64(id)) }
        if let priority { obj["priority"] = .int(Int64(priority)) }
        if let concurrency { obj["concurrency"] = .int(Int64(concurrency)) }
        if actionName == "load_factor" || load_factor != nil {
            obj["load_factor"] = load_factor.map { .int(Int64($0)) } ?? .null
        }
        if actionName == "proxy" || proxy_id != nil {
            obj["proxy_id"] = proxy_id.map { .int(Int64($0)) } ?? .null
        }
        if let name { obj["name"] = .string(name) }
        if let platform { obj["platform"] = .string(platform) }
        if let type { obj["type"] = .string(type) }
        if let status { obj["status"] = .string(status) }
        if let schedulable { obj["schedulable"] = .bool(schedulable) }
        if let marked { obj["marked"] = .bool(marked) }
        if let group_ids { obj["group_ids"] = .array(group_ids.map { .int(Int64($0)) }) }
        if let whitelist { obj["whitelist"] = .array(whitelist.map(JSONValue.string)) }
        if let mappings {
            obj["mappings"] = .array(mappings.map {
                .object(["source": .string($0.source), "target": .string($0.target)])
            })
        }
        return obj
    }
}

extension OperationHeader: Identifiable {
    public var repeat_mode: String? { self.repeat }
    public var accountNameText: String { account_name ?? "" }

    public init(
        id: String,
        account_id: Int,
        account_name: String,
        action: String,
        status: String,
        reason: String? = nil,
        requested: OperationRequestFields = .empty,
        current: OperationCurrentFields? = nil,
        result: OperationReceipt? = nil,
        scheduled_at: String? = nil,
        repeat_mode: String? = nil,
        repeat_state: String? = nil
    ) {
        self.init(
            id: id,
            account_id: account_id,
            action: action,
            status: status,
            account_name: account_name,
            batch_id: nil,
            reason: reason,
            started_at: nil,
            scheduled_at: scheduled_at,
            updated_at: nil,
            created_at: nil,
            completed_at: nil,
            next_retry_at: nil,
            repeat: repeat_mode,
            repeat_series_id: nil,
            repeat_state: repeat_state,
            repeat_reason: nil,
            next_operation_id: nil,
            previous_operation_id: nil,
            execution_mode: nil,
            dispatch_id: nil,
            attempt_count: nil,
            next_event: nil,
            parent_account_id: nil,
            child_account_ids: nil,
            native_target: nil,
            requested: requested,
            current: current,
            result: result,
            events: nil
        )
    }
}

extension BatchHeader {
    public init(batch_id: String, items: [OperationHeader], pending: Int) {
        self.init(batch_id: batch_id, pending: pending, items: items)
    }
}
