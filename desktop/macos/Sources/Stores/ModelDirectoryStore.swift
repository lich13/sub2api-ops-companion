import Foundation
import Observation

extension ModelOption: Identifiable {
    public var titleText: String {
        let d = display_name ?? ""
        return d.isEmpty ? id : d
    }
}

@MainActor
@Observable
public final class ModelDirectoryStore {
    public static let shared = ModelDirectoryStore()

    private var connectionKey: String = ""
    private var cache: [Int: [ModelOption]] = [:]

    public init() {}

    public func bindConnection(_ key: String) {
        guard connectionKey != key else { return }
        connectionKey = key
        cache.removeAll()
    }

    public static func isRevoked(_ error: Error) -> Bool {
        guard let code = (error as? RustBridgeError)?.failure.code else { return false }
        return code == "auth_rejected" || code == "model_directory_revoked"
    }

    public func readModels(accountId: Int, refresh: Bool = false) async throws -> [ModelOption] {
        let capturedKey = connectionKey
        if !refresh, let cached = cache[accountId] {
            return cached
        }
        // The client allowlist and the server only serve the model-test directory.
        let suffix = refresh ? "&refresh=true" : ""
        let list: [ModelOption] = try await RustBridge.shared.api("GET", "/accounts/\(accountId)/models?purpose=model_test\(suffix)")
        if connectionKey == capturedKey {
            cache[accountId] = list
        }
        return list
    }
}
