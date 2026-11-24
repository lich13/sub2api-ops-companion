import AppKit
import Foundation
import os

// MARK: - C ABI Host Function Table (`desktop/src-tauri/src/macos_bridge.rs`)

public struct Sub2OpsHostCallbacks: Sendable {
    public var invoke: @convention(c) (
        UInt64,
        UnsafePointer<UInt8>?,
        Int,
        UnsafePointer<UInt8>?,
        Int
    ) -> Void
    public var set_visibility: @convention(c) (Bool, Bool, Bool, Bool) -> Void
    public var panel_trace: @convention(c) (
        UnsafePointer<UInt8>?,
        Int,
        UnsafePointer<UInt8>?,
        Int
    ) -> Void
    public var quit: @convention(c) () -> Void

    public init(
        invoke: @escaping @convention(c) (UInt64, UnsafePointer<UInt8>?, Int, UnsafePointer<UInt8>?, Int) -> Void,
        set_visibility: @escaping @convention(c) (Bool, Bool, Bool, Bool) -> Void,
        panel_trace: @escaping @convention(c) (UnsafePointer<UInt8>?, Int, UnsafePointer<UInt8>?, Int) -> Void,
        quit: @escaping @convention(c) () -> Void
    ) {
        self.invoke = invoke
        self.set_visibility = set_visibility
        self.panel_trace = panel_trace
        self.quit = quit
    }
}

public struct RustBridgeError: LocalizedError, Sendable, Equatable {
    public let failure: BridgeFailure
    public var message: String { failure.message }
    public var needsCredentials: Bool { ["missing_credentials", "auth_rejected"].contains(failure.code) }
    public var ignored: Bool { ["connection_changed", "background"].contains(failure.code) }
    public init(_ message: String, code: String = "client_error") {
        failure = BridgeFailure(code: code, status: nil, message: message, request_id: nil)
    }
    public init(failure: BridgeFailure) { self.failure = failure }
    public static var connectionChanged: Self { Self("连接已切换；请求保留在原连接", code: "connection_changed") }


    public var errorDescription: String? { message }
}

// MARK: - RustBridge Singleton

public final class RustBridge: Sendable {
    public static let shared = RustBridge()

    public enum EventKind: UInt32, Sendable {
        case opsState = 1
        case updateResult = 2
        case trayTooltip = 3
        case reopen = 4
    }

    public enum InboundEvent: Sendable {
        case opsState(seq: UInt64, state: ViewState)
        case opsStateDecodeError(String)
        case updateResult(String)
        case trayTooltip(String)
        case reopen
    }

    public typealias MockHandler = @Sendable (String, String) async throws -> String

    private static let eventDecodeQueue = DispatchQueue(label: "sub2ops.bridge.events")

    private struct State: Sendable {
        var callbacks: Sub2OpsHostCallbacks?
        var nextRequestId: UInt64 = 1
        var nextEventSeq: UInt64 = 0
        var pending: [UInt64: CheckedContinuation<Data, Error>] = [:]
        var appActive: Bool = false
        var mockHandler: MockHandler?
        var eventHandler: (@MainActor @Sendable (InboundEvent) -> Void)?
    }

    private let state = OSAllocatedUnfairLock(initialState: State())

    private init() {}

    public func installHostCallbacks(_ callbacks: Sub2OpsHostCallbacks) {
        state.withLock { s in
            s.callbacks = callbacks
        }
    }

    public func setMockHandler(_ handler: MockHandler?) {
        state.withLock { s in
            s.mockHandler = handler
        }
    }

    public func setEventHandler(_ handler: (@MainActor @Sendable (InboundEvent) -> Void)?) {
        state.withLock { s in
            s.eventHandler = handler
        }
    }

    public func cachedAppActive() -> Bool {
        state.withLock { $0.appActive }
    }

    public func updateCachedAppActive(_ active: Bool) {
        state.withLock { $0.appActive = active }
    }

    // MARK: - Outbound Host Calls (Swift -> Rust)

    public func rawCommand(_ name: String, payloadJSON: String = "{}") async throws -> Data {
        let mock = state.withLock { $0.mockHandler }
        if let mock {
            let reply = try await mock(name, payloadJSON)
            return Data(reply.utf8)
        }

        return try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Data, Error>) in
            // Lock-ordering invariant: register pending continuation and release the lock
            // BEFORE calling `callbacks.invoke` so synchronous error callbacks cannot deadlock.
            let registration: (UInt64, Sub2OpsHostCallbacks?) = state.withLock { s in
                let id = s.nextRequestId
                s.nextRequestId &+= 1
                guard let cb = s.callbacks else {
                    return (0, nil)
                }
                s.pending[id] = continuation
                return (id, cb)
            }

            guard let callbacks = registration.1 else {
                continuation.resume(throwing: RustBridgeError("客户端桥接尚未初始化"))
                return
            }

            let requestId = registration.0
            let cmdBytes = Array(name.utf8)
            let payloadBytes = Array(payloadJSON.utf8)
            cmdBytes.withUnsafeBufferPointer { cmdBuf in
                payloadBytes.withUnsafeBufferPointer { payloadBuf in
                    callbacks.invoke(
                        requestId,
                        cmdBuf.baseAddress,
                        cmdBuf.count,
                        payloadBuf.baseAddress,
                        payloadBuf.count
                    )
                }
            }
        }
    }

    private func decodeCommandResponse<T: Decodable>(_ responseData: Data) throws -> T {
        do {
            return try JSONDecoder().decode(T.self, from: responseData)
        } catch let error as DecodingError {
            let path: [any CodingKey]
            switch error {
            case .keyNotFound(let key, let context): path = context.codingPath + [key]
            case .dataCorrupted(let context), .typeMismatch(_, let context), .valueNotFound(_, let context): path = context.codingPath
            @unknown default: path = []
            }
            // Dictionary keys and array indices can be user data. Only log model CodingKeys.
            let fields = path.map { key in String(describing: type(of: key)) == "CodingKeys" ? key.stringValue : "[]" }.joined(separator: ".")
            Logger(subsystem: "Sub2Ops", category: "bridge").error("response_decode path=\(fields, privacy: .public)")
            throw RustBridgeError("响应数据解析失败", code: "invalid_response")
        }
    }

    public func command<T: Decodable & Sendable>(_ name: String, args: JSONValue = .object([:])) async throws -> T {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.withoutEscapingSlashes]
        let payloadData = try encoder.encode(args)
        let payloadJSON = String(data: payloadData, encoding: .utf8) ?? "{}"
        let responseData = try await rawCommand(name, payloadJSON: payloadJSON)
        return try decodeCommandResponse(responseData)
    }

    public func commandVoid(_ name: String, args: JSONValue = .object([:])) async throws {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.withoutEscapingSlashes]
        let payloadData = try encoder.encode(args)
        let payloadJSON = String(data: payloadData, encoding: .utf8) ?? "{}"
        _ = try await rawCommand(name, payloadJSON: payloadJSON)
    }

    public func api<T: Decodable & Sendable>(
        _ method: String,
        _ path: String,
        body: JSONValue = .null
    ) async throws -> T {
        try await command(
            "api_request",
            args: .object([
                "method": .string(method),
                "path": .string(path),
                "body": body,
            ])
        )
    }

    public func api<T: Decodable & Sendable, B: Encodable & Sendable>(
        _ method: String,
        _ path: String,
        encodable body: B
    ) async throws -> T {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.withoutEscapingSlashes]
        let methodJSON = String(decoding: try encoder.encode(method), as: UTF8.self)
        let pathJSON = String(decoding: try encoder.encode(path), as: UTF8.self)
        let bodyJSON = String(decoding: try encoder.encode(body), as: UTF8.self)
        let payloadJSON = "{\"method\":\(methodJSON),\"path\":\(pathJSON),\"body\":\(bodyJSON)}"
        let responseData = try await rawCommand("api_request", payloadJSON: payloadJSON)
        return try decodeCommandResponse(responseData)
    }

    public func apiVoid(
        _ method: String,
        _ path: String,
        body: JSONValue = .null
    ) async throws {
        try await commandVoid(
            "api_request",
            args: .object([
                "method": .string(method),
                "path": .string(path),
                "body": body,
            ])
        )
    }

    public func setVisibility(
        mainVisible: Bool,
        mainMinimized: Bool,
        quickVisible: Bool,
        appActive: Bool
    ) {
        let cb = state.withLock { s -> Sub2OpsHostCallbacks? in
            s.appActive = appActive
            return s.callbacks
        }
        cb?.set_visibility(mainVisible, mainMinimized, quickVisible, appActive)
    }

    public func panelTrace(_ stage: String, _ detail: String = "") {
        let cb = state.withLock { $0.callbacks }
        guard let cb else { return }
        let stageBytes = Array(stage.utf8)
        let detailBytes = Array(detail.utf8)
        stageBytes.withUnsafeBufferPointer { sBuf in
            detailBytes.withUnsafeBufferPointer { dBuf in
                cb.panel_trace(sBuf.baseAddress, sBuf.count, dBuf.baseAddress, dBuf.count)
            }
        }
    }

    public func quit() {
        let cb = state.withLock { $0.callbacks }
        cb?.quit()
    }

    // MARK: - Inbound Callbacks (Rust -> Swift)

    fileprivate func handleResponse(requestId: UInt64, ok: Bool, data: Data) {
        // Remove continuation inside lock and release lock BEFORE calling `resume`.
        let continuation = state.withLock { s in
            s.pending.removeValue(forKey: requestId)
        }
        guard let continuation else { return }
        if ok {
            continuation.resume(returning: data)
        } else {
            let failure = try? JSONDecoder().decode(BridgeFailure.self, from: data)
            continuation.resume(throwing: failure.map(RustBridgeError.init(failure:)) ?? RustBridgeError("响应数据解析失败", code: "invalid_response"))
        }
    }

    fileprivate func handleEvent(kindRaw: UInt32, data: Data) {
        guard let kind = EventKind(rawValue: kindRaw) else { return }
        let (seq, hasHandler) = state.withLock { s -> (UInt64, Bool) in
            guard s.eventHandler != nil else { return (0, false) }
            if kind == .opsState {
                s.nextEventSeq &+= 1
                return (s.nextEventSeq, true)
            }
            return (0, true)
        }
        guard hasHandler else { return }
        Self.eventDecodeQueue.async { [self] in
            let event: InboundEvent?
            switch kind {
            case .opsState:
                do {
                    let next = try JSONDecoder().decode(ViewState.self, from: data)
                    event = .opsState(seq: seq, state: next)
                } catch {
                    event = .opsStateDecodeError(error.localizedDescription)
                }
            case .updateResult:
                let msg = String(data: data, encoding: .utf8) ?? ""
                event = msg.isEmpty ? nil : .updateResult(msg)
            case .trayTooltip:
                let tip = String(data: data, encoding: .utf8) ?? ""
                event = tip.isEmpty ? nil : .trayTooltip(tip)
            case .reopen:
                event = .reopen
            }
            guard let event else { return }
            DispatchQueue.main.async { [self] in
                MainActor.assumeIsolated {
                    let handler = self.state.withLock { $0.eventHandler }
                    handler?(event)
                }
            }
        }
    }
}

private func copyBorrowedBytes(ptr: UnsafePointer<UInt8>?, len: Int) -> Data {
    guard len > 0, let ptr else {
        return Data()
    }
    return Data(bytes: ptr, count: len)
}

// MARK: - Exported `@_cdecl` Entry Points

@_cdecl("sub2ops_macos_init")
public func sub2ops_macos_init(
    invoke: @escaping @convention(c) (UInt64, UnsafePointer<UInt8>?, Int, UnsafePointer<UInt8>?, Int) -> Void,
    setVisibility: @escaping @convention(c) (Bool, Bool, Bool, Bool) -> Void,
    panelTrace: @escaping @convention(c) (UnsafePointer<UInt8>?, Int, UnsafePointer<UInt8>?, Int) -> Void,
    quit: @escaping @convention(c) () -> Void
) {
    let callbacks = Sub2OpsHostCallbacks(
        invoke: invoke,
        set_visibility: setVisibility,
        panel_trace: panelTrace,
        quit: quit
    )
    RustBridge.shared.installHostCallbacks(callbacks)
    let silentLaunch = CommandLine.arguments.contains("--autostart")
    if Thread.isMainThread {
        MainActor.assumeIsolated {
            MacHostCoordinator.shared.start(silentLaunch: silentLaunch)
        }
    } else {
        DispatchQueue.main.async {
            MainActor.assumeIsolated {
                MacHostCoordinator.shared.start(silentLaunch: silentLaunch)
            }
        }
    }
}

@_cdecl("sub2ops_macos_on_response")
public func sub2ops_macos_on_response(
    requestId: UInt64,
    ok: Bool,
    ptr: UnsafePointer<UInt8>?,
    len: Int
) {
    let copied = copyBorrowedBytes(ptr: ptr, len: len)
    RustBridge.shared.handleResponse(requestId: requestId, ok: ok, data: copied)
}

@_cdecl("sub2ops_macos_on_event")
public func sub2ops_macos_on_event(
    kind: UInt32,
    ptr: UnsafePointer<UInt8>?,
    len: Int
) {
    let copied = copyBorrowedBytes(ptr: ptr, len: len)
    RustBridge.shared.handleEvent(kindRaw: kind, data: copied)
}

@_cdecl("sub2ops_macos_app_active")
public func sub2ops_macos_app_active() -> Bool {
    if Thread.isMainThread {
        return MainActor.assumeIsolated {
            let active = NSApp?.isActive ?? RustBridge.shared.cachedAppActive()
            RustBridge.shared.updateCachedAppActive(active)
            return active
        }
    }
    return RustBridge.shared.cachedAppActive()
}
