import Foundation

/// Matches `Protocol.swift` & `desktop/src/proxyData.ts`.
public typealias ProxyManagedRow = ProxyRowReply
public typealias ProxyManagedPage = ProxyPageReply
public typealias ProxyCredentialsDTO = ProxyLogin
public typealias ProxyQualityResult = ProxyQualityReply

extension ProxyRowReply: Identifiable {
    public var protocolName: String { self.protocol }

    public var formattedAddress: String {
        let clean = host.replacingOccurrences(of: "^\\[|\\]$", with: "", options: .regularExpression)
        return host.contains(":") ? "[\(clean)]:\(port)" : "\(host):\(port)"
    }

    public func fullURL(credentials: ProxyCredentialsDTO?) -> String {
        if let cred = credentials, (!cred.username.isEmpty || !cred.password.isEmpty) {
            let u = cred.username.addingPercentEncoding(withAllowedCharacters: .urlUserAllowed) ?? cred.username
            let p = cred.password.addingPercentEncoding(withAllowedCharacters: .urlPasswordAllowed) ?? cred.password
            return "\(protocolName)://\(u):\(p)@\(formattedAddress)"
        }
        return "\(protocolName)://\(formattedAddress)"
    }
}

extension ProxyPageReply {
    public static let empty = ProxyPageReply(items: [], page: 1, page_size: 20, total: 0, pages: 0)
}

extension AccountIdentity: Identifiable {}

extension ProxyQualityCheck: Identifiable {
    public var id: String { target }
}

public struct ParsedProxyLine: Sendable, Equatable {
    public var protocolName: String
    public var host: String
    public var port: Int
    public var username: String
    public var password: String

    public var jsonObject: JSONValue {
        .object([
            "protocol": .string(protocolName),
            "host": .string(host),
            "port": .int(Int64(port)),
            "username": .string(username),
            "password": .string(password),
        ])
    }

    public var displayAddress: String {
        let hostPart = host.contains(":") ? "[\(host)]" : host
        return "\(protocolName)://\(hostPart):\(port)"
    }
}

public struct ProxyLineParseOutput: Sendable, Equatable {
    public var proxies: [ParsedProxyLine]
    public var errors: [Int]
    public var duplicates: Int
}

public enum ProxyDataParser {
    private static let lineRegex = try! NSRegularExpression(
        pattern: #"^(https?|socks5h?)://(?:([^@]*)@)?(\[[^\]]+\]|[^:/?#\s]+):(\d+)$"#,
        options: [.caseInsensitive]
    )

    public static func parseProxyLines(_ text: String) -> ProxyLineParseOutput {
        var proxies: [ParsedProxyLine] = []
        var errors: [Int] = []
        var seen = Set<String>()
        var duplicates = 0

        let lines = text.components(separatedBy: .newlines)
        for (idx, raw) in lines.enumerated() {
            let line = raw.trimmingCharacters(in: .whitespacesAndNewlines)
            if line.isEmpty { continue }
            let nsRange = NSRange(line.startIndex..<line.endIndex, in: line)
            guard let match = lineRegex.firstMatch(in: line, options: [], range: nsRange),
                  let protoRange = Range(match.range(at: 1), in: line),
                  let hostRange = Range(match.range(at: 3), in: line),
                  let portRange = Range(match.range(at: 4), in: line),
                  let port = Int(line[portRange]),
                  (1...65535).contains(port)
            else {
                errors.append(idx + 1)
                continue
            }

            let proto = line[protoRange].lowercased()
            let rawHost = String(line[hostRange])
            let cleanHost = rawHost.replacingOccurrences(of: "^\\[|\\]$", with: "", options: .regularExpression)

            var username = ""
            var password = ""
            if match.range(at: 2).location != NSNotFound, let authRange = Range(match.range(at: 2), in: line) {
                let authStr = String(line[authRange])
                let parts = authStr.split(separator: ":", omittingEmptySubsequences: false).map(String.init)
                if let first = parts.first {
                    username = first.removingPercentEncoding ?? first
                    password = parts.dropFirst().joined(separator: ":").removingPercentEncoding
                        ?? parts.dropFirst().joined(separator: ":")
                }
            }

            let parsed = ParsedProxyLine(
                protocolName: proto,
                host: cleanHost,
                port: port,
                username: username,
                password: password
            )
            let dedupKey = "\(parsed.protocolName)|\(parsed.host)|\(parsed.port)|\(parsed.username)|\(parsed.password)"
            if !seen.insert(dedupKey).inserted {
                duplicates += 1
                continue
            }
            proxies.append(parsed)
        }
        return ProxyLineParseOutput(proxies: proxies, errors: errors, duplicates: duplicates)
    }

    public static func parseImport(_ text: String) throws -> JSONValue {
        if text.utf8.count > 8_000_000 {
            throw RustBridgeError("文件超过 8 MB")
        }
        guard let data = text.data(using: .utf8),
              let val = try? JSONDecoder().decode(JSONValue.self, from: data),
              let obj = val.objectValue
        else {
            throw RustBridgeError("JSON 格式无效")
        }
        guard let proxies = obj["proxies"]?.arrayValue, !proxies.isEmpty, proxies.count <= 1000 else {
            throw RustBridgeError("请选择包含 1–1000 个代理的 Sub2API JSON 文件")
        }
        if let accounts = obj["accounts"] {
            guard let arr = accounts.arrayValue, arr.isEmpty else {
                throw RustBridgeError("仅支持代理数据，请从网页端 IP 管理导出")
            }
        }
        return val
    }
}
