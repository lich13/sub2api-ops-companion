import AppKit
import SwiftUI
import UniformTypeIdentifiers

/// Matches `ProxyManagement` in `desktop/src/ProxyManagement.tsx` (`features_ip_light.png`).
public struct ProxyManagementTabView: View {
    public var store: AppStore?
    public let active: Bool
    public let online: Bool
    public let connectionKey: String

    enum ModalKind: Identifiable {
        case edit(ProxyManagedRow?)
        case batchAdd
        case confirmDelete([ProxyManagedRow])
        case quality(ProxyManagedRow, ProxyQualityResult)
        case credentials(ProxyManagedRow, ProxyCredentialsDTO)
        case accounts(ProxyManagedRow, [AccountIdentity])
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

    @State var pageData: ProxyManagedPage = .empty
    @State var search: String = ""
    @State var protocolFilter: String = ""
    @State var statusFilter: String = ""
    @State var sortBy: String = "id"
    @State var sortOrder: String = "desc"
    @State var page: Int = 1
    @State var pageSize: Int = 20
    @State var selectedIds: Set<Int> = []
    @State var loading: Bool = false
    @State var busyLabel: String = ""
    @State var errorText: String = ""
    @State var messageText: String = ""
    @State var fallbackModal: ModalKind? = nil
    @State private var fallbackBackdropGuard = ModalBackdropGuard()
    @State var qualityById: [Int: ProxyQualityResult] = [:]
    @State private var viewportWidth: CGFloat = 1100
    @State private var hoveredRowId: Int? = nil

    private static let statusLabels: [String: String] = [
        "active": "启用", "inactive": "停用", "expired": "已到期", "error": "异常",
    ]
    private static let qualityLabels: [String: String] = [
        "healthy": "优质", "warn": "警告", "challenge": "挑战", "failed": "失败", "fail": "失败",
    ]
    static let qualityTargetLabels: [String: String] = [
        "pass": "通过", "warn": "警告", "fail": "失败", "challenge": "挑战",
    ]

    public init(store: AppStore? = nil, active: Bool, online: Bool, connectionKey: String) {
        self.store = store
        self.active = active
        self.online = online
        self.connectionKey = connectionKey
    }

    private var selectedRows: [ProxyManagedRow] {
        pageData.items.filter { selectedIds.contains($0.id) }
    }

    private let leftPinnedWidth: CGFloat = 148
    private let rightPinnedWidth: CGFloat = 156
    private let rowHeight: CGFloat = 54

    public var body: some View {
        ZStack {
            VStack(alignment: .leading, spacing: 0) {
                toolbarView
                    .padding(16)

                if !messageText.isEmpty || !errorText.isEmpty {
                    VStack(alignment: .leading, spacing: 4) {
                        if !messageText.isEmpty {
                            Text(messageText).font(DS.fontCompactMedium).foregroundStyle(DS.good)
                        }
                        if !errorText.isEmpty {
                            Text(errorText).font(DS.fontCompact).foregroundStyle(DS.bad)
                        }
                    }
                    .padding(.horizontal, 16)
                    .padding(.bottom, 10)
                }

                Rectangle().fill(DS.line).frame(height: 1)

                proxyTableSurface
                    .background(LinearWidthObserver { viewportWidth = $0 })

                Rectangle().fill(DS.line).frame(height: 1)

                paginationFooter
                    .padding(.horizontal, 16)
                    .padding(.vertical, 12)
            }
            .background(DS.surface)
            .overlay(RoundedRectangle(cornerRadius: 12).strokeBorder(DS.line, lineWidth: 1))
            .clipShape(RoundedRectangle(cornerRadius: 12))

            if let modal = fallbackModal {
                ZStack {
                    DS.backdrop.ignoresSafeArea().onTapGesture {
                        fallbackBackdropGuard.attemptDismiss(defaultClose: { fallbackModal = nil })
                    }
                    modalView(modal, dismiss: { fallbackModal = nil })
                        .id(modal.id)
                        .environment(\.modalBackdropGuard, fallbackBackdropGuard)
                }
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
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 10) {
                LinearSearchField(
                    text: $search,
                    prompt: "搜索名称或地址",
                    height: DS.controlHeight,
                    width: 340
                )
                .onChange(of: search) { _, _ in page = 1 }

                LinearSelect(
                    label: protocolFilter.isEmpty ? "全部协议" : protocolFilter.uppercased(),
                    accessibilityTitle: "协议筛选",
                    height: DS.controlHeight,
                    minWidth: 112,
                    entries: [("", "全部协议"), ("http", "HTTP"), ("https", "HTTPS"), ("socks5", "SOCKS5"), ("socks5h", "SOCKS5H")].map { opt in
                        LinearMenuEntry(id: opt.0.isEmpty ? "__all__" : opt.0, title: opt.1, checked: protocolFilter == opt.0) {
                            protocolFilter = opt.0
                            page = 1
                        }
                    }
                )

                LinearSelect(
                    label: statusFilter.isEmpty ? "全部状态" : (Self.statusLabels[statusFilter] ?? statusFilter),
                    accessibilityTitle: "状态筛选",
                    height: DS.controlHeight,
                    minWidth: 112,
                    entries: [("", "全部状态"), ("active", "启用"), ("inactive", "停用"), ("expired", "已到期")].map { opt in
                        LinearMenuEntry(id: opt.0.isEmpty ? "__all__" : opt.0, title: opt.1, checked: statusFilter == opt.0) {
                            statusFilter = opt.0
                            page = 1
                        }
                    }
                )

                Spacer()

                Text("\(pageData.total) 个 IP")
                    .font(DS.fontCompactMedium)
                    .monospacedDigit()
                    .foregroundStyle(DS.muted)
            }

            HStack(spacing: 8) {
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
                        Button("批量删除") { presentModal(.confirmDelete(selectedRows)) }
                            .buttonStyle(LinearButtonStyle(variant: .danger, compact: true))
                        Button("取消选择") { selectedIds.removeAll() }
                            .buttonStyle(LinearButtonStyle(variant: .ghost, compact: true))
                    }
                }

                Spacer()

                LinearIconButton(
                    systemName: "arrow.clockwise",
                    label: "刷新 IP 列表",
                    size: 13,
                    boxSize: DS.controlHeightCompact,
                    bordered: true,
                    disabled: !online || loading || !busyLabel.isEmpty
                ) {
                    Task { await loadPage() }
                }

                LinearMenuIconTrigger(
                    systemName: "ellipsis",
                    label: "更多代理操作",
                    titleText: "更多",
                    boxSize: DS.controlHeightCompact,
                    bordered: true,
                    disabled: !online || !busyLabel.isEmpty,
                    entries: [
                        LinearMenuEntry(id: "batch", title: "批量添加") { presentModal(.batchAdd) },
                        LinearMenuEntry(id: "import", title: "导入 JSON") { Task { await importJSON() } },
                        LinearMenuEntry(id: "export", title: "导出 JSON") { Task { await exportJSON() } },
                    ]
                )

                Button {
                    presentModal(.edit(nil))
                } label: {
                    HStack(spacing: 5) {
                        Image(systemName: "plus")
                            .font(DS.iconFont(12, weight: .semibold))
                        Text("新增 IP")
                    }
                }
                .buttonStyle(LinearButtonStyle(variant: .primary))
                .disabled(!online || !busyLabel.isEmpty)
            }
        }
    }

    private var middleAvailableWidth: CGFloat {
        max(0, viewportWidth - leftPinnedWidth - rightPinnedWidth - 2)
    }

    private var middleMinWidth: CGFloat {
        732
    }

    private var extraWidth: CGFloat {
        max(0, middleAvailableWidth - middleMinWidth)
    }

    @ViewBuilder
    private var proxyTableSurface: some View {
        let allChecked = !pageData.items.isEmpty && pageData.items.allSatisfy { selectedIds.contains($0.id) }
        if pageData.items.isEmpty {
            VStack(spacing: 0) {
                HStack(spacing: 0) {
                    leftPinnedHeader(allChecked: allChecked)
                    Rectangle().fill(DS.line).frame(width: 1, height: 36)
                    middleHeader
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .clipped()
                    Rectangle().fill(DS.line).frame(width: 1, height: 36)
                    rightPinnedHeader
                }
                .background(DS.surfaceSubtle)

                Rectangle().fill(DS.line).frame(height: 1)

                Text(loading ? "正在读取 IP 列表…" : "暂无匹配的代理 IP")
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.muted)
                    .padding(.vertical, 32)
                    .frame(maxWidth: .infinity, alignment: .center)
            }
        } else {
            HStack(alignment: .top, spacing: 0) {
                // Sticky Left: Checkbox + Name (DESIGN.md line 32)
                VStack(spacing: 0) {
                    leftPinnedHeader(allChecked: allChecked)
                        .background(DS.surfaceSubtle)
                    Rectangle().fill(DS.line).frame(height: 1)
                    ForEach(Array(pageData.items.enumerated()), id: \.element.id) { idx, row in
                        leftPinnedCell(row)
                        if idx < pageData.items.count - 1 {
                            Rectangle().fill(DS.line).frame(height: 1)
                        }
                    }
                }
                .frame(width: leftPinnedWidth)

                Rectangle().fill(DS.line).frame(width: 1)

                // Horizontally Scrollable Middle Columns
                ScrollView(.horizontal, showsIndicators: false) {
                    VStack(spacing: 0) {
                        middleHeader
                            .background(DS.surfaceSubtle)
                        Rectangle().fill(DS.line).frame(height: 1)
                        ForEach(Array(pageData.items.enumerated()), id: \.element.id) { idx, row in
                            middleRowCells(row)
                            if idx < pageData.items.count - 1 {
                                Rectangle().fill(DS.line).frame(height: 1)
                            }
                        }
                    }
                    .frame(minWidth: max(middleMinWidth, middleAvailableWidth), alignment: .leading)
                }
                .focusEffectDisabled()

                Rectangle().fill(DS.line).frame(width: 1)

                // Sticky Right: Single-row Actions (DESIGN.md line 32 & proxy-management.css:270)
                VStack(spacing: 0) {
                    rightPinnedHeader
                        .background(DS.surfaceSubtle)
                    Rectangle().fill(DS.line).frame(height: 1)
                    ForEach(Array(pageData.items.enumerated()), id: \.element.id) { idx, row in
                        rightPinnedCell(row)
                        if idx < pageData.items.count - 1 {
                            Rectangle().fill(DS.line).frame(height: 1)
                        }
                    }
                }
                .frame(width: rightPinnedWidth)
            }
        }
    }

    private func leftPinnedHeader(allChecked: Bool) -> some View {
        HStack(spacing: 8) {
            LinearCheckbox(isChecked: allChecked, accessibilityTitle: "全选 IP") {
                if allChecked { selectedIds.removeAll() } else { selectedIds = Set(pageData.items.map(\.id)) }
            }
            .frame(width: 20, alignment: .leading)

            sortHead("名称", key: "name", width: leftPinnedWidth - 48)
        }
        .padding(.horizontal, 10)
        .frame(width: leftPinnedWidth, height: 36, alignment: .leading)
    }

    private var rightPinnedHeader: some View {
        colHead("操作", width: rightPinnedWidth - 20)
            .padding(.horizontal, 10)
            .frame(width: rightPinnedWidth, height: 36, alignment: .leading)
    }

    private var middleHeader: some View {
        HStack(spacing: 8) {
            sortHead("协议", key: "protocol", width: 64)
            colHead("地址", width: 146 + round(extraWidth * 0.28))
            colHead("认证", width: 76)
            colHead("地理位置", width: 108 + round(extraWidth * 0.32))
            sortHead("账号数", key: "account_count", width: 74)
            sortHead("延迟与质量", key: "latency_ms", width: 112 + round(extraWidth * 0.24))
            colHead("有效期", width: 88 + round(extraWidth * 0.16))
            colHead("状态", width: 52)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 10)
        .frame(height: 36)
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
        .buttonStyle(.linearPlain)
        .disabled(!busyLabel.isEmpty)
    }

    private func rowBackground(_ row: ProxyManagedRow) -> Color {
        if selectedIds.contains(row.id) { return DS.accentBg }
        if hoveredRowId == row.id { return DS.rowHover }
        return DS.surface
    }

    private func bindHover(_ rowId: Int, hovering: Bool) {
        if hovering {
            hoveredRowId = rowId
        } else if hoveredRowId == rowId {
            hoveredRowId = nil
        }
    }

    private func leftPinnedCell(_ row: ProxyManagedRow) -> some View {
        let isSelected = selectedIds.contains(row.id)
        return HStack(spacing: 8) {
            LinearCheckbox(isChecked: isSelected, accessibilityTitle: "选择 \(row.name)") {
                if isSelected { selectedIds.remove(row.id) } else { selectedIds.insert(row.id) }
            }
            .frame(width: 20, alignment: .leading)

            Text(row.name)
                .font(DS.fontBodySemibold)
                .foregroundStyle(DS.ink)
                .lineLimit(1)
                .help(row.name)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .padding(.horizontal, 10)
        .frame(width: leftPinnedWidth, height: rowHeight, alignment: .leading)
        .background(rowBackground(row))
        .onHover { bindHover(row.id, hovering: $0) }
    }

    private func middleRowCells(_ row: ProxyManagedRow) -> some View {
        let hasAuth = row.username_set == true || row.password_set == true
        let locParts = [row.country, row.region, row.city].compactMap { $0 }.filter { !$0.isEmpty }
        var seenLoc = Set<String>()
        let locationText = locParts.filter { seenLoc.insert($0).inserted }.joined(separator: " · ")

        return HStack(spacing: 8) {
            HStack(spacing: 0) {
                Text(verbatim: row.protocolName.uppercased())
                    .font(DS.fontBadge)
                    .foregroundStyle(DS.accent)
                    .lineLimit(1)
                    .fixedSize(horizontal: true, vertical: false)
                    .padding(.horizontal, 6)
                    .padding(.vertical, 3)
                    .background(DS.accentBg)
                    .clipShape(RoundedRectangle(cornerRadius: 5))
            }
            .frame(width: 64, alignment: .leading)

            HStack(spacing: 5) {
                Text(verbatim: row.formattedAddress)
                    .font(DS.fontPriorityMeta).monospacedDigit()
                    .foregroundStyle(DS.ink)
                    .lineLimit(1)
                Button {
                    NSPasteboard.general.clearContents()
                    NSPasteboard.general.setString(row.formattedAddress, forType: .string)
                    messageText = "已复制 \(row.name) 地址"
                } label: {
                    Image(systemName: "doc.on.doc")
                        .font(DS.iconFont(11))
                        .foregroundStyle(DS.muted)
                }
                .buttonStyle(.linearPlain)
                .disabled(!online || !busyLabel.isEmpty)
                .help("复制 \(row.name) 地址")
                .accessibilityLabel("复制 \(row.name) 地址")
            }
            .frame(width: 146 + round(extraWidth * 0.28), alignment: .leading)

            SwiftUI.Group {
                if hasAuth {
                    Button {
                        Task { await showCredentials(row) }
                    } label: {
                        HStack(spacing: 4) {
                            Text("••••••")
                                .font(DS.fontPriorityMeta).monospacedDigit()
                                .lineLimit(1)
                                .fixedSize(horizontal: true, vertical: false)
                            Image(systemName: "eye")
                                .font(DS.iconFont(10))
                        }
                        .foregroundStyle(DS.muted)
                        .padding(.horizontal, 6)
                        .padding(.vertical, 3)
                        .background(DS.hover)
                        .clipShape(RoundedRectangle(cornerRadius: 5))
                    }
                    .buttonStyle(.linearPlain)
                    .disabled(!online || !busyLabel.isEmpty)
                    .help("查看 \(row.name) 认证")
                    .accessibilityLabel("查看 \(row.name) 认证")
                } else {
                    Text("—")
                        .font(DS.fontCompact)
                        .foregroundStyle(DS.muted)
                }
            }
            .frame(width: 76, alignment: .leading)

            Text(locationText.isEmpty ? "—" : locationText)
                .font(DS.fontCompact)
                .foregroundStyle(locationText.isEmpty ? DS.muted : DS.ink)
                .lineLimit(1)
                .help(locationText)
                .frame(width: 108 + round(extraWidth * 0.32), alignment: .leading)

            Button {
                Task { await showAccounts(row) }
            } label: {
                Text("\(row.account_count ?? 0) 个账号")
                    .font(DS.fontResetTimeSemibold)
                    .monospacedDigit()
                    .foregroundStyle(DS.accent)
                    .padding(.horizontal, 7)
                    .padding(.vertical, 3)
                    .background(DS.accentBg)
                    .clipShape(Capsule())
            }
            .buttonStyle(.linearPlain)
            .disabled(!online || !busyLabel.isEmpty)
            .frame(width: 74, alignment: .leading)

            VStack(alignment: .leading, spacing: 3) {
                if row.latency_status == "failed" {
                    Text("连接失败")
                        .font(DS.fontResetTimeSemibold)
                        .foregroundStyle(DS.bad)
                        .padding(.horizontal, 7)
                        .padding(.vertical, 2)
                        .background(DS.badBg)
                        .clipShape(RoundedRectangle(cornerRadius: 4))
                } else if let ms = row.latency_ms {
                    Text("\(ms) ms")
                        .font(DS.fontResetTimeSemibold)
                        .monospacedDigit()
                        .foregroundStyle(ms <= 1000 ? DS.good : DS.warn)
                        .padding(.horizontal, 7)
                        .padding(.vertical, 2)
                        .background(ms <= 1000 ? DS.goodBg : DS.warnBg)
                        .clipShape(RoundedRectangle(cornerRadius: 4))
                } else {
                    Text("未测试")
                        .font(DS.fontCompact)
                        .foregroundStyle(DS.muted)
                }
                if let qGrade = row.quality_grade, !qGrade.isEmpty {
                    let scorePart = row.quality_score.map { "/\($0)" } ?? ""
                    let statusPart = Self.qualityLabels[row.quality_status ?? ""] ?? ""
                    let cachedQ = qualityById[row.id]
                    Button {
                        if let cachedQ {
                            presentModal(.quality(row, cachedQ))
                        }
                    } label: {
                        HStack(spacing: 4) {
                            Text("质量 \(qGrade)\(scorePart)")
                                .foregroundStyle(DS.muted)
                            if !statusPart.isEmpty {
                                Text(statusPart)
                                    .foregroundStyle(row.quality_status == "healthy" ? DS.good : DS.warn)
                            }
                        }
                        .font(DS.fontResetTime)
                        .monospacedDigit()
                    }
                    .buttonStyle(.linearPlain)
                    .disabled(cachedQ == nil)
                }
            }
            .frame(width: 112 + round(extraWidth * 0.24), alignment: .leading)

            Text(row.expires_at == nil ? "长期有效" : BeijingTime.fullTime(row.expires_at))
                .font(DS.fontCompact)
                .monospacedDigit()
                .foregroundStyle(row.status == "expired" ? DS.bad : DS.muted)
                .lineLimit(1)
                .frame(width: 88 + round(extraWidth * 0.16), alignment: .leading)

            Button {
                Task { await toggleRowStatus(row) }
            } label: {
                Text(Self.statusLabels[row.status] ?? row.status)
                    .font(DS.fontResetTimeSemibold)
                    .foregroundStyle(row.status == "active" ? DS.good : DS.muted)
                    .padding(.horizontal, 7)
                    .padding(.vertical, 3)
                    .background(row.status == "active" ? DS.goodBg : DS.hover)
                    .clipShape(RoundedRectangle(cornerRadius: 5))
            }
            .buttonStyle(.linearPlain)
            .disabled(!online || !busyLabel.isEmpty)
            .accessibilityLabel("\(row.status == "active" ? "停用" : "启用") \(row.name)")
            .frame(width: 52, alignment: .leading)

            Spacer(minLength: 0)
        }
        .padding(.horizontal, 10)
        .frame(height: rowHeight)
        .background(rowBackground(row))
        .onHover { bindHover(row.id, hovering: $0) }
    }

    private func rightPinnedCell(_ row: ProxyManagedRow) -> some View {
        HStack(spacing: 2) {
            proxyActionCellButton(icon: "checkmark", label: "测试", destructive: false) {
                Task { await runDiagnose(rows: [row], quality: false) }
            }
            proxyActionCellButton(icon: "checkmark.shield", label: "质量", destructive: false) {
                Task { await runDiagnose(rows: [row], quality: true) }
            }
            proxyActionCellButton(icon: "pencil", label: "编辑", destructive: false) {
                presentModal(.edit(row))
            }
            proxyActionCellButton(icon: "trash", label: "删除", destructive: true) {
                presentModal(.confirmDelete([row]))
            }
        }
        .padding(.horizontal, 8)
        .frame(width: rightPinnedWidth, height: rowHeight, alignment: .leading)
        .background(rowBackground(row))
        .onHover { bindHover(row.id, hovering: $0) }
    }

    private func proxyActionCellButton(
        icon: String,
        label: String,
        destructive: Bool,
        action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            VStack(spacing: 3) {
                Image(systemName: icon)
                    .font(DS.iconFont(12, weight: .medium))
                Text(label)
                    .font(DS.fontResetTime)
            }
            .foregroundStyle(destructive ? DS.bad : DS.muted)
            .frame(width: 33, height: 36)
            .contentShape(Rectangle())
        }
        .buttonStyle(.linearPlain)
        .disabled(!online || !busyLabel.isEmpty)
        .help(label)
    }

    private var paginationFooter: some View {
        HStack(spacing: 10) {
            Text("每页")
                .font(DS.fontCompact)
                .foregroundStyle(DS.muted)

            LinearSelect(
                label: "\(pageSize)",
                accessibilityTitle: "每页数量",
                height: 32,
                width: 68,
                compactFont: true,
                entries: [20, 50, 100].map { sz in
                    LinearMenuEntry(id: "\(sz)", title: "\(sz)", checked: pageSize == sz) {
                        pageSize = sz
                        page = 1
                    }
                }
            )

            Spacer()

            Text("第 \(page)/\(max(1, pageData.pages)) 页")
                .font(DS.fontCompact)
                .monospacedDigit()
                .foregroundStyle(DS.muted)

            Button("上一页") { page = max(1, page - 1) }
                .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                .disabled(page <= 1 || loading)

            Button("下一页") { page += 1 }
                .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                .disabled(page >= max(1, pageData.pages) || loading)
        }
    }
}
