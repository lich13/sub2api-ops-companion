import Foundation

/// Pure Swift YAML subset parser & emitter for `Sub2ApiConfig` structured (`array` / `object`) fields.
/// Supports JSON/flow syntax, block mappings & sequences, scalars, comments, duplicate-key rejection, and expected root validation.
public enum LightYAML {
    public struct ParseResult: Sendable, Equatable {
        public var value: JSONValue?
        public var error: String
    }

    public static func stringify(_ value: JSONValue?, indent: Int = 0) -> String {
        guard let value else { return "" }
        let pad = String(repeating: "  ", count: indent)
        switch value {
        case .null:
            return "null"
        case .bool(let b):
            return b ? "true" : "false"
        case .int(let i):
            return "\(i)"
        case .double(let d):
            return "\(d)"
        case .string(let s):
            if s.isEmpty || s.contains(":") || s.contains("#") || s.contains("\n") || s.hasPrefix(" ") || s.hasSuffix(" ") {
                let escaped = s.replacingOccurrences(of: "\\", with: "\\\\").replacingOccurrences(of: "\"", with: "\\\"")
                return "\"\(escaped)\""
            }
            return s
        case .array(let items):
            if items.isEmpty { return "[]" }
            return items.map { item -> String in
                switch item {
                case .object, .array:
                    let nested = stringify(item, indent: indent + 1)
                    let trimmed = nested.trimmingCharacters(in: .whitespaces)
                    return "\(pad)- \(trimmed)"
                default:
                    return "\(pad)- \(stringify(item, indent: 0))"
                }
            }.joined(separator: "\n")
        case .object(let dict):
            if dict.isEmpty { return "{}" }
            return dict.keys.sorted().map { k -> String in
                let v = dict[k] ?? .null
                switch v {
                case .object(let sub) where !sub.isEmpty:
                    return "\(pad)\(k):\n\(stringify(v, indent: indent + 1))"
                case .array(let arr) where !arr.isEmpty:
                    return "\(pad)\(k):\n\(stringify(v, indent: indent + 1))"
                default:
                    return "\(pad)\(k): \(stringify(v, indent: 0))"
                }
            }.joined(separator: "\n")
        }
    }

    public static func parseStructured(_ text: String, expectedType: String) -> ParseResult {
        let cleanedLines = text.components(separatedBy: .newlines).map(stripComment)
        let joined = cleanedLines.joined(separator: "\n").trimmingCharacters(in: .whitespacesAndNewlines)
        if joined.isEmpty {
            return ParseResult(
                value: nil,
                error: expectedType == "array" ? "需要 YAML 列表" : "需要 YAML 映射"
            )
        }

        // Fast path for flow JSON (`[...]` / `{...}`) with duplicate-key check
        if (joined.hasPrefix("[") && joined.hasSuffix("]")) || (joined.hasPrefix("{") && joined.hasSuffix("}")) {
            if hasDuplicateJSONKeys(joined) {
                return ParseResult(value: nil, error: "YAML 语法无效，请检查缩进和重复字段")
            }
            if let data = joined.data(using: .utf8),
               let decoded = try? JSONDecoder().decode(JSONValue.self, from: data)
            {
                return validateRoot(decoded, expectedType: expectedType)
            }
        }

        let indexed: [(indent: Int, content: String)] = cleanedLines.compactMap { raw in
            let trimmed = raw.trimmingCharacters(in: .whitespaces)
            if trimmed.isEmpty { return nil }
            let indent = raw.prefix(while: { $0 == " " }).count
            if raw.dropFirst(indent).first == "\t" {
                return (-1, trimmed)
            }
            return (indent, trimmed)
        }
        if indexed.contains(where: { $0.indent < 0 }) {
            return ParseResult(value: nil, error: "YAML 语法无效，请检查缩进和重复字段")
        }

        var cursor = 0
        do {
            let val = try parseBlock(lines: indexed, index: &cursor, minIndent: indexed[0].indent)
            if cursor != indexed.count {
                return ParseResult(value: nil, error: "YAML 语法无效，请检查缩进和重复字段")
            }
            return validateRoot(val, expectedType: expectedType)
        } catch {
            return ParseResult(value: nil, error: "YAML 语法无效，请检查缩进和重复字段")
        }
    }

    private static func validateRoot(_ value: JSONValue, expectedType: String) -> ParseResult {
        if expectedType == "array" {
            if case .array = value { return ParseResult(value: value, error: "") }
            return ParseResult(value: nil, error: "需要 YAML 列表")
        } else {
            if case .object = value { return ParseResult(value: value, error: "") }
            return ParseResult(value: nil, error: "需要 YAML 映射")
        }
    }

    private struct YAMLSyntaxError: Error {}

    private static func parseBlock(
        lines: [(indent: Int, content: String)],
        index: inout Int,
        minIndent: Int
    ) throws -> JSONValue {
        guard index < lines.count else { throw YAMLSyntaxError() }
        let baseIndent = lines[index].indent
        if baseIndent < minIndent { throw YAMLSyntaxError() }

        if lines[index].content == "-" || lines[index].content.hasPrefix("- ") {
            var arr: [JSONValue] = []
            while index < lines.count && lines[index].indent == baseIndent {
                let line = lines[index].content
                guard line == "-" || line.hasPrefix("- ") else { throw YAMLSyntaxError() }
                let rest = line == "-" ? "" : String(line.dropFirst(2)).trimmingCharacters(in: .whitespaces)
                index += 1
                if rest.isEmpty {
                    if index < lines.count && lines[index].indent > baseIndent {
                        arr.append(try parseBlock(lines: lines, index: &index, minIndent: baseIndent + 1))
                    } else {
                        arr.append(.null)
                    }
                } else if let (k, vPart) = splitMappingPair(rest) {
                    var obj: [String: JSONValue] = [:]
                    if vPart.isEmpty {
                        if index < lines.count && lines[index].indent > baseIndent {
                            obj[k] = try parseBlock(lines: lines, index: &index, minIndent: baseIndent + 1)
                        } else {
                            obj[k] = .null
                        }
                    } else {
                        obj[k] = try parseScalar(vPart)
                    }
                    while index < lines.count && lines[index].indent > baseIndent {
                        let subLine = lines[index].content
                        guard let (sk, sv) = splitMappingPair(subLine), obj[sk] == nil else {
                            throw YAMLSyntaxError()
                        }
                        let subIndent = lines[index].indent
                        index += 1
                        if sv.isEmpty && index < lines.count && lines[index].indent > subIndent {
                            obj[sk] = try parseBlock(lines: lines, index: &index, minIndent: subIndent + 1)
                        } else {
                            obj[sk] = sv.isEmpty ? .null : try parseScalar(sv)
                        }
                    }
                    arr.append(.object(obj))
                } else {
                    arr.append(try parseScalar(rest))
                }
            }
            return .array(arr)
        } else {
            var map: [String: JSONValue] = [:]
            while index < lines.count && lines[index].indent == baseIndent {
                let line = lines[index].content
                guard let (key, valPart) = splitMappingPair(line), map[key] == nil else {
                    throw YAMLSyntaxError()
                }
                index += 1
                if valPart.isEmpty {
                    if index < lines.count && lines[index].indent > baseIndent {
                        map[key] = try parseBlock(lines: lines, index: &index, minIndent: baseIndent + 1)
                    } else {
                        map[key] = .null
                    }
                } else {
                    map[key] = try parseScalar(valPart)
                }
            }
            return .object(map)
        }
    }

    private static func splitMappingPair(_ s: String) -> (String, String)? {
        var inQuotes: Character? = nil
        for i in s.indices {
            let ch = s[i]
            if let q = inQuotes {
                if ch == q { inQuotes = nil }
            } else if ch == "\"" || ch == "'" {
                inQuotes = ch
            } else if ch == ":" {
                let nextIdx = s.index(after: i)
                if nextIdx == s.endIndex || s[nextIdx] == " " {
                    let rawKey = String(s[..<i]).trimmingCharacters(in: .whitespaces)
                    guard !rawKey.isEmpty else { return nil }
                    let key = unquote(rawKey)
                    let val = nextIdx == s.endIndex ? "" : String(s[nextIdx...]).trimmingCharacters(in: .whitespaces)
                    return (key, val)
                }
            }
        }
        return nil
    }

    private static func parseScalar(_ raw: String) throws -> JSONValue {
        if raw == "[]" { return .array([]) }
        if raw == "{}" { return .object([:]) }
        if (raw.hasPrefix("[") && raw.hasSuffix("]")) || (raw.hasPrefix("{") && raw.hasSuffix("}")) {
            guard let data = raw.data(using: .utf8),
                  let val = try? JSONDecoder().decode(JSONValue.self, from: data)
            else { throw YAMLSyntaxError() }
            return val
        }
        if (raw.hasPrefix("\"") && raw.hasSuffix("\"")) || (raw.hasPrefix("'") && raw.hasSuffix("'")) {
            return .string(unquote(raw))
        }
        if raw.contains(": ") { throw YAMLSyntaxError() }
        switch raw {
        case "null", "~": return .null
        case "true": return .bool(true)
        case "false": return .bool(false)
        default:
            if let i = Int64(raw) { return .int(i) }
            if let d = Double(raw), raw.contains(".") { return .double(d) }
            return .string(raw)
        }
    }

    private static func unquote(_ s: String) -> String {
        guard s.count >= 2 else { return s }
        if (s.hasPrefix("\"") && s.hasSuffix("\"")) || (s.hasPrefix("'") && s.hasSuffix("'")) {
            return String(s.dropFirst().dropLast())
                .replacingOccurrences(of: "\\\"", with: "\"")
                .replacingOccurrences(of: "\\\\", with: "\\")
        }
        return s
    }

    private static func stripComment(_ line: String) -> String {
        var inQuote: Character? = nil
        for i in line.indices {
            let ch = line[i]
            if let q = inQuote {
                if ch == q { inQuote = nil }
            } else if ch == "\"" || ch == "'" {
                inQuote = ch
            } else if ch == "#" {
                if i == line.startIndex || line[line.index(before: i)] == " " {
                    return String(line[..<i])
                }
            }
        }
        return line
    }

    private static func hasDuplicateJSONKeys(_ raw: String) -> Bool {
        guard let data = raw.data(using: .utf8),
              let obj = try? JSONSerialization.jsonObject(with: data)
        else { return false }
        if obj is [String: Any] {
            // Simple top-level key count comparison against serialized key count
            let pattern = try! NSRegularExpression(pattern: #""([^"\\]+)"\s*:"#)
            let range = NSRange(raw.startIndex..<raw.endIndex, in: raw)
            let matches = pattern.matches(in: raw, range: range)
            if !raw.dropFirst().contains("{") {
                let keys = matches.compactMap { m -> String? in
                    guard let r = Range(m.range(at: 1), in: raw) else { return nil }
                    return String(raw[r])
                }
                return Set(keys).count != keys.count
            }
        }
        return false
    }
}
