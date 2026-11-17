import Foundation
import Observation

@MainActor
@Observable
public final class RecordFeedStore {
    public private(set) var items: [UsageRecord] = []
    public private(set) var cursor: String? = nil
    public private(set) var latestId: Int? = nil
    public private(set) var newCount: Int = 0
    public private(set) var observedAt: String = ""
    public private(set) var totalCost: String? = nil
    public private(set) var loading: Bool = false
    public private(set) var error: String = ""
    public private(set) var pageError: String = ""

    private var activeQuery: String = ""
    private var epoch: UInt64 = 0
    private var pageRevision: UInt64 = 0
    private var consumedCursors: Set<String> = []
    private var pollTask: Task<Void, Never>?

    public init() {}

    public func bind(query: String, active: Bool, holding: Bool) {
        if activeQuery != query {
            activeQuery = query
            epoch &+= 1
            items = []
            cursor = nil
            latestId = nil
            newCount = 0
            observedAt = ""
            totalCost = nil
            loading = false
            error = ""
            pageError = ""
            consumedCursors.removeAll()
        }
        pollTask?.cancel()
        guard active else { return }
        let capturedEpoch = epoch
        let capturedQuery = query
        pollTask = Task { @MainActor [weak self] in
            guard let self else { return }
            var failures = 0
            while !Task.isCancelled && self.epoch == capturedEpoch && self.activeQuery == capturedQuery {
                let ok: Bool
                if self.latestId == nil {
                    ok = await self.loadHead(capturedEpoch: capturedEpoch, query: capturedQuery)
                } else {
                    ok = await self.pollNewRecords(capturedEpoch: capturedEpoch, query: capturedQuery, holding: holding)
                }
                failures = ok ? 0 : (failures + 1)
                let delaySeconds = min(60, 10 * (1 << min(failures, 3)))
                try? await Task.sleep(for: .seconds(delaySeconds))
            }
        }
    }

    public func stop() {
        epoch &+= 1
        pollTask?.cancel()
        pollTask = nil
    }

    @discardableResult
    public func refresh() async -> Bool {
        guard !activeQuery.isEmpty else { return false }
        return await loadHead(capturedEpoch: epoch, query: activeQuery)
    }

    public func loadMore(retry: Bool = false) async {
        guard let nextCursor = cursor, !loading else { return }
        if !pageError.isEmpty && !retry { return }
        let capturedEpoch = epoch
        let capturedRev = pageRevision
        let capturedQuery = activeQuery
        loading = true
        pageError = ""
        defer {
            if epoch == capturedEpoch { loading = false }
        }
        do {
            let encodedCursor = nextCursor.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed) ?? nextCursor
            let page: RecordPageEnvelope = try await RustBridge.shared.api(
                "GET",
                "/usage-records?\(capturedQuery)&cursor=\(encodedCursor)"
            )
            guard epoch == capturedEpoch && pageRevision == capturedRev && activeQuery == capturedQuery else { return }
            var seen = Set(items.map(\.id))
            var merged = items
            for row in page.items where seen.insert(row.id).inserted {
                merged.append(row)
            }
            let stalled = page.next_cursor != nil && (
                page.next_cursor == nextCursor
                || consumedCursors.contains("\(capturedRev):\(page.next_cursor!)")
                || merged.count == items.count
            )
            if !stalled {
                consumedCursors.insert("\(capturedRev):\(nextCursor)")
            }
            items = merged
            cursor = stalled ? nextCursor : page.next_cursor
            pageError = stalled ? "分页未返回后续记录，请重试" : ""
        } catch {
            if epoch == capturedEpoch {
                pageError = error.localizedDescription
            }
        }
    }

    public func loadDetail(id: Int) async throws -> UsageRecord {
        let capturedEpoch = epoch
        let row: UsageRecord = try await RustBridge.shared.api("GET", "/usage-records/\(id)")
        guard epoch == capturedEpoch else {
            throw RustBridgeError("读取已取消")
        }
        return row
    }

    private func loadHead(capturedEpoch: UInt64, query: String) async -> Bool {
        loading = true
        defer {
            if epoch == capturedEpoch { loading = false }
        }
        do {
            let page: RecordPageEnvelope = try await RustBridge.shared.api(
                "GET",
                "/usage-records?\(query)&include_summary=true"
            )
            guard epoch == capturedEpoch && activeQuery == query else { return false }
            pageRevision &+= 1
            consumedCursors.removeAll()
            items = page.items
            cursor = page.next_cursor
            latestId = page.latest_id
            observedAt = page.observed_at
            totalCost = page.summary?.actual_cost
            newCount = 0
            error = ""
            pageError = ""
            return true
        } catch {
            if epoch == capturedEpoch {
                self.error = error.localizedDescription
            }
            return false
        }
    }

    private func pollNewRecords(capturedEpoch: UInt64, query: String, holding: Bool) async -> Bool {
        guard let latest = latestId else { return false }
        do {
            let check: RecordNewCountEnvelope = try await RustBridge.shared.api(
                "GET",
                "/usage-records?\(query)&after_id=\(latest)"
            )
            guard epoch == capturedEpoch && activeQuery == query else { return false }
            newCount = check.new_count
            error = ""
            if check.new_count > 0 && !holding && pageError.isEmpty && items.count <= 50 {
                return await loadHead(capturedEpoch: capturedEpoch, query: query)
            }
            return true
        } catch {
            if epoch == capturedEpoch {
                self.error = error.localizedDescription
            }
            return false
        }
    }
}
