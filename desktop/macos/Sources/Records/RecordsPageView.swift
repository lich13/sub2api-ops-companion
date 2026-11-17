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
    @State private var detailRecord: UsageRecord? = nil
    @State private var detailLoading: Bool = false
    @State private var detailError: String = ""
    @State private var selectedRecordId: Int? = nil

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
        ZStack(alignment: .trailing) {
            VStack(alignment: .leading, spacing: 12) {
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
            }

            if selectedRecordId != nil {
                RecordDetailDrawerView(
                    row: detailRecord,
                    loading: detailLoading,
                    error: detailError,
                    onClose: {
                        selectedRecordId = nil
                        detailRecord = nil
                        detailError = ""
                    }
                )
            }
        }
        .task(id: modelInput) {
            let trimmed = modelInput.trimmingCharacters(in: .whitespacesAndNewlines)
            guard trimmed != appliedModel else { return }
            try? await Task.sleep(for: .milliseconds(300))
            guard !Task.isCancelled else { return }
            appliedModel = trimmed
        }
        .task(id: "\(store.connectionKey):\(queryString):\(active):\(selectedRecordId != nil)") {
            feed.bind(query: queryString, active: active, holding: selectedRecordId != nil)
        }
        .onDisappear {
            feed.stop()
        }
    }

    private var filterToolbar: some View {
        let platforms = PlatformMeta.sortPlatforms(PlatformMeta.preferredOrder + store.accounts.map(\.platform))
        return HStack(spacing: 8) {
            RecordDatePickerView(value: range, disabled: !active || feed.loading) { next in
                if next.start == range.start && next.end == range.end {
                    Task { await feed.refresh() }
                }
                range = next
            }

            Menu {
                Button("全部平台") { platformFilter = "" }
                ForEach(platforms, id: \.self) { pid in
                    Button(PlatformMeta.priorityName(pid)) {
                        platformFilter = pid
                        if let selAcc = accountFilter, selAcc.platform != pid {
                            accountFilter = nil
                        }
                    }
                }
            } label: {
                Text(platformFilter.isEmpty ? "全部平台" : PlatformMeta.priorityName(platformFilter))
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.ink)
                    .padding(.horizontal, 10)
                    .frame(height: DS.controlHeightCompact)
                    .background(DS.surface)
                    .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                    .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
            }
            .menuStyle(.borderlessButton)
            .fixedSize()

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

            HStack(spacing: 6) {
                Image(systemName: "magnifyingglass")
                    .font(DS.iconFont(12))
                    .foregroundStyle(DS.muted)
                TextField(text: $modelInput, prompt: Text(verbatim: "搜索模型")) { EmptyView() }
                    .textFieldStyle(.plain)
                    .font(DS.fontCompact)
                    .onSubmit {
                        appliedModel = modelInput.trimmingCharacters(in: .whitespacesAndNewlines)
                    }
            }
            .padding(.horizontal, 8)
            .frame(minWidth: 140, maxWidth: 200)
            .frame(height: DS.controlHeightCompact)
            .background(DS.surface)
            .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
            .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))

            Button {
                mismatchOnly.toggle()
            } label: {
                HStack(spacing: 5) {
                    Image(systemName: mismatchOnly ? "checkmark.square.fill" : "square")
                        .font(DS.iconFont(12))
                        .foregroundStyle(mismatchOnly ? DS.accent : DS.muted)
                    Text("返回差异")
                        .font(DS.fontCompact)
                        .foregroundStyle(DS.ink)
                }
            }
            .buttonStyle(.plain)
        }
    }

    private var summaryAndActionsBar: some View {
        HStack(spacing: 12) {
            HStack(spacing: 8) {
                Text("总消费")
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.muted)
                Text(RecordFormatting.money(feed.totalCost, digits: 6))
                    .font(DS.fontSectionTitle)
                    .monospacedDigit()
                    .foregroundStyle(DS.ink)
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 6)
            .background(DS.surface)
            .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
            .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))

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

            LinearIconButton(systemName: "arrow.clockwise", label: "刷新记录", disabled: !active || feed.loading) {
                Task { await feed.refresh() }
            }

            Menu {
                let currentKeys = shownColumns.map(\.rawValue)
                ForEach(RecordColumnSpec.allCases, id: \.rawValue) { col in
                    let isOn = currentKeys.contains(col.rawValue)
                    Button {
                        let next = isOn
                            ? currentKeys.filter { $0 != col.rawValue }
                            : (currentKeys + [col.rawValue])
                        Task { await store.updatePreferences(recordColumns: next) }
                    } label: {
                        HStack {
                            Text(col.label)
                            if isOn { Image(systemName: "checkmark") }
                        }
                    }
                }
            } label: {
                Text("列")
                    .font(DS.fontCompactMedium)
                    .foregroundStyle(DS.ink)
                    .padding(.horizontal, 10)
                    .frame(height: DS.controlHeightCompact)
                    .background(DS.surface)
                    .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                    .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
            }
            .menuStyle(.borderlessButton)
            .fixedSize()
        }
    }

    private var recordsTableSurface: some View {
        let cols = shownColumns
        let hasApiKeyCol = cols.contains(.api_key)
        let trailingCols = cols.filter { $0 != .api_key }
        let rowHeight: CGFloat = 58

        return VStack(spacing: 0) {
            if feed.items.isEmpty {
                Text(feed.loading ? "正在读取记录…" : (feed.error.isEmpty ? "暂无记录" : "记录暂不可用"))
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.muted)
                    .frame(maxWidth: .infinity, alignment: .center)
                    .padding(.vertical, 32)
            } else {
                HStack(alignment: .top, spacing: 0) {
                    // Sticky User column
                    VStack(spacing: 0) {
                        Text("用户")
                            .font(DS.fontTableHeader)
                            .foregroundStyle(DS.muted)
                            .padding(.horizontal, 12)
                            .frame(width: 132, height: 36, alignment: .leading)
                            .background(DS.surfaceSubtle)
                        Rectangle().fill(DS.line).frame(height: 1)

                        ForEach(Array(feed.items.enumerated()), id: \.element.id) { idx, row in
                            Button {
                                openDetail(id: row.id)
                            } label: {
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(row.user_name ?? row.user_email ?? "#\(row.user_id)")
                                        .font(DS.fontCompactSemibold)
                                        .foregroundStyle(DS.ink)
                                        .lineLimit(1)
                                    Text("#\(row.user_id)")
                                        .font(DS.fontMono11)
                                        .foregroundStyle(DS.muted)
                                }
                                .padding(.horizontal, 12)
                                .frame(width: 132, height: rowHeight, alignment: .leading)
                                .contentShape(Rectangle())
                            }
                            .buttonStyle(.plain)

                            if idx < feed.items.count - 1 {
                                Rectangle().fill(DS.line).frame(height: 1)
                            }
                        }
                    }
                    .frame(width: 132)

                    Rectangle().fill(DS.line).frame(width: 1)

                    // Horizontally scrollable data columns
                    ScrollView(.horizontal) {
                        VStack(spacing: 0) {
                            HStack(spacing: 12) {
                                if hasApiKeyCol {
                                    headerCell("API 密钥", width: RecordColumnSpec.api_key.width)
                                }
                                headerCell("时间", width: 108)
                                ForEach(trailingCols, id: \.rawValue) { col in
                                    headerCell(col.label, width: col.width, trailing: col == .cost || col == .tokens)
                                }
                            }
                            .padding(.horizontal, 12)
                            .frame(height: 36)
                            .background(DS.surfaceSubtle)

                            Rectangle().fill(DS.line).frame(height: 1)

                            ForEach(Array(feed.items.enumerated()), id: \.element.id) { idx, row in
                                HStack(spacing: 12) {
                                    if hasApiKeyCol {
                                        cellView(.api_key, row: row)
                                    }
                                    Text(BeijingTime.fullTime(row.created_at))
                                        .font(DS.fontResetTime)
                                        .monospacedDigit()
                                        .foregroundStyle(DS.muted)
                                        .frame(width: 108, alignment: .leading)
                                    ForEach(trailingCols, id: \.rawValue) { col in
                                        cellView(col, row: row)
                                    }
                                }
                                .padding(.horizontal, 12)
                                .frame(height: rowHeight)
                                .contentShape(Rectangle())
                                .onTapGesture {
                                    openDetail(id: row.id)
                                }

                                if idx < feed.items.count - 1 {
                                    Rectangle().fill(DS.line).frame(height: 1)
                                }
                            }
                        }
                    }
                }
            }

            if !feed.pageError.isEmpty || feed.cursor != nil || !feed.items.isEmpty {
                Rectangle().fill(DS.line).frame(height: 1)
                HStack(spacing: 8) {
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
                .padding(.vertical, 8)
                .background(DS.surfaceSubtle)
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
                    .font(DS.fontMono11)
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
                ForEach(Array(steps.prefix(2).enumerated()), id: \.offset) { idx, step in
                    HStack(spacing: 4) {
                        if idx > 0 {
                            Text("↳")
                                .font(DS.fontResetTime)
                                .foregroundStyle(DS.muted)
                        }
                        Text(verbatim: step.model)
                            .font(DS.fontMono11)
                            .foregroundStyle(DS.ink)
                            .lineLimit(1)
                    }
                }
                if row.upstream_model_mismatch {
                    Text("返回差异")
                        .font(DS.fontUsageStatMedium)
                        .foregroundStyle(DS.warn)
                }
            }
            .frame(width: col.width, alignment: .leading)

        case .type:
            let rt = RecordFormatting.requestTypeKey(row)
            HStack(spacing: 4) {
                Text(rt.label)
                    .font(DS.fontBadge)
                    .foregroundStyle(DS.accent)
                    .padding(.horizontal, 6)
                    .padding(.vertical, 1)
                    .background(DS.accentBg)
                    .clipShape(RoundedRectangle(cornerRadius: 4))
                if row.native_compaction_v2 == true {
                    Text("压缩")
                        .font(DS.fontBadge)
                        .foregroundStyle(DS.windowEmerald)
                        .padding(.horizontal, 5)
                        .padding(.vertical, 1)
                        .background(DS.windowEmeraldBg)
                        .clipShape(RoundedRectangle(cornerRadius: 4))
                }
            }
            .frame(width: col.width, alignment: .leading)

        case .reasoning:
            VStack(alignment: .leading, spacing: 2) {
                Text(row.requested_reasoning_effort ?? row.reasoning_effort ?? "—")
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.ink)
                if let req = row.requested_reasoning_effort, let act = row.reasoning_effort, req != act {
                    Text("↳ \(act)")
                        .font(DS.fontResetTime)
                        .foregroundStyle(DS.muted)
                }
            }
            .frame(width: col.width, alignment: .leading)

        case .tokens:
            VStack(alignment: .trailing, spacing: 2) {
                HStack(spacing: 6) {
                    Text("↓\(RecordFormatting.tokenCount(row.input_tokens))")
                        .foregroundStyle(DS.good)
                    Text("↑\(RecordFormatting.tokenCount(row.output_tokens))")
                        .foregroundStyle(DS.windowPurple)
                }
                HStack(spacing: 6) {
                    Text("R \(RecordFormatting.cacheTokenCount(row.cache_read_tokens))")
                        .foregroundStyle(DS.tokenCyan)
                    Text("W \(RecordFormatting.cacheTokenCount(row.cache_creation_tokens))")
                        .foregroundStyle(DS.warn)
                }
            }
            .font(DS.fontResetTime)
            .monospacedDigit()
            .frame(width: col.width, alignment: .trailing)

        case .cost:
            CostDetailsPopover(row: row)
                .frame(width: col.width, alignment: .trailing)

        case .latency:
            let firstTone = latencyColor(RecordFormatting.latencyTone(row.first_token_ms, firstToken: true))
            let totalTone = latencyColor(RecordFormatting.latencyTone(row.duration_ms, firstToken: false))
            HStack(spacing: 5) {
                VStack(spacing: 1) {
                    RoundedRectangle(cornerRadius: 1).fill(firstTone).frame(width: 3, height: 14)
                    RoundedRectangle(cornerRadius: 1).fill(totalTone).frame(width: 3, height: 14)
                }
                VStack(alignment: .leading, spacing: 1) {
                    Text("首 \(RecordFormatting.latency(row.first_token_ms))")
                        .foregroundStyle(firstTone)
                    Text("总 \(RecordFormatting.latency(row.duration_ms))")
                        .foregroundStyle(totalTone)
                    Text(RecordFormatting.outputRate(row))
                        .foregroundStyle(DS.muted)
                }
                .font(DS.fontUsageStat)
                .monospacedDigit()
            }
            .frame(width: col.width, alignment: .leading)

        case .user_agent:
            Text(verbatim: row.user_agent ?? "—")
                .font(DS.fontCompact)
                .foregroundStyle(DS.muted)
                .lineLimit(1)
                .frame(width: col.width, alignment: .leading)

        case .ip:
            Text(verbatim: row.ip_address ?? "—")
                .font(DS.fontMono11)
                .foregroundStyle(DS.muted)
                .lineLimit(1)
                .frame(width: col.width, alignment: .leading)

        case .endpoint:
            VStack(alignment: .leading, spacing: 2) {
                Text(verbatim: row.inbound_endpoint ?? "—")
                    .font(DS.fontMono11)
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
        selectedRecordId = id
        detailRecord = feed.items.first(where: { $0.id == id })
        detailLoading = true
        detailError = ""
        Task {
            do {
                let loaded = try await feed.loadDetail(id: id)
                if selectedRecordId == id {
                    detailRecord = loaded
                }
            } catch {
                if selectedRecordId == id && detailRecord == nil {
                    detailError = error.localizedDescription
                }
            }
            if selectedRecordId == id {
                detailLoading = false
            }
        }
    }
}

private struct RecordDetailDrawerView: View {
    let row: UsageRecord?
    let loading: Bool
    let error: String
    let onClose: () -> Void

    var body: some View {
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
                .buttonStyle(.plain)
            }
        }
    }
}
