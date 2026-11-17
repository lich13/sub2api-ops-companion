import Foundation
import Observation

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
        case .records: return "doc.text.magnifyingglass"
        case .events: return "bell"
        case .features: return "slider.horizontal.3"
        case .settings: return "gearshape"
        }
    }
}

public enum SwitcherAnchor: String, Sendable {
    case sidebar
    case topbar
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

    public var groupChangesCount: Int = 0
    public var groupSavingBusy: Bool = false
    public var priorityChangesCount: Int = 0
    public var pendingDiscardContinuation: CheckedContinuation<Bool, Never>?

    public var detailError: OpsError?
    public var detailBusy: Bool = false
    private var detailGeneration: UInt64 = 0
    public var accountModal: AccountModalDestination?

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

    public var accounts: [Account] {
        state.snapshot?.accounts ?? []
    }

    public var groups: [Group] {
        state.snapshot?.groups ?? []
    }

    public var errors: [OpsError] {
        (state.snapshot?.errors ?? []).sorted { $0.id > $1.id }
    }

    public func bindBridge() {
        RustBridge.shared.setEventHandler { [weak self] kind, data in
            guard let self else { return }
            switch kind {
            case .opsState:
                if let next = try? JSONDecoder().decode(ViewState.self, from: data) {
                    self.applyViewState(next)
                }
            case .updateResult:
                if let message = String(data: data, encoding: .utf8), !message.isEmpty {
                    self.showToast(message)
                }
            case .trayTooltip:
                if let tip = String(data: data, encoding: .utf8), !tip.isEmpty {
                    self.trayTooltip = tip
                }
            case .reopen:
                self.onReopenMainWindow?()
            }
        }
        Task { @MainActor [weak self] in
            guard let self else { return }
            do {
                let initial: ViewState = try await RustBridge.shared.command("get_state")
                self.applyViewState(initial)
                self.ready = true
            } catch {
                self.ready = true
                self.report(error)
            }
        }
    }

    public func applyViewState(_ next: ViewState) {
        let oldKey = connectionKey
        state = next
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
        accountModal = nil
        connectionModal = nil
        switcherAnchor = nil
        busyScheduleAccountId = nil
        config = nil
        groupChangesCount = 0
        groupSavingBusy = false
        priorityChangesCount = 0
        if let pending = pendingDiscardContinuation {
            pendingDiscardContinuation = nil
            pending.resume(returning: false)
        }
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
            Task {
                do {
                    _ = try await operations.accountOperation(account: acc, action: "recover", payload: [:])
                    showToast("\(acc.name)：已恢复状态")
                } catch {
                    report(error)
                }
            }
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
        let cleaned = message.replacingOccurrences(of: "^Error: ", with: "", options: .regularExpression)
        guard !cleaned.contains("连接已切换；请求保留在原连接") else { return }
        toast = cleaned
        toastClearTask?.cancel()
        guard !cleaned.isEmpty else { return }
        toastClearTask = Task { @MainActor [weak self] in
            try? await Task.sleep(for: .seconds(6))
            guard !Task.isCancelled else { return }
            self?.toast = ""
        }
    }

    public func report(_ error: Error) {
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
            let msg = error.localizedDescription
            report(error)
            if msg.contains("Key") || msg.contains("401") || msg.contains("403") || msg.contains("未找到") || msg.contains("拒绝") {
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
        let p = state.preferences
        var args: [String: JSONValue] = [
            "pinned": .bool(pinned ?? p.pinned),
            "launchAtLogin": .bool(launchAtLogin ?? p.launch_at_login),
        ]
        if let cols = recordColumns ?? p.record_columns {
            args["recordColumns"] = .array(cols.map(JSONValue.string))
        }
        if let conc = modelTestConcurrency ?? p.model_test_concurrency {
            args["modelTestConcurrency"] = .int(Int64(conc))
        }
        if let prefsMap = modelTestPreferences ?? p.model_test_preferences,
           let encoded = try? JSONEncoder().encode(prefsMap),
           let jsonVal = try? JSONDecoder().decode(JSONValue.self, from: encoded)
        {
            args["modelTestPreferences"] = jsonVal
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
            if result["verified"]?.boolValue != true {
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
