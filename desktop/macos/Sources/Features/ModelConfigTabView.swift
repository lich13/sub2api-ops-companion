import SwiftUI

/// Matches `ModelConfig` in `desktop/src/ModelConfig.tsx` (`features_models_light.png`).
public struct ModelConfigTabView: View {
    public var store: AppStore?
    public let online: Bool
    public let connectionKey: String

    @State private var groups: [ModelReasoningGroup] = []
    @State private var selectedGroupId: Int = 0
    @State private var config: ModelReasoningConfig? = nil
    @State private var fallbackDraft: ModelReasoningDraft? = nil
    @State private var loading: Bool = false
    @State private var removingModel: String = ""
    @State private var errorText: String = ""
    @State private var noticeText: String = ""
    @State private var hoveredModel: String? = nil

    private static let stateLabels: [String: String] = [
        "active": "目录已补全",
        "native": "原生已支持",
        "limited": "转发受限",
        "unverified": "转发未核实",
        "draft": "待保存",
    ]
    private static let sourceLabels: [String: String] = [
        "upstream": "真实上游",
        "native": "Sub2API 原生目录",
        "manual": "手动填写",
        "legacy": "旧配置待核对",
    ]

    public init(store: AppStore? = nil, online: Bool, connectionKey: String) {
        self.store = store
        self.online = online
        self.connectionKey = connectionKey
    }

    public var body: some View {
        ZStack {
            VStack(alignment: .leading, spacing: 14) {
                HStack(spacing: 8) {
                    let current = groups.first(where: { $0.id == selectedGroupId })
                    LinearSelect(
                        label: current?.name ?? "选择模型分组",
                        accessibilityTitle: "选择模型分组",
                        height: DS.controlHeight,
                        minWidth: 196,
                        mutedLabel: current == nil,
                        disabled: !online || groups.isEmpty || loading,
                        entries: groups.map { g in
                            LinearMenuEntry(
                                id: "\(g.id)",
                                title: "\(g.name) (\(g.platform))",
                                checked: g.id == selectedGroupId
                            ) {
                                selectedGroupId = g.id
                            }
                        }
                    )

                    LinearIconButton(
                        systemName: "arrow.clockwise",
                        label: "刷新思考档位",
                        size: 13,
                        boxSize: DS.controlHeight,
                        bordered: true,
                        disabled: !online || loading || selectedGroupId == 0
                    ) {
                        Task { await loadGroupReasoning(id: selectedGroupId) }
                    }

                    if let status = config?.status, !status.message.isEmpty {
                        Text(status.message)
                            .font(DS.fontResetTime)
                            .foregroundStyle(status.state == "healthy" || status.state == "active" ? DS.good : DS.muted)
                            .padding(.leading, 4)
                    }

                    Spacer()

                    Button {
                        openEditor(
                            ModelReasoningDraft(
                                model: "",
                                efforts: [],
                                default_effort: "",
                                editing: false,
                                loaded: false
                            )
                        )
                    } label: {
                        HStack(spacing: 5) {
                            Image(systemName: "plus")
                                .font(DS.iconFont(12, weight: .semibold))
                            Text("添加模型")
                        }
                    }
                    .buttonStyle(LinearButtonStyle(variant: .primary))
                    .disabled(!online || selectedGroupId == 0 || loading)
                }

                if !noticeText.isEmpty {
                    Text(noticeText)
                        .font(DS.fontCompactMedium)
                        .foregroundStyle(DS.good)
                }
                if !errorText.isEmpty {
                    Text(errorText)
                        .font(DS.fontCompact)
                        .foregroundStyle(DS.bad)
                }

                let hasItems = !(config?.items ?? []).isEmpty
                VStack(spacing: 0) {
                    HStack(spacing: 12) {
                        Text("模型 ID")
                            .frame(width: 240, alignment: .leading)
                        Text("思考档位")
                            .frame(maxWidth: .infinity, alignment: .leading)
                        Text("默认")
                            .frame(width: 84, alignment: .leading)
                        Text("状态")
                            .frame(width: 170, alignment: .leading)
                        if hasItems {
                            DS.clear
                                .frame(width: 136)
                        }
                    }
                    .font(DS.fontTableHeader)
                    .foregroundStyle(DS.muted)
                    .padding(.horizontal, 16)
                    .frame(height: 36)
                    .background(DS.surfaceSubtle)

                    Rectangle().fill(DS.line).frame(height: 1)

                    if let items = config?.items, !items.isEmpty {
                        ForEach(Array(items.enumerated()), id: \.element.model) { idx, item in
                            HStack(spacing: 12) {
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(verbatim: item.model)
                                        .font(DS.fontCompactMedium)
                                        .foregroundStyle(DS.ink)
                                        .lineLimit(1)
                                    if let updated = item.updated_at, !updated.isEmpty {
                                        Text(BeijingTime.fullTime(updated))
                                            .font(DS.fontResetTime)
                                            .monospacedDigit()
                                            .foregroundStyle(DS.muted)
                                    }
                                }
                                .frame(width: 240, alignment: .leading)

                                HStack(spacing: 5) {
                                    ForEach(item.efforts, id: \.self) { ef in
                                        Text(verbatim: ef)
                                            .font(DS.fontPriorityMeta).monospacedDigit()
                                            .foregroundStyle(ef == item.default_effort ? DS.accent : DS.ink)
                                            .padding(.horizontal, 6)
                                            .padding(.vertical, 2)
                                            .background(ef == item.default_effort ? DS.accentBg : DS.surfaceSubtle)
                                            .clipShape(RoundedRectangle(cornerRadius: 4))
                                    }
                                }
                                .frame(maxWidth: .infinity, alignment: .leading)

                                Text(verbatim: item.default_effort)
                                    .font(DS.fontPriorityMeta).monospacedDigit()
                                    .foregroundStyle(DS.ink)
                                    .frame(width: 84, alignment: .leading)

                                VStack(alignment: .leading, spacing: 2) {
                                    Text(Self.stateLabels[item.state] ?? "转发未核实")
                                        .font(DS.fontCompactMedium)
                                        .foregroundStyle(item.state == "active" || item.state == "native" ? DS.good : DS.warn)
                                    Text(Self.sourceLabels[item.source] ?? item.source)
                                        .font(DS.fontResetTime)
                                        .foregroundStyle(DS.muted)
                                }
                                .frame(width: 170, alignment: .leading)

                                HStack(spacing: 6) {
                                    Button("编辑") {
                                        openEditor(
                                            ModelReasoningDraft(
                                                model: item.model,
                                                efforts: item.efforts,
                                                default_effort: item.default_effort,
                                                editing: true,
                                                loaded: true
                                            )
                                        )
                                    }
                                    .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                                    .disabled(!online || !removingModel.isEmpty)

                                    Button(item.state == "native" ? "恢复原生" : "移除") {
                                        Task { await removeItem(item) }
                                    }
                                    .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                                    .disabled(!online || !removingModel.isEmpty)
                                }
                                .frame(width: 136, alignment: .trailing)
                            }
                            .padding(.horizontal, 16)
                            .padding(.vertical, 10)
                            .background(hoveredModel == item.model ? DS.rowHover : DS.clear)
                            .onHover { hovering in
                                if hovering {
                                    hoveredModel = item.model
                                } else if hoveredModel == item.model {
                                    hoveredModel = nil
                                }
                            }

                            if idx < items.count - 1 {
                                Rectangle().fill(DS.line).frame(height: 1)
                            }
                        }
                    } else {
                        Text(loading ? "正在读取…" : "暂无补全")
                            .font(DS.fontCompact)
                            .foregroundStyle(DS.muted)
                            .padding(.vertical, 32)
                            .frame(maxWidth: .infinity, alignment: .center)
                    }
                }
                .background(DS.surface)
                .overlay(RoundedRectangle(cornerRadius: DS.cardRadius).strokeBorder(DS.line, lineWidth: 1))
                .clipShape(RoundedRectangle(cornerRadius: DS.cardRadius))
            }

            if let draft = fallbackDraft {
                ZStack {
                    DS.backdrop.ignoresSafeArea().onTapGesture { fallbackDraft = nil }
                    ModelReasoningEditSheet(
                        groupId: selectedGroupId,
                        online: online,
                        initialDraft: draft,
                        onSaved: { nextCfg, msg in
                            config = nextCfg
                            noticeText = msg
                            fallbackDraft = nil
                        },
                        onClose: { fallbackDraft = nil }
                    )
                }
            }
        }
        .task(id: "\(connectionKey):\(online)") {
            await loadGroups()
        }
        .onChange(of: selectedGroupId) { _, nextId in
            guard nextId > 0 else { return }
            Task { await loadGroupReasoning(id: nextId) }
        }
    }

    private func openEditor(_ draft: ModelReasoningDraft) {
        if let store {
            let gid = selectedGroupId
            let isOnline = online
            store.presentWindowModal(onBackdrop: { store.dismissWindowModal() }) {
                ModelReasoningEditSheet(
                    groupId: gid,
                    online: isOnline,
                    initialDraft: draft,
                    onSaved: { nextCfg, msg in
                        config = nextCfg
                        noticeText = msg
                        store.dismissWindowModal()
                    },
                    onClose: { store.dismissWindowModal() }
                )
            }
        } else {
            fallbackDraft = draft
        }
    }

    private func loadGroups() async {
        guard online else { return }
        struct Envelope: Decodable, Sendable { var groups: [ModelReasoningGroup] }
        loading = true
        errorText = ""
        defer { loading = false }
        do {
            let env: Envelope = try await RustBridge.shared.api("GET", "/model-groups")
            groups = env.groups
            let targetId: Int
            if env.groups.contains(where: { $0.id == selectedGroupId }) {
                targetId = selectedGroupId
            } else {
                targetId = env.groups.first?.id ?? 0
                selectedGroupId = targetId
            }
            if targetId > 0 {
                await loadGroupReasoning(id: targetId)
            }
        } catch {
            errorText = error.localizedDescription
        }
    }

    private func loadGroupReasoning(id: Int) async {
        guard online, id > 0 else { return }
        loading = true
        errorText = ""
        defer { loading = false }
        do {
            let next: ModelReasoningConfig = try await RustBridge.shared.api("GET", "/model-groups/\(id)/reasoning")
            config = next
        } catch {
            errorText = error.localizedDescription
        }
    }

    private func removeItem(_ item: ModelReasoningItem) async {
        guard let cfg = config, online, removingModel.isEmpty else { return }
        removingModel = item.model
        errorText = ""
        noticeText = ""
        defer { removingModel = "" }
        do {
            let next: ModelReasoningConfig = try await RustBridge.shared.api(
                "DELETE",
                "/model-groups/\(selectedGroupId)/reasoning",
                body: .object([
                    "model": .string(item.model),
                    "expected_version": .string(cfg.group.version),
                    "expected_revision": .string(cfg.revision),
                ])
            )
            config = next
            noticeText = "已恢复原生"
        } catch {
            errorText = error.localizedDescription
        }
    }
}
