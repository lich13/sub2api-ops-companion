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
    @State private var scrolled = false
    @State private var openFilters = Set<String>()
    @State private var datePending = false
    @State private var scrollReset = 0
    @State private var footerNear: Bool = false

    public init(store: AppStore) {
        self.store = store
    }

    private var active: Bool {
        store.state.online && (store.state.foreground != false)
    }

    private var overlayOpen: Bool { store.selectedRecordId != nil || !openFilters.isEmpty }
    private var holding: Bool { scrolled || overlayOpen || datePending }
    private func filterOpened(_ key: String, _ open: Bool) {
        if open { openFilters.insert(key) } else { openFilters.remove(key) }
    }

    private var autoLoadKey: String {
        "\(footerNear):\(active):\(overlayOpen):\(feed.loading):\(feed.cursor ?? ""):\(feed.items.count)"
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
                        Task { await refreshRecords() }
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
        .task(id: "\(store.connectionKey):\(queryString):\(active)") {
            feed.holding = holding
            feed.bind(query: queryString, active: active)
        }
        .onChange(of: holding) { feed.holding = holding }
        .onChange(of: queryString) {
            scrolled = false
            scrollReset += 1
        }
        .task(id: "\(active):\(range.preset?.rawValue ?? "")") {
            guard active, range.preset != nil else { return }
            while !Task.isCancelled {
                let now = Date()
                datePending = range.refreshPreset(at: now, holding: holding || LinearMenuTracking.isTracking)
                var calendar = Calendar(identifier: .gregorian)
                calendar.timeZone = TimeZone(identifier: "Asia/Shanghai")!
                guard let midnight = calendar.dateInterval(of: .day, for: now)?.end else { return }
                do { try await Task.sleep(for: .seconds(midnight.timeIntervalSince(now))) }
                catch { return }
            }
        }
        // Re-evaluated on the same changes the web observer re-arms on; `loadMore` itself
        // ignores calls without a cursor, while loading, or after a page error.
        .onChange(of: autoLoadKey, initial: true) {
            guard RecordPagination.autoLoads(
                footerNear: footerNear, active: active, detailOpen: overlayOpen || LinearMenuTracking.isTracking
            ) else { return }
            Task { await feed.loadMore() }
        }
        .onDisappear {
            feed.stop()
        }
    }

    private var filterToolbar: some View {
        let platforms = PlatformMeta.sortPlatforms(PlatformMeta.preferredOrder + store.accounts.map(\.platform))
        return HStack(spacing: 6) {
            RecordDatePickerView(value: range, disabled: !active || feed.loading, onOpenChange: { filterOpened("date", $0) }) { next in
                datePending = false
                if next.start == range.start && next.end == range.end {
                    Task { await refreshRecords() }
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

            RecordOptionPickerView(kind: .users, active: active, selected: userFilter, onOpenChange: { filterOpened("users", $0) }) { next in
                userFilter = next
                keyFilter = nil
            }

            RecordOptionPickerView(kind: .accounts, active: active, selected: accountFilter, localOptions: accountOptions, onOpenChange: { filterOpened("accounts", $0) }) { next in
                accountFilter = next
            }

            RecordOptionPickerView(kind: .api_keys, active: active, selected: keyFilter, userId: userFilter?.id, onOpenChange: { filterOpened("keys", $0) }) { next in
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

    private func refreshRecords() async {
        if datePending, let preset = range.preset {
            datePending = false
            range = BeijingDateLogic.presetRange(preset)
        } else if await feed.refresh() {
            scrolled = false
            scrollReset += 1
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

            if feed.newCount > 0 || datePending {
                Button(datePending ? "日期已更新 · 刷新" : "\(feed.newCount) 条新记录") {
                    Task { await refreshRecords() }
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
                Task { await refreshRecords() }
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
                GeometryReader { viewport in
                    ScrollView(.vertical) {
                        VStack(spacing: 0) {
                            HStack(alignment: .top, spacing: 0) {
                                VStack(spacing: 0) {
                                    ForEach(Array(feed.items.enumerated()), id: \.element.id) { idx, item in
                                        let row = item.record
                                        let rh = item.height(model: hasModelCol, latency: hasLatencyCol)
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
                                        .help("\(row.user_name ?? row.user_email ?? "用户") #\(row.user_id)")
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

                                        ForEach(Array(feed.items.enumerated()), id: \.element.id) { idx, item in
                                            let row = item.record
                                            let rh = item.height(model: hasModelCol, latency: hasLatencyCol)
                                            HStack(spacing: colSpacing) {
                                                if hasApiKeyCol {
                                                    cellView(.api_key, row: row, route: item.route)
                                                }
                                                Text(BeijingTime.fullTime(row.created_at))
                                                    .font(DS.fontPriorityMeta)
                                                    .monospacedDigit()
                                                    .lineLimit(1)
                                                    .foregroundStyle(DS.muted)
                                                    .frame(width: timeColWidth, alignment: .leading)
                                                ForEach(trailingCols, id: \.rawValue) { col in
                                                    cellView(col, row: row, route: item.route)
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
                                        if feed.loading {
                                            Text("正在读取…")
                                                .font(DS.fontResetTime)
                                                .foregroundStyle(DS.muted)
                                        }
                                    } else if !feed.loading {
                                        Text("已全部加载")
                                            .font(DS.fontResetTime)
                                            .foregroundStyle(DS.muted)
                                    }
                                    Spacer()
                                }
                                .padding(.horizontal, 12)
                                .padding(.vertical, 9)
                                .frame(minHeight: 28)
                                .background(
                                    GeometryReader { geo in
                                        DS.clear.preference(
                                            key: RecordsFooterNearKey.self,
                                            value: RecordPagination.footerNear(
                                                footerMinY: geo.frame(in: .named("recordsVScroll")).minY,
                                                viewportHeight: viewport.size.height
                                            )
                                        )
                                    }
                                )
                            }
                        }
                        .onGeometryChange(for: Bool.self) { $0.frame(in: .named("recordsVScroll")).minY < -8 } action: { scrolled = $0 }
                    }
                    .coordinateSpace(name: "recordsVScroll")
                    .id(scrollReset)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            }
        }
        .background(DS.surface)
        .overlay(RoundedRectangle(cornerRadius: DS.cardRadius).strokeBorder(DS.line, lineWidth: 1))
        .clipShape(RoundedRectangle(cornerRadius: DS.cardRadius))
        // Attached outside the scroll view so the flag resets when the table is replaced
        // by the empty state.
        .onPreferenceChange(RecordsFooterNearKey.self) { next in
            if next != footerNear {
                footerNear = next
            }
        }
    }

    private func headerCell(_ title: String, width: CGFloat, trailing: Bool = false) -> some View {
        Text(title)
            .font(DS.fontTableHeader)
            .foregroundStyle(DS.muted)
            .frame(width: width, alignment: trailing ? .trailing : .leading)
    }

    @ViewBuilder
    private func cellView(_ col: RecordColumnSpec, row: UsageRecord, route steps: [ModelRouteStep]) -> some View {
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
            .help("\(row.api_key_name ?? "API 密钥") #\(row.api_key_id)")

        case .account:
            Text(row.account_name ?? "#\(row.account_id)")
                .font(DS.fontCompact)
                .foregroundStyle(DS.ink)
                .lineLimit(1)
                .frame(width: col.width, alignment: .leading)
                .help("\(row.account_name ?? "账户") #\(row.account_id)")

        case .model:
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
            RecordTypeBadges(row: row)
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
                .help(row.user_agent ?? "—")

        case .ip:
            Text(verbatim: row.ip_address ?? "—")
                .font(DS.fontPriorityMeta).monospacedDigit()
                .foregroundStyle(DS.muted)
                .lineLimit(1)
                .frame(width: col.width, alignment: .leading)
                .help(row.ip_address ?? "—")

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
            Text(RecordFormatting.billingMode(row.billing_mode))
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
        store.openRecordDetail(id: id, seed: seed?.record)
    }
}

private struct RecordsFooterNearKey: PreferenceKey {
    static let defaultValue: Bool = false
    static func reduce(value: inout Bool, nextValue: () -> Bool) {
        value = value || nextValue()
    }
}

private struct RecordsHScrollOffsetKey: PreferenceKey {
    static let defaultValue: CGFloat = 0
    static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) {
        value = nextValue()
    }
}

struct RecordTypeBadges: View {
    let row: UsageRecord
    var body: some View {
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
}
