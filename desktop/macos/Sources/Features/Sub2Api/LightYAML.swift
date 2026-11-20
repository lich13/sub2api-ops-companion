import Foundation
import Yams

public enum LightYAML {
    public struct ParseResult: Sendable, Equatable {
        public var value: JSONValue?
        public var error: String
    }

    public static func stringify(_ value: JSONValue?) -> String {
        guard let value else { return "" }
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
        guard let data = try? encoder.encode(value) else { return "" }
        return String(decoding: data, as: UTF8.self)
    }

    // The other configuration editor uses the YAML 1.2 core scalar schema.
    private static let resolver = Resolver.basic
        .appending(.null)
        .appending(try! .init(.bool, "^(?:true|True|TRUE|false|False|FALSE)$"))
        .appending(try! .init(.int, #"^(?:[-+]?[0-9]+|0o[0-7]+|0x[0-9a-fA-F]+)$"#))
        .appending(try! .init(.float, #"^[-+]?(?:(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+)(?:[eE][-+]?[0-9]+)?|\.(?:inf|Inf|INF|nan|NaN|NAN))$"#))

    private struct InvalidValue: Error {}

    public static func parseStructured(_ text: String, expectedType: String) -> ParseResult {
        let rootError = expectedType == "array" ? "需要 YAML 列表" : "需要 YAML 映射"
        do {
            guard let node = try compose(yaml: text, resolver) else { return .init(value: nil, error: rootError) }
            var visits = [Int: Set<Int>]()
            var repeated = 0
            let value = try decode(node, visited: &visits, repeated: &repeated)
            if expectedType == "array" ? value.arrayValue == nil : value.objectValue == nil {
                return .init(value: nil, error: rootError)
            }
            return .init(value: value, error: "")
        } catch {
            return .init(value: nil, error: "YAML 语法无效，请检查缩进和重复字段")
        }
    }

    private static func decode(_ node: Node, visited: inout [Int: Set<Int>], repeated: inout Int) throws -> JSONValue {
        // Composed aliases point back to their source mark. Bound expanded input as in the Web editor.
        if let mark = node.mark, !visited[mark.line, default: []].insert(mark.column).inserted {
            repeated += 1
            guard repeated <= 100 else { throw InvalidValue() }
        }
        switch node {
        case let .mapping(mapping):
            var object = [String: JSONValue]()
            for (key, value) in mapping {
                guard case let .scalar(scalar) = key, object[scalar.string] == nil else { throw InvalidValue() }
                object[scalar.string] = try decode(value, visited: &visited, repeated: &repeated)
            }
            return .object(object)
        case let .sequence(items):
            return .array(try items.map { try decode($0, visited: &visited, repeated: &repeated) })
        case let .scalar(scalar):
            let text = scalar.string
            switch node.tag {
            case Tag(.str): return .string(text)
            case Tag(.null): return .null
            case Tag(.bool):
                guard let value = node.bool else { throw InvalidValue() }
                return .bool(value)
            case Tag(.int):
                let value: Int64?
                if text.hasPrefix("0x") { value = Int64(text.dropFirst(2), radix: 16) }
                else if text.hasPrefix("0o") { value = Int64(text.dropFirst(2), radix: 8) }
                else { value = Int64(text) }
                guard let value else { throw InvalidValue() }
                return .int(value)
            case Tag(.float):
                guard let value = Double(text), value.isFinite else { throw InvalidValue() }
                return .double(value)
            default: throw InvalidValue()
            }
        case .alias: throw InvalidValue()
        }
    }
}
