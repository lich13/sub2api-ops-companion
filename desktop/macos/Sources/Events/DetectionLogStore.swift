import Foundation
import Observation

extension DetectionLog: Identifiable {
    public var id: String { job_id }
}

@MainActor
@Observable
public final class DetectionLogStore {
    public private(set) var items: [DetectionLog] = []
    public private(set) var cursor: String? = nil
    public private(set) var loading: Bool = false
    public private(set) var error: String = ""

    private var connectionKey: String = ""
    private var generation: UInt64 = 0

    public init() {}

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
        let encodedBefore = before.flatMap { $0.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed) }
        let suffix = encodedBefore.map { "&before=\($0)" } ?? ""
        do {
            let res: DetectionLogPageReply = try await RustBridge.shared.api("GET", "/detection-logs?limit=50\(suffix)")
            guard generation == capturedGen && connectionKey == capturedKey else { return }
            if replace {
                items = res.items
            } else {
                var seen = Set(items.map(\.job_id))
                for item in res.items where seen.insert(item.job_id).inserted {
                    items.append(item)
                }
            }
            cursor = res.next_cursor
        } catch {
            if generation == capturedGen {
                self.error = error.localizedDescription
            }
        }
    }
}
