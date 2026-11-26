import SwiftUI

public struct AccountActionMenuView: View {
    public let account: Account
    public var disabled: Bool
    public let onTrigger: @MainActor @Sendable (AccountDialogAction) -> Void

    public init(
        account: Account,
        disabled: Bool = false,
        onTrigger: @escaping @MainActor @Sendable (AccountDialogAction) -> Void
    ) {
        self.account = account
        self.disabled = disabled
        self.onTrigger = onTrigger
    }

    public var body: some View {
        let handler = onTrigger
        LinearMenuIconTrigger(
            systemName: "ellipsis",
            label: "\(account.name)操作",
            boxSize: 26,
            disabled: disabled,
            entries: AccountActionEntry.entries(for: account).map { entry in
                let command = entry.command
                return LinearMenuEntry(
                    id: entry.id,
                    title: entry.label,
                    icon: entry.icon,
                    destructive: entry.destructive,
                    disabled: entry.itemDisabled
                ) {
                    handler(command)
                }
            }
        )
    }
}
