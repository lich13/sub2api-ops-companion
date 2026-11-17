import Foundation
import Observation

public enum ErrorCategoryKey: String, CaseIterable, Sendable {
    case degradation
    case other
}

private struct ErrorPageEnvelope: Decodable, Sendable {
    var items: [OpsError]
    var next_cursor: Int?
}

@MainActor
@Observable
public final class ErrorFeedStore {
    public let category: ErrorCategoryKey
    public private(set) var items: [OpsError] = []
    public private(set) var cursor: Int? = nil
    public private(set) var loading: Bool = false
    public private(set) var error: String = ""

    private var connectionKey: String = ""
    private var generation: UInt64 = 0

    public init(category: ErrorCategoryKey) {
        self.category = category
    }

    public func bind(connectionKey key: String) {
        guard connectionKey != key else { return }
        connectionKey = key
        generation &+= 1
        items = []
        cursor = nil
        loading = false
        error = ""
    }

    public func load(replace: Bool = true, online: Bool) async {
        guard online else { return }
        let before = replace ? nil : cursor
        if !replace && before == nil { return }
        generation &+= 1
        let capturedGen = generation
        let capturedKey = connectionKey
        loading = true
        error = ""
        defer {
            if generation == capturedGen { loading = false }
        }
        let suffix = before.map { "&before_id=\($0)" } ?? ""
        do {
            let res: ErrorPageEnvelope = try await RustBridge.shared.api(
                "GET",
                "/errors?category=\(category.rawValue)\(suffix)"
            )
            guard generation == capturedGen && connectionKey == capturedKey else { return }
            let base = replace ? [] : items
            var byId: [Int: OpsError] = [:]
            for item in base + res.items {
                byId[item.id] = item
            }
            items = byId.values.sorted { $0.id > $1.id }
            cursor = res.next_cursor
        } catch {
            if generation == capturedGen {
                self.error = error.localizedDescription
            }
        }
    }
}
