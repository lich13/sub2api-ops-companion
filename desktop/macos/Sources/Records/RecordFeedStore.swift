import Foundation
import Observation

public struct RecordTableRow: Identifiable, Sendable {
    public let record: UsageRecord
    public let route: [ModelRouteStep]
    private let modelHeight: CGFloat
    public var id: Int { record.id }

    init(_ record: UsageRecord) {
        self.record = record
        route = RecordFormatting.modelRoute(record)
        let lines = max(1, route.count) + (record.upstream_model_mismatch ? 1 : 0)
        modelHeight = CGFloat(lines) * 14 + 8
    }

    func height(model: Bool, latency: Bool) -> CGFloat {
        max(latency ? 42 : 38, model ? modelHeight : 0)
    }
}

@MainActor
@Observable
public final class RecordFeedStore {
    public private(set) var items: [RecordTableRow] = []
    public private(set) var cursor: String? = nil
    public private(set) var latestId: Int? = nil
    public private(set) var newCount: Int = 0
    public private(set) var observedAt: String = ""
    public private(set) var totalCost: String? = nil
    public private(set) var loading: Bool = false
    public private(set) var error: String = ""
    public private(set) var pageError: String = ""

    public var holding = false
    private var autoRefreshAllowed: Bool { !holding && !LinearMenuTracking.isTracking }
    private var activeConnectionKey: String = ""
    private var activeQuery: String = ""
    private var epoch: UInt64 = 0
    private var pageRevision: UInt64 = 0
    private var loadSeq: UInt64 = 0
    private var consumedCursors: Set<String> = []
    @ObservationIgnored nonisolated(unsafe) private var pollTask: Task<Void, Never>?

    deinit {
        pollTask?.cancel()
    }

    public init() {}

    public func bind(connectionKey: String = "", query: String, active: Bool) {
        if activeConnectionKey != connectionKey || activeQuery != query {
            activeConnectionKey = connectionKey
            activeQuery = query
            epoch &+= 1
            pageRevision &+= 1
            loadSeq &+= 1
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
        let capturedConnectionKey = connectionKey
        let capturedQuery = query
        pollTask = Task { @MainActor [weak self] in
            guard let self else { return }
            var failures = 0
            while !Task.isCancelled
                && self.epoch == capturedEpoch
                && self.activeConnectionKey == capturedConnectionKey
                && self.activeQuery == capturedQuery
            {
                let ok: Bool
                if self.latestId == nil {
                    ok = await self.loadHead(
                        capturedEpoch: capturedEpoch,
                        connectionKey: capturedConnectionKey,
                        query: capturedQuery
                    )
                } else {
                    ok = await self.pollNewRecords(
                        capturedEpoch: capturedEpoch,
                        connectionKey: capturedConnectionKey,
                        query: capturedQuery
                    )
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
        return await loadHead(
            capturedEpoch: epoch,
            connectionKey: activeConnectionKey,
            query: activeQuery
        )
    }

    public func loadMore(retry: Bool = false) async {
        guard let nextCursor = cursor, !loading else { return }
        if !pageError.isEmpty && !retry { return }
        let capturedEpoch = epoch
        let capturedRev = pageRevision
        let capturedConnectionKey = activeConnectionKey
        let capturedQuery = activeQuery
        loadSeq &+= 1
        let capturedLoadSeq = loadSeq
        loading = true
        pageError = ""
        defer {
            if epoch == capturedEpoch && loadSeq == capturedLoadSeq { loading = false }
        }
        do {
            let encodedCursor = nextCursor.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed) ?? nextCursor
            let page: RecordPageEnvelope = try await RustBridge.shared.api(
                "GET",
                "/usage-records?\(capturedQuery)&cursor=\(encodedCursor)"
            )
            guard epoch == capturedEpoch
                && pageRevision == capturedRev
                && activeConnectionKey == capturedConnectionKey
                && activeQuery == capturedQuery
            else { return }
            var seen = Set(items.map(\.id))
            var merged = items
            for row in page.items where seen.insert(row.id).inserted {
                merged.append(RecordTableRow(row))
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
            if epoch == capturedEpoch
                && pageRevision == capturedRev
                && activeConnectionKey == capturedConnectionKey
                && activeQuery == capturedQuery
            {
                pageError = error.localizedDescription
            }
        }
    }

    private func loadHead(
        capturedEpoch: UInt64,
        connectionKey: String,
        query: String,
        automatic: Bool = false
    ) async -> Bool {
        loadSeq &+= 1
        let capturedLoadSeq = loadSeq
        loading = true
        defer {
            if epoch == capturedEpoch && loadSeq == capturedLoadSeq { loading = false }
        }
        do {
            let page: RecordPageEnvelope = try await RustBridge.shared.api(
                "GET",
                "/usage-records?\(query)&include_summary=true"
            )
            guard !Task.isCancelled,
                  epoch == capturedEpoch && activeConnectionKey == connectionKey && activeQuery == query
            else { return false }
            if automatic && !autoRefreshAllowed { return true }
            pageRevision &+= 1
            consumedCursors.removeAll()
            items = page.items.map(RecordTableRow.init)
            cursor = page.next_cursor
            latestId = page.latest_id
            observedAt = page.observed_at
            totalCost = page.summary?.actual_cost
            newCount = 0
            error = ""
            pageError = ""
            return true
        } catch {
            if !Task.isCancelled && epoch == capturedEpoch && activeConnectionKey == connectionKey && activeQuery == query {
                self.error = error.localizedDescription
            }
            return false
        }
    }

    private func pollNewRecords(
        capturedEpoch: UInt64,
        connectionKey: String,
        query: String
    ) async -> Bool {
        guard let latest = latestId else { return false }
        do {
            let check: UsageDeltaReply = try await RustBridge.shared.api(
                "GET",
                "/usage-records?\(query)&after_id=\(latest)"
            )
            guard !Task.isCancelled,
                  epoch == capturedEpoch && activeConnectionKey == connectionKey && activeQuery == query
            else { return false }
            newCount = check.new_count
            error = ""
            if check.new_count > 0 && autoRefreshAllowed && pageError.isEmpty {
                return await loadHead(
                    capturedEpoch: capturedEpoch,
                    connectionKey: connectionKey,
                    query: query,
                    automatic: true
                )
            }
            return true
        } catch {
            if !Task.isCancelled && epoch == capturedEpoch && activeConnectionKey == connectionKey && activeQuery == query {
                self.error = error.localizedDescription
            }
            return false
        }
    }
}
