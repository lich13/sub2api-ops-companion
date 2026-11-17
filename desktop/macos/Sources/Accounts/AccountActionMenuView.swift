import SwiftUI

public struct AccountActionMenuView: View {
    public let account: Account
    public var disabled: Bool
    public let onTrigger: (AccountDialogAction) -> Void

    @State private var isOpen: Bool = false

    public init(account: Account, disabled: Bool = false, onTrigger: @escaping (AccountDialogAction) -> Void) {
        self.account = account
        self.disabled = disabled
        self.onTrigger = onTrigger
    }

    public var body: some View {
        Button {
            isOpen.toggle()
        } label: {
            Image(systemName: "ellipsis")
                .font(DS.iconFont(13, weight: .semibold))
                .foregroundStyle(DS.muted)
                .frame(width: 26, height: 26)
                .background(isOpen ? DS.hover : DS.clear)
                .clipShape(RoundedRectangle(cornerRadius: 6))
        }
        .buttonStyle(.linearPlain)
        .disabled(disabled)
        .accessibilityLabel("\(account.name)操作")
        .popover(isPresented: $isOpen, arrowEdge: .bottom) {
            VStack(alignment: .leading, spacing: 2) {
                ForEach(AccountActionEntry.entries(for: account)) { entry in
                    Button {
                        isOpen = false
                        onTrigger(entry.command)
                    } label: {
                        HStack(spacing: 8) {
                            Image(systemName: entry.icon)
                                .font(DS.iconFont(11, weight: .medium))
                                .frame(width: 14, alignment: .center)
                            Text(entry.label)
                                .font(DS.fontCompact)
                            Spacer(minLength: 8)
                        }
                        .foregroundStyle(entry.destructive ? DS.bad : DS.ink)
                        .padding(.horizontal, 10)
                        .padding(.vertical, 6)
                        .frame(width: 152, alignment: .leading)
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.linearPlain)
                }
            }
            .padding(6)
            .background(DS.surface)
            .onExitCommand {
                isOpen = false
            }
        }
    }
}
