import SwiftUI

public struct FingerprintBankStatusView: View {
    public let online: Bool
    public let connectionKey: String
    public var engines: [ModelTestEngine]

    @State private var statuses: [ModelTestEngine: FingerprintBankStatusInfo] = [:]
    @State private var loading: Set<ModelTestEngine> = []
    @State private var syncing: Set<ModelTestEngine> = []
    @State private var errors: [ModelTestEngine: String] = [:]

    public init(
        online: Bool,
        connectionKey: String,
        engines: [ModelTestEngine] = ModelTestEngine.allCases
    ) {
        self.online = online
        self.connectionKey = connectionKey
        self.engines = engines
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            ForEach(engines, id: \.rawValue) { engine in
                let info = statuses[engine]
                let err = errors[engine]
                let isBusy = loading.contains(engine) || syncing.contains(engine)
                HStack(spacing: 10) {
                    StatusDot(info?.available == true ? .good : (err != nil ? .bad : .warning))
                    VStack(alignment: .leading, spacing: 2) {
                        HStack(spacing: 6) {
                            Text("\(engine.displayName) 指纹词库")
                                .font(DS.fontCompactSemibold)
                                .foregroundStyle(DS.ink)
                            if let count = info?.model_count {
                                Text("\(count) 模型")
                                    .font(DS.fontResetTime)
                                    .monospacedDigit()
                                    .foregroundStyle(DS.muted)
                            }
                            if let rev = info?.revision, !rev.isEmpty {
                                Text(verbatim: String(rev.prefix(8)))
                                    .font(DS.fontMono11)
                                    .foregroundStyle(DS.muted)
                            }
                        }
                        if let err, !err.isEmpty {
                            Text(err)
                                .font(DS.fontResetTime)
                                .foregroundStyle(DS.bad)
                        } else if let updated = info?.updated_at, !updated.isEmpty {
                            Text("更新于 \(BeijingTime.fullTime(updated))")
                                .font(DS.fontResetTime)
                                .monospacedDigit()
                                .foregroundStyle(DS.muted)
                        } else {
                            Text(isBusy ? "正在读取词库状态…" : (info?.message ?? "词库状态待核对"))
                                .font(DS.fontResetTime)
                                .foregroundStyle(DS.muted)
                        }
                    }
                    Spacer()
                    Button(syncing.contains(engine) ? "同步中…" : "同步词库") {
                        Task { await syncBank(engine) }
                    }
                    .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                    .disabled(!online || isBusy)
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 8)
                .background(DS.surfaceSubtle)
                .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
            }
        }
        .task(id: "\(connectionKey):\(online)") {
            guard online else { return }
            for engine in engines {
                await loadBank(engine)
            }
        }
    }

    private func endpointPrefix(for engine: ModelTestEngine) -> String {
        switch engine {
        case .modeltrace: return "/modeltrace/fingerprint-bank"
        case .lm_detector: return "/lm-detector/fingerprint-bank"
        }
    }

    private func loadBank(_ engine: ModelTestEngine) async {
        loading.insert(engine)
        defer { loading.remove(engine) }
        do {
            let info: FingerprintBankStatusInfo = try await RustBridge.shared.api("GET", endpointPrefix(for: engine))
            statuses[engine] = info
            errors[engine] = nil
        } catch {
            errors[engine] = error.localizedDescription
        }
    }

    private func syncBank(_ engine: ModelTestEngine) async {
        guard online, !syncing.contains(engine) else { return }
        syncing.insert(engine)
        defer { syncing.remove(engine) }
        do {
            let info: FingerprintBankStatusInfo = try await RustBridge.shared.api(
                "POST",
                "\(endpointPrefix(for: engine))/sync",
                body: .object([:])
            )
            statuses[engine] = info
            errors[engine] = nil
        } catch {
            errors[engine] = error.localizedDescription
        }
    }
}
