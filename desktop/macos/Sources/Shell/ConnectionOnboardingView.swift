import SwiftUI

/// Matches `Connection` form in `desktop/src/main.tsx` (onboarding, settings embed, quick panel, and modal).
public struct ConnectionOnboardingView: View {
    @Bindable public var store: AppStore
    public var initialUrl: String?
    public var initialName: String?
    public var requireKey: Bool
    public var showNameField: Bool
    public var embeddedInSettings: Bool
    public var compactQuick: Bool
    public var onConnected: (() -> Void)?
    public var onCancel: (() -> Void)?

    @State private var url: String = ""
    @State private var name: String = ""
    @State private var nameOpen: Bool = false
    @State private var apiKey: String = ""
    @State private var busy: Bool = false
    @FocusState private var focusedField: Field?

    private enum Field: Hashable {
        case url
        case key
        case name
    }

    public init(
        store: AppStore,
        initialUrl: String? = nil,
        initialName: String? = nil,
        requireKey: Bool = false,
        showNameField: Bool = false,
        embeddedInSettings: Bool = false,
        compactQuick: Bool = false,
        onConnected: (() -> Void)? = nil,
        onCancel: (() -> Void)? = nil
    ) {
        self.store = store
        self.initialUrl = initialUrl
        self.initialName = initialName
        self.requireKey = requireKey
        self.showNameField = showNameField
        self.embeddedInSettings = embeddedInSettings
        self.compactQuick = compactQuick
        self.onConnected = onConnected
        self.onCancel = onCancel
    }

    private var defaultUrl: String {
        initialUrl ?? store.state.preferences.base_url
    }

    private var defaultName: String {
        if let initialName { return initialName }
        return store.savedConnections.first(where: { $0.base_url == defaultUrl })?.name ?? ""
    }

    private var isEditingSaved: Bool {
        let normCurrent = url.trimmingCharacters(in: .whitespacesAndNewlines)
            .replacingOccurrences(of: "/+$", with: "", options: .regularExpression)
        let normInitial = (initialUrl ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            .replacingOccurrences(of: "/+$", with: "", options: .regularExpression)
        return showNameField && !normInitial.isEmpty && !requireKey && normCurrent == normInitial
    }

    private var headingTitle: String {
        if initialUrl == "" && showNameField {
            return "新增 Sub2API 实例"
        }
        return store.state.connected ? "更换或添加连接" : "连接你的运维服务"
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: compactQuick ? 12 : 16) {
            if !embeddedInSettings {
                HStack(alignment: .top) {
                    ZStack {
                        RoundedRectangle(cornerRadius: 14)
                            .fill(DS.accentBg)
                            .frame(width: compactQuick ? 42 : 52, height: compactQuick ? 42 : 52)
                        Image(systemName: "powerplug")
                            .font(DS.iconFont(compactQuick ? 18 : 22, weight: .semibold))
                            .foregroundStyle(DS.accent)
                    }
                    Spacer()
                    if let onCancel {
                        LinearIconButton(systemName: "xmark", label: "关闭连接窗口", action: onCancel)
                    }
                }
            }

            Text(headingTitle)
                .font(embeddedInSettings ? DS.fontCardHeading : (compactQuick ? DS.fontDialogTitle : DS.fontOnboardingTitle))
                .foregroundStyle(DS.ink)

            if embeddedInSettings {
                Rectangle().fill(DS.line).frame(height: 1)
            }

            if !store.state.connected && !store.savedConnections.isEmpty {
                VStack(alignment: .leading, spacing: 8) {
                    Text("已保存实例")
                        .font(DS.fontBadge)
                        .foregroundStyle(DS.muted)

                    VStack(spacing: 6) {
                        ForEach(store.savedConnections, id: \.base_url) { item in
                            HStack(spacing: 8) {
                                Button {
                                    onCancel?()
                                    Task { await store.switchConnection(item) }
                                } label: {
                                    HStack(spacing: 6) {
                                        Image(systemName: "powerplug")
                                            .font(DS.iconFont(11, weight: .medium))
                                            .foregroundStyle(DS.accent)
                                        Text(item.name)
                                            .font(DS.fontCompactSemibold)
                                            .foregroundStyle(DS.ink)
                                            .lineLimit(1)
                                        Text(verbatim: item.base_url)
                                            .font(DS.fontPriorityMeta)
                                            .foregroundStyle(DS.muted)
                                            .lineLimit(1)
                                            .truncationMode(.middle)
                                        Spacer(minLength: 4)
                                    }
                                    .contentShape(Rectangle())
                                }
                                .buttonStyle(.plain)
                                .disabled(busy || store.switchingUrl != nil)

                                Button("填入") {
                                    url = item.base_url
                                    name = item.name
                                    nameOpen = true
                                }
                                .font(DS.fontPriorityMeta)
                                .foregroundStyle(DS.muted)
                                .buttonStyle(.plain)
                            }
                            .padding(.horizontal, 10)
                            .padding(.vertical, 6)
                            .background(DS.surface)
                            .overlay(Capsule().strokeBorder(DS.line, lineWidth: 1))
                            .clipShape(Capsule())
                        }
                    }
                }
                .padding(10)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(DS.surfaceSubtle)
                .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(DS.line, lineWidth: 1))
                .clipShape(RoundedRectangle(cornerRadius: 8))
            }

            VStack(alignment: .leading, spacing: 7) {
                Text("服务地址")
                    .font(DS.fontCompactMedium)
                    .foregroundStyle(DS.ink)
                TextField(text: $url, prompt: Text(verbatim: "https://example.invalid/sub2ops").foregroundStyle(DS.placeholder)) { EmptyView() }
                    .textFieldStyle(.plain)
                    .font(DS.fontBody)
                    .foregroundStyle(DS.ink)
                    .focused($focusedField, equals: .url)
                    .padding(.horizontal, 10)
                    .frame(height: DS.controlHeightField)
                    .background(DS.surface)
                    .overlay(
                        RoundedRectangle(cornerRadius: DS.cornerRadius)
                            .strokeBorder(focusedField == .url ? DS.accent : DS.line, lineWidth: 1)
                    )
                    .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
            }

            VStack(alignment: .leading, spacing: 7) {
                Text("管理员 API Key")
                    .font(DS.fontCompactMedium)
                    .foregroundStyle(DS.ink)
                SecureField(text: $apiKey, prompt: Text(verbatim: isEditingSaved ? "留空则仅保存实例名称" : "粘贴已有的管理员 Key").foregroundStyle(DS.placeholder)) { EmptyView() }
                    .textFieldStyle(.plain)
                    .font(DS.fontBody)
                    .foregroundStyle(DS.ink)
                    .focused($focusedField, equals: .key)
                    .padding(.horizontal, 10)
                    .frame(height: DS.controlHeightField)
                    .background(DS.surface)
                    .overlay(
                        RoundedRectangle(cornerRadius: DS.cornerRadius)
                            .strokeBorder(focusedField == .key ? DS.accent : DS.line, lineWidth: 1)
                    )
                    .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
            }

            if nameOpen {
                VStack(alignment: .leading, spacing: 7) {
                    Text("实例名称（可选）")
                        .font(DS.fontCompactMedium)
                        .foregroundStyle(DS.ink)
                    TextField(text: $name, prompt: Text(verbatim: "例如：主力生产节点").foregroundStyle(DS.placeholder)) { EmptyView() }
                        .textFieldStyle(.plain)
                        .font(DS.fontBody)
                        .foregroundStyle(DS.ink)
                        .focused($focusedField, equals: .name)
                        .padding(.horizontal, 10)
                        .frame(height: DS.controlHeightField)
                        .background(DS.surface)
                        .overlay(
                            RoundedRectangle(cornerRadius: DS.cornerRadius)
                                .strokeBorder(focusedField == .name ? DS.accent : DS.line, lineWidth: 1)
                        )
                        .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
                }
            } else {
                Button("设置实例备注名") {
                    nameOpen = true
                }
                .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
            }

            HStack(spacing: 10) {
                if !embeddedInSettings {
                    Spacer(minLength: 0)
                }
                if let onCancel {
                    Button("取消", action: onCancel)
                        .buttonStyle(LinearButtonStyle(variant: .secondary))
                        .disabled(busy)
                }
                Button {
                    Task { await submit() }
                } label: {
                    HStack(spacing: 6) {
                        if busy {
                            ProgressView().controlSize(.small)
                        } else {
                            Image(systemName: "powerplug")
                                .font(DS.iconFont(13, weight: .medium))
                        }
                        Text(isEditingSaved && apiKey.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? "保存实例名称" : (embeddedInSettings ? "更新连接" : "验证并连接"))
                    }
                }
                .buttonStyle(LinearButtonStyle(variant: .primary))
                .disabled(busy || url.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                if embeddedInSettings {
                    Spacer(minLength: 0)
                }
            }
            .padding(.top, 2)
        }
        .frame(maxWidth: embeddedInSettings ? .infinity : (compactQuick ? .infinity : 420), alignment: .leading)
        .onAppear {
            url = defaultUrl
            name = defaultName
            nameOpen = showNameField || !(initialName ?? "").isEmpty
        }
    }

    private func submit() async {
        let trimmedUrl = url.trimmingCharacters(in: .whitespacesAndNewlines)
        let trimmedName = name.trimmingCharacters(in: .whitespacesAndNewlines)
        let trimmedKey = apiKey.trimmingCharacters(in: .whitespacesAndNewlines)

        if isEditingSaved && trimmedKey.isEmpty {
            busy = true
            defer { busy = false }
            do {
                try await RustBridge.shared.commandVoid(
                    "save_connection_profile",
                    args: .object([
                        "baseUrl": .string(trimmedUrl),
                        "name": .string(trimmedName),
                    ])
                )
                onConnected?()
            } catch {
                store.report(error)
            }
            return
        }

        guard await store.beforeConnectionChange() else { return }
        busy = true
        defer { busy = false }
        var payload: [String: JSONValue] = [
            "baseUrl": .string(trimmedUrl),
            "apiKey": .string(trimmedKey),
        ]
        if !trimmedName.isEmpty {
            payload["name"] = .string(trimmedName)
        }
        do {
            try await RustBridge.shared.commandVoid("connect", args: .object(payload))
            apiKey = ""
            onConnected?()
        } catch {
            store.report(error)
        }
    }
}
