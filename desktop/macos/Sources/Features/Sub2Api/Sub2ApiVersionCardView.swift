import SwiftUI

/// Matches `Sub2ApiVersion` in `desktop/src/Sub2ApiVersion.tsx`.
public struct Sub2ApiVersionCardView: View {
    public let active: Bool
    public let online: Bool
    public var knownVersion: String
    public var embedded: Bool
    public var onRuntimeChanged: (() -> Void)?

    private struct ReleaseInfo: Codable, Sendable, Equatable {
        var name: String?
        var html_url: String?
        var published_at: String?
        var body: String?
    }

    private struct UpdateInfo: Codable, Sendable, Equatable {
        var current_version: String
        var latest_version: String
        var has_update: Bool
        var cached: Bool
        var build_type: String
        var warning: String?
        var release_info: ReleaseInfo?
    }

    private struct UpdateActionReply: Codable, Sendable, Equatable {
        var message: String
        var need_restart: Bool?
        var already_up_to_date: Bool?
        var current_version: String?
        var latest_version: String?
    }

    @State private var version: String = ""
    @State private var info: UpdateInfo? = nil
    @State private var busy: Bool = false
    @State private var needRestart: Bool = false
    @State private var waitingRestart: Bool = false
    @State private var targetVersion: String = ""
    @State private var messageText: String = ""
    @State private var errorText: String = ""

    public init(
        active: Bool,
        online: Bool,
        knownVersion: String = "",
        embedded: Bool = false,
        onRuntimeChanged: (() -> Void)? = nil
    ) {
        self.active = active
        self.online = online
        self.knownVersion = knownVersion
        self.embedded = embedded
        self.onRuntimeChanged = onRuntimeChanged
    }

    private func normalize(_ v: String) -> String {
        v.hasPrefix("v") ? String(v.dropFirst()) : v
    }

    private var displayVersion: String {
        version.isEmpty ? (knownVersion.isEmpty ? "—" : knownVersion) : version
    }

    public var body: some View {
        let content = VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 10) {
                Text("运行 \(displayVersion)")
                    .font(DS.fontCompactSemibold)
                    .monospacedDigit()
                    .foregroundStyle(DS.ink)

                Button("检查更新") {
                    Task { await checkUpdates() }
                }
                .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                .disabled(busy || waitingRestart || !online)

                Button("查看发布") {
                    Task {
                        let releaseUrl = info?.release_info?.html_url ?? "https://github.com/Wei-Shaw/sub2api/releases"
                        do {
                            try await RustBridge.shared.commandVoid(
                                "open_sub2api_release",
                                args: .object(["url": .string(releaseUrl)])
                            )
                        } catch {
                            errorText = error.localizedDescription
                        }
                    }
                }
                .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))

                if let info {
                    Text(info.has_update ? "可更新至 \(info.latest_version)" : "已是最新版本")
                        .font(DS.fontResetTimeSemibold)
                        .foregroundStyle(info.has_update ? DS.accent : DS.good)
                        .padding(.horizontal, 6)
                        .padding(.vertical, 2)
                        .background(info.has_update ? DS.accentBg : DS.goodBg)
                        .clipShape(RoundedRectangle(cornerRadius: 4))
                }

                Spacer()

                if let info, info.has_update && !needRestart {
                    Button(busy ? "正在更新…" : "更新 Sub2API") {
                        Task { await performAction("update") }
                    }
                    .buttonStyle(LinearButtonStyle(variant: .primary, compact: true))
                    .disabled(busy || waitingRestart || !online || info.build_type != "release")
                }

                if needRestart {
                    Button("重启生效") {
                        Task { await performAction("restart") }
                    }
                    .buttonStyle(LinearButtonStyle(variant: .primary, compact: true))
                    .disabled(busy || waitingRestart || !online)
                }
            }

            if info?.build_type == "source" {
                Text("源码构建版本不支持在线更新")
                    .font(DS.fontResetTime)
                    .foregroundStyle(DS.muted)
            }
            if let warn = info?.warning, !warn.isEmpty {
                Text(warn).font(DS.fontResetTime).foregroundStyle(DS.warn)
            }
            if !messageText.isEmpty {
                Text(messageText).font(DS.fontResetTime).foregroundStyle(DS.good)
            }
            if !errorText.isEmpty {
                Text(errorText).font(DS.fontResetTime).foregroundStyle(DS.bad)
            }
        }

        SwiftUI.Group {
            if embedded {
                content
            } else {
                content
                    .padding(12)
                    .background(DS.surface)
                    .overlay(RoundedRectangle(cornerRadius: DS.cardRadius).strokeBorder(DS.line, lineWidth: 1))
                    .clipShape(RoundedRectangle(cornerRadius: DS.cardRadius))
            }
        }
        .onAppear {
            if !knownVersion.isEmpty && version.isEmpty {
                version = knownVersion
            }
        }
        .onChange(of: knownVersion) { _, next in
            if !next.isEmpty { version = next }
        }
        .task(id: "\(waitingRestart):\(active)") {
            guard waitingRestart, active else {
                if !active { waitingRestart = false }
                return
            }
            await pollRestartVersion()
        }
    }

    private func checkUpdates() async {
        guard online, !busy else { return }
        busy = true
        errorText = ""
        messageText = ""
        defer { busy = false }
        do {
            let res: UpdateInfo = try await RustBridge.shared.api("GET", "/sub2api-system/check-updates?force=true")
            let prev = version
            info = res
            version = res.current_version
            if !targetVersion.isEmpty && normalize(res.current_version) == normalize(targetVersion) {
                targetVersion = ""
                needRestart = false
                waitingRestart = false
            }
            if !prev.isEmpty && normalize(res.current_version) != normalize(prev) {
                onRuntimeChanged?()
            }
        } catch {
            errorText = error.localizedDescription
        }
    }

    private func performAction(_ action: String) async {
        guard online, !busy else { return }
        busy = true
        errorText = ""
        messageText = ""
        defer { busy = false }
        do {
            let res: UpdateActionReply = try await RustBridge.shared.api("POST", "/sub2api-system/\(action)")
            if action == "restart" {
                waitingRestart = true
                messageText = "正在等待重启生效"
            } else if res.need_restart == true {
                targetVersion = info?.latest_version ?? ""
                needRestart = true
                messageText = "更新已下载，重启后生效"
            } else if res.already_up_to_date == true, let cur = res.current_version {
                version = cur
                needRestart = false
                messageText = "已是最新版本"
                if var updated = info {
                    updated.current_version = cur
                    if let lat = res.latest_version { updated.latest_version = lat }
                    updated.has_update = false
                    info = updated
                }
            }
        } catch {
            errorText = error.localizedDescription
            if action == "restart" { waitingRestart = true }
        }
    }

    private func pollRestartVersion() async {
        struct VerDTO: Decodable, Sendable { var version: String }
        try? await Task.sleep(for: .milliseconds(1500))
        for _ in 0..<20 {
            guard !Task.isCancelled, waitingRestart, active else { return }
            if let cur: VerDTO = try? await RustBridge.shared.api("GET", "/sub2api-system/version") {
                version = cur.version
                if !targetVersion.isEmpty && normalize(cur.version) == normalize(targetVersion) {
                    waitingRestart = false
                    needRestart = false
                    errorText = ""
                    messageText = "更新已生效"
                    targetVersion = ""
                    onRuntimeChanged?()
                    return
                }
            }
            try? await Task.sleep(for: .seconds(3))
        }
        if !Task.isCancelled {
            waitingRestart = false
            messageText = "尚未确认运行版本，请手动刷新"
        }
    }
}
