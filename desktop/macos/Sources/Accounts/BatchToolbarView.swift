import SwiftUI

/// Matches `.selection-bar` in `desktop/src/main.tsx` & `AccountManagement.tsx`.
public struct BatchToolbarView: View {
    public let selectedAccounts: [Account]
    public let online: Bool
    public let busy: Bool
    public let onBatchTest: () -> Void
    public let onBatchRecover: () -> Void
    public let onBatchCredits: () -> Void
    public let onBatchProxy: () -> Void
    public let onBatchDelete: () -> Void
    public let onClear: () -> Void

    public init(
        selectedAccounts: [Account],
        online: Bool,
        busy: Bool,
        onBatchTest: @escaping () -> Void,
        onBatchRecover: @escaping () -> Void,
        onBatchCredits: @escaping () -> Void,
        onBatchProxy: @escaping () -> Void,
        onBatchDelete: @escaping () -> Void,
        onClear: @escaping () -> Void
    ) {
        self.selectedAccounts = selectedAccounts
        self.online = online
        self.busy = busy
        self.onBatchTest = onBatchTest
        self.onBatchRecover = onBatchRecover
        self.onBatchCredits = onBatchCredits
        self.onBatchProxy = onBatchProxy
        self.onBatchDelete = onBatchDelete
        self.onClear = onClear
    }

    public static func isResetCreditEligible(_ account: Account) -> Bool {
        (account.usage?.actions.contains("reset_quota") == true)
            && ((account.usage?.reset_credits?.available ?? 0) > 0)
    }

    private var recoverableSelected: [Account] {
        selectedAccounts.filter { $0.recoverable == true }
    }

    private var creditEligibleSelected: [Account] {
        selectedAccounts.filter(Self.isResetCreditEligible)
    }

    public var body: some View {
        HStack(spacing: 8) {
            HStack(spacing: 8) {
                Text("已选择 \(selectedAccounts.count) 个账号")
                    .font(DS.fontPriorityMeta)
                    .monospacedDigit()
                    .foregroundStyle(DS.ink)

                Button("取消选择", action: onClear)
                    .font(DS.fontPriorityMeta)
                    .foregroundStyle(DS.muted)
                    .buttonStyle(.plain)
                    .disabled(busy)
            }

            Spacer(minLength: 8)

            HStack(spacing: 6) {
                Button(action: onBatchTest) {
                    HStack(spacing: 4) {
                        Image(systemName: "checkmark.circle")
                            .font(DS.iconFont(11, weight: .medium))
                        Text("测试连接")
                    }
                }
                .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                .disabled(!online || busy)

                if !recoverableSelected.isEmpty {
                    Button(action: onBatchRecover) {
                        HStack(spacing: 4) {
                            Image(systemName: "arrow.counterclockwise")
                                .font(DS.iconFont(11, weight: .medium))
                            Text("恢复状态")
                        }
                    }
                    .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                    .disabled(!online || busy)
                }

                if !creditEligibleSelected.isEmpty {
                    Button(action: onBatchCredits) {
                        HStack(spacing: 4) {
                            Image(systemName: "ticket")
                                .font(DS.iconFont(11, weight: .medium))
                            Text("使用重置卡")
                        }
                    }
                    .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                    .disabled(!online || busy)
                }

                Button(action: onBatchProxy) {
                    HStack(spacing: 4) {
                        Image(systemName: "network")
                            .font(DS.iconFont(11, weight: .medium))
                        Text("选择代理")
                    }
                }
                .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                .disabled(!online || busy)

                Button(action: onBatchDelete) {
                    HStack(spacing: 4) {
                        Image(systemName: "trash")
                            .font(DS.iconFont(11, weight: .medium))
                        Text("删除")
                    }
                }
                .buttonStyle(LinearButtonStyle(variant: .danger, compact: true))
                .disabled(!online || busy)
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(DS.surface)
        .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
        .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
    }
}
