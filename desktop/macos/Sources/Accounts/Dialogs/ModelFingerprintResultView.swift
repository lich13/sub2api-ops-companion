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
                if let created = job.completed_at ?? job.created_at {
                    Text(BeijingTime.fullTime(created))
                        .font(DS.fontResetTime)
                        .monospacedDigit()
                        .foregroundStyle(DS.muted)
                }
            }

            if let report = job.report {
                HStack(spacing: 12) {
                    metricPill("检测源", value: ModelTestEngine.name(for: report.engine ?? job.engine))
                    if let pred = report.prediction_name ?? report.prediction, !pred.isEmpty {
                        metricPill("汇总预测", value: pred, mono: true)
                    }
                    if let prob = report.probability {
                        metricPill("匹配度", value: String(format: "%.1f%%", prob * 100.0))
                    }
                    if let first = report.first_prediction, !first.isEmpty {
                        metricPill("首组", value: first, mono: true)
                    }
                }

                if let reason = report.reason, !reason.isEmpty {
                    Text(reason)
                        .font(DS.fontResetTime)
                        .foregroundStyle(DS.warn)
                }

                if let candidates = report.candidates, !candidates.isEmpty {
                    VStack(alignment: .leading, spacing: 5) {
                        Text("候选模型分布")
                            .font(DS.fontResetTimeSemibold)
                            .foregroundStyle(DS.muted)
                        ForEach(Array(candidates.prefix(5).enumerated()), id: \.offset) { _, cand in
                            let pct = cand.probability.map { min(100, max(0, $0 * 100.0)) }
                            HStack(spacing: 8) {
                                Text(verbatim: cand.display_name ?? cand.model)
                                    .font(DS.fontMono11)
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
                                        .font(DS.fontMono11)
                                        .monospacedDigit()
                                        .foregroundStyle(DS.ink)
                                        .frame(width: 48, alignment: .trailing)
                                } else if let sc = cand.score {
                                    Spacer()
                                    Text(String(format: "%.3f", sc))
                                        .font(DS.fontMono11)
                                        .monospacedDigit()
                                        .foregroundStyle(DS.muted)
                                }
                            }
                        }
                    }
                }
            }

            if let groups = job.groups, !groups.isEmpty {
                HStack(spacing: 6) {
                    ForEach(groups) { g in
                        Text("第\(g.index)组：\(groupLabel(g.status))\(g.duration_ms.map { String(format: " %.1fs", $0 / 1000.0) } ?? "")")
                            .font(DS.fontResetTime)
                            .monospacedDigit()
                            .foregroundStyle(DS.muted)
                            .padding(.horizontal, 6)
                            .padding(.vertical, 2)
                            .background(DS.surface)
                            .overlay(RoundedRectangle(cornerRadius: 4).strokeBorder(DS.line, lineWidth: 1))
                            .clipShape(RoundedRectangle(cornerRadius: 4))
                    }
                }
            }

            if let err = job.error ?? job.reason, !err.isEmpty, job.status != "completed" {
                DiagnosticTextView(text: err)
            }
        }
        .padding(12)
        .background(DS.surfaceSubtle)
        .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
        .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
    }

    private var statusTone: StatusDot.Tone {
        if job.status != "completed" {
            return job.status == "failed" ? .bad : .warning
        }
        let outcome = job.report?.outcome ?? ""
        if outcome == "degraded" || outcome == "aggregate_degraded" { return .bad }
        if outcome == "normal" { return .good }
        return .warning
    }

    private var headlineText: String {
        switch job.status {
        case "queued": return "排队等待采样…"
        case "running": return "正在采样与计算模型指纹…"
        case "cancelled": return "测试已取消"
        case "failed": return "模型测试失败"
        default:
            switch job.report?.outcome {
            case "degraded", "aggregate_degraded": return "判定存在降智特征"
            case "normal": return "未确认降智（指纹正常）"
            case "unavailable": return "样本不足，暂无法判断"
            default: return "测试已完成"
            }
        }
    }

    private func groupLabel(_ raw: String) -> String {
        switch raw {
        case "completed": return "完成"
        case "running": return "采样中"
        case "queued": return "排队"
        case "skipped": return "跳过"
        case "failed": return "失败"
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
