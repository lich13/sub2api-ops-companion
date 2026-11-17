import SwiftUI

/// Matches `desktop/src/FingerprintBankStatus.tsx` (`react_model_test_dialog_light.png`).
public struct FingerprintBankStatusView: View {
    public let online: Bool
    public let connectionKey: String
    public var engines: [ModelTestEngine]
    public var taskVersion: FingerprintBankVersion?

    private static let errorLabels: [String: String] = [
        "network": "网络失败",
        "timeout": "网络超时",
        "rate-limit": "限流等待",
        "cache": "缓存保存失败",
        "incompatible": "需要升级应用",
        "invalid-data": "指纹库校验失败",
    ]

    private static let sourceLabels: [String: String] = [
        "bundled": "内置库",
        "cache": "有效缓存",
        "remote": "官方远程库",
    ]

    @State private var statuses: [ModelTestEngine: FingerprintBankStatusInfo] = [:]
    @State private var loading: Set<ModelTestEngine> = []
    @State private var syncing: Set<ModelTestEngine> = []
    @State private var expanded: Set<ModelTestEngine> = []
    @State private var errors: [ModelTestEngine: String] = [:]

    public init(
        online: Bool,
        connectionKey: String,
        engines: [ModelTestEngine] = ModelTestEngine.allCases,
        taskVersion: FingerprintBankVersion? = nil
    ) {
        self.online = online
        self.connectionKey = connectionKey
        self.engines = engines
        self.taskVersion = taskVersion
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            ForEach(engines, id: \.rawValue) { engine in
                bankBlock(engine)
            }
        }
        .task(id: "\(connectionKey):\(online)") {
            guard online else { return }
            for engine in engines {
                await loadBank(engine, follow: false)
            }
        }
    }

    private func bankBlock(_ engine: ModelTestEngine) -> some View {
        let info = statuses[engine]
        let err = errors[engine]
        let isBusy = loading.contains(engine) || syncing.contains(engine) || ["checking", "syncing"].contains(info?.status ?? "")
        let isOpen = expanded.contains(engine)
        let revShort: String = {
            if let r = info?.version?.revision, !r.isEmpty {
                return String(r.prefix(8))
            }
            return "—"
        }()
        let titlePrefix = (engine == .lm_detector) ? "LM Detector 检测库" : "指纹库"
        let statusResult: (text: String, isBad: Bool) = {
            if let err, !err.isEmpty { return (err, true) }
            if isBusy { return ("检查中", false) }
            if let srvErr = info?.error, !srvErr.isEmpty {
                let mapped = Self.errorLabels[srvErr] ?? (engine == .lm_detector ? srvErr : "检查失败")
                return (mapped, true)
            }
            if info?.result == "updated" { return ("已更新", false) }
            if info?.result == "up-to-date" { return ("已是最新", false) }
            if let synced = info?.synced_at, !synced.isEmpty { return ("更新于 \(BeijingTime.fullTime(synced))", false) }
            return ("", false)
        }()

        return VStack(alignment: .leading, spacing: 5) {
            Rectangle().fill(DS.line).frame(height: 1).padding(.bottom, 3)

            HStack(spacing: 10) {
                Button {
                    if isOpen { expanded.remove(engine) } else { expanded.insert(engine) }
                } label: {
                    HStack(spacing: 5) {
                        Image(systemName: isOpen ? "arrowtriangle.down.fill" : "arrowtriangle.right.fill")
                            .font(DS.iconFont(8))
                            .foregroundStyle(DS.muted)
                        Text("\(titlePrefix) \(revShort)")
                            .font(DS.fontCompactMedium)
                            .monospacedDigit()
                            .foregroundStyle(DS.ink)
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.linearPlain)

                Spacer()

                Button {
                    Task { await syncBank(engine) }
                } label: {
                    HStack(spacing: 4) {
                        Image(systemName: "arrow.clockwise")
                            .font(DS.iconFont(11, weight: .medium))
                        Text("检查更新")
                            .font(DS.fontCompact)
                    }
                    .foregroundStyle(DS.muted)
                }
                .buttonStyle(.linearPlain)
                .disabled(!online || isBusy)
            }

            if !statusResult.text.isEmpty {
                Text(statusResult.text)
                    .font(DS.fontResetTime)
                    .foregroundStyle(statusResult.isBad ? DS.bad : DS.muted)
            }

            if isOpen, let info {
                VStack(alignment: .leading, spacing: 4) {
                    let src = info.source.flatMap { Self.sourceLabels[$0] ?? $0 } ?? "—"
                    detailRow("来源", src)
                    detailRow("构建时间", BeijingTime.fullTime(info.version?.built_at))
                    detailRow("最近检查", info.checked_at.map { BeijingTime.fullTime($0) } ?? "—")
                    detailRow("最近更新", info.synced_at.map { BeijingTime.fullTime($0) } ?? "—")
                    detailRow("下次重试", info.cooldown_until.map { BeijingTime.fullTime($0) } ?? "—")
                    if let sha = info.version?.sha256, !sha.isEmpty {
                        detailRow("SHA-256", sha, mono: true)
                    }
                    if let tv = taskVersion, !tv.revision.isEmpty {
                        detailRow("本轮使用", "\(tv.revision.prefix(8)) · 分析器 \(tv.analyzer_version ?? "—")", mono: true)
                    }
                }
                .padding(10)
                .background(DS.surfaceSubtle)
                .clipShape(RoundedRectangle(cornerRadius: 6))
            }
        }
    }

    private func detailRow(_ key: String, _ value: String, mono: Bool = false) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Text(key)
                .font(DS.fontResetTime)
                .foregroundStyle(DS.muted)
                .frame(width: 68, alignment: .leading)
            Text(verbatim: value)
                .font(mono ? DS.fontMono11 : DS.fontResetTime)
                .monospacedDigit()
                .foregroundStyle(DS.ink)
                .lineLimit(1)
                .truncationMode(.middle)
            Spacer(minLength: 0)
        }
    }

    private func endpointPrefix(for engine: ModelTestEngine) -> String {
        switch engine {
        case .modeltrace: return "/modeltrace/fingerprint-bank"
        case .lm_detector: return "/lm-detector/fingerprint-bank"
        }
    }

    private func loadBank(_ engine: ModelTestEngine, follow: Bool) async {
        loading.insert(engine)
        defer { loading.remove(engine) }
        do {
            let info: FingerprintBankStatusInfo = try await RustBridge.shared.api("GET", endpointPrefix(for: engine))
            statuses[engine] = info
            errors[engine] = nil
            var cur = info
            var attempts = 0
            while !Task.isCancelled && online && attempts < 15 && (follow && attempts == 0 || ["checking", "syncing"].contains(cur.status ?? "")) {
                attempts += 1
                try? await Task.sleep(for: .seconds(1))
                guard !Task.isCancelled, online else { break }
                if let next: FingerprintBankStatusInfo = try? await RustBridge.shared.api("GET", endpointPrefix(for: engine)) {
                    statuses[engine] = next
                    cur = next
                    if !["checking", "syncing"].contains(next.status ?? "") {
                        break
                    }
                } else {
                    break
                }
            }
        } catch {
            errors[engine] = error.localizedDescription
        }
    }

    private func syncBank(_ engine: ModelTestEngine) async {
        guard online, !syncing.contains(engine) else { return }
        syncing.insert(engine)
        errors[engine] = nil
        defer { syncing.remove(engine) }
        do {
            let _: JSONValue = try await RustBridge.shared.api(
                "POST",
                "\(endpointPrefix(for: engine))/sync",
                body: .object([:])
            )
            await loadBank(engine, follow: true)
        } catch {
            errors[engine] = error.localizedDescription
        }
    }
}
