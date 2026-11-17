import SwiftUI

/// Matches `desktop/src/ConnectionStatusCard.tsx`.
public struct ConnectionStatusCardView: View {
    public let connectionKey: String
    @State private var status: UpstreamConnectionStatus?
    @State private var error: String = ""
    @State private var loading: Bool = false
    @State private var revision: Int = 0

    private static let labels: [String: String] = [
        "ok": "上游连接正常",
        "network_unreachable": "上游不可达",
        "auth_rejected": "管理员 Key 被拒绝，请重新连接",
        "route_not_found": "管理接口路径不兼容",
        "protocol_mismatch": "管理接口响应不兼容",
        "upstream_error": "上游服务异常",
    ]

    public init(connectionKey: String) {
        self.connectionKey = connectionKey
    }

    public var body: some View {
        CardSection {
            HStack(alignment: .center) {
                Text("连接诊断")
                    .font(DS.fontSectionTitle)
                    .foregroundStyle(DS.ink)
                Spacer()
                Button {
                    revision &+= 1
                } label: {
                    HStack(spacing: 6) {
                        Image(systemName: "arrow.clockwise")
                            .font(DS.iconFont(12, weight: .medium))
                        Text("重新检查")
                    }
                }
                .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                .disabled(loading)
            }
            Divider().overlay(DS.line)

            if let status {
                VStack(alignment: .leading, spacing: 6) {
                    HStack(spacing: 7) {
                        StatusDot(status.state == "ok" ? .good : .bad)
                        Text(Self.labels[status.state] ?? "连接状态未知")
                            .font(DS.fontBodySemibold)
                            .foregroundStyle(status.state == "ok" ? DS.good : DS.bad)
                    }
                    if !status.message.isEmpty {
                        Text(status.message)
                            .font(DS.fontCompact)
                            .foregroundStyle(DS.ink)
                    }
                    HStack(spacing: 4) {
                        if let code = status.http_status {
                            Text("HTTP \(code) ·")
                                .monospacedDigit()
                        }
                        Text(BeijingTime.fullTime(status.checked_at))
                            .monospacedDigit()
                    }
                    .font(DS.fontPriorityMeta)
                    .foregroundStyle(DS.muted)
                }
            } else if loading {
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                    Text("正在检查上游连接")
                        .font(DS.fontCompact)
                        .foregroundStyle(DS.muted)
                }
            }

            if !error.isEmpty {
                Text(error)
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.bad)
            }
        }
        .task(id: "\(connectionKey)#\(revision)") {
            loading = true
            status = nil
            error = ""
            do {
                let fetched: UpstreamConnectionStatus = try await RustBridge.shared.api("GET", "/connection-status")
                if !Task.isCancelled {
                    status = fetched
                }
            } catch {
                if !Task.isCancelled {
                    self.error = error.localizedDescription
                }
            }
            if !Task.isCancelled {
                loading = false
            }
        }
    }
}
