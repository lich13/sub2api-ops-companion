import SwiftUI

public struct MiniUsageView: View {
    public let windows: [UsageWindow]

    public init(windows: [UsageWindow]) {
        self.windows = windows
    }

    public var body: some View {
        HStack(spacing: 6) {
            ForEach(windows.prefix(2), id: \.key) { w in
                HStack(spacing: 4) {
                    Text(w.label)
                        .font(DS.fontUsageStatMedium)
                        .foregroundStyle(labelForeground(w))
                        .padding(.horizontal, 4)
                        .padding(.vertical, 1)
                        .background(labelBackground(w))
                        .clipShape(RoundedRectangle(cornerRadius: 3))

                    if let pct = w.used_percent {
                        GeometryReader { geo in
                            ZStack(alignment: .leading) {
                                Capsule().fill(DS.line)
                                Capsule()
                                    .fill(barFill(pct: pct, window: w))
                                    .frame(width: max(0, min(geo.size.width, geo.size.width * CGFloat(pct / 100.0))))
                            }
                        }
                        .frame(width: 24, height: 4)

                        Text("\(Int(pct.rounded()))%")
                            .font(DS.fontResetTime)
                            .monospacedDigit()
                            .foregroundStyle(pct >= 90 ? DS.bad : DS.ink)
                    } else {
                        Text("—")
                            .font(DS.fontResetTime)
                            .foregroundStyle(DS.muted)
                    }
                }
            }
        }
    }

    private func labelForeground(_ w: UsageWindow) -> Color {
        w.color == "emerald" ? DS.windowEmerald : DS.windowIndigo
    }

    private func labelBackground(_ w: UsageWindow) -> Color {
        w.color == "emerald" ? DS.windowEmeraldBg : DS.windowIndigoBg
    }

    private func barFill(pct: Double, window: UsageWindow) -> Color {
        if pct >= 90 { return DS.bad }
        if pct >= 75 { return DS.warn }
        return window.color == "emerald" ? DS.windowEmerald : DS.windowIndigo
    }
}
