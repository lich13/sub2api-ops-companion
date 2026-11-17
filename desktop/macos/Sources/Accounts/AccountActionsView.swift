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
    public let command: AccountDialogAction

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
            let marked = account.degradation_mark?.marked == true
            list.append(AccountActionEntry(
                id: "degradation",
                label: marked ? "取消标记" : "标记降智",
                icon: marked ? "bell" : "bell.slash",
                destructive: false,
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
            icon: "bolt.heart",
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
    public let disabled: Bool
    public let onTrigger: (AccountDialogAction) -> Void

    private let columns = [
        GridItem(.flexible(minimum: 94), spacing: 6),
        GridItem(.flexible(minimum: 94), spacing: 6),
    ]

    public init(account: Account, disabled: Bool = false, onTrigger: @escaping (AccountDialogAction) -> Void) {
        self.account = account
        self.disabled = disabled
        self.onTrigger = onTrigger
    }

    public var body: some View {
        let items = AccountActionEntry.entries(for: account)
        LazyVGrid(columns: columns, alignment: .leading, spacing: 5) {
            ForEach(items) { item in
                Button {
                    onTrigger(item.command)
                } label: {
                    HStack(spacing: 5) {
                        Image(systemName: item.icon)
                            .font(DS.iconFont(11, weight: .medium))
                            .frame(width: 12, alignment: .center)
                        Text(item.label)
                            .font(DS.fontCompact)
                            .lineLimit(1)
                        Spacer(minLength: 0)
                    }
                    .foregroundStyle(item.destructive ? DS.bad : DS.ink)
                    .padding(.horizontal, 8)
                    .frame(height: 26)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(DS.surface)
                    .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(DS.line, lineWidth: 1))
                    .clipShape(RoundedRectangle(cornerRadius: 6))
                }
                .buttonStyle(.plain)
                .disabled(disabled)
                .opacity(disabled ? 0.45 : 1.0)
            }
        }
        .frame(width: 214)
    }
}
