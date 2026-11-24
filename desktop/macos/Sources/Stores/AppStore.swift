import Foundation
import OSLog
import Observation
import SwiftUI

public enum AppPage: String, CaseIterable, Sendable, Identifiable {
    case accounts
    case groups
    case priorities
    case records
    case events
    case features
    case settings

    public var id: String { rawValue }

    public var label: String {
        switch self {
        case .accounts: return "账号"
        case .groups: return "分组"
        case .priorities: return "优先级"
        case .records: return "记录"
        case .events: return "事件"
        case .features: return "功能"
        case .settings: return "设置"
        }
    }

    public var symbolName: String {
        switch self {
        case .accounts: return "person.2"
        case .groups: return "square.3.layers.3d"
        case .priorities: return "list.number"
        case .records: return "list.bullet.rectangle"
        case .events: return "bell"
        case .features: return "slider.horizontal.3"
        case .settings: return "gearshape"
        }
    }
}

public enum SwitcherAnchor: String, Sendable {
    case sidebar
    case topbar
    case quick
}

public enum QuickTab: String, Sendable {
    case groups
    case errors
}

public struct ConnectionModalTarget: Identifiable, Sendable, Equatable {
    public let id = UUID()
    public var baseUrl: String
    public var name: String
    public var requireKey: Bool

    public init(baseUrl: String, name: String, requireKey: Bool = false) {
        self.baseUrl = baseUrl
        self.name = name
        self.requireKey = requireKey
    }
}

@MainActor
@Observable
public final class AppStore {
    public var state: ViewState = ViewState()
    public var ready: Bool = false
    public var page: AppPage = .accounts {
        didSet {
            if oldValue != page {
                switcherAnchor = nil
                closeDetail()
                closeRecordDetail()
                selectedDetectionLog = nil
                windowModalView = nil
                windowModalOnBackdrop = nil
            }
        }
    }
    public var quickTab: QuickTab = .groups
    public var trayTooltip: String = "Sub2Ops"
    public var toast: String = ""
    public var switchingUrl: String?
    public var connectionModal: ConnectionModalTarget?
    public var switcherAnchor: SwitcherAnchor?
    public var busyScheduleAccountId: Int?
    public var busyRecoverAccountIds: Set<Int> = []

    public var groupChangesCount: Int = 0
    public var groupSavingBusy: Bool = false
    public var priorityChangesCount: Int = 0
    public var pendingDiscardContinuation: CheckedContinuation<Bool, Never>?

    public var detailError: OpsError?
    public var detailBusy: Bool = false
    private var detailGeneration: UInt64 = 0
    public var accountModal: AccountModalDestination?

    // Window-level Record Detail Drawer & Secondary Modals
    public var selectedRecordId: Int?
    public var detailRecord: UsageRecord?
    public var recordDetailLoading: Bool = false
    public var recordDetailError: String = ""
    private var recordDetailGeneration: UInt64 = 0
    public var selectedDetectionLog: DetectionLog?
    public var windowModalView: AnyView?
    public var windowModalOnBackdrop: (@MainActor () -> Void)?

    public var config: [String: JSONValue]?
    public let operations = OperationStore()

    private var toastClearTask: Task<Void, Never>?
    public var onReopenMainWindow: (@MainActor () -> Void)?
    public var onShowQuickPanel: (@MainActor () -> Void)?
    public var onHideQuickPanel: (@MainActor () -> Void)?
    public var onOpenModelTest: (@MainActor (Account) -> Void)?
    public var onOpenTemplates: (@MainActor (Account?) -> Void)?

    public init() {
        self.onOpenModelTest = { [weak self] acc in
            self?.accountModal = .modelTest(acc)
        }
        self.onOpenTemplates = { [weak self] acc in
            self?.accountModal = .templates(acc)
        }
    }

    public var connectionKey: String {
        "\(state.preferences.base_url):\(state.connected):\(state.connection_revision ?? 0)"
    }

    public var savedConnections: [SavedConnection] {
        normalizedSavedConnections(state.preferences)
    }

    public var activeUrl: String {
        state.preferences.base_url
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .replacingOccurrences(of: "/+$", with: "", options: .regularExpression)
    }

    public var activeConnection: SavedConnection? {
        savedConnections.first(where: { $0.base_url == activeUrl })
            ?? (activeUrl.isEmpty ? nil : SavedConnection(name: defaultConnectionName(activeUrl), base_url: activeUrl))
    }

    public private(set) var accounts: [Account] = []
    public private(set) var groups: [Group] = []
    public private(set) var errors: [OpsError] = []

    private var lastOpsStateSeq: UInt64 = 0
    private static let bridgeLogger = Logger(subsystem: "Sub2Ops", category: "bridge")

    public func bindBridge() {
        RustBridge.shared.setEventHandler { [weak self] event in
            guard let self else { return }
            switch event {
            case .opsState(let seq, let next):
                guard seq > self.lastOpsStateSeq else { return }
                self.lastOpsStateSeq = seq
                self.applyViewState(next)
            case .opsStateDecodeError(let description):
                Self.bridgeLogger.error("Failed to decode ops-state event: \(description, privacy: .public)")
            case .updateResult(let message):
                self.showToast(message)
            case .trayTooltip(let tip):
                self.trayTooltip = tip
            case .reopen:
                self.onReopenMainWindow?()
            }
        }
        Task { @MainActor [weak self] in
            guard let self else { return }
            do {
                let initial: ViewState = try await RustBridge.shared.command("get_state")
                if self.lastOpsStateSeq == 0 {
                    self.applyViewState(initial)
                }
                self.ready = true
            } catch {
                self.ready = true
                self.report(error)
            }
        }
    }

    public func applyViewState(_ next: ViewState) {
        let oldKey = connectionKey
        let nextKey = "\(next.preferences.base_url):\(next.connected):\(next.connection_revision ?? 0)"
        let sameSnapshotContent = oldKey == nextKey
            && state.snapshot != nil
            && next.snapshot != nil
            && (state.snapshot_revision ?? 0) != 0
            && state.snapshot_revision == next.snapshot_revision

        if sameSnapshotContent, let preserved = state.snapshot {
            var merged = next
            merged.snapshot = Snapshot(
                observed_at: next.snapshot?.observed_at ?? preserved.observed_at,
                accounts: preserved.accounts,
                groups: preserved.groups,
                errors: preserved.errors,
                recoveries: preserved.recoveries,
                schema_version: preserved.schema_version
            )
            if state != merged {
                state = merged
            }
        } else {
            state = next
            let nextAccounts = next.snapshot?.accounts ?? []
            if accounts != nextAccounts {
                accounts = nextAccounts
            }
            let nextGroups = next.snapshot?.groups ?? []
            if groups != nextGroups {
                groups = nextGroups
            }
            let nextErrors = (next.snapshot?.errors ?? []).sorted { ($0.id ?? 0) > ($1.id ?? 0) }
            if errors != nextErrors {
                errors = nextErrors
            }
        }

        let newKey = connectionKey
        operations.bindConnection(newKey)
        if oldKey != newKey {
            resetConnectionScopedState()
        }
        operations.startPollingIfNeeded(online: state.online, active: state.foreground != false)
        if state.online && (page == .settings || page == .features) && config == nil {
            Task { await reloadConfig() }
        }
    }

    private func resetConnectionScopedState() {
        closeDetail()
        closeRecordDetail()
        selectedDetectionLog = nil
        windowModalView = nil
        windowModalOnBackdrop = nil
        accountModal = nil
        connectionModal = nil
        switcherAnchor = nil
        busyScheduleAccountId = nil
        busyRecoverAccountIds.removeAll()
        config = nil
        groupChangesCount = 0
        groupSavingBusy = false
        priorityChangesCount = 0
        if let pending = pendingDiscardContinuation {
            pendingDiscardContinuation = nil
            pending.resume(returning: false)
        }
    }

    public func recoverAccount(_ acc: Account) async {
        guard state.online, !busyRecoverAccountIds.contains(acc.id) else { return }
        let capturedEpoch = operations.epoch
        busyRecoverAccountIds.insert(acc.id)
        defer {
            busyRecoverAccountIds.remove(acc.id)
        }
        do {
            let res = try await operations.accountOperation(account: acc, action: "recover", payload: [:])
            if operations.epoch == capturedEpoch {
                let verified = res?.verified == true
                showToast(verified ? "\(acc.name)：状态已恢复" : "恢复状态未确认，请刷新")
                refresh()
            }
        } catch {
            if operations.epoch == capturedEpoch {
                report(error)
                refresh()
            }
        }
    }

    public func openRecordDetail(id: Int, seed: UsageRecord? = nil) {
        recordDetailGeneration &+= 1
        let gen = recordDetailGeneration
        selectedRecordId = id
        recordDetailLoading = true
        recordDetailError = ""
        detailRecord = seed
        Task { @MainActor [weak self] in
            guard let self else { return }
            do {
                let row: UsageRecord = try await RustBridge.shared.api("GET", "/usage-records/\(id)")
                if self.recordDetailGeneration == gen {
                    self.detailRecord = row
                }
            } catch {
                if self.recordDetailGeneration == gen {
                    self.recordDetailError = error.localizedDescription
                }
            }
            if self.recordDetailGeneration == gen {
                self.recordDetailLoading = false
            }
        }
    }

    public func closeRecordDetail() {
        recordDetailGeneration &+= 1
        selectedRecordId = nil
        detailRecord = nil
        recordDetailLoading = false
        recordDetailError = ""
    }

    public func presentWindowModal<Content: View>(onBackdrop: (@MainActor () -> Void)? = nil, @ViewBuilder content: () -> Content) {
        windowModalOnBackdrop = onBackdrop
        windowModalView = AnyView(content())
    }

    public func dismissWindowModal() {
        windowModalView = nil
        windowModalOnBackdrop = nil
    }

    public func triggerAccountAction(_ action: AccountDialogAction) {
        switch action {
        case .modelTest(let acc):
            accountModal = .modelTest(acc)
        case .toggleDegradation(let acc):
            Task { await DegradationMarkActions.toggle(account: acc, store: self) }
        case .modelDetection(let acc):
            accountModal = .modelDetection(acc)
        case .connectionTest(let acc):
            accountModal = .testConnection(acc)
        case .keepalive(let acc):
            accountModal = .keepalive(acc)
        case .reauthorize(let acc):
            accountModal = .reauthorize(acc)
        case .recoverState(let acc):
            Task { await recoverAccount(acc) }
        case .applyTemplate(let acc):
            accountModal = .templates(acc)
        case .settingsWhitelist(let acc):
            accountModal = .accountSettings(acc, .whitelist)
        case .settingsMapping(let acc):
            accountModal = .accountSettings(acc, .mapping)
        case .settingsProxy(let acc):
            accountModal = .accountSettings(acc, .proxy)
        case .deleteAccount(let acc):
            accountModal = .deleteAccounts([acc])
        }
    }

    public func showToast(_ message: String) {
        toast = message
        toastClearTask?.cancel()
        guard !message.isEmpty else { return }
        toastClearTask = Task { @MainActor [weak self] in
            try? await Task.sleep(for: .seconds(6))
            guard !Task.isCancelled else { return }
            self?.toast = ""
        }
    }

    public func report(_ error: Error) {
        guard (error as? RustBridgeError)?.ignored != true else { return }
        showToast(error.localizedDescription)
    }

    public func refresh() {
        Task {
            do {
                try await RustBridge.shared.commandVoid("refresh")
            } catch {
                report(error)
            }
        }
    }

    public func reloadConfig() async {
        guard state.online else { return }
        let key = connectionKey
        do {
            let loaded: [String: JSONValue] = try await RustBridge.shared.api("GET", "/config")
            if connectionKey == key {
                config = loaded
            }
        } catch {
            if connectionKey == key {
                report(error)
            }
        }
    }

    public func beforeConnectionChange() async -> Bool {
        if groupSavingBusy {
            showToast("分组正在保存，请等待结果")
            return false
        }
        if groupChangesCount == 0 && priorityChangesCount == 0 {
            return true
        }
        return await withCheckedContinuation { continuation in
            if let existing = pendingDiscardContinuation {
                existing.resume(returning: false)
            }
            pendingDiscardContinuation = continuation
        }
    }

    public func resolveDiscardPrompt(proceed: Bool) {
        guard let pending = pendingDiscardContinuation else { return }
        pendingDiscardContinuation = nil
        pending.resume(returning: proceed)
    }

    public func switchConnection(_ item: SavedConnection) async {
        let targetUrl = item.base_url
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .replacingOccurrences(of: "/+$", with: "", options: .regularExpression)
        guard !targetUrl.isEmpty, switchingUrl == nil else { return }
        switcherAnchor = nil
        if state.connected && targetUrl == activeUrl { return }
        guard await beforeConnectionChange() else { return }
        switchingUrl = targetUrl
        defer { switchingUrl = nil }
        do {
            try await RustBridge.shared.commandVoid(
                "connect",
                args: .object([
                    "baseUrl": .string(targetUrl),
                    "apiKey": .string(""),
                    "name": .string(item.name),
                ])
            )
            showToast("已切换到 \(item.name)")
        } catch {
            report(error)
            if (error as? RustBridgeError)?.needsCredentials == true {
                connectionModal = ConnectionModalTarget(baseUrl: targetUrl, name: item.name, requireKey: true)
            }
        }
    }

    public func updatePreferences(
        pinned: Bool? = nil,
        launchAtLogin: Bool? = nil,
        recordColumns: [String]? = nil,
        modelTestConcurrency: Int? = nil,
        modelTestPreferences: [String: ModelTestPreference]? = nil
    ) async {
        var args: [String: JSONValue] = [:]
        if let pinned {
            args["pinned"] = .bool(pinned)
        }
        if let launchAtLogin {
            args["launchAtLogin"] = .bool(launchAtLogin)
        }
        if let cols = recordColumns {
            args["recordColumns"] = .array(cols.map(JSONValue.string))
        }
        if let conc = modelTestConcurrency {
            args["modelTestConcurrency"] = .int(Int64(conc))
        }
        if let prefsMap = modelTestPreferences {
            args["modelTestPreferences"] = .object(
                prefsMap.mapValues { pref in
                    .object([
                        "engine": .string(pref.engine),
                        "sample_count": .int(Int64(pref.sample_count)),
                    ])
                }
            )
        }
        do {
            try await RustBridge.shared.commandVoid("preferences", args: .object(args))
        } catch {
            report(error)
        }
    }

    public func toggleSchedulable(_ account: Account) async {
        guard state.online, busyScheduleAccountId == nil else { return }
        let capturedEpoch = operations.epoch
        busyScheduleAccountId = account.id
        defer {
            if operations.epoch == capturedEpoch {
                busyScheduleAccountId = nil
            }
        }
        do {
            let result = try await operations.accountOperation(
                account: account,
                action: "schedulable",
                payload: ["schedulable": .bool(!account.schedulable)]
            )
            if result?.verified != true {
                throw RustBridgeError("调度写入未确认，请刷新查看实际状态")
            }
            if operations.epoch == capturedEpoch {
                showToast("\(account.name)：\(account.schedulable ? "已关闭" : "已打开")调度")
            }
        } catch {
            if operations.epoch == capturedEpoch {
                report(error)
                refresh()
            }
        }
    }

    public func openErrorDetail(id: Int, category: String? = nil) {
        detailGeneration &+= 1
        let gen = detailGeneration
        detailBusy = true
        Task { @MainActor [weak self] in
            guard let self else { return }
            let suffix = category.map { "?category=\($0)" } ?? ""
            do {
                let value: OpsError = try await RustBridge.shared.api("GET", "/errors/\(id)\(suffix)")
                if self.detailGeneration == gen {
                    self.detailError = value
                }
            } catch {
                if self.detailGeneration == gen {
                    self.report(error)
                }
            }
            if self.detailGeneration == gen {
                self.detailBusy = false
            }
        }
    }

    public func closeDetail() {
        detailGeneration &+= 1
        detailError = nil
        detailBusy = false
    }

    public func checkUpdatesAndNotify() async {
        do {
            let msg: String = try await RustBridge.shared.command("check_updates")
            showToast(msg)
        } catch {
            report(error)
        }
    }
}
