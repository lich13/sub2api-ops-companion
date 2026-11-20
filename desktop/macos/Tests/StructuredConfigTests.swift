import Foundation
import Testing
@testable import Sub2OpsMacOS

struct StructuredConfigTests {
    @Test func sharedConfigurationFixtures() throws {
        struct Sample: Decodable { let type: String, text: String, value: JSONValue }
        struct Cases: Decodable { let valid: [Sample], invalid: [String] }
        let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        let fixtures = try JSONDecoder().decode(Cases.self, from: Data(contentsOf: root.appending(path: "tests/contracts/structured-config.json")))
        for sample in fixtures.valid {
            #expect(LightYAML.parseStructured(sample.text, expectedType: sample.type).value == sample.value)
            #expect(LightYAML.parseStructured(LightYAML.stringify(sample.value), expectedType: sample.type).value == sample.value)
        }
        for input in fixtures.invalid { #expect(LightYAML.parseStructured(input, expectedType: "array").value == nil) }
    }

    @Test func typedValuesRoundTripThroughTheEditor() {
        let value = JSONValue.object(["key: #\n\"": .array([
            .string("true"), .string("123"), .string("null"), .string("[x]"), .string("quote \"\\\n\t"),
            .double(0.00001), .int(Int64.max), .null, .bool(false)
        ])])
        let parsed = LightYAML.parseStructured(LightYAML.stringify(value), expectedType: "object")
        #expect(parsed.value == value)
        #expect(parsed.error.isEmpty)
    }

    @Test(arguments: [#"[{"a":1,"a":2}]"#, "- nested: {a: 1, \"a\": 2}", "- a: 1\n  a: 2"])
    func nestedDuplicateKeysAreRejected(_ input: String) {
        #expect(LightYAML.parseStructured(input, expectedType: "array").value == nil)
    }
}
