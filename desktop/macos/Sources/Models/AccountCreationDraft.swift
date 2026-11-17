import Foundation

public struct CreationPlatformSpec: Codable, Sendable, Equatable, Identifiable {
    public var id: String
    public var label: String
    public var types: [String]
    public var base_url: String
}

public struct CreationModelPreset: Codable, Sendable, Equatable, Identifiable {
    public var label: String
    public var from: String
    public var to: String

    public var id: String { "\(label):\(from):\(to)" }
}

public struct CreationProtocolRule: Codable, Sendable, Equatable, Identifiable {
    public var id: UUID = UUID()
    public var pattern: String
    public var protocolName: String
    public var protocols: [String]?

    enum CodingKeys: String, CodingKey {
        case pattern
        case protocolName = "protocol"
        case protocols
    }

    public init(pattern: String = "", protocolName: String = "responses", protocols: [String]? = nil) {
        self.pattern = pattern
        self.protocolName = protocolName
        self.protocols = protocols
    }
}

public struct CreationProviderModeProfile: Codable, Sendable, Equatable {
    public var mode: String
    public var base_urls: [String: String]
    public var protocol_rules: [CreationProtocolRule]?
}

public struct CreationProviderProfile: Codable, Sendable, Equatable {
    public var default_mode: String
    public var routing: String
    public var responses_path: String?
    public var model_catalog: Bool?
    public var modes: [CreationProviderModeProfile]
}

public struct CreationFieldSpec: Codable, Sendable, Equatable, Identifiable {
    public var path: String
    public var label: String
    public var kind: String
    public var group: String
    public var platforms: [String]?
    public var types: [String]?
    public var defaultValue: JSONValue?
    public var choices: [String]?
    public var when: [String: JSONValue]?
    public var min: Double?
    public var max: Double?

    public var id: String {
        "\(path):\(platforms?.joined(separator: ",") ?? "*")"
    }

    enum CodingKeys: String, CodingKey {
        case path, label, kind, group, platforms, types, choices, when, min, max
        case defaultValue = "default"
    }
}

public struct AccountCreationCatalog: Codable, Sendable {
    public var upstream: String
    public var platforms: [CreationPlatformSpec]
    public var fields: [CreationFieldSpec]
    public var models: [String: [String]]?
    public var blocked_headers: [String]?
    public var mapping_presets: [String: [CreationModelPreset]]
    public var provider_profiles: [String: CreationProviderProfile]

    public static let shared: AccountCreationCatalog = {
        let data: Data
        #if SWIFT_PACKAGE
        let url = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .appendingPathComponent("server/src/account_creation_fields.json")
        data = (try? Data(contentsOf: url)) ?? Data("{}".utf8)
        #else
        data = Data(AccountCreationCatalogGenerated.json.utf8)
        #endif
        if let decoded = try? JSONDecoder().decode(AccountCreationCatalog.self, from: data) {
            return decoded
        }
        return AccountCreationCatalog(
            upstream: "",
            platforms: [],
            fields: [],
            models: nil,
            blocked_headers: nil,
            mapping_presets: [:],
            provider_profiles: [:]
        )
    }()
}

public struct OAuthAuthorizationSession: Codable, Sendable, Equatable {
    public var auth_url: String
    public var session_id: String
    public var state: String?
}

public struct ReusableAccountCandidate: Codable, Sendable, Equatable, Identifiable {
    public var id: Int
    public var name: String
    public var platform: String
    public var type: String
    public var status: String?
    public var group_ids: [Int]?
}

public struct KeyValuePairDraft: Sendable, Equatable, Identifiable {
    public var id: UUID = UUID()
    public var key: String
    public var value: String

    public init(key: String = "", value: String = "") {
        self.key = key
        self.value = value
    }
}

public struct AccountCreationFormDraft: Sendable, Equatable {
    public var name: String = ""
    public var notes: String = ""
    public var platform: String = "openai"
    public var type: String = "oauth"
    public var apiKey: String = ""
    public var serviceAccountJSON: String = ""
    public var awsAccessKeyId: String = ""
    public var awsSecretAccessKey: String = ""
    public var awsSessionToken: String = ""
    public var proxyId: Int? = nil
    public var concurrency: String = "5"
    public var priority: String = "1"
    public var rateMultiplier: String = "1"
    public var schedulable: Bool = true
    public var groupIds: Set<Int> = []
    public var values: [String: JSONValue] = [:]
    public var mappings: [KeyValuePairDraft] = []
    public var compactMappings: [KeyValuePairDraft] = []
    public var headers: [KeyValuePairDraft] = []
    public var oauthCredentials: [String: JSONValue]? = nil

    public init() {
        applyPlatformDefaults(for: "openai", authType: "oauth")
    }

    public mutating func selectPlatform(_ nextPlatform: String) {
        let catalog = AccountCreationCatalog.shared
        let spec = catalog.platforms.first(where: { $0.id == nextPlatform })
        let nextType = spec?.types.contains(type) == true ? type : (spec?.types.first ?? "apikey")
        applyPlatformDefaults(for: nextPlatform, authType: nextType)
    }

    public mutating func selectType(_ nextType: String) {
        applyPlatformDefaults(for: platform, authType: nextType)
    }

    public mutating func applyPlatformDefaults(for nextPlatform: String, authType: String) {
        platform = nextPlatform
        type = authType
        oauthCredentials = nil
        var seeded: [String: JSONValue] = [:]
        let catalog = AccountCreationCatalog.shared
        if let platSpec = catalog.platforms.first(where: { $0.id == nextPlatform }),
           authType == "apikey",
           !platSpec.base_url.isEmpty
        {
            seeded["credentials.base_url"] = .string(platSpec.base_url)
        }
        for field in catalog.fields where matchesScope(field) {
            if let defVal = field.defaultValue {
                seeded[field.path] = defVal
            }
        }
        if let profile = catalog.provider_profiles[nextPlatform], authType == "apikey" {
            seeded["credentials.account_mode"] = .string(profile.default_mode)
            if let modeSpec = profile.modes.first(where: { $0.mode == profile.default_mode }) {
                seeded["credentials.api_base_urls"] = .object(modeSpec.base_urls.mapValues(JSONValue.string))
            }
        }
        values = seeded
    }

    public func matchesScope(_ field: CreationFieldSpec) -> Bool {
        if let platforms = field.platforms, !platforms.contains(platform) { return false }
        if let types = field.types, !types.contains(type) { return false }
        return true
    }

    public func isVisible(_ field: CreationFieldSpec) -> Bool {
        guard matchesScope(field) else { return false }
        guard let conditions = field.when else { return true }
        for (depPath, expected) in conditions {
            let actual = values[depPath] ?? .null
            if actual != expected { return false }
        }
        return true
    }

    public var visibleFields: [CreationFieldSpec] {
        var seen = Set<String>()
        return AccountCreationCatalog.shared.fields.filter { field in
            guard isVisible(field) else { return false }
            return seen.insert(field.path).inserted
        }
    }

    public func buildPayload() throws -> [String: JSONValue] {
        let trimmedName = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmedName.isEmpty else {
            throw RustBridgeError("请填写账号名称")
        }
        guard let conc = Int(concurrency), conc >= 1 else {
            throw RustBridgeError("并发数必须为正整数")
        }
        guard let prio = Int(priority), prio >= 1 else {
            throw RustBridgeError("优先级必须为正整数")
        }
        guard let rate = Double(rateMultiplier), rate >= 0 else {
            throw RustBridgeError("倍率必须为非负数值")
        }

        var credentials: [String: JSONValue] = oauthCredentials ?? [:]
        var extra: [String: JSONValue] = [:]
        var root: [String: JSONValue] = [
            "name": .string(trimmedName),
            "platform": .string(platform),
            "type": .string(type),
            "concurrency": .int(Int64(conc)),
            "priority": .int(Int64(prio)),
            "rate_multiplier": .double(rate),
            "schedulable": .bool(schedulable),
            "group_ids": .array(groupIds.sorted().map { .int(Int64($0)) }),
        ]
        if !notes.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            root["notes"] = .string(notes.trimmingCharacters(in: .whitespacesAndNewlines))
        }
        if let proxyId {
            root["proxy_id"] = .int(Int64(proxyId))
        }

        if type == "apikey" || type == "setup-token" {
            let trimmedKey = apiKey.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !trimmedKey.isEmpty else {
                throw RustBridgeError(type == "setup-token" ? "请填写 Setup Token" : "请填写 API Key")
            }
            credentials[type == "setup-token" ? "setup_token" : "api_key"] = .string(trimmedKey)
        } else if type == "oauth" && credentials.isEmpty {
            throw RustBridgeError("请先完成 OAuth 授权交换")
        } else if type == "service_account" {
            let trimmedSA = serviceAccountJSON.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !trimmedSA.isEmpty else {
                throw RustBridgeError("请粘贴 Service Account JSON")
            }
            credentials["service_account_json"] = .string(trimmedSA)
        } else if type == "bedrock" {
            let mode = values["credentials.auth_mode"]?.stringValue ?? "sigv4"
            if mode == "apikey" {
                let k = apiKey.trimmingCharacters(in: .whitespacesAndNewlines)
                guard !k.isEmpty else { throw RustBridgeError("请填写 Bedrock API Key") }
                credentials["api_key"] = .string(k)
            } else {
                let ak = awsAccessKeyId.trimmingCharacters(in: .whitespacesAndNewlines)
                let sk = awsSecretAccessKey.trimmingCharacters(in: .whitespacesAndNewlines)
                guard !ak.isEmpty && !sk.isEmpty else {
                    throw RustBridgeError("请填写 AWS Access Key 与 Secret Key")
                }
                credentials["aws_access_key_id"] = .string(ak)
                credentials["aws_secret_access_key"] = .string(sk)
                if !awsSessionToken.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                    credentials["aws_session_token"] = .string(awsSessionToken.trimmingCharacters(in: .whitespacesAndNewlines))
                }
            }
        }

        for field in visibleFields {
            guard let val = values[field.path], val != .null else { continue }
            if case .string(let s) = val, s.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                continue
            }
            if field.path.hasPrefix("credentials.") {
                let sub = String(field.path.dropFirst("credentials.".count))
                credentials[sub] = val
            } else if field.path.hasPrefix("extra.") {
                let sub = String(field.path.dropFirst("extra.".count))
                extra[sub] = val
            } else {
                root[field.path] = val
            }
        }

        let mapDict = Dictionary(
            mappings.compactMap { pair -> (String, JSONValue)? in
                let k = pair.key.trimmingCharacters(in: .whitespacesAndNewlines)
                let v = pair.value.trimmingCharacters(in: .whitespacesAndNewlines)
                return (!k.isEmpty && !v.isEmpty) ? (k, .string(v)) : nil
            },
            uniquingKeysWith: { _, last in last }
        )
        if !mapDict.isEmpty { credentials["model_mapping"] = .object(mapDict) }

        let compactDict = Dictionary(
            compactMappings.compactMap { pair -> (String, JSONValue)? in
                let k = pair.key.trimmingCharacters(in: .whitespacesAndNewlines)
                let v = pair.value.trimmingCharacters(in: .whitespacesAndNewlines)
                return (!k.isEmpty && !v.isEmpty) ? (k, .string(v)) : nil
            },
            uniquingKeysWith: { _, last in last }
        )
        if !compactDict.isEmpty { credentials["compact_model_mapping"] = .object(compactDict) }

        let headerDict = Dictionary(
            headers.compactMap { pair -> (String, JSONValue)? in
                let k = pair.key.trimmingCharacters(in: .whitespacesAndNewlines)
                return !k.isEmpty ? (k, .string(pair.value)) : nil
            },
            uniquingKeysWith: { _, last in last }
        )
        if !headerDict.isEmpty { credentials["header_overrides"] = .object(headerDict) }

        root["credentials"] = .object(credentials)
        if !extra.isEmpty { root["extra"] = .object(extra) }
        return root
    }
}
