import SwiftUI

public struct DegradationBadgeView: View {
    public let account: Account
    public var compact: Bool

    public init(account: Account, compact: Bool = false) {
        self.account = account
        self.compact = compact
    }

    public var body: some View {
        if account.degradation_mark?.marked == true {
            if compact {
                Image(systemName: "bell.slash.fill")
                    .font(DS.iconFont(10, weight: .semibold))
                    .foregroundStyle(DS.warn)
                    .frame(width: 16, height: 16)
                    .background(DS.warnBg)
                    .clipShape(RoundedRectangle(cornerRadius: 4))
                    .help(tooltipText)
                    .accessibilityLabel("已标记降智")
            } else {
                HStack(spacing: 3) {
                    Image(systemName: "bell.slash.fill")
                        .font(DS.iconFont(9, weight: .semibold))
                    Text("降智")
                        .font(DS.fontResetTimeSemibold)
                }
                .foregroundStyle(DS.warn)
                .padding(.horizontal, 5)
                .padding(.vertical, 2)
                .background(DS.warnBg)
                .clipShape(RoundedRectangle(cornerRadius: 4))
                .help(tooltipText)
            }
        }
    }

    private var tooltipText: String {
        if let markedAt = account.degradation_mark?.marked_at, !markedAt.isEmpty {
            return "标记降智于 \(BeijingTime.fullTime(markedAt))"
        }
        return "已标记降智"
    }
}

public enum DegradationMarkActions {
    public static func supportsDegradationMark(_ account: Account) -> Bool {
        account.platform == "openai" && ["oauth", "apikey"].contains(account.type)
    }

    @MainActor
    public static func toggle(account: Account, store: AppStore) async {
        guard store.state.online, let expectedVersion = account.degradation_mark?.version, !expectedVersion.isEmpty else { return }
        let targetMarked = !(account.degradation_mark?.marked == true)
        do {
            let res = try await store.operations.accountOperation(
                account: account,
                action: "degradation_mark",
                payload: ["marked": .bool(targetMarked)],
                expectedVersion: expectedVersion
            )
            if res["verified"]?.boolValue == false {
                throw RustBridgeError("降智标记保存未确认")
            }
            store.refresh()
            store.showToast("\(account.name)：\(targetMarked ? "已标记降智" : "已取消降智标记")")
        } catch {
            store.report(error)
        }
    }
}
