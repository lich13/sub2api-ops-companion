import SwiftUI

public struct ModelReasoningGroup: Codable, Sendable, Equatable, Identifiable {
    public var id: Int
    public var name: String
    public var platform: String
    public var version: String

    public init(id: Int, name: String, platform: String, version: String) {
        self.id = id
        self.name = name
        self.platform = platform
        self.version = version
    }
}

public struct ModelReasoningItem: Codable, Sendable, Equatable, Identifiable {
    public var id: String { model }
    public var model: String
    public var efforts: [String]
    public var default_effort: String
    public var state: String
    public var reason: String
    public var source: String
    public var updated_at: String?
}

public struct ModelReasoningStatus: Codable, Sendable, Equatable {
    public var state: String
    public var message: String
}

public struct ModelReasoningConfig: Codable, Sendable, Equatable {
    public var group: ModelReasoningGroup
    public var revision: String
    public var items: [ModelReasoningItem]
    public var status: ModelReasoningStatus
}

public struct ModelReasoningForwarding: Codable, Sendable, Equatable {
    public var state: String
    public var reason: String
    public var version: String
}

public struct ModelReasoningResolution: Codable, Sendable, Equatable {
    public var group: ModelReasoningGroup
    public var revision: String
    public var model: String
    public var binding: String
    public var efforts: [String]
    public var default_effort: String
    public var source: String
    public var needs_allowlist: Bool
    public var descriptor_available: Bool
    public var native_efforts: [String]
    public var native_default: String
    public var forwarding: ModelReasoningForwarding
}

public struct ModelReasoningDraft: Sendable, Equatable {
    public var model: String
    public var efforts: [String]
    public var default_effort: String
    public var editing: Bool
    public var loaded: Bool

    public static let allEfforts: [String] = [
        "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
    ]

    public var signature: String {
        "\(model)|\(efforts.sorted().joined(separator: ","))|\(default_effort)"
    }
}

/// 460pt modal matching `ModelConfig.tsx` reasoning edit dialog.
public struct ModelReasoningEditSheet: View {
    public let groupId: Int
    public let online: Bool
    public let initialDraft: ModelReasoningDraft
    public let onSaved: (ModelReasoningConfig, String) -> Void
    public let onClose: () -> Void

    @State private var draft: ModelReasoningDraft
    @State private var resolution: ModelReasoningResolution? = nil
    @State private var checkedSignature: String = ""
    @State private var confirmAllowlist: Bool = false
    @State private var detailsExpanded: Bool = false
    @State private var resolving: Bool = false
    @State private var saving: Bool = false
    @State private var formError: String = ""

    public init(
        groupId: Int,
        online: Bool,
        initialDraft: ModelReasoningDraft,
        onSaved: @escaping (ModelReasoningConfig, String) -> Void,
        onClose: @escaping () -> Void
    ) {
        self.groupId = groupId
        self.online = online
        self.initialDraft = initialDraft
        self.onSaved = onSaved
        self.onClose = onClose
        self._draft = State(initialValue: initialDraft)
    }

    private var isValidDraft: Bool {
        !draft.efforts.isEmpty && draft.efforts.contains(draft.default_effort)
    }

    private var isVerified: Bool {
        resolution?.forwarding.state == "verified" && resolution?.descriptor_available == true
    }

    private var isNativeMatch: Bool {
        guard let r = resolution, isVerified else { return false }
        return r.native_default == draft.default_effort
            && r.native_efforts.sorted() == draft.efforts.sorted()
            && !r.needs_allowlist
    }

    private var needsAllowlist: Bool {
        isVerified && resolution?.needs_allowlist == true && !isNativeMatch
    }

    private var canSave: Bool {
        online && isValidDraft && checkedSignature == draft.signature && !resolving && !saving
            && (!needsAllowlist || confirmAllowlist)
    }

    public var body: some View {
        LinearModalCard(
            title: draft.editing ? "编辑模型思考档位" : "补全模型思考档位",
            width: 480,
            closeDisabled: saving,
            onClose: onClose
        ) {
            VStack(alignment: .leading, spacing: 6) {
                Text("模型 ID")
                    .font(DS.fontCompactMedium)
                    .foregroundStyle(DS.muted)
                TextField(
                    text: $draft.model,
                    prompt: Text(verbatim: "例如 gpt-5.4 或 claude-opus-4-6")
                ) { EmptyView() }
                .textFieldStyle(.plain)
                .font(DS.fontBody)
                .foregroundStyle(DS.ink)
                .padding(.horizontal, 10)
                .frame(height: DS.controlHeight)
                .background(DS.surface)
                .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
                .disabled(draft.editing || saving)
                .onChange(of: draft.model) { _, next in
                    let trimmed = next.trimmingCharacters(in: .whitespaces)
                    if trimmed != next { draft.model = trimmed }
                    confirmAllowlist = false
                }
            }

            VStack(alignment: .leading, spacing: 6) {
                Text("可用推理强度")
                    .font(DS.fontCompactMedium)
                    .foregroundStyle(DS.muted)
                LazyVGrid(columns: Array(repeating: GridItem(.flexible(), spacing: 6), count: 4), spacing: 6) {
                    ForEach(ModelReasoningDraft.allEfforts, id: \.self) { effort in
                        let selected = draft.efforts.contains(effort)
                        Button {
                            toggleEffort(effort)
                        } label: {
                            Text(verbatim: effort)
                                .frame(maxWidth: .infinity)
                        }
                        .buttonStyle(LinearButtonStyle(variant: selected ? .selectedGhost : .secondary, compact: true))
                        .disabled(saving)
                    }
                }
            }

            VStack(alignment: .leading, spacing: 6) {
                Text("默认推理强度")
                    .font(DS.fontCompactMedium)
                    .foregroundStyle(DS.muted)
                LinearSelect(
                    label: draft.default_effort.isEmpty ? "请选择默认强度" : draft.default_effort,
                    accessibilityTitle: "默认推理强度",
                    height: DS.controlHeight,
                    expandWidth: true,
                    mutedLabel: draft.default_effort.isEmpty,
                    disabled: draft.efforts.isEmpty || saving,
                    entries: draft.efforts.map { effort in
                        LinearMenuEntry(id: effort, title: effort, checked: draft.default_effort == effort) {
                            draft.default_effort = effort
                            draft.loaded = true
                            confirmAllowlist = false
                        }
                    }
                )
            }

            if resolving {
                Text("正在核实上游转发与目录状态…")
                    .font(DS.fontResetTime)
                    .foregroundStyle(DS.muted)
            } else if let r = resolution {
                VStack(alignment: .leading, spacing: 6) {
                    HStack {
                        Text("转发状态：\(r.forwarding.state == "verified" ? "已核实" : r.forwarding.state)")
                            .font(DS.fontResetTimeSemibold)
                            .foregroundStyle(isVerified ? DS.good : DS.warn)
                        Spacer()
                        Button(detailsExpanded ? "收起详情" : "核实详情") {
                            detailsExpanded.toggle()
                        }
                        .buttonStyle(LinearButtonStyle(variant: .ghost, compact: true))
                    }
                    if detailsExpanded {
                        VStack(alignment: .leading, spacing: 3) {
                            Text("来源：\(r.source) · 原生默认：\(r.native_default.isEmpty ? "—" : r.native_default)")
                            if !r.forwarding.reason.isEmpty {
                                Text(r.forwarding.reason)
                            }
                        }
                        .font(DS.fontResetTime)
                        .foregroundStyle(DS.muted)
                    }
                }
                .padding(10)
                .background(DS.surfaceSubtle)
                .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
            }

            if needsAllowlist {
                LinearCheckbox(
                    isChecked: confirmAllowlist,
                    label: "确认同步将该模型加入组内账号的模型白名单",
                    disabled: saving
                ) {
                    confirmAllowlist.toggle()
                }
            }

            if !formError.isEmpty {
                Text(formError)
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.bad)
            }
        } footer: {
            Spacer()
            Button("取消") { onClose() }
                .buttonStyle(LinearButtonStyle(variant: .secondary))
                .disabled(saving)
            Button(saving ? "保存中…" : "保存") {
                Task { await save() }
            }
            .buttonStyle(LinearButtonStyle(variant: .primary))
            .disabled(!canSave)
        }
        .sub2opsDialogFocus(onEscape: { if !saving { onClose() } })
        .task(id: "\(groupId):\(online):\(draft.signature)") {
            await debounceResolve()
        }
    }

    private func toggleEffort(_ effort: String) {
        let next: [String]
        if draft.efforts.contains(effort) {
            next = draft.efforts.filter { $0 != effort }
        } else {
            next = ModelReasoningDraft.allEfforts.filter { draft.efforts.contains($0) || $0 == effort }
        }
        draft.loaded = true
        draft.efforts = next
        if !next.contains(draft.default_effort) {
            draft.default_effort = next.first ?? ""
        }
        confirmAllowlist = false
    }

    private func debounceResolve() async {
        guard online, !draft.model.isEmpty,
              draft.model.rangeOfCharacter(from: .whitespacesAndNewlines) == nil,
              !draft.model.contains("*")
        else {
            resolution = nil
            checkedSignature = ""
            resolving = false
            return
        }
        if draft.loaded && !isValidDraft {
            checkedSignature = ""
            resolving = false
            return
        }
        resolving = true
        try? await Task.sleep(for: .milliseconds(350))
        guard !Task.isCancelled else { return }
        var body: [String: JSONValue] = ["model": .string(draft.model)]
        if draft.loaded {
            body["efforts"] = .array(draft.efforts.map(JSONValue.string))
            body["default_effort"] = .string(draft.default_effort)
        }
        do {
            let res: ModelReasoningResolution = try await RustBridge.shared.api(
                "POST",
                "/model-groups/\(groupId)/reasoning/resolve",
                body: .object(body)
            )
            guard !Task.isCancelled else { return }
            resolution = res
            if !draft.loaded {
                draft.efforts = res.efforts
                draft.default_effort = res.default_effort
                draft.loaded = true
            }
            checkedSignature = ModelReasoningDraft(
                model: res.model,
                efforts: res.efforts,
                default_effort: res.default_effort,
                editing: draft.editing,
                loaded: true
            ).signature
            formError = ""
        } catch {
            guard !Task.isCancelled else { return }
            formError = error.localizedDescription
            checkedSignature = ""
        }
        resolving = false
    }

    private func save() async {
        guard let r = resolution, canSave else { return }
        saving = true
        formError = ""
        defer { saving = false }
        struct SaveResponse: Decodable, Sendable {
            var outcome: String
            var message: String
            var state: ModelReasoningConfig
        }
        do {
            let result: SaveResponse = try await RustBridge.shared.api(
                "PUT",
                "/model-groups/\(groupId)/reasoning",
                body: .object([
                    "model": .string(draft.model),
                    "efforts": .array(draft.efforts.map(JSONValue.string)),
                    "default_effort": .string(draft.default_effort),
                    "expected_version": .string(r.group.version),
                    "expected_revision": .string(r.revision),
                    "expected_binding": .string(r.binding),
                    "confirm_allowlist": .bool(confirmAllowlist),
                ])
            )
            if result.outcome == "partial" {
                formError = result.message
                checkedSignature = ""
            } else {
                onSaved(result.state, result.message)
            }
        } catch {
            formError = error.localizedDescription
            checkedSignature = ""
        }
    }
}
