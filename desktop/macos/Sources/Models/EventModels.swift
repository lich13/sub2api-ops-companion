import Foundation

public typealias ErrorDispositionNotification = ErrorDispositionNotificationRecord
public typealias ErrorClueNotification = ErrorClueNotificationRecord
public typealias OpsError = OpsErrorRecord
public typealias RecoveryResetCredit = RecoveryResetCreditRecord
public typealias Recovery = RecoveryRecord
public typealias Snapshot = SnapshotRecord

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
