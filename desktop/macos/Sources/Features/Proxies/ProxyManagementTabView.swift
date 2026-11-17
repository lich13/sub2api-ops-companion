import AppKit
import SwiftUI
import UniformTypeIdentifiers

/// Matches `ProxyManagement` in `desktop/src/ProxyManagement.tsx` (`features_ip_light.png`).
public struct ProxyManagementTabView: View {
    public var store: AppStore?
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
    @State private var fallbackModal: ModalKind? = nil
    @State private var qualityById: [Int: ProxyQualityResult] = [:]
    @State private var viewportWidth: CGFloat = 1100
    @State private var hoveredRowId: Int? = nil

    private static let statusLabels: [String: String] = [
        "active": "启用", "inactive": "停用", "expired": "已到期", "error": "异常",
    ]
    private static let qualityLabels: [String: String] = [
        "healthy": "优质", "warn": "警告", "challenge": "挑战", "failed": "失败", "fail": "失败",
    ]
    private static let qualityTargetLabels: [String: String] = [
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
                    DS.backdrop.ignoresSafeArea().onTapGesture { fallbackModal = nil }
                    modalView(modal, dismiss: { fallbackModal = nil })
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
        .buttonStyle(.plain)
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
                .buttonStyle(.plain)
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
                    .buttonStyle(.plain)
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
            .buttonStyle(.plain)
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
                    .buttonStyle(.plain)
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
            .buttonStyle(.plain)
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
        .buttonStyle(.plain)
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

    private func presentModal(_ kind: ModalKind) {
        if let store {
            store.presentWindowModal(onBackdrop: { store.dismissWindowModal() }) {
                modalView(kind, dismiss: { store.dismissWindowModal() })
            }
        } else {
            fallbackModal = kind
        }
    }

    private func dismissCurrentModal() {
        if let store {
            store.dismissWindowModal()
        } else {
            fallbackModal = nil
        }
    }

    @ViewBuilder
    private func modalView(_ kind: ModalKind, dismiss: @escaping () -> Void) -> some View {
        switch kind {
        case .edit(let row):
            ProxyEditorSheet(
                row: row,
                online: online,
                onSaved: {
                    dismiss()
                    messageText = "已保存"
                    Task { await loadPage() }
                },
                onClose: dismiss
            )
        case .batchAdd:
            ProxyBatchAddSheet(
                online: online,
                onSaved: { msg in
                    dismiss()
                    messageText = msg
                    Task { await loadPage() }
                },
                onClose: dismiss
            )
        case .confirmDelete(let rows):
            LinearModalCard(title: "确认删除 IP", width: 440, onClose: dismiss) {
                Text("确定删除所选的 \(rows.count) 个代理节点吗？关联账号将不再保留此代理。")
                    .font(DS.fontBody)
                    .foregroundStyle(DS.ink)
            } footer: {
                Spacer()
                Button("取消", action: dismiss)
                    .buttonStyle(LinearButtonStyle(variant: .secondary))
                Button("确认删除") {
                    Task { await deleteRows(rows) }
                }
                .buttonStyle(LinearButtonStyle(variant: .solidDanger))
            }
            .sub2opsDialogFocus(onEscape: dismiss)

        case .quality(let row, let q):
            LinearModalCard(
                title: "\(row.name) · 质量检测",
                subtitle: "\(q.grade) · \(q.score)/100",
                width: 600,
                onClose: dismiss
            ) {
                VStack(alignment: .leading, spacing: 6) {
                    if !q.summary.isEmpty {
                        DiagnosticTextView(text: q.summary)
                    }
                    let metaParts = [
                        q.exit_ip,
                        q.country,
                        q.base_latency_ms.map { "\($0) ms" } ?? "",
                    ].filter { !$0.isEmpty }
                    HStack(spacing: 8) {
                        if !metaParts.isEmpty {
                            Text(metaParts.joined(separator: " · "))
                                .font(DS.fontResetTime)
                                .monospacedDigit()
                                .foregroundStyle(DS.muted)
                        }
                        Spacer()
                        if let ts = q.checked_at, ts > 0 {
                            let iso = ISO8601DateFormatter().string(from: Date(timeIntervalSince1970: ts))
                            Text(BeijingTime.fullTime(iso))
                                .font(DS.fontResetTime)
                                .monospacedDigit()
                                .foregroundStyle(DS.muted)
                        }
                    }
                }

                LinearBoundedScrollView(maxHeight: 280) {
                    VStack(spacing: 0) {
                        HStack(spacing: 8) {
                            Text("目标").frame(width: 112, alignment: .leading)
                            Text("状态").frame(width: 54, alignment: .leading)
                            Text("HTTP").frame(width: 44, alignment: .trailing)
                            Text("延迟").frame(width: 64, alignment: .trailing)
                            Text("详情").frame(maxWidth: .infinity, alignment: .leading)
                        }
                        .font(DS.fontTableHeader)
                        .foregroundStyle(DS.muted)
                        .padding(.horizontal, 10)
                        .frame(height: 28)
                        .background(DS.surfaceSubtle)

                        Rectangle().fill(DS.line).frame(height: 1)

                        ForEach(Array(q.items.enumerated()), id: \.offset) { idx, item in
                            HStack(alignment: .top, spacing: 8) {
                                Text(verbatim: item.target)
                                    .font(DS.fontPriorityMeta).monospacedDigit()
                                    .foregroundStyle(DS.ink)
                                    .frame(width: 112, alignment: .leading)

                                Text(Self.qualityTargetLabels[item.status] ?? item.status)
                                    .font(DS.fontResetTimeSemibold)
                                    .foregroundStyle(item.status == "pass" ? DS.good : DS.warn)
                                    .padding(.horizontal, 5)
                                    .padding(.vertical, 1)
                                    .background(item.status == "pass" ? DS.goodBg : DS.warnBg)
                                    .clipShape(RoundedRectangle(cornerRadius: 4))
                                    .frame(width: 54, alignment: .leading)

                                Text(item.http_status.map(String.init) ?? "—")
                                    .font(DS.fontPriorityMeta).monospacedDigit()
                                    .monospacedDigit()
                                    .foregroundStyle(DS.muted)
                                    .frame(width: 44, alignment: .trailing)

                                Text(item.latency_ms.map { "\($0) ms" } ?? "—")
                                    .font(DS.fontResetTime)
                                    .monospacedDigit()
                                    .foregroundStyle(DS.muted)
                                    .frame(width: 64, alignment: .trailing)

                                SwiftUI.Group {
                                    if item.message.isEmpty {
                                        Text("—")
                                            .font(DS.fontCompact)
                                            .foregroundStyle(DS.muted)
                                    } else {
                                        DiagnosticTextView(text: item.message)
                                    }
                                }
                                .frame(maxWidth: .infinity, alignment: .leading)
                            }
                            .padding(.horizontal, 10)
                            .padding(.vertical, 7)

                            if idx < q.items.count - 1 {
                                Rectangle().fill(DS.line).frame(height: 1)
                            }
                        }
                    }
                    .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(DS.line, lineWidth: 1))
                    .clipShape(RoundedRectangle(cornerRadius: 6))
                }
            } footer: {
                Spacer()
                Button("关闭", action: dismiss).buttonStyle(LinearButtonStyle(variant: .secondary))
            }
            .sub2opsDialogFocus(onEscape: dismiss)

        case .credentials(let row, let cred):
            LinearModalCard(title: "认证凭据", subtitle: row.name, width: 480, onClose: dismiss) {
                Text(verbatim: row.fullURL(credentials: cred))
                    .font(DS.fontMono12)
                    .foregroundStyle(DS.ink)
                    .textSelection(.enabled)
                    .padding(10)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(DS.surfaceSubtle)
                    .clipShape(RoundedRectangle(cornerRadius: 6))
            } footer: {
                Button("复制完整地址") {
                    NSPasteboard.general.clearContents()
                    NSPasteboard.general.setString(row.fullURL(credentials: cred), forType: .string)
                    messageText = "已复制认证地址"
                    dismiss()
                }
                .buttonStyle(LinearButtonStyle(variant: .secondary))
                Spacer()
                Button("关闭", action: dismiss).buttonStyle(LinearButtonStyle(variant: .primary))
            }
            .sub2opsDialogFocus(onEscape: dismiss)

        case .accounts(let row, let list):
            LinearModalCard(title: "关联账号", subtitle: "\(row.name) · \(list.count) 个账号", width: 460, onClose: dismiss) {
                if list.isEmpty {
                    Text("当前无关联账号").font(DS.fontCompact).foregroundStyle(DS.muted)
                } else {
                    LinearBoundedScrollView(maxHeight: 220) {
                        VStack(alignment: .leading, spacing: 6) {
                            ForEach(list) { acc in
                                HStack {
                                    Text(acc.name).font(DS.fontCompactSemibold).foregroundStyle(DS.ink)
                                    Text("#\(acc.id)").font(DS.fontPriorityMeta).monospacedDigit().foregroundStyle(DS.muted)
                                    Spacer()
                                    Text("\(acc.platform) · \(acc.type)").font(DS.fontResetTime).foregroundStyle(DS.muted)
                                }
                            }
                        }
                    }
                }
            } footer: {
                Spacer()
                Button("关闭", action: dismiss).buttonStyle(LinearButtonStyle(variant: .secondary))
            }
            .sub2opsDialogFocus(onEscape: dismiss)

        case .results(let title, let rows):
            LinearModalCard(title: title, width: 500, onClose: dismiss) {
                LinearBoundedScrollView(maxHeight: 240) {
                    VStack(alignment: .leading, spacing: 8) {
                        ForEach(Array(rows.enumerated()), id: \.offset) { _, r in
                            VStack(alignment: .leading, spacing: 3) {
                                Text(r.name).font(DS.fontCompactSemibold).foregroundStyle(DS.ink)
                                if r.ok {
                                    Text(r.message)
                                        .font(DS.fontResetTime)
                                        .foregroundStyle(DS.good)
                                } else {
                                    DiagnosticTextView(text: r.message)
                                }
                            }
                            .frame(maxWidth: .infinity, alignment: .leading)
                        }
                    }
                }
            } footer: {
                Spacer()
                Button("关闭", action: dismiss).buttonStyle(LinearButtonStyle(variant: .secondary))
            }
            .sub2opsDialogFocus(onEscape: dismiss)
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

    private func toggleRowStatus(_ row: ProxyManagedRow) async {
        guard online, busyLabel.isEmpty else { return }
        busyLabel = "status"
        defer { busyLabel = "" }
        do {
            let nextStatus = row.status == "active" ? "inactive" : "active"
            let _: JSONValue = try await RustBridge.shared.api(
                "PUT",
                "/proxy-management/\(row.id)",
                body: .object(["status": .string(nextStatus)])
            )
            messageText = "状态已更新"
            await loadPage()
            store?.refresh()
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
                    qualityById[row.id] = q
                    out.append((row.name, true, "\(q.grade) · \(q.score)/100 \(q.summary)"))
                    if rows.count == 1 {
                        presentModal(.quality(row, q))
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
            presentModal(.results(quality ? "质量检测结果" : "连接测试结果", out))
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
                dismissCurrentModal()
                messageText = "已删除"
            } else {
                struct SkippedItem: Decodable, Sendable { var id: Int; var reason: String }
                struct BatchDel: Decodable, Sendable { var deleted_ids: [Int]; var skipped: [SkippedItem]? }
                let res: BatchDel = try await RustBridge.shared.api(
                    "POST",
                    "/proxy-management/batch-delete",
                    body: .object(["ids": .array(rows.map { .int(Int64($0.id)) })])
                )
                let skipped = res.skipped ?? []
                selectedIds = Set(skipped.map(\.id))
                let outcomeRows: [(name: String, ok: Bool, message: String)] = rows.map { p in
                    let ok = res.deleted_ids.contains(p.id)
                    let msg = ok ? "已删除" : (skipped.first(where: { $0.id == p.id })?.reason ?? "删除未确认")
                    return (p.name, ok, msg)
                }
                presentModal(.results("批量删除结果", outcomeRows))
            }
            await loadPage()
            store?.refresh()
        } catch {
            errorText = error.localizedDescription
        }
    }

    private func showCredentials(_ row: ProxyManagedRow) async {
        do {
            let cred: ProxyCredentialsDTO = try await RustBridge.shared.api("GET", "/proxy-management/\(row.id)/credentials")
            presentModal(.credentials(row, cred))
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
            presentModal(.accounts(row, list))
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
            if !r.errors.isEmpty {
                presentModal(.results(
                    "导入结果",
                    r.errors.map { ($0.name.isEmpty ? "代理" : $0.name, false, $0.message) }
                ))
            }
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
