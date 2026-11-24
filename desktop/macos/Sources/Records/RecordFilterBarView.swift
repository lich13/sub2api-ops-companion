import SwiftUI

public struct RecordOptionPickerView: View {
    public enum Kind: String, Sendable {
        case users
        case accounts
        case api_keys

        public var label: String {
            switch self {
            case .users: return "用户"
            case .accounts: return "账户"
            case .api_keys: return "API 密钥"
            }
        }
    }

    public let kind: Kind
    public let active: Bool
    public let selected: RecordOption?
    public var userId: Int? = nil
    public var localOptions: [RecordOption]? = nil
    public var onOpenChange: (Bool) -> Void
    public let onSelect: (RecordOption?) -> Void

    @State private var isOpen: Bool = false
    @State private var query: String = ""
    @State private var directory = RecordOptionDirectory()
    @State private var contentHeight: CGFloat = 240

    public init(
        kind: Kind,
        active: Bool,
        selected: RecordOption?,
        userId: Int? = nil,
        localOptions: [RecordOption]? = nil,
        onOpenChange: @escaping (Bool) -> Void = { _ in },
        onSelect: @escaping (RecordOption?) -> Void
    ) {
        self.kind = kind
        self.active = active
        self.selected = selected
        self.userId = userId
        self.localOptions = localOptions
        self.onOpenChange = onOpenChange
        self.onSelect = onSelect
    }

    private var buttonTitle: String {
        if let selected {
            return "\(selected.name ?? selected.email ?? kind.label) #\(selected.id)"
        }
        return "全部\(kind.label)"
    }

    private var parsedPositiveId: Int? {
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        let digits = trimmed.hasPrefix("#") ? String(trimmed.dropFirst()) : trimmed
        guard !digits.isEmpty, digits.first != "0", digits.allSatisfy(\.isNumber), let id = Int(digits), id > 0 else {
            return nil
        }
        return id
    }

    private var displayedItems: [RecordOption] {
        if kind == .accounts {
            let needle = query.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
            let idMatch = parsedPositiveId
            return (localOptions ?? []).filter { opt in
                guard !opt.deleted else { return false }
                if needle.isEmpty { return true }
                if let idMatch, opt.id == idMatch { return true }
                return (opt.name ?? "").lowercased().contains(needle)
            }
        }
        return directory.items
    }

    public var body: some View {
        Button {
            query = ""
            isOpen.toggle()
        } label: {
            HStack(spacing: 6) {
                Text(buttonTitle)
                    .font(DS.fontCompact)
                    .foregroundStyle(selected == nil ? DS.muted : DS.ink)
                    .lineLimit(1)
                Spacer(minLength: 4)
                Image(systemName: "chevron.down")
                    .font(DS.iconFont(10, weight: .semibold))
                    .foregroundStyle(DS.muted)
            }
            .padding(.horizontal, 9)
            .frame(minWidth: 74, maxWidth: 156)
            .frame(height: DS.controlHeightDense)
            .background(DS.surface)
            .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
            .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
        }
        .buttonStyle(.linearPlain)
        .disabled(!active)
        .accessibilityLabel("\(kind.label)筛选")
        .popover(isPresented: $isOpen, arrowEdge: .bottom) {
            VStack(alignment: .leading, spacing: 8) {
                HStack(spacing: 6) {
                    Image(systemName: "magnifyingglass")
                        .font(DS.iconFont(12))
                        .foregroundStyle(DS.muted)
                    TextField(text: $query, prompt: Text(verbatim: kind == .users ? "名称、邮箱或 ID" : "名称或 ID")) { EmptyView() }
                        .textFieldStyle(.plain)
                        .font(DS.fontCompact)
                        .onSubmit {
                            if let id = parsedPositiveId, let match = displayedItems.first(where: { $0.id == id }) {
                                onSelect(match)
                                isOpen = false
                            } else if displayedItems.count == 1 {
                                onSelect(displayedItems[0])
                                isOpen = false
                            }
                        }
                }
                .padding(.horizontal, 8)
                .frame(height: 28)
                .background(DS.surface)
                .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(DS.line, lineWidth: 1))

                ScrollView {
                    VStack(alignment: .leading, spacing: 2) {
                        optionButton(nil)
                        ForEach(displayedItems) { opt in
                            optionButton(opt)
                        }
                        if directory.loading {
                            Text("正在读取…")
                                .font(DS.fontCompact)
                                .foregroundStyle(DS.muted)
                                .padding(6)
                        } else if displayedItems.isEmpty && directory.error.isEmpty {
                            Text("无匹配项")
                                .font(DS.fontCompact)
                                .foregroundStyle(DS.muted)
                                .padding(6)
                        }
                        if !directory.error.isEmpty {
                            HStack {
                                Text(directory.error)
                                    .font(DS.fontCompact)
                                    .foregroundStyle(DS.bad)
                                Button("重试") {
                                    directory.loadMoreOrRetry()
                                }
                                .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                            }
                            .padding(6)
                        }
                        if directory.cursor != nil, !directory.loading && directory.error.isEmpty {
                            Button("加载更多") {
                                directory.loadMoreOrRetry()
                            }
                            .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                            .padding(.top, 4)
                        }
                    }
                    .fixedSize(horizontal: false, vertical: true)
                    .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { contentHeight = $0 }
                }
                .frame(height: min(contentHeight, 240))
            }
            .padding(10)
            .fixedSize(horizontal: false, vertical: true)
            .frame(width: 328)
            .background(DS.surface)
            .onAppear {
                onOpenChange(true)
                search()
            }
            .onChange(of: query) { search() }
            .onChange(of: userId) { search() }
            .onDisappear {
                directory.stop()
                onOpenChange(false)
            }
            .onExitCommand {
                guard !ResponderRestorer.shouldSuppressActiveEscape() else { return }
                isOpen = false
            }
        }
    }

    private func optionButton(_ item: RecordOption?) -> some View {
        let isSelected = item?.id == selected?.id
        return Button {
            onSelect(item)
            isOpen = false
        } label: {
            HStack(spacing: 6) {
                VStack(alignment: .leading, spacing: 1) {
                    if let item {
                        HStack(spacing: 4) {
                            Text(item.name ?? item.email ?? kind.label)
                                .font(DS.fontCompactMedium)
                                .foregroundStyle(DS.ink)
                                .lineLimit(1)
                            Text("#\(item.id)")
                                .font(DS.fontPriorityMeta).monospacedDigit()
                                .foregroundStyle(DS.muted)
                        }
                        if let uid = item.user_id {
                            Text("\(item.user_name ?? item.user_email ?? "用户") #\(uid)")
                                .font(DS.fontCompact)
                                .foregroundStyle(DS.muted)
                                .lineLimit(1)
                        } else if let email = item.email, !email.isEmpty {
                            Text(verbatim: email)
                                .font(DS.fontCompact)
                                .foregroundStyle(DS.muted)
                                .lineLimit(1)
                        }
                    } else {
                        Text("全部\(kind.label)")
                            .font(DS.fontCompactMedium)
                            .foregroundStyle(DS.ink)
                    }
                }
                Spacer()
                if let status = item?.status, !status.isEmpty, status != "active" {
                    Text(status == "error" ? "异常" : "停用")
                        .font(DS.fontCompact)
                        .foregroundStyle(status == "error" ? DS.bad : DS.muted)
                        .padding(.horizontal, 5)
                        .padding(.vertical, 2)
                        .background(status == "error" ? DS.badBg : DS.surfaceSubtle)
                        .clipShape(RoundedRectangle(cornerRadius: 4))
                }
                if isSelected {
                    Image(systemName: "checkmark")
                        .font(DS.iconFont(11, weight: .semibold))
                        .foregroundStyle(DS.accent)
                }
            }
            .padding(.horizontal, 8)
            .padding(.vertical, 5)
            .background(isSelected ? DS.accentBg : DS.clear)
            .clipShape(RoundedRectangle(cornerRadius: 5))
        }
        .buttonStyle(.linearPlain)
    }

    private func search() {
        if active, kind != .accounts {
            directory.search(kind: kind, query: query, userId: userId)
        }
    }
}

@MainActor
@Observable
final class RecordOptionDirectory {
    private(set) var items: [RecordOption] = []
    private(set) var cursor: String?
    private(set) var loading = false
    private(set) var error = ""
    private var filters: [URLQueryItem] = []
    private var request: Task<Void, Never>?

    func stop() {
        request?.cancel()
        request = nil
        loading = false
    }

    func search(kind: RecordOptionPickerView.Kind, query: String, userId: Int?) {
        stop()
        items = []
        cursor = nil
        filters = [.init(name: "kind", value: kind.rawValue), .init(name: "limit", value: "50")]
        let q = query.trimmingCharacters(in: .whitespacesAndNewlines)
        if !q.isEmpty { filters.append(.init(name: "q", value: q)) }
        if kind == .api_keys, let userId { filters.append(.init(name: "user_id", value: String(userId))) }
        load(debounce: !q.isEmpty)
    }

    func loadMoreOrRetry() {
        guard !loading, cursor != nil || !error.isEmpty else { return }
        load(debounce: false)
    }

    private func load(debounce: Bool) {
        let next = cursor
        var url = URLComponents()
        url.queryItems = filters + (next.map { [.init(name: "cursor", value: $0)] } ?? [])
        let path = "/usage-record-options?\(url.percentEncodedQuery ?? "")"
        loading = true
        error = ""
        request = Task {
            do {
                if debounce { try await Task.sleep(for: .milliseconds(250)) }
                let page: RecordOptionPage = try await RustBridge.shared.api("GET", path)
                try Task.checkCancellation()
                var seen = Set(items.map(\.id))
                items.append(contentsOf: page.items.filter { !$0.deleted && seen.insert($0.id).inserted })
                cursor = page.next_cursor
            } catch {
                guard !Task.isCancelled else { return }
                self.error = error.localizedDescription
            }
            loading = false
        }
    }
}
