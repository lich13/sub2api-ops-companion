import AppKit
import SwiftUI
import UniformTypeIdentifiers

/// Matches `ProxyManagement` in `desktop/src/ProxyManagement.tsx`.
public struct ProxyManagementTabView: View {
    public let active: Bool
    public let online: Bool
    public let connectionKey: String

    private enum ModalKind: Identifiable {
        case edit(ProxyManagedRow?)
        case batchAdd
        case confirmDelete([ProxyManagedRow])
        case quality(ProxyManagedRow, ProxyQualityResult)
        case credentials(ProxyManagedRow, ProxyCredentialsDTO)
        case accounts(ProxyManagedRow, [ProxyAssociatedAccount])
        case results(String, [(name: String, ok: Bool, message: String)])

        var id: String {
            switch self {
            case .edit(let r): return "edit-\(r?.id ?? 0)"
            case .batchAdd: return "batchAdd"
            case .confirmDelete(let rows): return "del-\(rows.map(\.id).description)"
            case .quality(let r, _): return "qual-\(r.id)"
            case .credentials(let r, _): return "cred-\(r.id)"
            case .accounts(let r, _): return "acc-\(r.id)"
            case .results(let t, _): return "res-\(t)"
            }
        }
    }

    @State private var pageData: ProxyManagedPage = .empty
    @State private var search: String = ""
    @State private var protocolFilter: String = ""
    @State private var statusFilter: String = ""
    @State private var sortBy: String = "id"
    @State private var sortOrder: String = "desc"
    @State private var page: Int = 1
    @State private var pageSize: Int = 20
    @State private var selectedIds: Set<Int> = []
    @State private var loading: Bool = false
    @State private var busyLabel: String = ""
    @State private var errorText: String = ""
    @State private var messageText: String = ""
    @State private var modal: ModalKind? = nil

    private static let statusLabels: [String: String] = [
        "active": "启用", "inactive": "停用", "expired": "已到期", "error": "异常",
    ]
    private static let qualityLabels: [String: String] = [
        "healthy": "优质", "warn": "警告", "challenge": "挑战", "failed": "失败", "fail": "失败",
    ]

    public init(active: Bool, online: Bool, connectionKey: String) {
        self.active = active
        self.online = online
        self.connectionKey = connectionKey
    }

    private var selectedRows: [ProxyManagedRow] {
        pageData.items.filter { selectedIds.contains($0.id) }
    }

    public var body: some View {
        ZStack {
            VStack(alignment: .leading, spacing: 12) {
                toolbarView

                if !messageText.isEmpty {
                    Text(messageText).font(DS.fontCompactMedium).foregroundStyle(DS.good)
                }
                if !errorText.isEmpty {
                    Text(errorText).font(DS.fontCompact).foregroundStyle(DS.bad)
                }

                ScrollView(.horizontal, showsIndicators: false) {
                    VStack(spacing: 0) {
                        tableHeader
                        Rectangle().fill(DS.line).frame(height: 1)
                        if pageData.items.isEmpty {
                            Text(loading ? "正在读取 IP 列表…" : "暂无匹配的代理 IP")
                                .font(DS.fontCompact)
                                .foregroundStyle(DS.muted)
                                .padding(16)
                                .frame(maxWidth: .infinity, alignment: .leading)
                        } else {
                            ForEach(Array(pageData.items.enumerated()), id: \.element.id) { idx, row in
                                tableRow(row)
                                if idx < pageData.items.count - 1 {
                                    Rectangle().fill(DS.line).frame(height: 1)
                                }
                            }
                        }
                    }
                    .frame(minWidth: 1036, maxWidth: .infinity, alignment: .leading)
                }
                .focusEffectDisabled()
                .background(DS.surface)
                .overlay(RoundedRectangle(cornerRadius: DS.cardRadius).strokeBorder(DS.line, lineWidth: 1))
                .clipShape(RoundedRectangle(cornerRadius: DS.cardRadius))

                paginationFooter
            }

            if let modal {
                modalOverlay(modal)
            }
        }
        .task(id: "\(connectionKey):\(active):\(online):\(page):\(pageSize):\(search):\(protocolFilter):\(statusFilter):\(sortBy):\(sortOrder)") {
            guard active, online else { return }
            if !search.isEmpty {
                try? await Task.sleep(for: .milliseconds(120))
                guard !Task.isCancelled else { return }
            }
            await loadPage()
        }
    }

    private var toolbarView: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                HStack(spacing: 6) {
                    Image(systemName: "magnifyingglass").font(DS.iconFont(11)).foregroundStyle(DS.muted)
                    TextField(text: $search, prompt: Text("搜索名称、地址或地区")) { EmptyView() }
                        .textFieldStyle(.plain)
                        .font(DS.fontCompact)
                        .frame(width: 180)
                        .onChange(of: search) { _, _ in page = 1 }
                }
                .padding(.horizontal, 8)
                .frame(height: DS.controlHeightCompact)
                .background(DS.surface)
                .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))

                filterMenu(
                    title: protocolFilter.isEmpty ? "全部协议" : protocolFilter.uppercased(),
                    options: [("", "全部协议"), ("http", "HTTP"), ("https", "HTTPS"), ("socks5", "SOCKS5"), ("socks5h", "SOCKS5H")],
                    onSelect: { protocolFilter = $0; page = 1 }
                )

                filterMenu(
                    title: statusFilter.isEmpty ? "全部状态" : (Self.statusLabels[statusFilter] ?? statusFilter),
                    options: [("", "全部状态"), ("active", "启用"), ("inactive", "停用"), ("expired", "已到期")],
                    onSelect: { statusFilter = $0; page = 1 }
                )

                Spacer()

                Button("导入 JSON") { Task { await importJSON() } }
                    .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                    .disabled(!online || !busyLabel.isEmpty)

                Button("导出 JSON") { Task { await exportJSON() } }
                    .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                    .disabled(!online || !busyLabel.isEmpty)

                Button("批量添加") { modal = .batchAdd }
                    .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                    .disabled(!online || !busyLabel.isEmpty)

                Button("+ 新增 IP") { modal = .edit(nil) }
                    .buttonStyle(LinearButtonStyle(variant: .primary, compact: true))
                    .disabled(!online || !busyLabel.isEmpty)
            }

            if !selectedRows.isEmpty {
                HStack(spacing: 8) {
                    Text("已选择 \(selectedRows.count) 项")
                        .font(DS.fontCompactSemibold)
                        .monospacedDigit()
                        .foregroundStyle(DS.ink)
                    Button("批量测试") { Task { await runDiagnose(rows: selectedRows, quality: false) } }
                        .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                    Button("批量质量检测") { Task { await runDiagnose(rows: selectedRows, quality: true) } }
                        .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                    Button("批量删除") { modal = .confirmDelete(selectedRows) }
                        .buttonStyle(LinearButtonStyle(variant: .danger, compact: true))
                    Spacer()
                    Button("取消选择") { selectedIds.removeAll() }
                        .buttonStyle(LinearButtonStyle(variant: .ghost, compact: true))
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 6)
                .background(DS.accentBg)
                .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
            }
        }
    }

    private func filterMenu(title: String, options: [(String, String)], onSelect: @escaping (String) -> Void) -> some View {
        Menu {
            ForEach(options, id: \.0) { opt in
                Button(opt.1) { onSelect(opt.0) }
            }
        } label: {
            HStack(spacing: 4) {
                Text(title).font(DS.fontCompactMedium).foregroundStyle(DS.ink)
                Image(systemName: "chevron.down").font(DS.iconFont(10)).foregroundStyle(DS.muted)
            }
            .padding(.horizontal, 10)
            .frame(height: DS.controlHeightCompact)
            .background(DS.surface)
            .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
            .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
    }

    private var tableHeader: some View {
        HStack(spacing: 8) {
            let allChecked = !pageData.items.isEmpty && pageData.items.allSatisfy { selectedIds.contains($0.id) }
            Button {
                if allChecked { selectedIds.removeAll() } else { selectedIds = Set(pageData.items.map(\.id)) }
            } label: {
                Image(systemName: allChecked ? "checkmark.square.fill" : "square")
                    .font(DS.iconFont(13))
                    .foregroundStyle(allChecked ? DS.accent : DS.muted)
            }
            .buttonStyle(.plain)
            .frame(width: 24)

            sortHead("名称", key: "name", width: 120)
            colHead("协议", width: 62)
            colHead("地址", width: 164)
            colHead("地区 / 出口", width: 126)
            sortHead("延迟 / 质量", key: "latency_ms", width: 108)
            colHead("状态", width: 58)
            sortHead("账号", key: "account_count", width: 52)
            colHead("到期", width: 96)
            colHead("操作", width: 152)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 12)
        .frame(height: 34)
        .background(DS.surfaceSubtle)
    }

    private func colHead(_ title: String, width: CGFloat) -> some View {
        Text(title).font(DS.fontTableHeader).foregroundStyle(DS.muted).frame(width: width, alignment: .leading)
    }

    private func sortHead(_ title: String, key: String, width: CGFloat) -> some View {
        Button {
            if sortBy == key {
                sortOrder = sortOrder == "asc" ? "desc" : "asc"
            } else {
                sortBy = key
                sortOrder = "desc"
            }
            page = 1
        } label: {
            HStack(spacing: 3) {
                Text(title)
                if sortBy == key {
                    Image(systemName: sortOrder == "asc" ? "arrow.up" : "arrow.down").font(DS.iconFont(9))
                }
            }
            .font(DS.fontTableHeader)
            .foregroundStyle(DS.muted)
            .frame(width: width, alignment: .leading)
        }
        .buttonStyle(.plain)
    }

    private func tableRow(_ row: ProxyManagedRow) -> some View {
        let isSelected = selectedIds.contains(row.id)
        return HStack(spacing: 8) {
            Button {
                if isSelected { selectedIds.remove(row.id) } else { selectedIds.insert(row.id) }
            } label: {
                Image(systemName: isSelected ? "checkmark.square.fill" : "square")
                    .font(DS.iconFont(13))
                    .foregroundStyle(isSelected ? DS.accent : DS.muted)
            }
            .buttonStyle(.plain)
            .frame(width: 24)

            VStack(alignment: .leading, spacing: 2) {
                Text(row.name).font(DS.fontCompactSemibold).foregroundStyle(DS.ink).lineLimit(1)
                Text("#\(row.id)").font(DS.fontMono11).foregroundStyle(DS.muted)
            }
            .frame(width: 120, alignment: .leading)

            Text(verbatim: row.protocolName.uppercased())
                .font(DS.fontMono11)
                .foregroundStyle(DS.ink)
                .frame(width: 62, alignment: .leading)

            Text(verbatim: row.formattedAddress)
                .font(DS.fontMono11)
                .foregroundStyle(DS.ink)
                .lineLimit(1)
                .frame(width: 164, alignment: .leading)

            VStack(alignment: .leading, spacing: 2) {
                Text([row.country, row.city].compactMap { $0 }.filter { !$0.isEmpty }.joined(separator: " · ").isEmpty ? "—" : [row.country, row.city].compactMap { $0 }.filter { !$0.isEmpty }.joined(separator: " · "))
                    .font(DS.fontResetTime)
                    .foregroundStyle(DS.ink)
                    .lineLimit(1)
                if let ip = row.ip_address, !ip.isEmpty {
                    Text(verbatim: ip).font(DS.fontMono11).foregroundStyle(DS.muted).lineLimit(1)
                }
            }
            .frame(width: 126, alignment: .leading)

            VStack(alignment: .leading, spacing: 2) {
                Text(row.latency_ms.map { "\($0) ms" } ?? "—")
                    .font(DS.fontResetTime)
                    .monospacedDigit()
                    .foregroundStyle(DS.ink)
                if let qGrade = row.quality_grade, !qGrade.isEmpty {
                    Text("\(qGrade) · \(Self.qualityLabels[row.quality_status ?? ""] ?? (row.quality_score.map { "\($0)分" } ?? ""))")
                        .font(DS.fontUsageStatMedium)
                        .foregroundStyle(row.quality_status == "healthy" ? DS.good : DS.warn)
                }
            }
            .frame(width: 108, alignment: .leading)

            Text(Self.statusLabels[row.status] ?? row.status)
                .font(DS.fontResetTimeSemibold)
                .foregroundStyle(row.status == "active" ? DS.good : DS.muted)
                .padding(.horizontal, 6)
                .padding(.vertical, 2)
                .background(row.status == "active" ? DS.goodBg : DS.surfaceSubtle)
                .clipShape(RoundedRectangle(cornerRadius: 4))
                .frame(width: 58, alignment: .leading)

            Button {
                Task { await showAccounts(row) }
            } label: {
                Text("\(row.account_count ?? 0)")
                    .font(DS.fontMono11)
                    .foregroundStyle(DS.accent)
            }
            .buttonStyle(.plain)
            .frame(width: 52, alignment: .leading)

            Text(row.expires_at == nil ? "长期" : BeijingTime.fullTime(row.expires_at))
                .font(DS.fontResetTime)
                .monospacedDigit()
                .foregroundStyle(DS.muted)
                .lineLimit(1)
                .frame(width: 96, alignment: .leading)

            HStack(spacing: 4) {
                Button("测试") { Task { await runDiagnose(rows: [row], quality: false) } }
                    .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                Button("编辑") { modal = .edit(row) }
                    .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                Menu {
                    Button("质量检测") { Task { await runDiagnose(rows: [row], quality: true) } }
                    Button("查看凭据") { Task { await showCredentials(row) } }
                    Button("复制地址") {
                        NSPasteboard.general.clearContents()
                        NSPasteboard.general.setString(row.formattedAddress, forType: .string)
                        messageText = "已复制地址"
                    }
                    Divider()
                    Button("删除", role: .destructive) { modal = .confirmDelete([row]) }
                } label: {
                    Image(systemName: "ellipsis")
                        .font(DS.iconFont(12))
                        .foregroundStyle(DS.muted)
                        .frame(width: 26, height: 26)
                }
                .menuStyle(.borderlessButton)
                .menuIndicator(.hidden)
                .fixedSize()
            }
            .frame(width: 152, alignment: .leading)

            Spacer(minLength: 0)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
    }

    private var paginationFooter: some View {
        HStack {
            Text("共 \(pageData.total) 条 · 第 \(page) / \(max(1, pageData.pages)) 页")
                .font(DS.fontResetTime)
                .monospacedDigit()
                .foregroundStyle(DS.muted)
            Spacer()
            Button("上一页") { page = max(1, page - 1) }
                .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                .disabled(page <= 1 || loading)
            Button("下一页") { page += 1 }
                .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                .disabled(page >= max(1, pageData.pages) || loading)
        }
    }

    @ViewBuilder
    private func modalOverlay(_ kind: ModalKind) -> some View {
        switch kind {
        case .edit(let row):
            ProxyEditorSheet(
                row: row,
                online: online,
                onSaved: {
                    modal = nil
                    messageText = "已保存"
                    Task { await loadPage() }
                },
                onClose: { modal = nil }
            )
        case .batchAdd:
            ProxyBatchAddSheet(
                online: online,
                onSaved: { msg in
                    modal = nil
                    messageText = msg
                    Task { await loadPage() }
                },
                onClose: { modal = nil }
            )
        case .confirmDelete(let rows):
            simpleModal(title: "确认删除 IP") {
                Text("确定删除所选的 \(rows.count) 个代理节点吗？关联账号将不再保留此代理。")
                    .font(DS.fontBody)
                    .foregroundStyle(DS.ink)
                HStack {
                    Spacer()
                    Button("取消") { modal = nil }
                        .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                    Button("确认删除") {
                        Task { await deleteRows(rows) }
                    }
                    .buttonStyle(LinearButtonStyle(variant: .danger, compact: true))
                }
            }
        case .quality(let row, let q):
            simpleModal(title: "\(row.name) · 质量报告 (\(q.grade) \(q.score)/100)") {
                Text(q.summary).font(DS.fontCompact).foregroundStyle(DS.ink)
                ForEach(q.items) { item in
                    HStack {
                        Text(verbatim: item.target).font(DS.fontMono11).foregroundStyle(DS.ink)
                        Spacer()
                        Text("\(item.status)\(item.latency_ms.map { " · \($0) ms" } ?? "")")
                            .font(DS.fontResetTime)
                            .monospacedDigit()
                            .foregroundStyle(item.status == "pass" ? DS.good : DS.warn)
                    }
                }
            }
        case .credentials(let row, let cred):
            simpleModal(title: "\(row.name) · 认证凭据") {
                Text(verbatim: "地址：\(row.fullURL(credentials: cred))")
                    .font(DS.fontMono11)
                    .foregroundStyle(DS.ink)
                    .textSelection(.enabled)
            }
        case .accounts(let row, let list):
            simpleModal(title: "\(row.name) · 关联账号 (\(list.count))") {
                if list.isEmpty {
                    Text("当前无关联账号").font(DS.fontCompact).foregroundStyle(DS.muted)
                } else {
                    ForEach(list) { acc in
                        Text("#\(acc.id) \(acc.name) (\(acc.platform) · \(acc.type))")
                            .font(DS.fontCompact)
                            .foregroundStyle(DS.ink)
                    }
                }
            }
        case .results(let title, let rows):
            simpleModal(title: title) {
                ScrollView {
                    VStack(alignment: .leading, spacing: 6) {
                        ForEach(Array(rows.enumerated()), id: \.offset) { _, r in
                            HStack(alignment: .top) {
                                Text(r.name).font(DS.fontCompactSemibold).foregroundStyle(DS.ink)
                                Spacer()
                                Text(r.message)
                                    .font(DS.fontResetTime)
                                    .foregroundStyle(r.ok ? DS.good : DS.bad)
                            }
                        }
                    }
                }
                .frame(maxHeight: 220)
            }
        }
    }

    private func simpleModal<Content: View>(title: String, @ViewBuilder content: () -> Content) -> some View {
        ZStack {
            DS.backdrop.ignoresSafeArea().onTapGesture { modal = nil }
            VStack(alignment: .leading, spacing: 12) {
                HStack {
                    Text(title).font(DS.fontDialogTitle).foregroundStyle(DS.ink)
                    Spacer()
                    LinearIconButton(systemName: "xmark", label: "关闭") { modal = nil }
                }
                content()
            }
            .padding(20)
            .frame(width: 480)
            .background(DS.surface)
            .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(DS.line, lineWidth: 1))
            .clipShape(RoundedRectangle(cornerRadius: 12))
            .sub2opsDialogFocus(onEscape: { modal = nil })
        }
    }

    private func buildQueryString(exporting: Bool = false) -> String {
        var comps = URLComponents()
        var items: [URLQueryItem] = [
            URLQueryItem(name: "sort_by", value: sortBy),
            URLQueryItem(name: "sort_order", value: sortOrder),
        ]
        if !exporting {
            items.append(URLQueryItem(name: "page", value: "\(page)"))
            items.append(URLQueryItem(name: "page_size", value: "\(pageSize)"))
        }
        let q = search.trimmingCharacters(in: .whitespacesAndNewlines)
        if !q.isEmpty { items.append(URLQueryItem(name: "search", value: q)) }
        if !protocolFilter.isEmpty { items.append(URLQueryItem(name: "protocol", value: protocolFilter)) }
        if !statusFilter.isEmpty { items.append(URLQueryItem(name: "status", value: statusFilter)) }
        comps.queryItems = items
        return comps.percentEncodedQuery ?? ""
    }

    private func loadPage() async {
        guard online else { return }
        loading = true
        errorText = ""
        defer { loading = false }
        do {
            let r: ProxyManagedPage = try await RustBridge.shared.api("GET", "/proxy-management?\(buildQueryString())")
            if r.pages > 0 && page > r.pages {
                page = r.pages
                return
            }
            pageData = r
            selectedIds = selectedIds.intersection(Set(r.items.map(\.id)))
        } catch {
            errorText = error.localizedDescription
        }
    }

    private func runDiagnose(rows: [ProxyManagedRow], quality: Bool) async {
        guard online, busyLabel.isEmpty else { return }
        busyLabel = quality ? "quality" : "test"
        defer { busyLabel = "" }
        var out: [(name: String, ok: Bool, message: String)] = []
        for row in rows {
            do {
                if quality {
                    let q: ProxyQualityResult = try await RustBridge.shared.api("POST", "/proxies/\(row.id)/quality-check")
                    out.append((row.name, true, "\(q.grade) · \(q.score)/100 \(q.summary)"))
                    if rows.count == 1 {
                        modal = .quality(row, q)
                        await loadPage()
                        return
                    }
                } else {
                    let c: ProxyConnectionResult = try await RustBridge.shared.api("POST", "/proxies/\(row.id)/test")
                    out.append((row.name, c.success, c.success ? "连接成功\(c.latency_ms.map { " · \($0) ms" } ?? "")" : (c.message.isEmpty ? "连接失败" : c.message)))
                }
            } catch {
                out.append((row.name, false, error.localizedDescription))
            }
        }
        if rows.count > 1 || out.contains(where: { !$0.ok }) {
            modal = .results(quality ? "质量检测结果" : "连接测试结果", out)
        } else if let first = out.first {
            messageText = first.message
        }
        await loadPage()
    }

    private func deleteRows(_ rows: [ProxyManagedRow]) async {
        guard online else { return }
        busyLabel = "delete"
        defer { busyLabel = "" }
        do {
            if rows.count == 1 {
                let _: JSONValue = try await RustBridge.shared.api("DELETE", "/proxy-management/\(rows[0].id)")
                selectedIds.remove(rows[0].id)
                modal = nil
                messageText = "已删除"
            } else {
                struct BatchDel: Decodable, Sendable { var deleted_ids: [Int] }
                let res: BatchDel = try await RustBridge.shared.api(
                    "POST",
                    "/proxy-management/batch-delete",
                    body: .object(["ids": .array(rows.map { .int(Int64($0.id)) })])
                )
                selectedIds.subtract(res.deleted_ids)
                modal = nil
                messageText = "已删除 \(res.deleted_ids.count) 项"
            }
            await loadPage()
        } catch {
            errorText = error.localizedDescription
        }
    }

    private func showCredentials(_ row: ProxyManagedRow) async {
        do {
            let cred: ProxyCredentialsDTO = try await RustBridge.shared.api("GET", "/proxy-management/\(row.id)/credentials")
            modal = .credentials(row, cred)
        } catch {
            errorText = error.localizedDescription
        }
    }

    private func showAccounts(_ row: ProxyManagedRow) async {
        struct Env: Decodable, Sendable { var items: [ProxyAssociatedAccount]? }
        do {
            let raw: JSONValue = try await RustBridge.shared.api("GET", "/proxy-management/\(row.id)/accounts")
            let data = try JSONEncoder().encode(raw)
            let list = (try? JSONDecoder().decode([ProxyAssociatedAccount].self, from: data))
                ?? (try? JSONDecoder().decode(Env.self, from: data))?.items ?? []
            modal = .accounts(row, list)
        } catch {
            errorText = error.localizedDescription
        }
    }

    private func importJSON() async {
        let panel = NSOpenPanel()
        panel.allowedContentTypes = [.json]
        panel.allowsMultipleSelection = false
        guard panel.runModal() == .OK, let url = panel.url else { return }
        do {
            let text = try String(contentsOf: url, encoding: .utf8)
            let val = try ProxyDataParser.parseImport(text)
            let r: ProxyImportResultDTO = try await RustBridge.shared.api(
                "POST",
                "/proxy-management/data",
                body: .object(["data": val])
            )
            messageText = "新增 \(r.proxy_created) 个，复用 \(r.proxy_reused) 个，失败 \(r.proxy_failed) 个"
            await loadPage()
        } catch {
            errorText = error.localizedDescription
        }
    }

    private func exportJSON() async {
        let panel = NSSavePanel()
        panel.allowedContentTypes = [.json]
        panel.nameFieldStringValue = "sub2api-proxies.json"
        guard panel.runModal() == .OK, let url = panel.url else { return }
        do {
            let raw: JSONValue = try await RustBridge.shared.api("GET", "/proxy-management/data?\(buildQueryString(exporting: true))")
            let encoder = JSONEncoder()
            encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
            let data = try encoder.encode(raw)
            try data.write(to: url, options: .atomic)
            messageText = "已导出代理 JSON"
        } catch {
            errorText = error.localizedDescription
        }
    }
}
