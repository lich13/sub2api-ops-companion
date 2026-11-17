import SwiftUI

public struct ProxyOptionItem: Codable, Sendable, Equatable, Identifiable {
    public var id: Int
    public var name: String
    public var protocolName: String?
    public var host: String?
    public var port: Int?
    public var status: String?
    public var country: String?

    enum CodingKeys: String, CodingKey {
        case id, name, host, port, status, country
        case protocolName = "protocol"
    }

    public var addressSummary: String {
        let proto = (protocolName ?? "socks5").uppercased()
        if let host, !host.isEmpty, let port {
            return "\(proto) · \(host):\(port)"
        }
        return proto
    }
}

public struct ProxyQuickTestResult: Codable, Sendable, Equatable {
    public var success: Bool
    public var message: String?
    public var latency_ms: Int?
    public var ip_address: String?
    public var country: String?
}

public struct ProxyQuickQualityItem: Codable, Sendable, Equatable {
    public var target: String
    public var status: String
    public var http_status: Int?
    public var latency_ms: Int?
    public var message: String?
}

public struct ProxyQuickQualityResult: Codable, Sendable, Equatable {
    public var score: Int
    public var grade: String
    public var summary: String
    public var exit_ip: String?
    public var country: String?
    public var base_latency_ms: Int?
    public var checked_at: Int?
    public var items: [ProxyQuickQualityItem]
}

private struct ProxyListEnvelope: Decodable, Sendable {
    var items: [ProxyOptionItem]?
    var proxies: [ProxyOptionItem]?
}

public struct ProxyOptionsPickerView: View {
    @Binding public var selectedProxyId: Int?
    public let online: Bool
    public let connectionKey: String

    @State private var proxies: [ProxyOptionItem] = []
    @State private var query: String = ""
    @State private var loading: Bool = false
    @State private var loadError: String = ""
    @State private var testingIds: Set<Int> = []
    @State private var qualityIds: Set<Int> = []
    @State private var testResults: [Int: ProxyQuickTestResult] = [:]
    @State private var qualityResults: [Int: ProxyQuickQualityResult] = [:]
    @State private var diagErrors: [Int: String] = [:]

    public init(selectedProxyId: Binding<Int?>, online: Bool, connectionKey: String) {
        self._selectedProxyId = selectedProxyId
        self.online = online
        self.connectionKey = connectionKey
    }

    private var filteredProxies: [ProxyOptionItem] {
        let q = query.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard !q.isEmpty else { return proxies }
        return proxies.filter { p in
            "\(p.name) \(p.host ?? "") \(p.id)".lowercased().contains(q)
        }
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                Image(systemName: "magnifyingglass")
                    .font(DS.iconFont(12))
                    .foregroundStyle(DS.muted)
                TextField(text: $query, prompt: Text(verbatim: "搜索代理名称或地址")) { EmptyView() }
                    .textFieldStyle(.plain)
                    .font(DS.fontCompact)
            }
            .padding(.horizontal, 10)
            .frame(height: DS.controlHeightCompact)
            .background(DS.surface)
            .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
            .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))

            if !loadError.isEmpty {
                Text(loadError)
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.bad)
            }

            LinearBoundedScrollView(maxHeight: 240) {
                VStack(alignment: .leading, spacing: 6) {
                    directOptionRow
                    ForEach(filteredProxies) { item in
                        proxyRow(item)
                    }
                    if loading {
                        Text("正在读取代理列表…")
                            .font(DS.fontCompact)
                            .foregroundStyle(DS.muted)
                            .padding(.vertical, 6)
                    }
                }
            }
        }
        .task(id: "\(connectionKey):\(online)") {
            guard online else { return }
            await loadProxies()
        }
    }

    private var directOptionRow: some View {
        let chosen = selectedProxyId == nil
        return Button {
            selectedProxyId = nil
        } label: {
            HStack(spacing: 8) {
                Image(systemName: chosen ? "largecircle.fill.circle" : "circle")
                    .font(DS.iconFont(13))
                    .foregroundStyle(chosen ? DS.accent : DS.muted)
                VStack(alignment: .leading, spacing: 2) {
                    Text("直连（不使用代理）")
                        .font(DS.fontCompactSemibold)
                        .foregroundStyle(DS.ink)
                    Text("直接由服务器出口发起上游请求")
                        .font(DS.fontResetTime)
                        .foregroundStyle(DS.muted)
                }
                Spacer()
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 8)
            .background(chosen ? DS.accentBg : DS.surfaceSubtle)
            .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(chosen ? DS.accent : DS.line, lineWidth: 1))
            .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
        }
        .buttonStyle(.linearPlain)
    }

    private func proxyRow(_ item: ProxyOptionItem) -> some View {
        let chosen = selectedProxyId == item.id
        let tResult = testResults[item.id]
        let qResult = qualityResults[item.id]
        let dError = diagErrors[item.id]
        return VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                Button {
                    selectedProxyId = item.id
                } label: {
                    HStack(spacing: 8) {
                        Image(systemName: chosen ? "largecircle.fill.circle" : "circle")
                            .font(DS.iconFont(13))
                            .foregroundStyle(chosen ? DS.accent : DS.muted)
                        VStack(alignment: .leading, spacing: 2) {
                            HStack(spacing: 6) {
                                Text(item.name)
                                    .font(DS.fontCompactSemibold)
                                    .foregroundStyle(DS.ink)
                                    .lineLimit(1)
                                Text("#\(item.id)")
                                    .font(DS.fontPriorityMeta)
                                    .monospacedDigit()
                                    .foregroundStyle(DS.muted)
                            }
                            Text(verbatim: item.addressSummary)
                                .font(DS.fontPriorityMeta)
                                .monospacedDigit()
                                .foregroundStyle(DS.muted)
                                .lineLimit(1)
                        }
                        Spacer(minLength: 4)
                    }
                }
                .buttonStyle(.linearPlain)

                HStack(spacing: 6) {
                    Button(testingIds.contains(item.id) ? "测试中" : "测试连接") {
                        Task { await runProxyTest(item.id) }
                    }
                    .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                    .disabled(!online || testingIds.contains(item.id))

                    Button(qualityIds.contains(item.id) ? "检测中" : "质量检测") {
                        Task { await runQualityCheck(item.id) }
                    }
                    .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                    .disabled(!online || qualityIds.contains(item.id))
                }
            }

            if let tResult {
                Text(
                    tResult.success
                        ? "连接成功\(tResult.ip_address.map { " · \($0)" } ?? "")\(tResult.latency_ms.map { " · \($0) ms" } ?? "")"
                        : (tResult.message ?? "连接失败")
                )
                .font(DS.fontResetTime)
                .monospacedDigit()
                .foregroundStyle(tResult.success ? DS.good : DS.bad)
            }
            if let qResult {
                Text("\(qResult.score) 分 · \(qResult.grade) · \(qResult.summary)")
                    .font(DS.fontResetTime)
                    .monospacedDigit()
                    .foregroundStyle(DS.ink)
            }
            if let dError, !dError.isEmpty {
                Text(dError)
                    .font(DS.fontResetTime)
                    .foregroundStyle(DS.bad)
            }
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 8)
        .background(chosen ? DS.accentBg : DS.surfaceSubtle)
        .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(chosen ? DS.accent : DS.line, lineWidth: 1))
        .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
    }

    private func loadProxies() async {
        loading = true
        defer { loading = false }
        do {
            let raw: JSONValue = try await RustBridge.shared.api("GET", "/proxies")
            let data = try JSONEncoder().encode(raw)
            if let direct = try? JSONDecoder().decode([ProxyOptionItem].self, from: data) {
                proxies = direct
            } else if let env = try? JSONDecoder().decode(ProxyListEnvelope.self, from: data) {
                proxies = env.items ?? env.proxies ?? []
            } else {
                proxies = []
            }
            loadError = ""
        } catch {
            loadError = error.localizedDescription
        }
    }

    private func runProxyTest(_ id: Int) async {
        testingIds.insert(id)
        diagErrors[id] = nil
        defer { testingIds.remove(id) }
        do {
            let result: ProxyQuickTestResult = try await RustBridge.shared.api("POST", "/proxies/\(id)/test", body: .object([:]))
            testResults[id] = result
        } catch {
            diagErrors[id] = error.localizedDescription
        }
    }

    private func runQualityCheck(_ id: Int) async {
        qualityIds.insert(id)
        diagErrors[id] = nil
        defer { qualityIds.remove(id) }
        do {
            let result: ProxyQuickQualityResult = try await RustBridge.shared.api("POST", "/proxies/\(id)/quality-check", body: .object([:]))
            qualityResults[id] = result
        } catch {
            diagErrors[id] = error.localizedDescription
        }
    }
}
