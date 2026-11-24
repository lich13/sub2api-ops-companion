import Foundation

extension AccountCreationFormDraft {
    /// Builds the `/account-creation` request body. Throws the first validation problem.
    public func buildPayload(
        identityCredentials: [String: JSONValue]? = nil,
        identityExtra: [String: JSONValue]? = nil,
        requireCredentials: Bool = true,
        requireName: Bool = true
    ) throws -> [String: JSONValue] {
        let problem = validate(
            requireCredentials: requireCredentials && identityCredentials == nil,
            requireName: requireName
        )
        if !problem.isEmpty {
            throw RustBridgeError(problem)
        }

        var root: [String: JSONValue] = [
            "name": .string(Self.trimmed(name)),
            "notes": .string(Self.trimmed(notes)),
            "platform": .string(platform),
            "type": .string(type),
            "proxy_id": proxyId.map { .int(Int64($0)) } ?? .null,
            "concurrency": .int(Int64(Self.trimmed(concurrency)) ?? 1),
            "load_factor": Int64(Self.trimmed(loadFactor)).map(JSONValue.int) ?? .null,
            "priority": .int(Int64(Self.trimmed(priority)) ?? 1),
            "rate_multiplier": .double(Double(Self.trimmed(rateMultiplier)) ?? 1),
            "group_ids": .array(groupIds.sorted().map { .int(Int64($0)) }),
            "expires_at": Self.parseExpiry(expiresAtText).map(JSONValue.int) ?? .null,
        ]
        var credentials: [String: JSONValue] = [:]
        var extra: [String: JSONValue] = identityExtra ?? [:]

        for field in visibleFields {
            guard let value = payloadValue(field) else { continue }
            if field.path.hasPrefix("credentials.") {
                credentials[String(field.path.dropFirst("credentials.".count))] = value
            } else if field.path.hasPrefix("extra.") {
                extra[String(field.path.dropFirst("extra.".count))] = value
            } else {
                root[field.path] = value
            }
        }

        if !isOAuthFlow {
            let secrets = [
                ("api_key", apiKey),
                ("aws_access_key_id", awsAccessKeyId),
                ("aws_secret_access_key", awsSecretAccessKey),
                ("aws_session_token", awsSessionToken),
                ("service_account_json", serviceAccountJSON),
            ]
            for (key, raw) in secrets where !Self.trimmed(raw).isEmpty {
                credentials[key] = .string(Self.trimmed(raw))
            }
        }

        if type == "service_account" {
            credentials["tier_id"] = .string("vertex")
            if isVertexApiKey {
                credentials.removeValue(forKey: "service_account_json")
                let location = Self.trimmed(values["credentials.location"]?.stringValue ?? "")
                credentials["location"] = .string(location.isEmpty ? "global" : location)
            } else {
                let identity = try serviceAccountIdentity()
                credentials["client_email"] = identity.email
                credentials["project_id"] = identity.project
                credentials.removeValue(forKey: "auth_mode")
                credentials.removeValue(forKey: "api_key")
            }
        }
        if platform == "gemini" && type == "apikey" {
            if Self.isOfficialGemini(values["credentials.base_url"]?.stringValue) {
                credentials["tier_id"] = .string("aistudio_free")
            }
            if credentials["explicit_cache_upstream"]?.boolValue != true {
                credentials.removeValue(forKey: "explicit_cache_upstream")
                credentials.removeValue(forKey: "explicit_cache_upstream_max_ttl_seconds")
            } else if let ttl = credentials["explicit_cache_upstream_max_ttl_seconds"]?.doubleValue {
                credentials["explicit_cache_upstream_max_ttl_seconds"] = .int(Int64(ttl.rounded(.down)))
            }
        }
        if platform == "openai" {
            let selected = values["extra.openai_image_size_tiers"]?.arrayValue ?? []
            let tiers = Self.imageTiers.filter { selected.contains(.string($0)) }
            if tiers.isEmpty || tiers.count == Self.imageTiers.count {
                extra.removeValue(forKey: "openai_image_size_tiers")
            } else {
                extra["openai_image_size_tiers"] = .array(tiers.map(JSONValue.string))
            }
            if extra["openai_passthrough"]?.boolValue != true || extra["openai_passthrough_model_filter"]?.boolValue != true {
                extra.removeValue(forKey: "openai_passthrough_model_filter")
            }
        }
        if root["upstream_billing_group_sync_enabled"]?.boolValue != true {
            root.removeValue(forKey: "upstream_billing_group_sync_enabled")
        }
        if type == "bedrock" {
            if credentials["auth_mode"]?.stringValue == "sigv4" {
                credentials.removeValue(forKey: "api_key")
            } else if credentials["auth_mode"]?.stringValue == "apikey" {
                for key in ["aws_access_key_id", "aws_secret_access_key", "aws_session_token"] {
                    credentials.removeValue(forKey: key)
                }
            }
        }
        if isMultiProtocol {
            if let mode = values["credentials.account_mode"] {
                credentials["account_mode"] = mode
            }
            if credentials["api_protocol"]?.stringValue == "adaptive",
               let chat = credentials["api_base_urls"]?["chat_completions"]
            {
                credentials["base_url"] = chat
            }
        }
        for (key, value) in identityCredentials ?? [:] {
            credentials[key] = value
        }
        if platform == "openai" {
            let mode = extra["openai_\(type)_responses_websockets_v2_mode"]?.stringValue ?? ""
            extra["openai_\(type)_responses_websockets_v2_enabled"] = .bool(!mode.isEmpty && mode != "off")
        }

        root["credentials"] = .object(credentials)
        root["extra"] = .object(extra)
        return root
    }

    /// Shared parameters of the native import endpoints: the creation body without
    /// platform/type, with the configuration credentials moved to `credential_extras`.
    public func importParameters() throws -> [String: JSONValue] {
        var body = try buildPayload(requireCredentials: false, requireName: false)
        body.removeValue(forKey: "platform")
        body.removeValue(forKey: "type")
        body["credential_extras"] = body.removeValue(forKey: "credentials") ?? .object([:])
        return body
    }

    // MARK: Reuse

    /// Replaces the configuration with a source account's, keeping name and newly typed secrets.
    public mutating func applyReusedSource(_ source: [String: JSONValue]) throws {
        guard source["platform"]?.stringValue == platform,
              source["type"]?.stringValue == type
        else {
            throw RustBridgeError("来源账号平台或类型已变化，请重新选择")
        }
        if let value = source["notes"] { notes = value.stringValue ?? "" }
        if let value = source["proxy_id"] { proxyId = value.intValue.map(Int.init) }
        if let value = source["concurrency"]?.intValue { concurrency = String(value) }
        if let value = source["load_factor"] { loadFactor = value.intValue.map { String($0) } ?? "" }
        if let value = source["priority"]?.intValue { priority = String(value) }
        if let value = source["rate_multiplier"]?.doubleValue {
            rateMultiplier = value == value.rounded() ? String(Int64(value)) : String(value)
        }
        if let value = source["group_ids"]?.arrayValue {
            groupIds = Set(value.compactMap { $0.intValue.map(Int.init) })
        }
        if let value = source["expires_at"] {
            expiresAtText = value.intValue.map(Self.formatExpiry) ?? ""
        }

        let credentials = source["credentials"]?.objectValue ?? [:]
        let extra = source["extra"]?.objectValue ?? [:]
        func pairs(_ value: JSONValue?) -> [KeyValuePairDraft] {
            let object = value?.objectValue ?? [:]
            return object.keys.sorted().map { KeyValuePairDraft(key: $0, value: object[$0]?.stringValue ?? "") }
        }
        for field in AccountCreationCatalog.shared.fields where matchesScope(field) {
            let value: JSONValue?
            if field.path.hasPrefix("credentials.") {
                value = credentials[String(field.path.dropFirst("credentials.".count))]
            } else if field.path.hasPrefix("extra.") {
                value = extra[String(field.path.dropFirst("extra.".count))]
            } else {
                value = source[field.path]
            }
            switch field.kind {
            case "mapping":
                if field.path == "credentials.compact_model_mapping" {
                    compactMappings = pairs(value)
                } else {
                    mappings = pairs(value)
                }
            case "headers":
                headers = pairs(value)
            case "pause-rules":
                pauseRules = (value?.arrayValue ?? []).map { rule in
                    PauseRuleDraft(
                        scope: rule["scope"]?.stringValue ?? "account",
                        errorCode: rule["error_code"]?.intValue.map { String($0) } ?? "",
                        durationMinutes: rule["duration_minutes"]?.intValue.map { String($0) } ?? "",
                        keywords: (rule["keywords"]?.arrayValue ?? []).compactMap(\.stringValue).joined(separator: ","),
                        description: rule["description"]?.stringValue ?? ""
                    )
                }
            case "protocols":
                protocolURLs = (value?.objectValue ?? [:]).compactMapValues(\.stringValue)
            case "protocol-rules":
                protocolRules = (value?.arrayValue ?? []).map { rule in
                    let accepted = (rule["protocols"]?.arrayValue ?? []).compactMap(\.stringValue)
                    let preferred = rule["protocol"]?.stringValue ?? accepted.first ?? ""
                    return ProtocolRuleDraft(
                        pattern: rule["pattern"]?.stringValue ?? "",
                        preferred: preferred,
                        also: accepted.filter { $0 != preferred }
                    )
                }
            default:
                if let value, value != .null {
                    values[field.path] = value
                } else {
                    values.removeValue(forKey: field.path)
                }
            }
        }
        values.removeValue(forKey: "upstream_billing_group_sync_enabled")
        if platform == "openai", (values["extra.openai_image_size_tiers"]?.arrayValue ?? []).isEmpty {
            values["extra.openai_image_size_tiers"] = .array(Self.imageTiers.map(JSONValue.string))
        }
    }

    // MARK: OAuth identity

    public static func extractTokenIdentity(
        platform: String,
        token: [String: JSONValue],
        fallbackRefresh: String? = nil
    ) -> (credentials: [String: JSONValue], extra: [String: JSONValue]) {
        let keys = [
            "access_token", "refresh_token", "id_token", "token_type", "expires_at",
            "scope", "email", "email_address", "org_uuid", "account_uuid",
            "chatgpt_account_id", "chatgpt_user_id", "organization_id", "plan_type",
            "subscription_expires_at", "client_id", "project_id", "oauth_type",
            "tier_id", "sub", "team_id", "subscription_tier", "entitlement_status",
        ]
        var credentials: [String: JSONValue] = [:]
        for key in keys {
            guard let val = token[key], val != .null else { continue }
            if case .string(let s) = val, s.isEmpty { continue }
            credentials[key] = val
        }
        if let fallbackRefresh, !fallbackRefresh.isEmpty, credentials["refresh_token"] == nil {
            credentials["refresh_token"] = .string(fallbackRefresh)
        }
        if ["gemini", "antigravity"].contains(platform), let exp = credentials["expires_at"] {
            credentials["expires_at"] = .string(exp.displayText)
        }
        var extra: [String: JSONValue] = [:]
        if platform == "gemini", let nestedExtra = token["extra"]?.objectValue {
            for k in ["drive_storage_limit", "drive_storage_usage", "drive_tier_updated_at"] {
                if let val = nestedExtra[k] { extra[k] = val }
            }
        }
        for k in ["email", "email_address", "org_uuid", "account_uuid", "subscription_tier", "entitlement_status", "name", "privacy_mode"] {
            if let val = token[k], val != .null, val != .bool(false), val != .string(""), val != .int(0) {
                extra[k] = val
            }
        }
        return (credentials, extra)
    }
}
