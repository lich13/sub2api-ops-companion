import Foundation
import Observation

@MainActor
@Observable
public final class OperationStore {
    public private(set) var rows: [Operation] = []
    public private(set) var epoch: UInt64 = 0
    public private(set) var connectionKey: String = ""
    public var isOpen: Bool = false
    public var error: String = ""
    public var batchId: String?
    public var batchItems: [Operation] = []

    private var memoryClientId: String = ""
    private var pollingTask: Task<Void, Never>?

    private static let terminalStatuses: Set<String> = [
        "completed", "failed", "cancelled", "superseded", "needs_confirmation",
    ]
    private static let clientKey = "sub2ops-operation-client"

    public init() {}

    public func clientId() -> String {
        if !memoryClientId.isEmpty { return memoryClientId }
        if let saved = UserDefaults.standard.string(forKey: Self.clientKey), !saved.isEmpty {
            memoryClientId = saved
            return saved
        }
        let generated = UUID().uuidString.lowercased()
        memoryClientId = generated
        UserDefaults.standard.set(generated, forKey: Self.clientKey)
        return generated
    }

    public static func isTerminal(_ status: String) -> Bool {
        terminalStatuses.contains(status)
    }

    public static func isRepeating(_ job: Operation) -> Bool {
        job.repeat_mode == "quota_cycle"
            && (job.repeat_state == "active" || job.repeat_state == "needs_confirmation")
    }

    public static func isPending(_ job: Operation) -> Bool {
        !isTerminal(job.status) || job.status == "needs_confirmation" || isRepeating(job)
    }

    public var pendingOperations: [Operation] {
        rows.filter(Self.isPending)
    }

    public func pendingForAccount(_ accountId: Int) -> [Operation] {
        rows.filter { $0.account_id == accountId && Self.isPending($0) }
    }

    public func bindConnection(_ key: String) {
        guard connectionKey != key else { return }
        connectionKey = key
        epoch &+= 1
        rows = []
        isOpen = false
        error = ""
        batchId = nil
        batchItems = []
    }

    public func setOperations(_ next: [Operation]) {
        guard rows != next else { return }
        rows = next
    }

    public func publish(_ job: Operation) {
        let filtered = rows.filter { row in
            row.id != job.id
                && (job.repeat_series_id == nil || row.repeat_series_id != job.repeat_series_id)
        }
        setOperations([job] + filtered)
    }

    public func startPollingIfNeeded(online: Bool, active: Bool) {
        pollingTask?.cancel()
        guard online && (isOpen || active) else { return }
        let capturedEpoch = epoch
        pollingTask = Task { @MainActor [weak self] in
            guard let self else { return }
            while !Task.isCancelled && self.epoch == capturedEpoch {
                await self.pollOnce(capturedEpoch: capturedEpoch)
                let fast = self.isOpen || self.rows.contains {
                    ["queued", "running", "checking"].contains($0.status) || $0.repeat_state == "active"
                }
                try? await Task.sleep(for: .seconds(fast ? 2 : 10))
            }
        }
    }

    public func stopPolling() {
        pollingTask?.cancel()
        pollingTask = nil
    }

    private struct OperationListEnvelope: Decodable, Sendable {
        let items: [Operation]
    }

    public func pollOnce(capturedEpoch: UInt64? = nil) async {
        let checkEpoch = capturedEpoch ?? epoch
        let path = (isOpen && batchId != nil) ? "/account-operations?batch_id=\(batchId!)" : "/account-operations"
        do {
            let envelope: OperationListEnvelope = try await RustBridge.shared.api("GET", path)
            guard epoch == checkEpoch else { return }
            if isOpen, batchId != nil {
                batchItems = envelope.items
                let ids = Set(envelope.items.map(\.id))
                setOperations(envelope.items + rows.filter { !ids.contains($0.id) })
            } else {
                setOperations(envelope.items)
            }
            error = ""
        } catch {
            guard epoch == checkEpoch else { return }
            self.error = error.localizedDescription
        }
    }

    public func submitOperation(
        account: Account,
        action: String,
        payload: [String: JSONValue],
        expectedVersion: String? = nil,
        scheduledAt: String? = nil,
        repeatMode: String? = nil
    ) async throws -> Operation {
        let currentEpoch = epoch
        let resolvedVersion = expectedVersion
            ?? (action == "degradation_mark" ? account.degradation_mark?.version : account.operation_versions[action])
        guard let version = resolvedVersion, !version.isEmpty else {
            throw RustBridgeError("请刷新账号后重试")
        }
        var body: [String: JSONValue] = [
            "action": .string(action),
            "payload": .object(payload),
            "expected_version": .string(version),
            "request_id": .string(UUID().uuidString.lowercased()),
            "client_id": .string(clientId()),
        ]
        if let scheduledAt, !scheduledAt.isEmpty {
            body["scheduled_at"] = .string(scheduledAt)
        }
        if let repeatMode, !repeatMode.isEmpty {
            body["repeat"] = .string(repeatMode)
        }
        let job: Operation = try await RustBridge.shared.api(
            "POST",
            "/accounts/\(account.id)/operations",
            body: .object(body)
        )
        guard epoch == currentEpoch else {
            throw RustBridgeError("连接已切换；请求保留在原连接")
        }
        publish(job)
        return job
    }

    public func followOperation(
        initial: Operation,
        onEvent: (@MainActor (TestEvent) -> Void)? = nil,
        onUpdate: (@MainActor (Operation) -> Void)? = nil
    ) async throws -> [String: JSONValue] {
        let currentEpoch = epoch
        var job = initial
        var after = 0
        while true {
            guard epoch == currentEpoch else {
                throw RustBridgeError("连接已切换；请求保留在原连接")
            }
            if let onEvent {
                for event in job.events ?? [] {
                    onEvent(event)
                }
                after = job.next_event ?? after
            }
            onUpdate?(job)
            if Self.isTerminal(job.status) {
                if onEvent != nil, (job.events?.count ?? 0) == 100 {
                    job = try await RustBridge.shared.api("GET", "/account-operations/\(job.id)?after_event=\(after)")
                    continue
                }
                if job.status == "completed" {
                    return job.result ?? [:]
                }
                let fallback: String
                switch job.status {
                case "needs_confirmation": fallback = "请求已保留，请在待办中核对"
                case "cancelled": fallback = "已取消"
                case "superseded": fallback = "已由新请求替代"
                default: fallback = "操作失败"
                }
                throw RustBridgeError(job.reason ?? fallback)
            }
            if let onEvent {
                let statusText: String
                switch job.status {
                case "queued": statusText = job.reason ?? "已排队，等待执行"
                case "checking": statusText = "核对结果"
                default: statusText = "测试中"
                }
                onEvent(TestEvent(type: "status", text: statusText))
            }
            try await Task.sleep(for: .milliseconds(500))
            guard epoch == currentEpoch else {
                throw RustBridgeError("连接已切换；请求保留在原连接")
            }
            let suffix = onEvent != nil ? "?after_event=\(after)" : ""
            let next: Operation = try await RustBridge.shared.api("GET", "/account-operations/\(job.id)\(suffix)")
            guard epoch == currentEpoch else {
                throw RustBridgeError("连接已切换；请求保留在原连接")
            }
            job = next
            if rows.contains(where: { $0.id == job.id }) {
                setOperations(rows.map { $0.id == job.id ? job : $0 })
            } else {
                setOperations([job] + rows)
            }
        }
    }

    @discardableResult
    public func accountOperation(
        account: Account,
        action: String,
        payload: [String: JSONValue],
        expectedVersion: String? = nil,
        onUpdate: (@MainActor (Operation) -> Void)? = nil
    ) async throws -> [String: JSONValue] {
        let job = try await submitOperation(account: account, action: action, payload: payload, expectedVersion: expectedVersion)
        let result = try await followOperation(initial: job, onUpdate: onUpdate)
        Task { try? await RustBridge.shared.commandVoid("refresh") }
        return result
    }

    public func submitBatch(
        action: String,
        accounts: [[String: JSONValue]],
        requestId: String,
        customPayload: [String: JSONValue]? = nil
    ) async throws -> OperationBatch {
        let currentEpoch = epoch
        let payloadObj: [String: JSONValue] = customPayload ?? (action == "delete" ? ["confirmed": .bool(true)] : [:])
        let batch: OperationBatch = try await RustBridge.shared.api(
            "POST",
            "/account-operations/batch",
            body: .object([
                "action": .string(action),
                "payload": .object(payloadObj),
                "accounts": .array(accounts.map(JSONValue.object)),
                "request_id": .string(requestId),
                "client_id": .string(clientId()),
            ])
        )
        guard epoch == currentEpoch else {
            throw RustBridgeError("连接已切换；请求保留在原连接")
        }
        let batchIds = Set(batch.items.map(\.id))
        setOperations(batch.items + rows.filter { !batchIds.contains($0.id) })
        return batch
    }

    public func followBatch(
        initial: OperationBatch,
        onUpdate: (@MainActor (OperationBatch) -> Void)? = nil
    ) async throws -> OperationBatch {
        let currentEpoch = epoch
        var current = initial
        onUpdate?(current)
        while !current.items.allSatisfy({ Self.isTerminal($0.status) }) {
            try await Task.sleep(for: .milliseconds(600))
            guard epoch == currentEpoch else {
                throw RustBridgeError("连接已切换；请求保留在原连接")
            }
            let envelope: OperationListEnvelope = try await RustBridge.shared.api(
                "GET",
                "/account-operations?batch_id=\(current.batch_id)"
            )
            guard epoch == currentEpoch else {
                throw RustBridgeError("连接已切换；请求保留在原连接")
            }
            let remaining = envelope.items.filter { !Self.isTerminal($0.status) }.count
            current = OperationBatch(batch_id: current.batch_id, items: envelope.items, pending: remaining)
            let batchIds = Set(current.items.map(\.id))
            setOperations(current.items + rows.filter { !batchIds.contains($0.id) })
            onUpdate?(current)
        }
        return current
    }

    public func cancelJobId(_ id: String) async throws -> Operation {
        let currentEpoch = epoch
        let next: Operation = try await RustBridge.shared.api("POST", "/account-operations/\(id)/cancel", body: .object([:]))
        if epoch == currentEpoch {
            publish(next)
            batchItems = batchItems.map { $0.id == next.id ? next : $0 }
        }
        return next
    }

    public func cancelJob(_ job: Operation) async throws {
        _ = try await cancelJobId(job.id)
    }
}
