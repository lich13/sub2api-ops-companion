import SwiftUI

public enum AccountModalDestination: Equatable, Sendable {
    case addAccount
    case keyQuotaSettings(Account?)
    case templates(Account?)
    case testConnection(Account)
    case modelTest(Account)
    case modelDetection(Account)
    case keepalive(Account)
    case reauthorize(Account)
    case accountSettings(Account, AccountSettingsTab)
    case deleteAccounts([Account])
    case batchCredits([Account])
    case batchProxy([Account])
}

public struct AccountModalHostView: View {
    @Bindable public var store: AppStore
    public let destination: AccountModalDestination
    public let onDeletedIds: (Set<Int>) -> Void
    public let onClose: () -> Void

    public init(
        store: AppStore,
        destination: AccountModalDestination,
        onDeletedIds: @escaping (Set<Int>) -> Void = { _ in },
        onClose: @escaping () -> Void
    ) {
        self.store = store
        self.destination = destination
        self.onDeletedIds = onDeletedIds
        self.onClose = onClose
    }

    public var body: some View {
        ZStack {
            DS.backdrop
                .ignoresSafeArea()
                .onTapGesture {
                    // The creation form guards unsaved input and in-flight submissions itself.
                    if case .addAccount = destination { return }
                    onClose()
                }

            switch destination {
            case .addAccount:
                AccountCreationSheet(store: store, onClose: onClose)
            case .keyQuotaSettings(let target):
                KeyQuotaSettingsSheet(store: store, targetAccount: target, onClose: onClose)
            case .templates(let target):
                AccountTemplatesSheet(store: store, targetAccount: target, onClose: onClose)
            case .testConnection(let acc):
                TestDialogSheet(account: acc, store: store, onClose: onClose)
            case .modelTest(let acc):
                ModelTestSheet(account: acc, store: store, onClose: onClose)
            case .modelDetection(let acc):
                ModelDetectionSheet(account: acc, store: store, onClose: onClose)
            case .keepalive(let acc):
                KeepaliveSheet(account: acc, store: store, onClose: onClose)
            case .reauthorize(let acc):
                ReauthorizeAccountSheet(account: acc, store: store, onClose: onClose)
            case .accountSettings(let acc, let tab):
                AccountSettingsSheet(account: acc, store: store, initialTab: tab, onClose: onClose)
            case .deleteAccounts(let accs):
                DeleteAccountsSheet(accounts: accs, store: store, onDeleted: onDeletedIds, onClose: onClose)
            case .batchCredits(let accs):
                ResetCreditsSheet(accounts: accs, store: store, onClose: onClose)
            case .batchProxy(let accs):
                ProxyBatchSheet(accounts: accs, store: store, onClose: onClose)
            }
        }
    }
}
