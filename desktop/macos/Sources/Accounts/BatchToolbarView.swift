import SwiftUI

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

    private var recoverableSelected: [Account] {
        selectedAccounts.filter { $0.recoverable == true }
    }

    private var creditSelected: [Account] {
        selectedAccounts.filter { $0.platform == "openai" && $0.type == "oauth" }
    }

    public var body: some View {
        HStack(spacing: 8) {
            Text("已选 \(selectedAccounts.count) 个账号")
                .font(DS.fontCompactSemibold)
                .monospacedDigit()
                .foregroundStyle(DS.accent)

            Button("批量测活", action: onBatchTest)
                .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                .disabled(!online || busy)

            if !recoverableSelected.isEmpty {
                Button("恢复状态 (\(recoverableSelected.count))", action: onBatchRecover)
                    .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                    .disabled(!online || busy)
            }

            if !creditSelected.isEmpty {
                Button("查询次数 (\(creditSelected.count))", action: onBatchCredits)
                    .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                    .disabled(!online || busy)
            }

            Button("修改代理", action: onBatchProxy)
                .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                .disabled(!online || busy)

            Button("批量删除", action: onBatchDelete)
                .buttonStyle(LinearButtonStyle(variant: .danger, compact: true))
                .disabled(!online || busy)

            Spacer()

            Button("取消选择", action: onClear)
                .buttonStyle(LinearButtonStyle(variant: .ghost, compact: true))
                .disabled(busy)
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 8)
        .background(DS.accentBg)
        .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.accent, lineWidth: 1))
        .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
    }
}
