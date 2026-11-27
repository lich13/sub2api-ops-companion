import Foundation
import Observation

@MainActor
@Observable
public final class OperationStore {
    public private(set) var rows: [Operation] = []
    public private(set) var pendingByAccount: [Int: [Operation]] = [:]
    public private(set) var epoch: UInt64 = 0
    public private(set) var connectionKey: String = ""
    public var isOpen: Bool = false {
        didSet {
            if oldValue != isOpen {
                pollSeq &+= 1
                startPollingIfNeeded(online: lastOnline, active: lastActive)
            }
        }
    }
    public var error: String = ""
    public var batchId: String? {
        didSet {
            if oldValue != batchId {
                pollSeq &+= 1
                if isOpen {
                    startPollingIfNeeded(online: lastOnline, active: lastActive)
                }
            }
        }
    }
    public var batchItems: [Operation] = []

    private struct PollingKey: Equatable {
        let online: Bool
        let active: Bool
        let isOpen: Bool
        let batchId: String?
        let epoch: UInt64
    }

    private var memoryClientId: String = ""
    private var pollSeq: UInt64 = 0
    @ObservationIgnored nonisolated(unsafe) private var pollingTask: Task<Void, Never>?
    private var activePollingKey: PollingKey?
    private var lastOnline: Bool = false
    private var lastActive: Bool = false

    deinit {
        pollingTask?.cancel()
    }

    private static let terminalStatuses: Set<String> = [
        "completed", "failed", "cancelled", "superseded", "needs_confirmation",
    ]
    private static let clientKey = "sub2ops-operation-client"
    public static let adaptivePollDelaysMs: [Int] = [35, 80, 180, 350, 500]

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

    public func stop() {
        epoch &+= 1
        pollSeq &+= 1
        pollingTask?.cancel()
        pollingTask = nil
        activePollingKey = nil
        lastOnline = false
        lastActive = false
    }

    public func bindConnection(_ key: String) {
        guard connectionKey != key else { return }
        pollingTask?.cancel()
        pollingTask = nil
        activePollingKey = nil
        connectionKey = key
        epoch &+= 1
        pollSeq &+= 1
        rows = []
        pendingByAccount = [:]
        isOpen = false
        error = ""
        batchId = nil
        batchItems = []
    }

    public func setOperations(_ next: [Operation]) {
        pollSeq &+= 1
        applyOperations(next)
    }

    private func applyOperations(_ next: [Operation]) {
        guard rows != next else { return }
        rows = next
        var indexed: [Int: [Operation]] = [:]
        for op in next where Self.isPending(op) {
            indexed[op.account_id, default: []].append(op)
        }
        if pendingByAccount != indexed {
            pendingByAccount = indexed
        }
    }

    public func publish(_ job: Operation) {
        let filtered = rows.filter { row in
            row.id != job.id
                && (job.repeat_series_id == nil || row.repeat_series_id != job.repeat_series_id)
        }
        setOperations([job] + filtered)
    }

    public func startPollingIfNeeded(online: Bool, active: Bool) {
        lastOnline = online
        lastActive = active
        let key = PollingKey(
            online: online,
            active: active,
            isOpen: isOpen,
            batchId: isOpen ? batchId : nil,
            epoch: epoch
        )
        guard online && (isOpen || active) else {
            pollingTask?.cancel()
            pollingTask = nil
            activePollingKey = nil
            return
        }
        if activePollingKey == key, let pollingTask, !pollingTask.isCancelled {
            return
        }
        pollingTask?.cancel()
        activePollingKey = key
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

    private enum PollTarget: Equatable {
        case all
        case batch(String)
    }

    private var currentPollTarget: PollTarget {
        if isOpen, let batchId {
            return .batch(batchId)
        }
        return .all
    }

    public func pollOnce(capturedEpoch: UInt64? = nil) async {
        let checkEpoch = capturedEpoch ?? epoch
        pollSeq &+= 1
        let capturedPollSeq = pollSeq
        let target = currentPollTarget
        let path: String
        switch target {
        case .all:
            path = "/account-operations"
        case .batch(let bid):
            path = "/account-operations?batch_id=\(bid)"
        }
        do {
            let reply: OperationListReply = try await RustBridge.shared.api("GET", path)
            guard !Task.isCancelled,
                  epoch == checkEpoch,
                  pollSeq == capturedPollSeq,
                  currentPollTarget == target
            else { return }
            switch target {
            case .batch(let bid):
                if isOpen && batchId == bid {
                    batchItems = reply.items
                }
                let ids = Set(reply.items.map(\.id))
                applyOperations(reply.items + rows.filter { !ids.contains($0.id) })
            case .all:
                applyOperations(reply.items)
            }
            error = ""
        } catch is CancellationError {
            return
        } catch {
            guard !Task.isCancelled,
                  epoch == checkEpoch,
                  pollSeq == capturedPollSeq,
                  currentPollTarget == target
            else { return }
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
            throw RustBridgeError.connectionChanged
        }
        publish(job)
        return job
    }

    public func followOperation(
        initial: Operation,
        onEvent: (@MainActor (TestEvent) -> Void)? = nil,
        onUpdate: (@MainActor (Operation) -> Void)? = nil
    ) async throws -> OperationReceipt? {
        let currentEpoch = epoch
        var job = initial
        var after = 0
        var step = 0
        while true {
            try Task.checkCancellation()
            guard epoch == currentEpoch else {
                throw RustBridgeError.connectionChanged
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
                    return job.result
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
            let delayMs = Self.adaptivePollDelaysMs[min(step, Self.adaptivePollDelaysMs.count - 1)]
            step += 1
            try await Task.sleep(for: .milliseconds(delayMs))
            guard epoch == currentEpoch else {
                throw RustBridgeError.connectionChanged
            }
            let suffix = onEvent != nil ? "?after_event=\(after)" : ""
            let next: Operation = try await RustBridge.shared.api("GET", "/account-operations/\(job.id)\(suffix)")
            guard epoch == currentEpoch else {
                throw RustBridgeError.connectionChanged
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
    ) async throws -> OperationReceipt? {
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
        // Server contract: `test` and `delete` batches require `{confirmed: true}`;
        // `groups` and `recover` require `{}`.
        let payloadObj: [String: JSONValue] = customPayload
            ?? (action == "delete" || action == "test" ? ["confirmed": .bool(true)] : [:])
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
            throw RustBridgeError.connectionChanged
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
        var step = 0
        onUpdate?(current)
        while !current.items.allSatisfy({ Self.isTerminal($0.status) }) {
            let delayMs = Self.adaptivePollDelaysMs[min(step, Self.adaptivePollDelaysMs.count - 1)]
            step += 1
            try await Task.sleep(for: .milliseconds(delayMs))
            guard epoch == currentEpoch else {
                throw RustBridgeError.connectionChanged
            }
            let reply: OperationListReply = try await RustBridge.shared.api(
                "GET",
                "/account-operations?batch_id=\(current.batch_id)"
            )
            guard epoch == currentEpoch else {
                throw RustBridgeError.connectionChanged
            }
            current = OperationBatch(batch_id: current.batch_id, items: reply.items, pending: reply.pending)
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
