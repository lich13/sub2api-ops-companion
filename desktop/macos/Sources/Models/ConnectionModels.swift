import Foundation

public struct SavedConnection: Codable, Sendable, Equatable, Hashable {
    public var name: String
    public var base_url: String

    public init(name: String, base_url: String) {
        self.name = name
        self.base_url = base_url
    }
}

public struct ModelTestPreference: Codable, Sendable, Equatable {
    public var engine: String
    public var sample_count: Int

    public init(engine: String, sample_count: Int) {
        self.engine = engine
        self.sample_count = sample_count
    }
}

public struct Preferences: Codable, Sendable, Equatable {
    public var base_url: String
    public var connections: [SavedConnection]
    public var pinned: Bool
    public var launch_at_login: Bool
    public var record_columns: [String]?
    public var model_test_concurrency: Int?
    public var model_test_preferences: [String: ModelTestPreference]?

    public init(
        base_url: String = "",
        connections: [SavedConnection] = [],
        pinned: Bool = false,
        launch_at_login: Bool = false,
        record_columns: [String]? = nil,
        model_test_concurrency: Int? = nil,
        model_test_preferences: [String: ModelTestPreference]? = nil
    ) {
        self.base_url = base_url
        self.connections = connections
        self.pinned = pinned
        self.launch_at_login = launch_at_login
        self.record_columns = record_columns
        self.model_test_concurrency = model_test_concurrency
        self.model_test_preferences = model_test_preferences
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        base_url = try c.decodeIfPresent(String.self, forKey: .base_url) ?? ""
        connections = try c.decodeIfPresent([SavedConnection].self, forKey: .connections) ?? []
        pinned = try c.decodeIfPresent(Bool.self, forKey: .pinned) ?? false
        launch_at_login = try c.decodeIfPresent(Bool.self, forKey: .launch_at_login) ?? false
        record_columns = try c.decodeIfPresent([String].self, forKey: .record_columns)
        model_test_concurrency = try c.decodeIfPresent(Int.self, forKey: .model_test_concurrency)
        model_test_preferences = try c.decodeIfPresent([String: ModelTestPreference].self, forKey: .model_test_preferences)
    }
}

public func defaultConnectionName(_ baseUrl: String) -> String {
    let trimmed = baseUrl.trimmingCharacters(in: .whitespacesAndNewlines).replacingOccurrences(of: "/+$", with: "", options: .regularExpression)
    guard !trimmed.isEmpty else { return "未命名实例" }
    guard let parsed = URL(string: trimmed), let host = parsed.host, !host.isEmpty else {
        return trimmed
    }
    if let port = parsed.port {
        return "\(host):\(port)"
    }
    return host
}

public func normalizedSavedConnections(_ preferences: Preferences) -> [SavedConnection] {
    var seen = Set<String>()
    var list: [SavedConnection] = []
    for item in preferences.connections {
        let url = item.base_url.trimmingCharacters(in: .whitespacesAndNewlines).replacingOccurrences(of: "/+$", with: "", options: .regularExpression)
        guard !url.isEmpty, seen.insert(url).inserted else { continue }
        let name = item.name.trimmingCharacters(in: .whitespacesAndNewlines)
        list.append(SavedConnection(name: name.isEmpty ? defaultConnectionName(url) : name, base_url: url))
    }
    let active = preferences.base_url.trimmingCharacters(in: .whitespacesAndNewlines).replacingOccurrences(of: "/+$", with: "", options: .regularExpression)
    if !active.isEmpty && seen.insert(active).inserted {
        list.append(SavedConnection(name: defaultConnectionName(active), base_url: active))
    }
    return list
}

public struct UpstreamConnectionStatus: Codable, Sendable, Equatable {
    public var state: String
    public var endpoint: String
    public var http_status: Int?
    public var message: String
    public var retryable: Bool
    public var checked_at: String?

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        state = try c.decodeIfPresent(String.self, forKey: .state) ?? ""
        endpoint = try c.decodeIfPresent(String.self, forKey: .endpoint) ?? ""
        http_status = try c.decodeIfPresent(Int.self, forKey: .http_status)
        message = try c.decodeIfPresent(String.self, forKey: .message) ?? ""
        retryable = try c.decodeIfPresent(Bool.self, forKey: .retryable) ?? false
        checked_at = try c.decodeIfPresent(String.self, forKey: .checked_at)
    }

    public init(state: String, endpoint: String = "", http_status: Int? = nil, message: String = "", retryable: Bool = false, checked_at: String? = nil) {
        self.state = state
        self.endpoint = endpoint
        self.http_status = http_status
        self.message = message
        self.retryable = retryable
        self.checked_at = checked_at
    }
}

public struct ViewState: Codable, Sendable, Equatable {
    public var platform: String?
    public var foreground: Bool?
    public var initializing: Bool?
    public var connection_revision: UInt64?
    public var connected: Bool
    public var online: Bool
    public var error: String
    public var upstream_connection: UpstreamConnectionStatus?
    public var snapshot: Snapshot?
    public var preferences: Preferences

    public init(
        platform: String? = "macos",
        foreground: Bool? = true,
        initializing: Bool? = false,
        connection_revision: UInt64? = 0,
        connected: Bool = false,
        online: Bool = false,
        error: String = "",
        upstream_connection: UpstreamConnectionStatus? = nil,
        snapshot: Snapshot? = nil,
        preferences: Preferences = Preferences()
    ) {
        self.platform = platform
        self.foreground = foreground
        self.initializing = initializing
        self.connection_revision = connection_revision
        self.connected = connected
        self.online = online
        self.error = error
        self.upstream_connection = upstream_connection
        self.snapshot = snapshot
        self.preferences = preferences
    }
}
