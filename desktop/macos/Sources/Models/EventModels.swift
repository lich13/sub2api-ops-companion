import Foundation

public typealias OpsError = OpsErrorRecord
public typealias Recovery = RecoveryRecord
public typealias Snapshot = SnapshotRecord

public struct CursorPage<Item: Codable & Sendable & Equatable, Cursor: Codable & Sendable & Equatable>: Codable, Sendable, Equatable {
    public var items: [Item]
    public var next_cursor: Cursor?

    public init(items: [Item], next_cursor: Cursor? = nil) {
        self.items = items
        self.next_cursor = next_cursor
    }
}

extension OpsErrorRecord: Identifiable {
    public var recordId: Int { id ?? 0 }
    public var accountNameText: String { account_name ?? "" }
    public var groupNameText: String { group_name ?? "" }
    public var modelText: String { model ?? "" }
    public var requestedModelText: String { requested_model ?? "" }
    public var upstreamModelText: String { upstream_model ?? "" }
    public var statusCodeValue: Int { status_code ?? 0 }
    public var providerErrorCodeText: String { provider_error_code ?? "" }
    public var messageText: String { message ?? "" }
    public var requestIdText: String { request_id ?? "" }
    public var isResolved: Bool { resolved ?? false }
}

extension RecoveryRecord: Identifiable {
    public var recordId: Int { id ?? 0 }
    public var accountNameText: String { account_name ?? "" }
    public var modelIdText: String { model_id ?? "" }
}

extension SnapshotRecord {
    public init(
        observed_at: String,
        accounts: [AccountRecord] = [],
        groups: [GroupRecord] = [],
        errors: [OpsErrorRecord] = [],
        recoveries: [RecoveryRecord] = []
    ) {
        self.init(
            observed_at: observed_at,
            accounts: accounts,
            groups: groups,
            errors: errors,
            recoveries: recoveries,
            schema_version: nil
        )
    }
}
