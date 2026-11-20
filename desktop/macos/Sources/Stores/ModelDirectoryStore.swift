import Foundation
import Observation

public struct AccountModelItem: Codable, Sendable, Equatable, Identifiable {
    public var id: String
    public var display_name: String

    public init(id: String, display_name: String? = nil) {
        self.id = id
        self.display_name = display_name ?? id
    }

    public init(from decoder: Decoder) throws {
        if let single = try? decoder.singleValueContainer(), let str = try? single.decode(String.self) {
            self.id = str
            self.display_name = str
            return
        }
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let idVal = try c.decode(String.self, forKey: .id)
        self.id = idVal
        self.display_name = (try? c.decodeIfPresent(String.self, forKey: .display_name)) ?? idVal
    }
}

@MainActor
@Observable
public final class ModelDirectoryStore {
    public static let shared = ModelDirectoryStore()

    private var connectionKey: String = ""
    private var cache: [Int: [AccountModelItem]] = [:]

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

    public func readModels(accountId: Int, refresh: Bool = false) async throws -> [AccountModelItem] {
        let capturedKey = connectionKey
        if !refresh, let cached = cache[accountId] {
            return cached
        }
        // The client allowlist and the server only serve the model-test directory.
        let suffix = refresh ? "&refresh=true" : ""
        let records: [ModelOption] = try await RustBridge.shared.api("GET", "/accounts/\(accountId)/models?purpose=model_test\(suffix)")
        let list = records.map { AccountModelItem(id: $0.id, display_name: $0.display_name) }
        if connectionKey == capturedKey {
            cache[accountId] = list
        }
        return list
    }
}
