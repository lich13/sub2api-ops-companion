import SwiftUI

public struct QuotaRefreshBarView: View {
    public let job: QuotaRefreshHeader?

    public init(job: QuotaRefreshHeader?) {
        self.job = job
    }

    public var body: some View {
        let total = job?.total ?? 0
        let completed = job?.completed ?? 0
        if let job, job.status != "idle", total > 0 {
            let active = job.status == "running" || job.status == "queued"
            let ratio = min(1.0, max(0.0, Double(completed) / Double(max(1, total))))
            HStack(spacing: 10) {
                if active {
                    ProgressView().controlSize(.small)
                } else {
                    StatusDot(job.status == "completed" ? .good : .warning)
                }
                Text(active ? "正在刷新全部用量" : "用量刷新已完成")
                    .font(DS.fontCompactMedium)
                    .foregroundStyle(DS.ink)

                GeometryReader { geo in
                    ZStack(alignment: .leading) {
                        Capsule().fill(DS.line)
                        Capsule()
                            .fill(DS.accent)
                            .frame(width: geo.size.width * CGFloat(ratio))
                    }
                }
                .frame(width: 120, height: 6)

                Text("\(completed) / \(total)")
                    .font(DS.fontCompact)
                    .monospacedDigit()
                    .foregroundStyle(DS.muted)

                Spacer()
                if let doneAt = job.completed_at, !doneAt.isEmpty {
                    Text(BeijingTime.fullTime(doneAt))
                        .font(DS.fontResetTime)
                        .monospacedDigit()
                        .foregroundStyle(DS.muted)
                }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .background(DS.surface)
            .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
            .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
        }
    }
}
