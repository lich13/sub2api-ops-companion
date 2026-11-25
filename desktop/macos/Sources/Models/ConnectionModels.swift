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

public struct CachedSnapshotContext: Sendable {
    public let connectionKey: String
    public let snapshotRevision: UInt64
    public let snapshot: Snapshot

    public init(connectionKey: String, snapshotRevision: UInt64, snapshot: Snapshot) {
        self.connectionKey = connectionKey
        self.snapshotRevision = snapshotRevision
        self.snapshot = snapshot
    }
}

private struct SnapshotObservedHeader: Decodable {
    let observed_at: String?
}

public struct ViewState: Codable, Sendable, Equatable {
    public static let cachedSnapshotUserInfoKey = CodingUserInfoKey(rawValue: "sub2ops.cachedSnapshot")!

    public var platform: String?
    public var foreground: Bool?
    public var initializing: Bool?
    public var connection_revision: UInt64?
    public var snapshot_revision: UInt64?
    public var connected: Bool
    public var online: Bool
    public var error: String
    public var upstream_connection: ConnectionReply?
    public var snapshot: Snapshot?
    public var preferences: Preferences

    private enum CodingKeys: String, CodingKey {
        case platform, foreground, initializing, connection_revision, snapshot_revision
        case connected, online, error, upstream_connection, snapshot, preferences
    }

    public init(
        platform: String? = "macos",
        foreground: Bool? = true,
        initializing: Bool? = false,
        connection_revision: UInt64? = 0,
        snapshot_revision: UInt64? = nil,
        connected: Bool = false,
        online: Bool = false,
        error: String = "",
        upstream_connection: ConnectionReply? = nil,
        snapshot: Snapshot? = nil,
        preferences: Preferences = Preferences()
    ) {
        self.platform = platform
        self.foreground = foreground
        self.initializing = initializing
        self.connection_revision = connection_revision
        self.snapshot_revision = snapshot_revision
        self.connected = connected
        self.online = online
        self.error = error
        self.upstream_connection = upstream_connection
        self.snapshot = snapshot
        self.preferences = preferences
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.platform = try container.decodeIfPresent(String.self, forKey: .platform)
        self.foreground = try container.decodeIfPresent(Bool.self, forKey: .foreground)
        self.initializing = try container.decodeIfPresent(Bool.self, forKey: .initializing)
        self.connection_revision = try container.decodeIfPresent(UInt64.self, forKey: .connection_revision)
        self.snapshot_revision = try container.decodeIfPresent(UInt64.self, forKey: .snapshot_revision)
        self.connected = try container.decode(Bool.self, forKey: .connected)
        self.online = try container.decode(Bool.self, forKey: .online)
        self.error = try container.decode(String.self, forKey: .error)
        self.upstream_connection = try container.decodeIfPresent(ConnectionReply.self, forKey: .upstream_connection)
        let prefs = try container.decode(Preferences.self, forKey: .preferences)
        self.preferences = prefs

        let connKey = "\(self.connection_revision ?? 0)|\(prefs.base_url)"
        if let rev = self.snapshot_revision,
           rev != 0,
           let cached = decoder.userInfo[Self.cachedSnapshotUserInfoKey] as? CachedSnapshotContext,
           cached.connectionKey == connKey,
           cached.snapshotRevision == rev,
           container.contains(.snapshot),
           (try? container.decodeNil(forKey: .snapshot)) == false
        {
            if let obs = (try? container.decode(SnapshotObservedHeader.self, forKey: .snapshot))?.observed_at,
               obs != cached.snapshot.observed_at
            {
                self.snapshot = Snapshot(
                    observed_at: obs,
                    accounts: cached.snapshot.accounts,
                    groups: cached.snapshot.groups,
                    errors: cached.snapshot.errors,
                    recoveries: cached.snapshot.recoveries,
                    schema_version: cached.snapshot.schema_version
                )
            } else {
                self.snapshot = cached.snapshot
            }
        } else {
            self.snapshot = try container.decodeIfPresent(Snapshot.self, forKey: .snapshot)
        }
    }
}
