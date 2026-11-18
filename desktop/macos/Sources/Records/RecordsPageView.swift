import AppKit
import SwiftUI

public struct RecordsPageView: View {
    @Bindable public var store: AppStore

    @State private var feed = RecordFeedStore()
    @State private var range: RecordDateRange = BeijingDateLogic.presetRange(.today)
    @State private var platformFilter: String = ""
    @State private var accountFilter: RecordOption? = nil
    @State private var userFilter: RecordOption? = nil
    @State private var keyFilter: RecordOption? = nil
    @State private var modelInput: String = ""
    @State private var appliedModel: String = ""
    @State private var mismatchOnly: Bool = false
    @State private var hScrollOffset: CGFloat = 0
    @State private var rightPaneWidth: CGFloat = 980
    @State private var hoveredRowId: Int? = nil

    public init(store: AppStore) {
        self.store = store
    }

    private var active: Bool {
        store.state.online && (store.state.foreground != false)
    }

    private var shownColumns: [RecordColumnSpec] {
        RecordColumnSpec.normalize(store.state.preferences.record_columns)
    }

    private var accountOptions: [RecordOption] {
        store.accounts
            .filter { platformFilter.isEmpty || $0.platform == platformFilter }
            .map { RecordOption(id: $0.id, name: $0.name, status: $0.status, platform: $0.platform, deleted: false) }
    }

    private var queryString: String {
        var comps = URLComponents()
        var items: [URLQueryItem] = [
            URLQueryItem(name: "limit", value: "50"),
            URLQueryItem(name: "start_date", value: range.start),
            URLQueryItem(name: "end_date", value: range.end),
        ]
        if !platformFilter.isEmpty { items.append(URLQueryItem(name: "platform", value: platformFilter)) }
        if let acc = accountFilter { items.append(URLQueryItem(name: "account_id", value: String(acc.id))) }
        if let usr = userFilter { items.append(URLQueryItem(name: "user_id", value: String(usr.id))) }
        if let key = keyFilter { items.append(URLQueryItem(name: "api_key_id", value: String(key.id))) }
        if !appliedModel.isEmpty { items.append(URLQueryItem(name: "model", value: appliedModel)) }
        if mismatchOnly { items.append(URLQueryItem(name: "mismatch_only", value: "true")) }
        comps.queryItems = items
        return comps.percentEncodedQuery ?? ""
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("记录")
                .font(DS.fontPageTitle)
                .foregroundStyle(DS.ink)

            filterToolbar
            summaryAndActionsBar

            if !feed.error.isEmpty {
                HStack(spacing: 8) {
                    Text(feed.error)
                        .font(DS.fontCompact)
                        .foregroundStyle(DS.bad)
                    Button("重试") {
                        Task { await feed.refresh() }
                    }
                    .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                    .disabled(!active || feed.loading)
                }
            }

            if !store.state.online {
                Text("连接已断开，显示已读取记录")
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.warn)
            }

            recordsTableSurface
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .task(id: modelInput) {
            let trimmed = modelInput.trimmingCharacters(in: .whitespacesAndNewlines)
            guard trimmed != appliedModel else { return }
            try? await Task.sleep(for: .milliseconds(300))
            guard !Task.isCancelled else { return }
            appliedModel = trimmed
        }
        .task(id: "\(store.connectionKey):\(queryString):\(active):\(store.selectedRecordId != nil)") {
            feed.bind(query: queryString, active: active, holding: store.selectedRecordId != nil)
        }
        .onDisappear {
            feed.stop()
        }
    }

    private var filterToolbar: some View {
        let platforms = PlatformMeta.sortPlatforms(PlatformMeta.preferredOrder + store.accounts.map(\.platform))
        return HStack(spacing: 6) {
            RecordDatePickerView(value: range, disabled: !active || feed.loading) { next in
                if next.start == range.start && next.end == range.end {
                    Task { await feed.refresh() }
                }
                range = next
            }
            .fixedSize(horizontal: true, vertical: false)

            LinearSelect(
                label: platformFilter.isEmpty ? "全部平台" : PlatformMeta.priorityName(platformFilter),
                accessibilityTitle: "平台筛选",
                height: DS.controlHeightDense,
                minWidth: 88,
                compactFont: true,
                entries: [
                    LinearMenuEntry(id: "__all__", title: "全部平台", checked: platformFilter.isEmpty) {
                        platformFilter = ""
                    },
                ] + platforms.map { pid in
                    LinearMenuEntry(
                        id: pid,
                        title: PlatformMeta.priorityName(pid),
                        checked: platformFilter == pid
                    ) {
                        platformFilter = pid
                        if let selAcc = accountFilter, selAcc.platform != pid {
                            accountFilter = nil
                        }
                    }
                }
            )
            .fixedSize(horizontal: true, vertical: false)

            RecordOptionPickerView(kind: .users, active: active, selected: userFilter) { next in
                userFilter = next
                keyFilter = nil
            }

            RecordOptionPickerView(kind: .accounts, active: active, selected: accountFilter, localOptions: accountOptions) { next in
                accountFilter = next
            }

            RecordOptionPickerView(kind: .api_keys, active: active, selected: keyFilter, userId: userFilter?.id) { next in
                keyFilter = next
            }

            LinearSearchField(
                text: $modelInput,
                prompt: "搜索模型",
                height: DS.controlHeightDense,
                compactFont: true,
                onSubmit: {
                    appliedModel = modelInput.trimmingCharacters(in: .whitespacesAndNewlines)
                }
            )
            .frame(minWidth: 96, maxWidth: .infinity)

            LinearCheckbox(
                isChecked: mismatchOnly,
                label: "返回差异"
            ) {
                mismatchOnly.toggle()
            }
            .fixedSize(horizontal: true, vertical: false)
        }
    }

    private var summaryAndActionsBar: some View {
        let currentKeys = shownColumns.map(\.rawValue)
        return HStack(spacing: 12) {
            HStack(spacing: 10) {
                Text("总消费")
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.muted)
                if feed.loading && feed.items.isEmpty {
                    RoundedRectangle(cornerRadius: 2)
                        .fill(DS.good)
                        .frame(width: 22, height: 3)
                } else {
                    Text(RecordFormatting.money(feed.totalCost, digits: 6))
                        .font(DS.fontSectionTitle)
                        .monospacedDigit()
                        .foregroundStyle(DS.good)
                }
            }

            Spacer()

            if !feed.items.isEmpty {
                Text("\(feed.items.count) 条")
                    .font(DS.fontCompact)
                    .monospacedDigit()
                    .foregroundStyle(DS.muted)
            }
            if !feed.observedAt.isEmpty {
                Text("更新于 \(BeijingTime.fullTime(feed.observedAt))")
                    .font(DS.fontPriorityMeta)
                    .monospacedDigit()
                    .foregroundStyle(DS.muted)
            }

            if feed.newCount > 0 {
                Button("\(feed.newCount) 条新记录") {
                    Task { await feed.refresh() }
                }
                .buttonStyle(LinearButtonStyle(variant: .selectedGhost, compact: true))
                .disabled(!active || feed.loading)
            }

            LinearIconButton(
                systemName: "arrow.clockwise",
                label: "刷新记录",
                size: 13,
                boxSize: DS.controlHeightCompact,
                bordered: true,
                disabled: !active || feed.loading
            ) {
                Task { await feed.refresh() }
            }

            LinearSelect(
                label: "列",
                accessibilityTitle: "显示列",
                leadingIcon: "line.3.horizontal.decrease",
                height: DS.controlHeightCompact,
                compactFont: true,
                entries: RecordColumnSpec.allCases.map { col in
                    let isOn = currentKeys.contains(col.rawValue)
                    return LinearMenuEntry(
                        id: col.rawValue,
                        title: col.label,
                        checked: isOn
                    ) {
                        let next = isOn
                            ? currentKeys.filter { $0 != col.rawValue }
                            : (currentKeys + [col.rawValue])
                        Task { await store.updatePreferences(recordColumns: next) }
                    }
                }
            )
        }
    }

    private func rowHeight(for row: UsageRecord, showModel: Bool, showLatency: Bool) -> CGFloat {
        let base: CGFloat = showLatency ? 42 : 38
        guard showModel else { return base }
        let routeCount = max(1, RecordFormatting.modelRoute(row).count) + (row.upstream_model_mismatch ? 1 : 0)
        return max(base, CGFloat(routeCount) * 14 + 8)
    }

    private var recordsTableSurface: some View {
        let cols = shownColumns
        let hasApiKeyCol = cols.contains(.api_key)
        let hasModelCol = cols.contains(.model)
        let hasLatencyCol = cols.contains(.latency)
        let trailingCols = cols.filter { $0 != .api_key }
        let userColWidth = RecordColumnSpec.userColumnWidth
        let userCellPad: CGFloat = 6
        let timeColWidth = RecordColumnSpec.timeColumnWidth
        let colSpacing = RecordColumnSpec.columnSpacing
        let hCellPad = RecordColumnSpec.cellPadding
        let headerHeight: CGFloat = 26
        let effectiveDataWidth = max(RecordColumnSpec.dataWidth(columns: cols), rightPaneWidth)

        return VStack(spacing: 0) {
            // Pinned top header bar (zero-minWidth clipped viewport synchronized horizontally with right ScrollView)
            HStack(spacing: 0) {
                Text("用户")
                    .font(DS.fontTableHeader)
                    .foregroundStyle(DS.muted)
                    .padding(.horizontal, userCellPad)
                    .frame(width: userColWidth, height: headerHeight, alignment: .leading)

                Rectangle().fill(DS.line).frame(width: 1, height: headerHeight)

                DS.clear
                    .frame(maxWidth: .infinity, minHeight: headerHeight, maxHeight: headerHeight)
                    .background(LinearWidthObserver { rightPaneWidth = $0 })
                    .overlay(alignment: .leading) {
                        HStack(spacing: colSpacing) {
                            if hasApiKeyCol {
                                headerCell("API 密钥", width: RecordColumnSpec.api_key.width)
                            }
                            headerCell("时间", width: timeColWidth)
                            ForEach(trailingCols, id: \.rawValue) { col in
                                headerCell(
                                    col.label,
                                    width: col.width,
                                    trailing: col == .cost || col == .tokens || col == .latency
                                )
                            }
                            Spacer(minLength: 0)
                        }
                        .padding(.horizontal, hCellPad)
                        .frame(width: effectiveDataWidth, height: headerHeight, alignment: .leading)
                        .offset(x: -hScrollOffset)
                    }
                    .clipped()
            }
            .frame(height: headerHeight)
            .background(DS.surfaceSubtle)

            Rectangle().fill(DS.line).frame(height: 1)

            if feed.items.isEmpty {
                Text(feed.loading ? "正在读取记录…" : (feed.error.isEmpty ? "暂无记录" : "记录暂不可用"))
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.muted)
                    .frame(maxWidth: .infinity, alignment: .center)
                    .padding(.vertical, 36)
                Spacer(minLength: 0)
            } else {
                // Vertically scrollable table body keeping left User column sticky & right columns horizontally scrollable
                ScrollView(.vertical) {
                    VStack(spacing: 0) {
                        HStack(alignment: .top, spacing: 0) {
                            VStack(spacing: 0) {
                                ForEach(Array(feed.items.enumerated()), id: \.element.id) { idx, row in
                                    let rh = rowHeight(for: row, showModel: hasModelCol, showLatency: hasLatencyCol)
                                    Button {
                                        openDetail(id: row.id)
                                    } label: {
                                        VStack(alignment: .leading, spacing: 2) {
                                            Text(row.user_name ?? row.user_email ?? "#\(row.user_id)")
                                                .font(DS.fontCompactSemibold)
                                                .foregroundStyle(DS.ink)
                                                .lineLimit(1)
                                            Text("#\(row.user_id)")
                                                .font(DS.fontResetTime)
                                                .monospacedDigit()
                                                .foregroundStyle(DS.muted)
                                        }
                                        .padding(.horizontal, userCellPad)
                                        .frame(width: userColWidth, height: rh, alignment: .leading)
                                        .background(hoveredRowId == row.id ? DS.rowHover : DS.surface)
                                        .contentShape(Rectangle())
                                    }
                                    .buttonStyle(.linearPlain)
                                    .onHover { hov in
                                        if hov { hoveredRowId = row.id } else if hoveredRowId == row.id { hoveredRowId = nil }
                                    }

                                    if idx < feed.items.count - 1 {
                                        Rectangle().fill(DS.line).frame(height: 1)
                                    }
                                }
                            }
                            .frame(width: userColWidth)

                            Rectangle().fill(DS.line).frame(width: 1)

                            ScrollView(.horizontal) {
                                VStack(spacing: 0) {
                                    GeometryReader { geo in
                                        DS.clear.preference(
                                            key: RecordsHScrollOffsetKey.self,
                                            value: max(0, -geo.frame(in: .named("recordsHScroll")).minX)
                                        )
                                    }
                                    .frame(height: 0)

                                    ForEach(Array(feed.items.enumerated()), id: \.element.id) { idx, row in
                                        let rh = rowHeight(for: row, showModel: hasModelCol, showLatency: hasLatencyCol)
                                        HStack(spacing: colSpacing) {
                                            if hasApiKeyCol {
                                                cellView(.api_key, row: row)
                                            }
                                            Text(BeijingTime.fullTime(row.created_at))
                                                .font(DS.fontPriorityMeta)
                                                .monospacedDigit()
                                                .lineLimit(1)
                                                .foregroundStyle(DS.muted)
                                                .frame(width: timeColWidth, alignment: .leading)
                                            ForEach(trailingCols, id: \.rawValue) { col in
                                                cellView(col, row: row)
                                            }
                                            Spacer(minLength: 0)
                                        }
                                        .padding(.horizontal, hCellPad)
                                        .frame(width: effectiveDataWidth, height: rh, alignment: .leading)
                                        .background(hoveredRowId == row.id ? DS.rowHover : DS.surface)
                                        .contentShape(Rectangle())
                                        .onHover { hov in
                                            if hov { hoveredRowId = row.id } else if hoveredRowId == row.id { hoveredRowId = nil }
                                        }
                                        .onTapGesture {
                                            openDetail(id: row.id)
                                        }

                                        if idx < feed.items.count - 1 {
                                            Rectangle().fill(DS.line).frame(height: 1)
                                        }
                                    }
                                }
                            }
                            .coordinateSpace(name: "recordsHScroll")
                            .onPreferenceChange(RecordsHScrollOffsetKey.self) { next in
                                if abs(next - hScrollOffset) >= 0.5 {
                                    hScrollOffset = next
                                }
                            }
                        }

                        if !feed.pageError.isEmpty || feed.cursor != nil || !feed.items.isEmpty {
                            Rectangle().fill(DS.line).frame(height: 1)
                            HStack(spacing: 8) {
                                Spacer()
                                if !feed.pageError.isEmpty {
                                    Text(feed.pageError)
                                        .font(DS.fontCompact)
                                        .foregroundStyle(DS.bad)
                                    Button("重试加载") {
                                        Task { await feed.loadMore(retry: true) }
                                    }
                                    .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                                } else if feed.cursor != nil {
                                    Button(feed.loading ? "正在读取…" : "加载下一页") {
                                        Task { await feed.loadMore() }
                                    }
                                    .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                                    .disabled(!active || feed.loading)
                                } else if !feed.loading {
                                    Text("已全部加载")
                                        .font(DS.fontResetTime)
                                        .foregroundStyle(DS.muted)
                                }
                                Spacer()
                            }
                            .padding(.horizontal, 12)
                            .padding(.vertical, 9)
                        }
                    }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            }
        }
        .background(DS.surface)
        .overlay(RoundedRectangle(cornerRadius: DS.cardRadius).strokeBorder(DS.line, lineWidth: 1))
        .clipShape(RoundedRectangle(cornerRadius: DS.cardRadius))
    }

    private func headerCell(_ title: String, width: CGFloat, trailing: Bool = false) -> some View {
        Text(title)
            .font(DS.fontTableHeader)
            .foregroundStyle(DS.muted)
            .frame(width: width, alignment: trailing ? .trailing : .leading)
    }

    @ViewBuilder
    private func cellView(_ col: RecordColumnSpec, row: UsageRecord) -> some View {
        switch col {
        case .api_key:
            VStack(alignment: .leading, spacing: 2) {
                Text(row.api_key_name ?? "API 密钥")
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.ink)
                    .lineLimit(1)
                Text("#\(row.api_key_id)")
                    .font(DS.fontPriorityMeta).monospacedDigit()
                    .foregroundStyle(DS.muted)
            }
            .frame(width: col.width, alignment: .leading)

        case .account:
            Text(row.account_name ?? "#\(row.account_id)")
                .font(DS.fontCompact)
                .foregroundStyle(DS.ink)
                .lineLimit(1)
                .frame(width: col.width, alignment: .leading)

        case .model:
            let steps = RecordFormatting.modelRoute(row)
            VStack(alignment: .leading, spacing: 2) {
                if steps.isEmpty {
                    Text("—")
                        .font(DS.fontCompact)
                        .foregroundStyle(DS.muted)
                } else {
                    ForEach(Array(steps.enumerated()), id: \.offset) { idx, step in
                        let stageLabel = step.labels.contains("返回")
                            ? "返回"
                            : (step.labels.contains("转发") ? "转发" : "映射")
                        HStack(spacing: 3) {
                            if idx > 0 {
                                Text("↳")
                                    .font(DS.fontResetTime)
                                    .foregroundStyle(DS.muted)
                                Text(stageLabel)
                                    .font(DS.fontResetTime)
                                    .foregroundStyle(DS.muted)
                            }
                            Text(verbatim: step.model)
                                .font(DS.fontPriorityMeta).monospacedDigit()
                                .foregroundStyle(idx == 0 ? DS.ink : DS.muted)
                                .lineLimit(1)
                        }
                        .help("\(step.labels.joined(separator: " · "))：\(step.model)")
                    }
                }
                if row.upstream_model_mismatch {
                    HStack(spacing: 3) {
                        Image(systemName: "exclamationmark.circle")
                            .font(DS.iconFont(9, weight: .semibold))
                        Text("返回差异")
                            .font(DS.fontResetTimeMedium)
                    }
                    .foregroundStyle(DS.warn)
                }
            }
            .frame(width: col.width, alignment: .leading)

        case .type:
            let rt = RecordFormatting.requestTypeKey(row)
            let tone = requestTypeColors(rt.key)
            SwiftUI.Group {
                if row.native_compaction_v2 == true {
                    ViewThatFits(in: .horizontal) {
                        HStack(spacing: 3) {
                            typeBadge(rt.label, fg: tone.fg, bg: tone.bg)
                            typeBadge("压缩", fg: DS.recordCompactFg, bg: DS.recordCompactBg)
                        }
                        VStack(alignment: .leading, spacing: 2) {
                            typeBadge(rt.label, fg: tone.fg, bg: tone.bg)
                            typeBadge("压缩", fg: DS.recordCompactFg, bg: DS.recordCompactBg)
                        }
                    }
                } else {
                    typeBadge(rt.label, fg: tone.fg, bg: tone.bg)
                }
            }
            .frame(width: col.width, alignment: .leading)

        case .reasoning:
            VStack(alignment: .leading, spacing: 2) {
                Text(row.requested_reasoning_effort ?? row.reasoning_effort ?? "—")
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.ink)
                    .lineLimit(1)
                if let req = row.requested_reasoning_effort, let act = row.reasoning_effort, req != act {
                    Text("↳ \(act)")
                        .font(DS.fontResetTime)
                        .foregroundStyle(DS.muted)
                        .lineLimit(1)
                }
            }
            .frame(width: col.width, alignment: .leading)

        case .tokens:
            Grid(alignment: .trailing, horizontalSpacing: 6, verticalSpacing: 2) {
                GridRow {
                    HStack(spacing: 2) {
                        Image(systemName: "arrow.down")
                            .font(DS.iconFont(9, weight: .semibold))
                            .foregroundStyle(DS.good)
                        Text(RecordFormatting.tokenCount(row.input_tokens))
                            .foregroundStyle(DS.ink)
                    }
                    HStack(spacing: 2) {
                        Image(systemName: "arrow.up")
                            .font(DS.iconFont(9, weight: .semibold))
                            .foregroundStyle(DS.windowPurple)
                        Text(RecordFormatting.tokenCount(row.output_tokens))
                            .foregroundStyle(DS.ink)
                    }
                }
                GridRow {
                    HStack(spacing: 2) {
                        Image(systemName: "archivebox")
                            .font(DS.iconFont(9))
                        Text(RecordFormatting.cacheTokenCount(row.cache_read_tokens))
                    }
                    .foregroundStyle(DS.tokenCyan)
                    HStack(spacing: 2) {
                        Image(systemName: "square.and.pencil")
                            .font(DS.iconFont(9))
                        Text(RecordFormatting.cacheTokenCount(row.cache_creation_tokens))
                    }
                    .foregroundStyle(DS.warn)
                }
            }
            .font(DS.fontPriorityMeta)
            .monospacedDigit()
            .lineLimit(1)
            .minimumScaleFactor(0.85)
            .frame(width: col.width, alignment: .trailing)

        case .cost:
            CostDetailsPopover(row: row)
                .frame(width: col.width, alignment: .trailing)

        case .latency:
            let firstTone = latencyColor(RecordFormatting.latencyTone(row.first_token_ms, firstToken: true))
            let totalTone = latencyColor(RecordFormatting.latencyTone(row.duration_ms, firstToken: false))
            let tpsRaw = RecordFormatting.outputRate(row)
            HStack(spacing: 5) {
                VStack(spacing: 1) {
                    RoundedRectangle(cornerRadius: 1.5).fill(firstTone).frame(width: 3, height: 15)
                    RoundedRectangle(cornerRadius: 1.5).fill(totalTone).frame(width: 3, height: 18)
                }
                VStack(spacing: 1) {
                    HStack(spacing: 0) {
                        Text("首字")
                            .font(DS.fontResetTime)
                            .lineLimit(1)
                            .foregroundStyle(DS.muted)
                        Spacer(minLength: 3)
                        Text(RecordFormatting.latency(row.first_token_ms))
                            .font(DS.fontPriorityMeta)
                            .lineLimit(1)
                            .foregroundStyle(firstTone)
                    }
                    HStack(spacing: 0) {
                        Text("总耗时")
                            .font(DS.fontResetTime)
                            .lineLimit(1)
                            .foregroundStyle(DS.muted)
                        Spacer(minLength: 3)
                        Text(RecordFormatting.latency(row.duration_ms))
                            .font(DS.fontPriorityMeta)
                            .lineLimit(1)
                            .foregroundStyle(totalTone)
                    }
                    HStack(spacing: 0) {
                        Text("输出 TPS")
                            .font(DS.fontResetTime)
                            .lineLimit(1)
                            .foregroundStyle(DS.muted)
                        Spacer(minLength: 3)
                        Text(tpsRaw)
                            .font(DS.fontResetTime)
                            .lineLimit(1)
                            .foregroundStyle(DS.ink)
                    }
                }
                .monospacedDigit()
            }
            .frame(width: col.width, alignment: .trailing)

        case .user_agent:
            Text(verbatim: row.user_agent ?? "—")
                .font(DS.fontCompact)
                .foregroundStyle(DS.muted)
                .lineLimit(1)
                .frame(width: col.width, alignment: .leading)

        case .ip:
            Text(verbatim: row.ip_address ?? "—")
                .font(DS.fontPriorityMeta).monospacedDigit()
                .foregroundStyle(DS.muted)
                .lineLimit(1)
                .frame(width: col.width, alignment: .leading)

        case .endpoint:
            VStack(alignment: .leading, spacing: 2) {
                Text(verbatim: row.inbound_endpoint ?? "—")
                    .font(DS.fontPriorityMeta).monospacedDigit()
                    .foregroundStyle(DS.ink)
                    .lineLimit(1)
                Text(verbatim: row.upstream_endpoint ?? "—")
                    .font(DS.fontResetTime)
                    .foregroundStyle(DS.muted)
                    .lineLimit(1)
            }
            .frame(width: col.width, alignment: .leading)

        case .group:
            Text(row.group_name ?? (row.group_id.map { "#\($0)" } ?? "—"))
                .font(DS.fontCompact)
                .foregroundStyle(DS.ink)
                .lineLimit(1)
                .frame(width: col.width, alignment: .leading)

        case .billing:
            let map = ["token": "Token", "image": "图片", "per_request": "按次"]
            Text(map[row.billing_mode ?? ""] ?? row.billing_mode ?? "—")
                .font(DS.fontCompact)
                .lineLimit(1)
                .foregroundStyle(DS.muted)
                .frame(width: col.width, alignment: .leading)

        case .request_id:
            Text(verbatim: row.request_id ?? "—")
                .font(DS.fontMono11)
                .foregroundStyle(DS.muted)
                .lineLimit(1)
                .frame(width: col.width, alignment: .leading)

        case .upstream_id:
            Text(verbatim: row.upstream_request_id ?? "—")
                .font(DS.fontMono11)
                .foregroundStyle(DS.muted)
                .lineLimit(1)
                .frame(width: col.width, alignment: .leading)
        }
    }

    private func typeBadge(_ text: String, fg: Color, bg: Color) -> some View {
        Text(text)
            .font(DS.fontResetTimeMedium)
            .lineLimit(1)
            .fixedSize(horizontal: true, vertical: false)
            .foregroundStyle(fg)
            .padding(.horizontal, 4)
            .padding(.vertical, 1)
            .background(bg)
            .clipShape(RoundedRectangle(cornerRadius: 4))
    }

    private func requestTypeColors(_ key: String) -> (fg: Color, bg: Color) {
        switch key {
        case "stream": return (DS.recordStreamFg, DS.recordStreamBg)
        case "ws_v2": return (DS.recordWsFg, DS.recordWsBg)
        case "cyber": return (DS.bad, DS.badBg)
        case "live": return (DS.windowEmerald, DS.windowEmeraldBg)
        case "unknown": return (DS.warn, DS.warnBg)
        default: return (DS.recordSyncFg, DS.recordSyncBg)
        }
    }

    private func latencyColor(_ level: RecordFormatting.LatencyLevel) -> Color {
        switch level {
        case .good: return DS.good
        case .warn: return DS.warn
        case .slow: return DS.latencySlow
        case .critical: return DS.bad
        case .unknown: return DS.muted
        }
    }

    private func openDetail(id: Int) {
        let seed = feed.items.first(where: { $0.id == id })
        store.openRecordDetail(id: id, seed: seed)
    }
}

private struct RecordsHScrollOffsetKey: PreferenceKey {
    static let defaultValue: CGFloat = 0
    static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) {
        value = nextValue()
    }
}

public struct RecordDetailDrawerView: View {
    public let row: UsageRecord?
    public let loading: Bool
    public let error: String
    public let onClose: () -> Void

    public init(row: UsageRecord?, loading: Bool, error: String, onClose: @escaping () -> Void) {
        self.row = row
        self.loading = loading
        self.error = error
        self.onClose = onClose
    }

    public var body: some View {
        ZStack(alignment: .trailing) {
            DS.backdrop.ignoresSafeArea().onTapGesture(perform: onClose)
            VStack(alignment: .leading, spacing: 0) {
                HStack {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("调用记录")
                            .font(DS.fontDialogTitle)
                            .foregroundStyle(DS.ink)
                        if let row {
                            Text("#\(row.id) · \(BeijingTime.fullTime(row.created_at))")
                                .font(DS.fontPriorityMeta)
                                .monospacedDigit()
                                .foregroundStyle(DS.muted)
                        }
                    }
                    Spacer()
                    LinearIconButton(systemName: "xmark", label: "关闭详情", action: onClose)
                }
                .padding(.horizontal, 18)
                .padding(.vertical, 14)
                .background(DS.topbar)

                Rectangle().fill(DS.line).frame(height: 1)

                ScrollView {
                    if !error.isEmpty {
                        Text(error)
                            .font(DS.fontCompact)
                            .foregroundStyle(DS.bad)
                            .padding(18)
                    } else if let row {
                        VStack(alignment: .leading, spacing: 14) {
                            section("模型路由") {
                                pair("请求", row.requested_model ?? row.model ?? "—", copyable: true)
                                if let chain = row.model_mapping_chain, !chain.isEmpty {
                                    pair("映射", chain, copyable: true)
                                }
                                pair("转发", row.upstream_model ?? row.requested_model ?? row.model ?? "—", copyable: true)
                                pair("返回", row.upstream_response_model ?? "—", copyable: true)
                                pair("请求强度", row.requested_reasoning_effort ?? "—")
                                pair("实际强度", row.reasoning_effort ?? "—")
                            }
                            section("请求") {
                                pair("用户", "\(row.user_name ?? row.user_email ?? "用户") #\(row.user_id)")
                                pair("API 密钥", "\(row.api_key_name ?? "密钥") #\(row.api_key_id)")
                                pair("账户", "\(row.account_name ?? "账户") #\(row.account_id)")
                                pair("入站端点", row.inbound_endpoint ?? "—", copyable: true)
                                pair("上游端点", row.upstream_endpoint ?? "—", copyable: true)
                                pair("请求 ID", row.request_id ?? "—", copyable: true)
                                pair("上游 ID", row.upstream_request_id ?? "—", copyable: true)
                                pair("IP", row.ip_address ?? "—", copyable: true)
                                pair("首字延迟", RecordFormatting.latency(row.first_token_ms))
                                pair("总耗时", RecordFormatting.latency(row.duration_ms))
                                pair("输出 TPS", RecordFormatting.outputRate(row))
                            }
                            section("Token 与费用") {
                                pair("输入 Token", RecordFormatting.tokenCount(row.input_tokens))
                                pair("输出 Token", RecordFormatting.tokenCount(row.output_tokens))
                                pair("缓存读取", RecordFormatting.tokenCount(row.cache_read_tokens))
                                pair("缓存写入", RecordFormatting.tokenCount(row.cache_creation_tokens))
                                pair("标准费用", RecordFormatting.money(row.total_cost, digits: 8))
                                pair("用户实扣", RecordFormatting.money(row.actual_cost, digits: 8))
                                pair("账户费用", RecordFormatting.money(row.account_cost, digits: 8))
                                pair("用户倍率", RecordFormatting.decimal(row.rate_multiplier, digits: 2))
                                pair("账户倍率", RecordFormatting.decimal(row.account_rate_multiplier ?? "1", digits: 2))
                            }
                        }
                        .padding(18)
                    } else if loading {
                        Text("正在读取记录…")
                            .font(DS.fontCompact)
                            .foregroundStyle(DS.muted)
                            .padding(18)
                    }
                }
            }
            .frame(width: 460)
            .frame(maxHeight: .infinity)
            .background(DS.surface)
            .overlay(alignment: .leading) { Rectangle().fill(DS.line).frame(width: 1) }
            .sub2opsDialogFocus(onEscape: onClose)
        }
    }

    private func section<C: View>(_ title: String, @ViewBuilder content: () -> C) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(title)
                .font(DS.fontCompactSemibold)
                .foregroundStyle(DS.ink)
            VStack(alignment: .leading, spacing: 5) {
                content()
            }
            .padding(12)
            .background(DS.surfaceSubtle)
            .clipShape(RoundedRectangle(cornerRadius: 8))
        }
    }

    private func pair(_ key: String, _ val: String, copyable: Bool = false) -> some View {
        HStack {
            Text(key)
                .font(DS.fontCompact)
                .foregroundStyle(DS.muted)
                .frame(width: 84, alignment: .leading)
            Text(verbatim: val)
                .font(copyable ? DS.fontMono11 : DS.fontCompactMedium)
                .monospacedDigit()
                .foregroundStyle(DS.ink)
                .lineLimit(1)
            Spacer()
            if copyable && val != "—" {
                Button {
                    NSPasteboard.general.clearContents()
                    NSPasteboard.general.setString(val, forType: .string)
                } label: {
                    Image(systemName: "doc.on.doc")
                        .font(DS.iconFont(10))
                        .foregroundStyle(DS.muted)
                }
                .buttonStyle(.linearPlain)
            }
        }
    }
}
