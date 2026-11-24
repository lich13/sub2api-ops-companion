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

public struct CreationURLPreset: Codable, Sendable, Equatable {
    public var mode: String?
    public var protocolName: String?
    public var label: String
    public var url: String

    enum CodingKeys: String, CodingKey {
        case mode, label, url
        case protocolName = "protocol"
    }
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
    public var url_presets: [String: [CreationURLPreset]]? = nil

    public static let choiceLabels: [String: String] = [
        "service_account": "Service Account JSON",
        "model": "模型",
        "account": "账号",
        "sigv4": "AWS Signature V4",
        "apikey": "API Key",
        "payg": "按量付费",
        "coding": "Coding Plan",
        "zen": "Zen",
        "go": "Go",
        "adaptive": "自动选择协议",
        "chat_completions": "Chat Completions",
        "anthropic": "Anthropic",
        "responses": "Responses",
        "code_assist": "GCP Code Assist",
        "google_one": "Google One",
        "ai_studio": "AI Studio",
        "google_one_free": "Google One 免费",
        "google_ai_pro": "Google AI Pro",
        "google_ai_ultra": "Google AI Ultra",
        "gcp_standard": "GCP Standard",
        "gcp_enterprise": "GCP Enterprise",
        "aistudio_free": "AI Studio 免费",
        "aistudio_paid": "AI Studio 付费",
        "rolling": "滚动窗口",
        "fixed": "固定值",
        "percentage": "百分比",
        "off": "关闭",
        "device": "设备",
        "session": "会话",
        "full": "完整",
        "auto": "自动",
        "force_on": "强制开启",
        "force_off": "强制关闭",
        "force_responses": "强制 Responses",
        "force_chat_completions": "强制 Chat Completions",
        "ctx_pool": "上下文池",
        "passthrough": "透传",
        "http_bridge": "HTTP Bridge",
        "enabled": "开启",
        "disabled": "关闭",
        "default": "跟随全局",
        "x_api_key": "X-API-Key",
        "authorization_bearer": "Authorization Bearer",
        "tiered": "分层",
        "sticky_exempt": "粘性豁免",
        "throttle": "节流",
        "serialize": "串行",
        "embeddings": "Embeddings",
        "images": "Images",
        "seedance": "Seedance (Ark)",
    ]

    public static func choiceLabel(_ raw: String) -> String {
        if raw.isEmpty { return "关闭" }
        return choiceLabels[raw] ?? raw
    }

    public static let shared: AccountCreationCatalog = {
        let data = Data(AccountCreationCatalogGenerated.json.utf8)
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

extension ChoiceRecord: Identifiable {}
