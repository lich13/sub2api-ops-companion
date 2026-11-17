import SwiftUI

public enum AccountDialogAction: Sendable, Equatable {
    case modelTest(Account)
    case toggleDegradation(Account)
    case modelDetection(Account)
    case connectionTest(Account)
    case keepalive(Account)
    case reauthorize(Account)
    case recoverState(Account)
    case applyTemplate(Account)
    case settingsWhitelist(Account)
    case settingsMapping(Account)
    case settingsProxy(Account)
    case deleteAccount(Account)
}

public struct AccountActionEntry: Identifiable, Sendable {
    public let id: String
    public let label: String
    public let icon: String
    public let destructive: Bool
    public let itemDisabled: Bool
    public let command: AccountDialogAction

    public init(
        id: String,
        label: String,
        icon: String,
        destructive: Bool,
        itemDisabled: Bool = false,
        command: AccountDialogAction
    ) {
        self.id = id
        self.label = label
        self.icon = icon
        self.destructive = destructive
        self.itemDisabled = itemDisabled
        self.command = command
    }

    public static func entries(for account: Account) -> [AccountActionEntry] {
        let isOpenAI = account.platform == "openai" && ["oauth", "apikey"].contains(account.type)
        let hasDetection = isOpenAI || (account.type == "apikey" && ["anthropic", "grok", "deepseek"].contains(account.platform))
        var list: [AccountActionEntry] = []

        if hasDetection {
            list.append(AccountActionEntry(
                id: "model_test",
                label: "模型测试",
                icon: "waveform.path.ecg",
                destructive: false,
                command: .modelTest(account)
            ))
            let mark = account.degradation_mark
            let marked = mark?.marked == true
            let hasError = (mark?.error ?? "").isEmpty == false
            list.append(AccountActionEntry(
                id: "degradation",
                label: hasError ? "标记读取失败" : (marked ? "取消标记" : "标记降智"),
                icon: "bell.slash",
                destructive: false,
                itemDisabled: (mark?.version ?? "").isEmpty,
                command: .toggleDegradation(account)
            ))
            list.append(AccountActionEntry(
                id: "model_detection",
                label: "定时检测",
                icon: "clock",
                destructive: false,
                command: .modelDetection(account)
            ))
        }

        list.append(AccountActionEntry(
            id: "test",
            label: "测试连接",
            icon: "checkmark.circle",
            destructive: false,
            command: .connectionTest(account)
        ))
        list.append(AccountActionEntry(
            id: "keepalive",
            label: "账号保活",
            icon: "waveform.path.ecg",
            destructive: false,
            command: .keepalive(account)
        ))
        if account.platform == "openai" && account.type == "oauth" {
            list.append(AccountActionEntry(
                id: "reauth",
                label: "重新授权",
                icon: "link",
                destructive: false,
                command: .reauthorize(account)
            ))
        }
        if account.recoverable == true {
            list.append(AccountActionEntry(
                id: "recover",
                label: "恢复状态",
                icon: "arrow.counterclockwise",
                destructive: false,
                command: .recoverState(account)
            ))
        }
        if isOpenAI {
            list.append(AccountActionEntry(
                id: "template",
                label: "应用模板",
                icon: "square.grid.2x2",
                destructive: false,
                command: .applyTemplate(account)
            ))
        }
        list.append(AccountActionEntry(
            id: "whitelist",
            label: "模型白名单",
            icon: "line.3.horizontal.decrease",
            destructive: false,
            command: .settingsWhitelist(account)
        ))
        list.append(AccountActionEntry(
            id: "mapping",
            label: "模型映射",
            icon: "arrow.left.arrow.right",
            destructive: false,
            command: .settingsMapping(account)
        ))
        list.append(AccountActionEntry(
            id: "proxy",
            label: "选择代理",
            icon: "network",
            destructive: false,
            command: .settingsProxy(account)
        ))
        list.append(AccountActionEntry(
            id: "delete",
            label: "删除",
            icon: "trash",
            destructive: true,
            command: .deleteAccount(account)
        ))
        return list
    }
}

public struct AccountActionsView: View {
    public let account: Account
    public var totalWidth: CGFloat
    public let disabled: Bool
    public let onTrigger: (AccountDialogAction) -> Void

    public init(
        account: Account,
        totalWidth: CGFloat = 212,
        disabled: Bool = false,
        onTrigger: @escaping (AccountDialogAction) -> Void
    ) {
        self.account = account
        self.totalWidth = totalWidth
        self.disabled = disabled
        self.onTrigger = onTrigger
    }

    public var body: some View {
        let items = AccountActionEntry.entries(for: account)
        // Mirrors `.account-actions`: repeat(auto-fit, minmax(96px, 1fr)); gap 4px 6px.
        let gap: CGFloat = 6
        let fit = max(1, Int(floor((totalWidth + gap) / (96 + gap))))
        let colCount = max(1, min(items.count, fit))
        let buttonWidth = max(96, floor((totalWidth - gap * CGFloat(colCount - 1)) / CGFloat(colCount)))
        let rowCount = (items.count + colCount - 1) / colCount

        VStack(alignment: .leading, spacing: 4) {
            ForEach(0..<rowCount, id: \.self) { rowIdx in
                HStack(spacing: gap) {
                    ForEach(0..<colCount, id: \.self) { colIdx in
                        let itemIdx = rowIdx * colCount + colIdx
                        if itemIdx < items.count {
                            ActionGridButton(
                                item: items[itemIdx],
                                width: buttonWidth,
                                disabled: disabled || items[itemIdx].itemDisabled,
                                onTrigger: onTrigger
                            )
                        } else {
                            DS.clear.frame(width: buttonWidth, height: 24)
                        }
                    }
                }
            }
        }
        .frame(width: totalWidth, alignment: .leading)
    }

    private struct ActionGridButton: View {
        let item: AccountActionEntry
        let width: CGFloat
        let disabled: Bool
        let onTrigger: (AccountDialogAction) -> Void
        @State private var hovered: Bool = false

        var body: some View {
            Button {
                onTrigger(item.command)
            } label: {
                HStack(spacing: 4) {
                    Image(systemName: item.icon)
                        .font(DS.iconFont(10, weight: .medium))
                        .frame(width: 12, alignment: .center)
                    Text(item.label)
                        .font(DS.fontPriorityMeta)
                        .lineLimit(1)
                    Spacer(minLength: 0)
                }
                .foregroundStyle(item.destructive ? DS.bad : DS.ink)
                .padding(.horizontal, 6)
                .frame(width: width, height: 24, alignment: .leading)
                .background(hovered && !disabled ? (item.destructive ? DS.badBg : DS.hover) : DS.surface)
                .overlay(
                    RoundedRectangle(cornerRadius: 6)
                        .strokeBorder(hovered && !disabled ? (item.destructive ? DS.bad : DS.lineStrong) : DS.line, lineWidth: 1)
                )
                .clipShape(RoundedRectangle(cornerRadius: 6))
                .contentShape(Rectangle())
            }
            .buttonStyle(.linearPlain)
            .disabled(disabled)
            .compositingGroup()
            .opacity(disabled ? 0.45 : 1.0)
            .onHover { hovered = $0 }
        }
    }
}
