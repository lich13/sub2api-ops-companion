import SwiftUI

/// Matches `ModelConfig` in `desktop/src/ModelConfig.tsx`.
public struct ModelConfigTabView: View {
    public let online: Bool
    public let connectionKey: String

    @State private var groups: [ModelReasoningGroup] = []
    @State private var selectedGroupId: Int = 0
    @State private var config: ModelReasoningConfig? = nil
    @State private var editingDraft: ModelReasoningDraft? = nil
    @State private var loading: Bool = false
    @State private var removingModel: String = ""
    @State private var errorText: String = ""
    @State private var noticeText: String = ""

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

    public init(online: Bool, connectionKey: String) {
        self.online = online
        self.connectionKey = connectionKey
    }

    public var body: some View {
        ZStack {
            VStack(alignment: .leading, spacing: 12) {
                HStack(spacing: 8) {
                    Menu {
                        ForEach(groups) { g in
                            Button {
                                selectedGroupId = g.id
                            } label: {
                                Text("\(g.name) (\(g.platform))")
                            }
                        }
                    } label: {
                        let current = groups.first(where: { $0.id == selectedGroupId })
                        HStack(spacing: 6) {
                            Text(current.map { "\($0.name) · \($0.platform)" } ?? "选择模型分组")
                                .font(DS.fontCompactSemibold)
                                .foregroundStyle(DS.ink)
                            Image(systemName: "chevron.down")
                                .font(DS.iconFont(10))
                                .foregroundStyle(DS.muted)
                        }
                        .padding(.horizontal, 10)
                        .frame(height: DS.controlHeightCompact)
                        .background(DS.surface)
                        .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                        .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
                    }
                    .menuStyle(.borderlessButton)
                    .menuIndicator(.hidden)
                    .fixedSize()
                    .disabled(!online || groups.isEmpty || loading)

                    if let status = config?.status {
                        Text(status.message)
                            .font(DS.fontResetTime)
                            .foregroundStyle(status.state == "healthy" || status.state == "active" ? DS.good : DS.muted)
                    }

                    Spacer()

                    LinearIconButton(
                        systemName: "arrow.clockwise",
                        label: "刷新思考档位",
                        disabled: !online || loading || selectedGroupId == 0
                    ) {
                        Task { await loadGroupReasoning(id: selectedGroupId) }
                    }

                    Button {
                        editingDraft = ModelReasoningDraft(
                            model: "",
                            efforts: [],
                            default_effort: "",
                            editing: false,
                            loaded: false
                        )
                    } label: {
                        HStack(spacing: 4) {
                            Image(systemName: "plus")
                                .font(DS.iconFont(11, weight: .semibold))
                            Text("新增模型档位")
                        }
                    }
                    .buttonStyle(LinearButtonStyle(variant: .primary, compact: true))
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

                VStack(spacing: 0) {
                    HStack(spacing: 10) {
                        Text("模型")
                            .frame(width: 190, alignment: .leading)
                        Text("可用档位")
                            .frame(maxWidth: .infinity, alignment: .leading)
                        Text("默认")
                            .frame(width: 80, alignment: .leading)
                        Text("状态 / 来源")
                            .frame(width: 170, alignment: .leading)
                        Text("更新时间")
                            .frame(width: 112, alignment: .leading)
                        Text("操作")
                            .frame(width: 124, alignment: .trailing)
                    }
                    .font(DS.fontTableHeader)
                    .foregroundStyle(DS.muted)
                    .padding(.horizontal, 14)
                    .frame(height: 34)
                    .background(DS.surfaceSubtle)

                    Rectangle().fill(DS.line).frame(height: 1)

                    if let items = config?.items, !items.isEmpty {
                        ForEach(Array(items.enumerated()), id: \.element.model) { idx, item in
                            HStack(spacing: 10) {
                                Text(verbatim: item.model)
                                    .font(DS.fontMono12)
                                    .foregroundStyle(DS.ink)
                                    .lineLimit(1)
                                    .frame(width: 190, alignment: .leading)

                                HStack(spacing: 4) {
                                    ForEach(item.efforts, id: \.self) { ef in
                                        Text(verbatim: ef)
                                            .font(DS.fontMono11)
                                            .foregroundStyle(ef == item.default_effort ? DS.accent : DS.ink)
                                            .padding(.horizontal, 6)
                                            .padding(.vertical, 2)
                                            .background(ef == item.default_effort ? DS.accentBg : DS.surfaceSubtle)
                                            .clipShape(RoundedRectangle(cornerRadius: 4))
                                    }
                                }
                                .frame(maxWidth: .infinity, alignment: .leading)

                                Text(verbatim: item.default_effort)
                                    .font(DS.fontMono11)
                                    .foregroundStyle(DS.ink)
                                    .frame(width: 80, alignment: .leading)

                                VStack(alignment: .leading, spacing: 2) {
                                    Text(Self.stateLabels[item.state] ?? item.state)
                                        .font(DS.fontCompactMedium)
                                        .foregroundStyle(item.state == "active" || item.state == "native" ? DS.good : DS.warn)
                                    Text(Self.sourceLabels[item.source] ?? item.source)
                                        .font(DS.fontResetTime)
                                        .foregroundStyle(DS.muted)
                                }
                                .frame(width: 170, alignment: .leading)

                                Text(BeijingTime.fullTime(item.updated_at))
                                    .font(DS.fontResetTime)
                                    .monospacedDigit()
                                    .foregroundStyle(DS.muted)
                                    .frame(width: 112, alignment: .leading)

                                HStack(spacing: 6) {
                                    Button("编辑") {
                                        editingDraft = ModelReasoningDraft(
                                            model: item.model,
                                            efforts: item.efforts,
                                            default_effort: item.default_effort,
                                            editing: true,
                                            loaded: true
                                        )
                                    }
                                    .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                                    .disabled(!online || !removingModel.isEmpty)

                                    Button("恢复原生") {
                                        Task { await removeItem(item) }
                                    }
                                    .buttonStyle(LinearButtonStyle(variant: .danger, compact: true))
                                    .disabled(!online || !removingModel.isEmpty)
                                }
                                .frame(width: 124, alignment: .trailing)
                            }
                            .padding(.horizontal, 14)
                            .padding(.vertical, 9)

                            if idx < items.count - 1 {
                                Rectangle().fill(DS.line).frame(height: 1)
                            }
                        }
                    } else {
                        Text(loading ? "正在读取模型思考档位…" : "当前分组暂无自定义模型思考档位补全")
                            .font(DS.fontCompact)
                            .foregroundStyle(DS.muted)
                            .padding(16)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                }
                .background(DS.surface)
                .overlay(RoundedRectangle(cornerRadius: DS.cardRadius).strokeBorder(DS.line, lineWidth: 1))
                .clipShape(RoundedRectangle(cornerRadius: DS.cardRadius))
            }

            if let draft = editingDraft {
                ModelReasoningEditSheet(
                    groupId: selectedGroupId,
                    online: online,
                    initialDraft: draft,
                    onSaved: { nextCfg, msg in
                        config = nextCfg
                        noticeText = msg
                        editingDraft = nil
                    },
                    onClose: { editingDraft = nil }
                )
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
