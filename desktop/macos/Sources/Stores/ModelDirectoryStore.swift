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

private struct AccountModelsEnvelope: Decodable, Sendable {
    var models: [AccountModelItem]?
    var items: [AccountModelItem]?
    var candidates: [AccountModelItem]?
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
        let msg = error.localizedDescription
        return msg.contains("401") || msg.contains("403") || msg.contains("认证") || msg.contains("失效")
    }

    public func readModels(accountId: Int, refresh: Bool = false) async throws -> [AccountModelItem] {
        let capturedKey = connectionKey
        if !refresh, let cached = cache[accountId] {
            return cached
        }
        // The client allowlist and the server only serve the model-test directory.
        let suffix = refresh ? "&refresh=true" : ""
        let raw: JSONValue = try await RustBridge.shared.api("GET", "/accounts/\(accountId)/models?purpose=model_test\(suffix)")
        let data = try JSONEncoder().encode(raw)
        let list: [AccountModelItem]
        if let direct = try? JSONDecoder().decode([AccountModelItem].self, from: data) {
            list = direct
        } else if let env = try? JSONDecoder().decode(AccountModelsEnvelope.self, from: data) {
            list = env.models ?? env.items ?? env.candidates ?? []
        } else {
            list = []
        }
        if connectionKey == capturedKey {
            cache[accountId] = list
        }
        return list
    }
}
