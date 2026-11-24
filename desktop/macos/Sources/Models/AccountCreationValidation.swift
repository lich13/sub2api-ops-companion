import Foundation

extension AccountCreationFormDraft {
    public static let headerOverrideVariables = ["user_id", "api_key_id", "group_id", "request_id"]
    public static let headerOverrideSuggestedNames: [String: String] = [
        "user_id": "x-gateway-user-id",
        "api_key_id": "x-gateway-api-key-id",
        "group_id": "x-gateway-group-id",
        "request_id": "x-gateway-request-id",
    ]

    public static func applyHeaderOverrideVariable(
        _ variable: String,
        into rows: inout [KeyValuePairDraft],
        focusedId: UUID? = nil
    ) {
        let token = "{{\(variable)}}"
        let suggested = headerOverrideSuggestedNames[variable] ?? ""
        if let focusedId, let idx = rows.firstIndex(where: { $0.id == focusedId }) {
            let curKey = trimmed(rows[idx].key)
            if curKey.isEmpty, !suggested.isEmpty,
               !rows.enumerated().contains(where: { $0.offset != idx && trimmed($0.element.key).lowercased() == suggested })
            {
                rows[idx].key = suggested
            }
            rows[idx].value += token
            return
        }
        let existing = rows.first { trimmed($0.key).lowercased() == suggested }
        if let existing, trimmed(existing.value) == token {
            return
        }
        rows.append(KeyValuePairDraft(key: existing == nil ? suggested : "", value: token))
    }

    public static func isValidHeaderOverrideValue(_ raw: String, allowVariables: Bool) -> Bool {
        if !allowVariables {
            return !raw.contains("{{")
        }
        var rest = raw[...]
        while let openRange = rest.range(of: "{{") {
            let afterOpen = rest[openRange.upperBound...]
            guard let closeRange = afterOpen.range(of: "}}") else {
                return false
            }
            let name = afterOpen[..<closeRange.lowerBound].trimmingCharacters(in: .whitespacesAndNewlines)
            if !headerOverrideVariables.contains(name) {
                return false
            }
            rest = afterOpen[closeRange.upperBound...]
        }
        return true
    }

    public func validateStepOne(requireName: Bool = true) -> String {
        if requireName && Self.trimmed(name).isEmpty {
            return "请输入账号名称"
        }
        guard let conc = Int(Self.trimmed(concurrency)), conc >= 1 else {
            return "并发数需为正整数"
        }
        guard let prio = Int(Self.trimmed(priority)), prio >= 0 else {
            return "优先级需为非负整数"
        }
        guard let rate = Double(Self.trimmed(rateMultiplier)), rate.isFinite, rate >= 0 else {
            return "倍率需为非负数"
        }
        let lfTrim = Self.trimmed(loadFactor)
        if !lfTrim.isEmpty {
            guard let lf = Int(lfTrim), lf >= 1 else {
                return "负载因子需为正整数"
            }
        }
        if !Self.trimmed(expiresAtText).isEmpty && Self.parseExpiry(expiresAtText) == nil {
            return "到期时间格式应为 YYYY-MM-DD HH:mm"
        }
        if values["upstream_billing_group_sync_enabled"]?.boolValue == true
            && (type != "apikey" || values["upstream_billing_probe_enabled"]?.boolValue == false)
        {
            return "自动同步上游分组名称与备注需开启自动探测上游倍率"
        }
        return ""
    }

    private func pairProblem(_ field: CreationFieldSpec, _ rows: [KeyValuePairDraft]) -> String {
        let isHeaders = field.kind == "headers"
        if rows.contains(where: { Self.trimmed($0.key).isEmpty || (!isHeaders && Self.trimmed($0.value).isEmpty) }) {
            return "\(field.label)存在未填项目"
        }
        let names = rows.map { isHeaders ? Self.trimmed($0.key).lowercased() : Self.trimmed($0.key) }
        if Set(names).count != names.count {
            return "\(field.label)存在重复名称"
        }
        if !isHeaders {
            let invalid = rows.count > 300 || rows.contains { row in
                let from = Self.trimmed(row.key), to = Self.trimmed(row.value)
                return !Self.matches(from, Self.modelIdPattern) || !Self.matches(to, Self.modelIdPattern)
                    || (to.contains("*") && from != to)
            }
            return invalid ? "\(field.label)需使用有效模型 ID；通配白名单请将请求和转发模型填为相同值" : ""
        }
        let blocked = Set(AccountCreationCatalog.shared.blocked_headers ?? [])
        for row in rows {
            let key = Self.trimmed(row.key)
            let controls = row.value.unicodeScalars.contains { ($0.value < 0x20 && $0.value != 0x09) || $0.value == 0x7f }
            if !Self.matches(key, Self.headerNamePattern) || key.count > 200 || blocked.contains(key.lowercased())
                || controls || row.value.utf8.count > 8192
            {
                return "请求头名称、值或重复项无效，认证头不能覆写"
            }
            if !Self.isValidHeaderOverrideValue(row.value, allowVariables: type == "apikey") {
                return "请求头模板变量无效；仅 API Key 账号支持 {{user_id}}、{{api_key_id}}、{{group_id}}、{{request_id}}"
            }
        }
        return rows.count > 64 ? "请求头最多 64 项" : ""
    }

    private func fieldProblem(_ field: CreationFieldSpec) -> String {
        switch field.kind {
        case "mapping":
            return pairProblem(field, rows(for: field.path))
        case "headers":
            return pairProblem(field, headers)
        case "pause-rules":
            let invalid = pauseRules.contains { rule in
                guard let code = Int(Self.trimmed(rule.errorCode)), (100...599).contains(code),
                      let minutes = Int(Self.trimmed(rule.durationMinutes)), minutes > 0
                else { return true }
                return rule.keywordList.isEmpty || !["model", "account"].contains(rule.scope)
            }
            return invalid ? "临时不可调度规则需包含状态码、关键词和正整数分钟" : ""
        case "protocols":
            for name in protocolNames {
                switch Self.httpURLProblem(protocolURLs[name] ?? "") {
                case .format: return "请填写各协议的有效 API 地址"
                case .scheme: return "协议地址需为 HTTP 或 HTTPS 地址"
                case .none: continue
                }
            }
            return ""
        case "protocol-rules":
            let available = protocolNames
            let invalid = protocolRules.contains { rule in
                Self.trimmed(rule.pattern).isEmpty || !rule.accepted.allSatisfy(available.contains)
            }
            return invalid ? "请填写模型匹配与有效协议" : ""
        default:
            break
        }
        guard let value = values[field.path], value != .string("") else { return "" }
        switch field.kind {
        case "number":
            guard let number = value.doubleValue, number.isFinite,
                  field.min.map({ number >= $0 }) ?? true,
                  field.max.map({ number <= $0 }) ?? true
            else { return "\(field.label)超出有效范围" }
        case "numbers":
            let low = field.min ?? 0, high = field.max ?? .infinity
            guard let list = Self.listNumbers(value),
                  list.allSatisfy({ $0.isFinite && $0.rounded() == $0 && $0 >= low && $0 <= high })
            else { return "\(field.label)需填写有效整数" }
        case "url":
            switch Self.httpURLProblem(value.stringValue ?? "") {
            case .format: return "\(field.label)格式无效"
            case .scheme: return "\(field.label)需为 HTTP 或 HTTPS 地址"
            case .none: break
            }
        default:
            break
        }
        return ""
    }

    /// Full form validation; returns the first problem or an empty string.
    public func validate(requireCredentials: Bool = true, requireName: Bool = true) -> String {
        let stepProblem = validateStepOne(requireName: requireName)
        if !stepProblem.isEmpty { return stepProblem }
        for field in visibleFields {
            let problem = fieldProblem(field)
            if !problem.isEmpty { return problem }
        }
        guard requireCredentials, !isOAuthFlow else { return "" }
        if isVertexApiKey {
            if Self.trimmed(apiKey).isEmpty { return "请输入 API Key" }
            if !(values["credentials.project_id"]?.stringValue ?? "").isEmpty,
               Self.trimmed(values["credentials.location"]?.stringValue ?? "").isEmpty
            {
                return "请输入 Vertex 区域"
            }
        } else if type == "service_account" {
            guard (try? serviceAccountIdentity()) != nil else { return "请选择有效的 Service Account JSON 文件" }
            if (values["credentials.location"]?.stringValue ?? "").isEmpty { return "请输入 Vertex 区域" }
        } else if type == "bedrock" && values["credentials.auth_mode"]?.stringValue == "sigv4" {
            if Self.trimmed(awsAccessKeyId).isEmpty || Self.trimmed(awsSecretAccessKey).isEmpty {
                return "请填写 AWS Access Key ID 和 Secret Access Key"
            }
        } else if Self.trimmed(apiKey).isEmpty {
            return "请输入 API Key"
        }
        if platform == "antigravity" && (values["credentials.base_url"]?.stringValue ?? "").isEmpty {
            return "请输入上游 API 地址"
        }
        return ""
    }

    // MARK: Payload

    func payloadValue(_ field: CreationFieldSpec) -> JSONValue? {
        switch field.kind {
        case "mapping":
            let pairs = rows(for: field.path).map { (Self.trimmed($0.key), JSONValue.string(Self.trimmed($0.value))) }
            return pairs.isEmpty ? nil : .object(Dictionary(pairs, uniquingKeysWith: { _, last in last }))
        case "headers":
            let pairs = headers.map { (Self.trimmed($0.key), JSONValue.string($0.value)) }
            return pairs.isEmpty ? nil : .object(Dictionary(pairs, uniquingKeysWith: { _, last in last }))
        case "pause-rules":
            if pauseRules.isEmpty { return nil }
            return .array(pauseRules.map { rule in
                .object([
                    "error_code": .int(Int64(Self.trimmed(rule.errorCode)) ?? 0),
                    "keywords": .array(rule.keywordList.map(JSONValue.string)),
                    "duration_minutes": .int(Int64(Self.trimmed(rule.durationMinutes)) ?? 0),
                    "description": .string(rule.description),
                    "scope": .string(rule.scope),
                ])
            })
        case "protocols":
            return .object(Dictionary(uniqueKeysWithValues: protocolNames.map {
                ($0, JSONValue.string(Self.trimmed(protocolURLs[$0] ?? "")))
            }))
        case "protocol-rules":
            return .array(protocolRules.map { rule in
                var object: [String: JSONValue] = [
                    "pattern": .string(Self.trimmed(rule.pattern).lowercased()),
                    "protocol": .string(rule.preferred),
                ]
                if rule.accepted.count > 1 {
                    object["protocols"] = .array(rule.accepted.map(JSONValue.string))
                }
                return .object(object)
            })
        default:
            break
        }
        guard let value = values[field.path], value != .null else { return nil }
        if case .string(let text) = value, text.isEmpty, field.kind != "select" { return nil }
        switch field.kind {
        case "numbers":
            return .array((Self.listNumbers(value) ?? []).compactMap { Int64(exactly: $0).map(JSONValue.int) })
        case "stringbool":
            if case .bool(let flag) = value { return .string(flag ? "true" : "false") }
            return value
        default:
            return value
        }
    }
}
