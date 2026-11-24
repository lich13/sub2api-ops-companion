import SwiftUI

public struct DetectionLogsTableView: View {
    public let online: Bool
    public let connectionKey: String
    public var onSelectLog: ((DetectionLog) -> Void)?

    @State private var feed = DetectionLogStore()
    @State private var fallbackSelectedRow: DetectionLog? = nil
    @State private var viewportWidth: CGFloat = 1100
    @State private var hoveredJobId: String? = nil

    private static let statuses: [String: String] = [
        "completed": "已完成", "failed": "失败", "cancelled": "已取消",
        "interrupted": "已中断", "needs_confirmation": "待确认",
    ]
    private static let outcomes: [String: String] = [
        "normal": "未确认降智", "degraded": "首组判定降智",
        "aggregate_degraded": "汇总符合降智特征", "unavailable": "历史详情不可用",
        "failed": "失败", "cancelled": "已取消",
        "interrupted": "已中断", "needs_confirmation": "待确认",
    ]
    private static let triggers: [String: String] = [
        "scheduled": "定时检测", "recheck": "降智复查", "error": "错误触发",
        "slow": "慢首字", "recovery": "额度恢复验证", "credit": "用卡恢复验证",
    ]
    private static let notices: [String: String] = [
        "pending": "待推送", "queued": "待推送", "retry": "待重试",
        "delivered": "已推送", "suppressed": "已取消", "failed": "投递失败",
    ]

    public init(online: Bool, connectionKey: String, onSelectLog: ((DetectionLog) -> Void)? = nil) {
        self.online = online
        self.connectionKey = connectionKey
        self.onSelectLog = onSelectLog
    }

    public var body: some View {
        ZStack {
            VStack(alignment: .leading, spacing: 10) {
                HStack {
                    Text("\(feed.items.count) 条记录")
                        .font(DS.fontCompact)
                        .monospacedDigit()
                        .foregroundStyle(DS.muted)
                    Spacer()
                    Button("刷新记录") {
                        Task { await feed.load(replace: true, online: online) }
                    }
                    .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                    .disabled(!online || feed.loading)
                }

                if !feed.error.isEmpty {
                    HStack(spacing: 8) {
                        Text(feed.error)
                            .font(DS.fontCompact)
                            .foregroundStyle(DS.bad)
                        Button("重试") {
                            Task { await feed.load(replace: true, online: online) }
                        }
                        .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                    }
                }

                ScrollView(.horizontal, showsIndicators: false) {
                    VStack(spacing: 0) {
                        headerRow
                        Rectangle().fill(DS.line).frame(height: 1)
                        if feed.items.isEmpty {
                            Text(feed.loading ? "正在读取…" : (feed.error.isEmpty ? "暂无自动检测记录" : "记录暂不可用"))
                                .font(DS.fontCompact)
                                .foregroundStyle(DS.muted)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .padding(16)
                        } else {
                            ForEach(Array(feed.items.enumerated()), id: \.element.job_id) { idx, row in
                                logRow(row)
                                if idx < feed.items.count - 1 {
                                    Rectangle().fill(DS.line).frame(height: 1)
                                }
                            }
                        }
                    }
                    .frame(minWidth: max(1041, viewportWidth), maxWidth: .infinity, alignment: .leading)
                }
                .focusEffectDisabled()
                .background(LinearWidthObserver { viewportWidth = $0 })
                .background(DS.surface)
                .overlay(RoundedRectangle(cornerRadius: DS.cardRadius).strokeBorder(DS.line, lineWidth: 1))
                .clipShape(RoundedRectangle(cornerRadius: DS.cardRadius))

                if feed.cursor != nil {
                    Button(feed.loading ? "加载中…" : "加载更早记录") {
                        Task { await feed.load(replace: false, online: online) }
                    }
                    .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                    .disabled(!online || feed.loading)
                }
            }

            if let selectedRow = fallbackSelectedRow {
                DetectionLogDetailModal(row: selectedRow, online: online) {
                    self.fallbackSelectedRow = nil
                }
            }
        }
        .task(id: "\(connectionKey):\(online)") {
            feed.bind(connectionKey: connectionKey)
            await feed.load(replace: true, online: online)
        }
    }

    private var extraWidth: CGFloat {
        max(0, viewportWidth - 1041)
    }

    private var headerRow: some View {
        HStack(spacing: 7) {
            colHead("结果时间", width: 96)
            colHead("账号", width: 112 + round(extraWidth * 0.25))
            colHead("模型", width: 100 + round(extraWidth * 0.20))
            colHead("触发", width: 104)
            colHead("执行", width: 58)
            colHead("结论", width: 106)
            colHead("预测", width: 116 + round(extraWidth * 0.30))
            colHead("匹配度", width: 80)
            colHead("处置／恢复", width: 106 + round(extraWidth * 0.25))
            colHead("通知", width: 76)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 12)
        .frame(height: 34)
        .background(DS.surfaceSubtle)
    }

    private func colHead(_ title: String, width: CGFloat) -> some View {
        Text(title)
            .font(DS.fontTableHeader)
            .foregroundStyle(DS.muted)
            .frame(width: width, alignment: .leading)
    }

    private func logRow(_ row: DetectionLog) -> some View {
        Button {
            if let onSelectLog {
                onSelectLog(row)
            } else {
                fallbackSelectedRow = row
            }
        } label: {
            HStack(alignment: .center, spacing: 7) {
                Text(BeijingTime.fullTime(row.created_at))
                    .font(DS.fontResetTime)
                    .monospacedDigit()
                    .foregroundStyle(DS.muted)
                    .frame(width: 96, alignment: .leading)

                VStack(alignment: .leading, spacing: 2) {
                    Text(row.account_name.isEmpty ? "账号 #\(row.account_id)" : row.account_name)
                        .font(DS.fontCompactSemibold)
                        .foregroundStyle(DS.ink)
                        .lineLimit(1)
                    Text("#\(row.account_id)")
                        .font(DS.fontPriorityMeta).monospacedDigit()
                        .foregroundStyle(DS.muted)
                }
                .frame(width: 112 + round(extraWidth * 0.25), alignment: .leading)

                Text(verbatim: row.model_id)
                    .font(DS.fontPriorityMeta).monospacedDigit()
                    .foregroundStyle(DS.ink)
                    .lineLimit(1)
                    .frame(width: 100 + round(extraWidth * 0.20), alignment: .leading)

                let triggerSummary = row.triggers.map { Self.triggers[$0] ?? "自动检测" }.joined(separator: "、")
                VStack(alignment: .leading, spacing: 2) {
                    Text(triggerSummary.isEmpty ? "自动检测" : triggerSummary)
                        .font(DS.fontCompact)
                        .foregroundStyle(DS.ink)
                        .lineLimit(1)
                    Text("\(ModelTestEngine.name(for: row.summary.engine)) · \(row.summary.sample_count ?? 3) 组")
                        .font(DS.fontResetTime)
                        .monospacedDigit()
                        .foregroundStyle(DS.muted)
                }
                .frame(width: 104, alignment: .leading)

                badge(
                    Self.statuses[row.status] ?? "待核对",
                    tone: row.status == "failed" ? .bad : ((row.status == "completed" || row.status == "cancelled") ? .neutral : .warn)
                )
                    .frame(width: 58, alignment: .leading)

                badge(outcomeLabel(row), tone: outcomeTone(row))
                    .frame(width: 106, alignment: .leading)

                VStack(alignment: .leading, spacing: 2) {
                    Text(verbatim: "首组 \(row.summary.first_prediction.isEmpty ? "—" : row.summary.first_prediction)")
                    Text(verbatim: "汇总 \((row.summary.prediction_name ?? row.summary.prediction).isEmpty ? "—" : (row.summary.prediction_name ?? row.summary.prediction))")
                }
                .font(DS.fontResetTime)
                .foregroundStyle(DS.ink)
                .lineLimit(1)
                .frame(width: 116 + round(extraWidth * 0.30), alignment: .leading)

                VStack(alignment: .leading, spacing: 2) {
                    Text("首组 \(probText(row.summary.first_probability))")
                    Text("汇总 \(probText(row.summary.probability))")
                }
                .font(DS.fontResetTime)
                .monospacedDigit()
                .foregroundStyle(DS.ink)
                .frame(width: 80, alignment: .leading)

                receiptsCell(row)
                    .frame(width: 106 + round(extraWidth * 0.25), alignment: .leading)

                if let n = row.notification {
                    VStack(alignment: .leading, spacing: 2) {
                        badge(
                            Self.notices[n.status] ?? "待核对",
                            tone: n.status == "failed" ? .bad : ((n.status == "delivered" || n.status == "suppressed") ? .neutral : .warn)
                        )
                        if n.merged {
                            Text("合并用卡通知")
                                .font(DS.fontUsageStat)
                                .foregroundStyle(DS.muted)
                        }
                    }
                    .frame(width: 76, alignment: .leading)
                } else {
                    Text("无通知记录")
                        .font(DS.fontResetTime)
                        .foregroundStyle(DS.muted)
                        .frame(width: 76, alignment: .leading)
                }
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 9)
            .background(hoveredJobId == row.job_id ? DS.rowHover : DS.clear)
            .contentShape(Rectangle())
        }
        .buttonStyle(.linearPlain)
        .onHover { hovering in
            if hovering {
                hoveredJobId = row.job_id
            } else if hoveredJobId == row.job_id {
                hoveredJobId = nil
            }
        }
    }

    private enum BadgeTone { case good, warn, bad, neutral }

    private func badge(_ text: String, tone: BadgeTone) -> some View {
        let fg: Color = switch tone {
        case .good: DS.good
        case .warn: DS.warn
        case .bad: DS.bad
        case .neutral: DS.muted
        }
        let bg: Color = switch tone {
        case .good: DS.goodBg
        case .warn: DS.warnBg
        case .bad: DS.badBg
        case .neutral: DS.surfaceSubtle
        }
        return Text(text)
            .font(DS.fontResetTimeSemibold)
            .foregroundStyle(fg)
            .padding(.horizontal, 6)
            .padding(.vertical, 2)
            .background(bg)
            .clipShape(RoundedRectangle(cornerRadius: 4))
    }

    private func outcomeLabel(_ row: DetectionLog) -> String {
        if row.summary.engine == "lm_detector" || row.summary.verdict_mode == "relaxed" {
            switch row.summary.outcome {
            case "degraded": return "判定降智"
            case "unavailable": return "无法判断"
            case "normal": return "未确认降智"
            default: break
            }
        }
        let basis = row.summary.disposition?.basis ?? row.summary.recovery?.basis
        if basis == "aggregate" { return "最终汇总判定降智" }
        return Self.outcomes[row.summary.outcome] ?? "待核对"
    }

    private func outcomeTone(_ row: DetectionLog) -> BadgeTone {
        switch row.summary.outcome {
        case "normal": return .good
        case "degraded", "aggregate_degraded", "failed": return .bad
        case "cancelled": return .neutral
        default: return .warn
        }
    }

    private func probText(_ val: Double?) -> String {
        guard let val else { return "—" }
        return String(format: "%.1f%%", val * 100.0)
    }

    private func pendingTone(_ status: String?) -> BadgeTone {
        if status == "failed" { return .bad }
        if status == "cancelled" { return .neutral }
        return .warn
    }

    @ViewBuilder
    private func receiptsCell(_ row: DetectionLog) -> some View {
        let s = row.summary
        let verifying = row.triggers.contains(where: { $0 == "recovery" || $0 == "credit" }) || s.verification != nil
        VStack(alignment: .leading, spacing: 3) {
            if let disp = s.disposition {
                badge(
                    disp.marked == true ? "标记已保存" : "标记待核对",
                    tone: disp.marked == true ? .bad : pendingTone(disp.status)
                )
                badge(
                    disp.schedule_verified == true ? "停调度已确认" : "停调度待核对",
                    tone: disp.schedule_verified == true ? .bad : pendingTone(disp.status)
                )
            }
            if let rec = s.recovery {
                badge(
                    rec.mark_cleared == true ? "已取消降智标记" : "取消标记待核对",
                    tone: rec.mark_cleared == true ? .good : pendingTone(rec.status)
                )
            }
            if verifying {
                let verLabel = s.verification?.status == "passed"
                    ? "模型验证通过"
                    : (s.verification?.status == "failed" ? "模型验证失败" : "模型验证待核对")
                badge(
                    verLabel,
                    tone: s.verification?.status == "passed" ? .good : pendingTone(s.verification?.status)
                )
            }
            if let rest = s.restoration {
                badge(
                    rest.status == "restored" ? "已恢复调度" : "恢复调度待核对",
                    tone: rest.status == "restored" ? .good : pendingTone(rest.status)
                )
            }
            if s.disposition == nil && s.recovery == nil && !verifying && s.restoration == nil {
                Text("—")
                    .font(DS.fontResetTime)
                    .foregroundStyle(DS.muted)
            }
        }
    }
}

public struct DetectionLogDetailModal: View {
    public let row: DetectionLog
    public let online: Bool
    public let onClose: () -> Void

    @State private var groups: [ModelTestGroupStatus] = []
    @State private var loading: Bool = false
    @State private var note: String = ""

    private static let statuses: [String: String] = [
        "completed": "已完成", "failed": "失败", "cancelled": "已取消",
        "interrupted": "已中断", "needs_confirmation": "待确认",
    ]
    private static let outcomes: [String: String] = [
        "normal": "未确认降智", "degraded": "首组判定降智",
        "aggregate_degraded": "汇总符合降智特征", "unavailable": "历史详情不可用",
        "failed": "失败", "cancelled": "已取消",
        "interrupted": "已中断", "needs_confirmation": "待确认",
    ]
    private static let triggers: [String: String] = [
        "scheduled": "定时检测", "recheck": "降智复查", "error": "错误触发",
        "slow": "慢首字", "recovery": "额度恢复验证", "credit": "用卡恢复验证",
    ]
    private static let notices: [String: String] = [
        "pending": "待推送", "queued": "待推送", "retry": "待重试",
        "delivered": "已推送", "suppressed": "已取消", "failed": "投递失败",
    ]

    public init(row: DetectionLog, online: Bool, onClose: @escaping () -> Void) {
        self.row = row
        self.online = online
        self.onClose = onClose
    }

    private var summaryBasis: String? {
        row.summary.disposition?.basis ?? row.summary.recovery?.basis
    }

    private var evidenceText: String {
        let kind = summaryBasis ?? row.summary.classification?.basis
        if kind == "sample_family" {
            let idxs = (row.summary.disposition?.group_indexes ?? row.summary.classification?.group_indexes ?? [])
                .map(String.init)
                .joined(separator: "、")
            return "第 \(idxs) 组跨家族证据"
        }
        return kind == "aggregate" ? "最终汇总" : "单组预测"
    }

    private var outcomeText: String {
        if row.summary.engine == "lm_detector" || row.summary.verdict_mode == "relaxed" {
            switch row.summary.outcome {
            case "degraded": return "判定降智"
            case "unavailable": return "无法判断"
            case "normal": return "未确认降智"
            default: return Self.outcomes[row.summary.outcome] ?? "待核对"
            }
        }
        if summaryBasis == "aggregate" { return "最终汇总判定降智" }
        return Self.outcomes[row.summary.outcome] ?? "待核对"
    }

    public var body: some View {
        ZStack {
            DS.backdrop.ignoresSafeArea().onTapGesture(perform: onClose)
            LinearModalCard(
                title: "自动检测日志",
                subtitle: "\(row.account_name) · #\(row.account_id)",
                width: 560,
                onClose: onClose
            ) {
                LinearBoundedScrollView(maxHeight: 460) {
                    VStack(alignment: .leading, spacing: 10) {
                        VStack(alignment: .leading, spacing: 6) {
                            fact("请求模型", row.model_id, mono: true)
                            let triggerText = row.triggers.map { Self.triggers[$0] ?? "自动检测" }.joined(separator: "、")
                            fact("触发", triggerText.isEmpty ? "自动检测" : triggerText)
                            fact("检测源", "\(ModelTestEngine.name(for: row.summary.engine)) · \(row.summary.sample_count ?? 3) 组")
                            fact(
                                "判定模式",
                                row.summary.verdict_mode == "relaxed"
                                    ? "宽松模式 · \(row.summary.degraded_model_id ?? DefaultModelId.legacyDegraded.rawValue)"
                                    : "严格模式"
                            )
                            fact("执行状态", Self.statuses[row.status] ?? "待核对")
                            fact("检测结论", outcomeText)
                            if summaryBasis != nil || row.summary.classification?.basis != nil {
                                let basisLabel = (row.summary.engine == "lm_detector" || row.summary.verdict_mode == "relaxed")
                                    ? evidenceText
                                    : (summaryBasis == "aggregate" ? "最终汇总" : "首组指纹")
                                fact("处置依据", basisLabel)
                            }
                            if row.summary.engine == "lm_detector" {
                                let decisionText: String = switch row.summary.decision {
                                case "partial": "初步结果"
                                case "scored": "汇总结果"
                                case "not_model": "序列异常"
                                case "unscorable": "样本不足"
                                default: "—"
                                }
                                fact("分析状态", decisionText)
                                if let reason = row.summary.reason, !reason.isEmpty {
                                    fact("无法判断原因", reason)
                                }
                                if let gps = row.summary.group_predictions {
                                    ForEach(Array(gps.enumerated()), id: \.offset) { _, gp in
                                        fact("第 \(gp.index) 组", (gp.prediction?.isEmpty == false) ? gp.prediction! : "无排名")
                                    }
                                }
                                fact("资源版本", row.summary.bank_version?.revision ?? "—", mono: true)
                            }
                            fact("首组预测", row.summary.first_prediction.isEmpty ? "暂无记录" : row.summary.first_prediction)
                            let aggPred = row.summary.prediction_name ?? row.summary.prediction
                            fact("汇总预测", aggPred.isEmpty ? "暂无记录" : aggPred)
                            fact(
                                "汇总匹配度",
                                row.summary.probability.map { String(format: "%.1f%%", $0 * 100.0) } ?? "—"
                            )
                            if let fp = row.summary.first_probability {
                                fact("首组匹配度", String(format: "%.1f%%", fp * 100.0))
                            }
                            if let disp = row.summary.disposition {
                                let m = disp.marked == true ? "标记已保存" : "标记待核对"
                                let sv = disp.schedule_verified == true ? "停调度已确认" : "停调度待核对"
                                let r = disp.reason.isEmpty ? "" : " · \(disp.reason)"
                                fact("降智处置", "\(m) · \(sv)\(r)")
                            }
                            if let rec = row.summary.recovery {
                                let recText = rec.mark_cleared == true
                                    ? "已取消降智标记"
                                    : (rec.reason.isEmpty ? "恢复待核对" : rec.reason)
                                fact("复查恢复", recText)
                            }
                            if row.triggers.contains(where: { $0 == "recovery" || $0 == "credit" }) || row.summary.verification != nil {
                                let verText = row.summary.verification?.status == "passed"
                                    ? "模型验证通过"
                                    : ((row.summary.verification?.reason.isEmpty == false) ? row.summary.verification!.reason : "验证回执不可用")
                                fact("恢复验证", verText)
                            }
                            if let rest = row.summary.restoration {
                                let restText = rest.status == "restored" ? "已恢复调度" : "恢复待核对"
                                fact("调度恢复", "\(restText) · \(BeijingTime.fullTime(rest.completed_at))")
                            }
                            if let n = row.notification {
                                let noticeText = (Self.notices[n.status] ?? "状态待核对") + (n.merged ? "（合并用卡通知）" : "")
                                fact("通知", noticeText)
                                if let delivered = n.delivered_at, !delivered.isEmpty {
                                    fact("推送时间", BeijingTime.fullTime(delivered))
                                }
                            } else {
                                fact("通知", "无通知记录")
                            }
                            fact("开始时间", BeijingTime.fullTime(row.started_at))
                            fact("结束时间", BeijingTime.fullTime(row.completed_at))
                            if !row.error.isEmpty {
                                fact("原因", row.error)
                            }
                        }
                        .padding(12)
                        .background(DS.surfaceSubtle)
                        .clipShape(RoundedRectangle(cornerRadius: 8))

                        if loading {
                            Text("正在读取任务详情")
                                .font(DS.fontCompact)
                                .foregroundStyle(DS.muted)
                        } else if !groups.isEmpty {
                            HStack(spacing: 8) {
                                ForEach(groups) { g in
                                    let statusLabel = Self.statuses[g.status]
                                        ?? (["skipped": "已跳过", "queued": "排队中", "running": "运行中"][g.status] ?? "待核对")
                                    Text("第 \(g.index) 组：\(statusLabel)\(g.duration_ms.map { String(format: " · %.1f 秒", $0 / 1000.0) } ?? "")")
                                        .font(DS.fontResetTime)
                                        .monospacedDigit()
                                        .foregroundStyle(DS.ink)
                                        .padding(.horizontal, 8)
                                        .padding(.vertical, 4)
                                        .background(DS.surfaceSubtle)
                                        .clipShape(RoundedRectangle(cornerRadius: 5))
                                }
                            }
                        } else if !note.isEmpty {
                            Text(note)
                                .font(DS.fontResetTime)
                                .foregroundStyle(DS.muted)
                        } else {
                            Text("历史详情不可用")
                                .font(DS.fontResetTime)
                                .foregroundStyle(DS.muted)
                        }
                    }
                }
            } footer: {
                Spacer()
                Button("关闭", action: onClose)
                    .buttonStyle(LinearButtonStyle(variant: .secondary))
            }
            .sub2opsDialogFocus(onEscape: onClose)
            .task(id: "\(row.job_id):\(online)") {
                groups = []
                note = ""
                guard online else {
                    loading = false
                    return
                }
                loading = true
                defer { loading = false }
                do {
                    let job: ModelTestJob = try await RustBridge.shared.api("GET", "/model-tests/\(row.job_id)")
                    guard job.id == row.job_id else {
                        note = "任务详情暂不可用，已保留结果摘要。"
                        return
                    }
                    groups = job.groups ?? []
                } catch {
                    note = "任务详情暂不可用，已保留结果摘要。"
                }
            }
        }
    }

    private func fact(_ key: String, _ value: String, mono: Bool = false) -> some View {
        HStack(alignment: .top) {
            Text(key)
                .font(DS.fontCompact)
                .foregroundStyle(DS.muted)
                .frame(width: 88, alignment: .leading)
            Text(verbatim: value)
                .font(mono ? DS.fontMono11 : DS.fontCompactMedium)
                .monospacedDigit()
                .foregroundStyle(DS.ink)
            Spacer()
        }
    }
}
