import SwiftUI

/// Matches `MiniUsage` in `desktop/src/AccountControls.tsx` and `.compact .mini-usage` in `style.css`.
public struct MiniUsageView: View {
    public let windows: [UsageWindow]

    public init(windows: [UsageWindow]) {
        self.windows = windows.filter { ["5h", "7d", "30d", "24h"].contains($0.label) }
    }

    public var body: some View {
        if !windows.isEmpty {
            HStack(spacing: 8) {
                ForEach(windows, id: \.key) { w in
                    let suffix = w.status == "stale" ? "*" : (w.status == "error" ? "!" : "")
                    let tipSuffix = w.status == "stale" ? " · 历史快照" : (w.status == "error" ? " · 查询异常" : "")
                    HStack(spacing: 4) {
                        Text(w.label)
                            .font(DS.fontResetTimeMedium)
                            .foregroundStyle(DS.muted)

                        ZStack(alignment: .leading) {
                            Capsule()
                                .fill(DS.line)
                                .frame(width: 24, height: 4)
                            if let pct = w.used_percent {
                                let clamped = max(0, min(100, pct))
                                Capsule()
                                    .fill(barFill(pct: clamped))
                                    .frame(width: 24 * CGFloat(clamped / 100.0), height: 4)
                            }
                        }

                        Text(w.used_percent == nil ? "未知\(suffix)" : "\(Int(w.used_percent!.rounded()))%\(suffix)")
                            .font(DS.fontResetTime)
                            .monospacedDigit()
                            .foregroundStyle(DS.muted)
                    }
                    .help("采集 \(BeijingTime.fullTime(w.observed_at)) · 重置 \(BeijingTime.fullTime(w.reset_at))\(tipSuffix)")
                }
            }
            .lineLimit(1)
        }
    }

    private func barFill(pct: Double) -> Color {
        if pct >= 90 { return DS.usageFillRed }
        if pct >= 75 { return DS.usageFillAmber }
        return DS.usageFillGreen
    }
}
