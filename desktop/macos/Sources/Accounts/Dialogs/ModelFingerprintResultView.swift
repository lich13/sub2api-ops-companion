import SwiftUI

public struct ModelFingerprintResultView: View {
    public let job: ModelTestJob

    public init(job: ModelTestJob) {
        self.job = job
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                StatusDot(statusTone)
                Text(headlineText)
                    .font(DS.fontCompactSemibold)
                    .foregroundStyle(DS.ink)
                Spacer()
                if let attempts = job.attempts {
                    Text("\(attempts) 次请求")
                        .font(DS.fontResetTime)
                        .monospacedDigit()
                        .foregroundStyle(DS.muted)
                }
                if let stamp = job.completed_at ?? job.started_at ?? job.queued_at {
                    Text(BeijingTime.fullTime(stamp))
                        .font(DS.fontResetTime)
                        .monospacedDigit()
                        .foregroundStyle(DS.muted)
                }
            }

            HStack(spacing: 12) {
                metricPill("检测源", value: "\(ModelTestEngine.name(for: job.engine)) · \(job.planned_groups ?? 3) 组")
                if let pred = job.report?.prediction_name ?? job.report?.prediction, !pred.isEmpty {
                    let suffix = job.report?.merged_class == true ? "（合并类别）" : ""
                    metricPill("汇总预测", value: pred + suffix)
                }
                if let prob = job.report?.probability {
                    let digits = job.engine == "lm_detector" ? 2 : 1
                    metricPill("匹配度", value: String(format: "%.\(digits)f%%", prob * 100.0))
                }
                metricPill(
                    "有效组数",
                    value: "\(job.report?.used_outputs ?? job.valid_groups ?? 0)/\(job.planned_groups ?? 3)"
                )
                if let duration = job.duration_ms, duration > 0 {
                    metricPill("耗时", value: String(format: "%.1fs", duration / 1000.0))
                }
            }

            if job.queued_at != nil || job.started_at != nil || job.requested_model != nil {
                HStack(spacing: 12) {
                    if let req = job.requested_model, !req.isEmpty {
                        metricPill("请求模型", value: req, mono: true)
                    }
                    if let fwd = job.forwarded_model, !fwd.isEmpty, fwd != job.requested_model {
                        metricPill("转发模型", value: fwd, mono: true)
                    }
                    if let queued = job.queued_at {
                        metricPill("排队时间", value: BeijingTime.fullTime(queued))
                    }
                    if let started = job.started_at {
                        metricPill("开始时间", value: BeijingTime.fullTime(started))
                    }
                }
            }

            if let reason = job.classification?.reason, !reason.isEmpty {
                Text(reason)
                    .font(DS.fontResetTime)
                    .foregroundStyle(DS.warn)
            }

            if let candidates = job.report?.results, !candidates.isEmpty {
                VStack(alignment: .leading, spacing: 5) {
                    Text("候选模型分布")
                        .font(DS.fontResetTimeSemibold)
                        .foregroundStyle(DS.muted)
                    ForEach(Array(candidates.prefix(5).enumerated()), id: \.offset) { _, cand in
                        let pct = cand.probability.map { min(100, max(0, $0 * 100.0)) }
                        HStack(spacing: 8) {
                            Text(verbatim: cand.display_name ?? cand.model)
                                .font(DS.fontCompact)
                                .foregroundStyle(DS.ink)
                                .frame(width: 170, alignment: .leading)
                                .lineLimit(1)
                            if let pct {
                                GeometryReader { geo in
                                    ZStack(alignment: .leading) {
                                        Capsule().fill(DS.line)
                                        Capsule()
                                            .fill(DS.accent)
                                            .frame(width: geo.size.width * CGFloat(pct / 100.0))
                                    }
                                }
                                .frame(height: 5)
                                Text(String(format: "%.1f%%", pct))
                                    .font(DS.fontPriorityMeta)
                                    .monospacedDigit()
                                    .foregroundStyle(DS.ink)
                                    .frame(width: 48, alignment: .trailing)
                            } else if let sc = cand.score {
                                Spacer()
                                Text(String(format: "%.3f", sc))
                                    .font(DS.fontPriorityMeta)
                                    .monospacedDigit()
                                    .foregroundStyle(DS.muted)
                            }
                        }
                    }
                }
            }

            if let groups = job.groups, !groups.isEmpty {
                VStack(alignment: .leading, spacing: 5) {
                    Text("各组采样与预测")
                        .font(DS.fontResetTimeSemibold)
                        .foregroundStyle(DS.muted)
                    ForEach(groups) { g in
                        VStack(alignment: .leading, spacing: 3) {
                            HStack(spacing: 8) {
                                Text("第 \(g.index) 组")
                                    .font(DS.fontResetTimeSemibold)
                                    .monospacedDigit()
                                    .foregroundStyle(DS.ink)
                                Text(groupLabel(g.status))
                                    .font(DS.fontResetTime)
                                    .foregroundStyle(g.status == "failed" ? DS.bad : DS.muted)
                                if let att = g.attempts {
                                    Text("\(att) 次")
                                        .font(DS.fontResetTime)
                                        .monospacedDigit()
                                        .foregroundStyle(DS.muted)
                                }
                                Text("首字 \(g.ttft_ms.map { String(format: "%.1fs", $0 / 1000.0) } ?? "—")")
                                    .font(DS.fontResetTime)
                                    .monospacedDigit()
                                    .foregroundStyle(DS.muted)
                                Text("耗时 \(g.duration_ms.map { String(format: "%.1fs", $0 / 1000.0) } ?? "—")")
                                    .font(DS.fontResetTime)
                                    .monospacedDigit()
                                    .foregroundStyle(DS.muted)
                                if let gReport = g.report {
                                    let gPred = gReport.prediction_name ?? gReport.prediction ?? "无排名"
                                    let merged = gReport.merged_class == true ? " · 合并类别，无法区分" : ""
                                    Text(verbatim: (gPred.isEmpty ? "无排名" : gPred) + merged)
                                        .font(DS.fontResetTimeSemibold)
                                        .foregroundStyle(DS.ink)
                                        .lineLimit(1)
                                }
                                Spacer(minLength: 0)
                            }
                            if let diag = g.diagnostics {
                                Text(diagnosticsSummary(diag))
                                    .font(DS.fontUsageStat)
                                    .monospacedDigit()
                                    .foregroundStyle(DS.muted)
                                    .lineLimit(1)
                            }
                            if let gErr = g.error, !gErr.isEmpty {
                                Text(verbatim: gErr)
                                    .font(DS.fontResetTime)
                                    .foregroundStyle(DS.bad)
                            }
                        }
                        .padding(.horizontal, 8)
                        .padding(.vertical, 5)
                        .background(DS.surface)
                        .overlay(RoundedRectangle(cornerRadius: 4).strokeBorder(DS.line, lineWidth: 1))
                        .clipShape(RoundedRectangle(cornerRadius: 4))
                    }
                }
            }

            if job.tokenizer_probe == true || job.tokenizer != nil {
                tokenizerSection
            }

            if let disp = job.automatic_disposition {
                Text(dispositionSummary(disp))
                    .font(DS.fontResetTime)
                    .foregroundStyle(disp.marked == true ? DS.bad : DS.ink)
            }

            if let err = job.error, !err.isEmpty, job.status != "completed" {
                DiagnosticTextView(text: err)
            }
        }
        .padding(12)
        .background(DS.surfaceSubtle)
        .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
        .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
    }

    @ViewBuilder
    private var tokenizerSection: some View {
        let tok = job.tokenizer
        let report = tok?.report
        let statusText: String = switch tok?.status {
        case "running": "探测中"
        case "completed": "已完成"
        case "cancelled": "已停止"
        case "interrupted": "已中断"
        case "failed": "探测失败"
        default: (job.status == "running" || job.status == "queued") ? "等待数字采样完成" : "未执行"
        }
        let claimText: String = if tok?.baseline_drift == true {
            "基线漂移，无法确认一致性"
        } else {
            switch report?.claim?.status {
            case "consistent": "与请求模型一致"
            case "inconsistent": "与请求模型不一致"
            case "uncertain": "无法确认一致性"
            default: "请求模型暂无分词器参照"
            }
        }
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                Text("分词器探测")
                    .font(DS.fontResetTimeSemibold)
                    .foregroundStyle(DS.ink)
                Text(statusText)
                    .font(DS.fontResetTime)
                    .foregroundStyle(tok?.status == "failed" ? DS.bad : DS.muted)
                Spacer()
            }
            HStack(spacing: 12) {
                metricPill("请求数", value: "\(tok?.requests ?? 0) / 22")
                if let report {
                    let candName = (report.kind == "unknown" ? "未知分词器" : report.name)
                        + (report.kind == "related" ? "（相近）" : "")
                    metricPill("候选", value: candName)
                    metricPill("一致性", value: claimText)
                }
            }
            if tok?.baseline_drift == true {
                Text("两次基线的输入 Token 用量不一致，分词器结果仅供参考。")
                    .font(DS.fontResetTime)
                    .foregroundStyle(DS.bad)
            }
            if let tokErr = tok?.error, !tokErr.isEmpty {
                Text(verbatim: tokErr)
                    .font(DS.fontResetTime)
                    .foregroundStyle(DS.bad)
            }
            if let cands = report?.candidates, !cands.isEmpty {
                HStack(spacing: 8) {
                    ForEach(Array(cands.prefix(4).enumerated()), id: \.offset) { _, c in
                        Text(verbatim: "\(c.name) \(String(format: "%.2f%%", c.probability * 100.0))")
                            .font(DS.fontUsageStat)
                            .monospacedDigit()
                            .foregroundStyle(DS.muted)
                    }
                }
            }
        }
        .padding(8)
        .background(DS.surface)
        .overlay(RoundedRectangle(cornerRadius: 4).strokeBorder(DS.line, lineWidth: 1))
        .clipShape(RoundedRectangle(cornerRadius: 4))
    }

    private func diagnosticsSummary(_ diag: ModelTestGroupDiagnostics) -> String {
        let proto = diag.response_protocol ?? diag.protocol ?? "—"
        let http = diag.http_status.map(String.init) ?? "—"
        let out = diag.output_tokens.map(String.init) ?? "—"
        let maxOut = diag.max_output_tokens.map { "/\($0)" } ?? ""
        let bytes = diag.bytes ?? 0
        let end = diag.end_reason ?? "—"
        return "\(proto) · HTTP \(http) · 输出 \(out)\(maxOut) tok · \(bytes) B · \(end)"
    }

    private func dispositionSummary(_ disp: ModelTestAutomaticDisposition) -> String {
        let basisText: String = switch disp.basis {
        case "sample_family":
            "第 \((disp.group_indexes ?? []).map(String.init).joined(separator: "、")) 组跨家族证据"
        case "aggregate":
            "最终汇总"
        default:
            "单组"
        }
        if disp.kind == "recheck" {
            return "\(basisText) · \(disp.reason ?? "")"
        }
        let markText = disp.marked == true ? "标记已保存" : "标记待核对"
        let schedText = disp.schedule_verified == true ? "停调度已确认" : "停调度未确认"
        let suffix = (disp.reason?.isEmpty == false) ? "：\(disp.reason!)" : ""
        return "\(basisText) · \(markText) · \(schedText)\(suffix)"
    }

    private var statusTone: StatusDot.Tone {
        if job.status != "completed" {
            return job.status == "failed" ? .bad : .warning
        }
        let outcome = job.classification?.outcome ?? ""
        if outcome == "degraded" || outcome == "aggregate_degraded" { return .bad }
        if outcome == "normal" { return .good }
        return .warning
    }

    private var headlineText: String {
        switch job.status {
        case "queued": return "已排队"
        case "needs_confirmation": return "账号已变化，需重新确认"
        case "cancelled": return "已停止"
        case "interrupted": return "服务重启后中断"
        case "failed": return "模型测试失败"
        case "running", "retrying":
            if job.phase == "tokenizer" {
                return "数字采样完成 · 分词器探测中"
            }
            return "\(job.completed_groups ?? 0)/\(job.planned_groups ?? 3) 组采样中"
        default:
            if job.completion_reason == "automatic_degradation" {
                return (job.engine == "lm_detector" || job.verdict_mode == "relaxed")
                    ? "检测完成 · 判定降智"
                    : "首组命中降智模型"
            }
            if job.completion_reason == "confidence_99" {
                return "已提前完成"
            }
            switch job.classification?.outcome {
            case "degraded", "aggregate_degraded": return "判定存在降智特征"
            case "normal": return "未确认降智（指纹正常）"
            case "unavailable": return "样本不足，暂无法判断"
            default: return "已完成"
            }
        }
    }

    private func groupLabel(_ raw: String) -> String {
        switch raw {
        case "completed": return "完成"
        case "running": return "等待响应"
        case "receiving": return "接收中"
        case "analyzing": return "分析中"
        case "retrying": return "等待重试"
        case "queued": return "等待"
        case "skipped": return "提前结束"
        case "failed": return "失败"
        case "cancelled": return "已停止"
        default: return raw
        }
    }

    private func metricPill(_ label: String, value: String, mono: Bool = false) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(label)
                .font(DS.fontUsageStat)
                .foregroundStyle(DS.muted)
            Text(verbatim: value)
                .font(mono ? DS.fontMono11 : DS.fontCompactSemibold)
                .monospacedDigit()
                .foregroundStyle(DS.ink)
                .lineLimit(1)
        }
    }
}
