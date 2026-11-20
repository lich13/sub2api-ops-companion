import Foundation

public struct ErrorDispositionNotification: Codable, Sendable, Equatable {
    public var status: String
}

public struct ErrorClueNotification: Codable, Sendable, Equatable {
    public var status: String
    public var reason: String?
    public var at: String?
    public var attempts: Int?
    public var next_at: String?
}

public struct OpsError: Codable, Sendable, Equatable, Identifiable {
    public var id: Int
    public var account_id: Int?
    public var account_name: String
    public var group_id: Int?
    public var group_name: String
    public var created_at: String
    public var model: String
    public var requested_model: String
    public var upstream_model: String
    public var status_code: Int
    public var upstream_status_code: Int?
    public var provider_error_code: String
    public var message: String
    public var request_id: String
    public var resolved: Bool
    public var error_owner: String?
    public var error_phase: String?
    public var error_source: String?
    public var disposition_notification: ErrorDispositionNotification?
    public var content: String?
    public var content_limited: Bool?
    public var notification: ErrorClueNotification?

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(Int.self, forKey: .id)
        account_id = try c.decodeIfPresent(Int.self, forKey: .account_id)
        account_name = try c.decodeIfPresent(String.self, forKey: .account_name) ?? ""
        group_id = try c.decodeIfPresent(Int.self, forKey: .group_id)
        group_name = try c.decodeIfPresent(String.self, forKey: .group_name) ?? ""
        created_at = try c.decodeIfPresent(String.self, forKey: .created_at) ?? ""
        model = try c.decodeIfPresent(String.self, forKey: .model) ?? ""
        requested_model = try c.decodeIfPresent(String.self, forKey: .requested_model) ?? ""
        upstream_model = try c.decodeIfPresent(String.self, forKey: .upstream_model) ?? ""
        status_code = try c.decodeIfPresent(Int.self, forKey: .status_code) ?? 0
        upstream_status_code = try c.decodeIfPresent(Int.self, forKey: .upstream_status_code)
        provider_error_code = try c.decodeIfPresent(String.self, forKey: .provider_error_code) ?? ""
        message = try c.decodeIfPresent(String.self, forKey: .message) ?? ""
        request_id = try c.decodeIfPresent(String.self, forKey: .request_id) ?? ""
        resolved = try c.decodeIfPresent(Bool.self, forKey: .resolved) ?? false
        error_owner = try c.decodeIfPresent(String.self, forKey: .error_owner)
        error_phase = try c.decodeIfPresent(String.self, forKey: .error_phase)
        error_source = try c.decodeIfPresent(String.self, forKey: .error_source)
        disposition_notification = try c.decodeIfPresent(ErrorDispositionNotification.self, forKey: .disposition_notification)
        content = try c.decodeIfPresent(String.self, forKey: .content)
        content_limited = try c.decodeIfPresent(Bool.self, forKey: .content_limited)
        notification = try c.decodeIfPresent(ErrorClueNotification.self, forKey: .notification)
    }
}

public struct RecoveryResetCredit: Codable, Sendable, Equatable {
    public var consumed: Bool?
    public var completed_at: String
    public var verification_method: String
}

public struct Recovery: Codable, Sendable, Equatable, Identifiable {
    public var id: Int
    public var account_id: Int
    public var account_name: String
    public var model_id: String
    public var test_completed_at: String?
    public var recovered_at: String?
    public var legacy: Bool
    public var kind: String?
    public var result_status: String?
    public var attempt_count: Int?
    public var reset_credit: RecoveryResetCredit?
}

public struct Snapshot: Codable, Sendable, Equatable {
    public var observed_at: String
    public var accounts: [Account]
    public var groups: [Group]
    public var errors: [OpsError]
    public var recoveries: [Recovery]

    public init(
        observed_at: String,
        accounts: [Account] = [],
        groups: [Group] = [],
        errors: [OpsError] = [],
        recoveries: [Recovery] = []
    ) {
        self.observed_at = observed_at
        self.accounts = accounts
        self.groups = groups
        self.errors = errors
        self.recoveries = recoveries
    }
}
