import SwiftUI

public struct AccountCreationFieldsView: View {
    @Binding public var draft: AccountCreationFormDraft
    public var tlsProfiles: [ChoiceRecord]
    public var quotaNotifyEnabled: Bool
    public var webSearchEnabled: Bool
    public var aiStudioEnabled: Bool
    public var groupLongContext: Bool

    @State private var expandedGroups: Set<String>
    @FocusState var focusedHeaderValueId: UUID?

    public init(
        draft: Binding<AccountCreationFormDraft>,
        tlsProfiles: [ChoiceRecord] = [],
        quotaNotifyEnabled: Bool = false,
        webSearchEnabled: Bool = false,
        aiStudioEnabled: Bool = false,
        groupLongContext: Bool = false,
        expandAll: Bool = false
    ) {
        self._expandedGroups = State(
            initialValue: expandAll ? Set(AccountCreationCatalog.shared.fields.map(\.group)) : ["连接", "调度"]
        )
        self._draft = draft
        self.tlsProfiles = tlsProfiles
        self.quotaNotifyEnabled = quotaNotifyEnabled
        self.webSearchEnabled = webSearchEnabled
        self.aiStudioEnabled = aiStudioEnabled
        self.groupLongContext = groupLongContext
    }

    private var shownFields: [CreationFieldSpec] {
        draft.visibleFields.filter { field in
            if field.group == "额度通知" && !quotaNotifyEnabled { return false }
            if field.path == "extra.web_search_emulation" && !webSearchEnabled { return false }
            if field.path == "extra.openai_long_context_billing_enabled" && groupLongContext { return false }
            return true
        }
    }

    private var groupedFields: [(group: String, items: [CreationFieldSpec])] {
        var order: [String] = []
        var map: [String: [CreationFieldSpec]] = [:]
        for field in shownFields {
            if map[field.group] == nil { order.append(field.group) }
            map[field.group, default: []].append(field)
        }
        return order.map { ($0, map[$0] ?? []) }
    }

    private func isWide(_ spec: CreationFieldSpec) -> Bool {
        ["mapping", "headers", "pause-rules", "protocols", "protocol-rules", "strings"].contains(spec.kind)
            || (spec.path == "credentials.base_url" && !draft.urlPresets.isEmpty)
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            ForEach(groupedFields, id: \.group) { section in
                let isExpanded = expandedGroups.contains(section.group)
                VStack(alignment: .leading, spacing: 8) {
                    Rectangle().fill(DS.line).frame(height: 1)

                    Button {
                        if isExpanded {
                            expandedGroups.remove(section.group)
                        } else {
                            expandedGroups.insert(section.group)
                        }
                    } label: {
                        HStack(spacing: 6) {
                            Image(systemName: isExpanded ? "chevron.down" : "chevron.right")
                                .font(DS.iconFont(10, weight: .semibold))
                                .foregroundStyle(DS.muted)
                                .frame(width: 12)
                            Text(section.group)
                                .font(DS.fontBodySemibold)
                                .foregroundStyle(DS.ink)
                            Spacer()
                        }
                        .padding(.vertical, 4)
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.linearPlain)

                    if isExpanded {
                        let scalars = section.items.filter { !isWide($0) }
                        let wides = section.items.filter { isWide($0) }

                        if !scalars.isEmpty {
                            LazyVGrid(
                                columns: [
                                    GridItem(.flexible(), spacing: 12, alignment: .bottomLeading),
                                    GridItem(.flexible(), spacing: 12, alignment: .bottomLeading),
                                ],
                                alignment: .leading,
                                spacing: 10
                            ) {
                                ForEach(scalars) { spec in
                                    fieldControl(spec)
                                }
                            }
                        }

                        ForEach(wides) { spec in
                            fieldControl(spec)
                        }
                    }
                }
                .padding(.top, 2)
            }
        }
    }

    @ViewBuilder
    private func fieldControl(_ spec: CreationFieldSpec) -> some View {
        switch spec.kind {
        case "mapping":
            mappingSection(
                title: spec.label,
                isHeaders: false,
                isPrimaryModelMapping: spec.path == "credentials.model_mapping",
                presets: spec.path == "credentials.model_mapping" ? draft.mappingPresets : [],
                rows: spec.path == "credentials.compact_model_mapping" ? $draft.compactMappings : $draft.mappings
            )

        case "headers":
            mappingSection(
                title: spec.label,
                isHeaders: true,
                isPrimaryModelMapping: false,
                presets: [],
                rows: $draft.headers
            )

        case "boolean", "stringbool":
            let isOn: Bool = {
                if spec.kind == "stringbool" {
                    return draft.values[spec.path]?.stringValue == "true" || draft.values[spec.path]?.boolValue == true
                }
                return draft.values[spec.path]?.boolValue ?? false
            }()
            HStack(spacing: 8) {
                LinearCheckbox(isChecked: isOn, label: spec.label) {
                    let next = !isOn
                    draft.setValue(spec.path, spec.kind == "stringbool" ? .string(next ? "true" : "false") : .bool(next))
                }
                Spacer(minLength: 0)
            }
            .frame(minHeight: DS.controlHeightCompact)

        case "strings":
            let choices = spec.choices ?? []
            let selectedSet = Set((draft.values[spec.path]?.arrayValue ?? []).compactMap(\.stringValue))
            VStack(alignment: .leading, spacing: 6) {
                Text(spec.label)
                    .font(DS.fontCompactMedium)
                    .foregroundStyle(DS.muted)
                HStack(spacing: 12) {
                    ForEach(choices, id: \.self) { item in
                        LinearCheckbox(
                            isChecked: selectedSet.contains(item),
                            label: AccountCreationCatalog.choiceLabel(item)
                        ) {
                            var next = selectedSet
                            if next.contains(item) {
                                if next.count > 1 { next.remove(item) }
                            } else {
                                next.insert(item)
                            }
                            let ordered = choices.filter { next.contains($0) }
                            draft.setValue(spec.path, .array(ordered.map(JSONValue.string)))
                        }
                    }
                    Spacer(minLength: 0)
                }
            }

        case "tls":
            let currentId = draft.values[spec.path]?.intValue.map(Int.init)
            let currentLabel = currentId.flatMap { id in tlsProfiles.first(where: { $0.id == id })?.name } ?? "默认配置"
            VStack(alignment: .leading, spacing: 4) {
                Text(spec.label)
                    .font(DS.fontCompactMedium)
                    .foregroundStyle(DS.muted)
                LinearSelect(
                    label: currentLabel,
                    accessibilityTitle: spec.label,
                    height: DS.controlHeightCompact,
                    expandWidth: true,
                    compactFont: true,
                    entries: [
                        LinearMenuEntry(id: "default", title: "默认配置", checked: currentId == nil) {
                            draft.setValue(spec.path, nil)
                        },
                    ] + tlsProfiles.map { p in
                        LinearMenuEntry(id: "\(p.id)", title: p.name, checked: currentId == p.id) {
                            draft.setValue(spec.path, .int(Int64(p.id)))
                        }
                    }
                )
            }

        case "select", "tier":
            let choices = spec.kind == "tier"
                ? draft.tierChoices
                : (spec.choices ?? []).filter {
                    spec.path != "credentials.api_protocol" || $0 == "adaptive" || draft.protocolNames.contains($0)
                }
            let allowUnset = !choices.contains("") && spec.defaultValue == nil && spec.kind != "tier"
            let currentRaw = draft.values[spec.path]?.stringValue ?? spec.defaultValue?.stringValue
                ?? (allowUnset ? "" : choices.first ?? "")
            let currentLabel = (currentRaw.isEmpty && allowUnset) ? "未设置" : AccountCreationCatalog.choiceLabel(currentRaw)
            VStack(alignment: .leading, spacing: 4) {
                Text(spec.label)
                    .font(DS.fontCompactMedium)
                    .foregroundStyle(DS.muted)
                LinearSelect(
                    label: currentLabel,
                    accessibilityTitle: spec.label,
                    height: DS.controlHeightCompact,
                    expandWidth: true,
                    compactFont: true,
                    entries: (allowUnset ? [
                        LinearMenuEntry(id: "__unset__", title: "未设置", checked: currentRaw.isEmpty) {
                            draft.setValue(spec.path, nil)
                        },
                    ] : []) + choices.map { choice in
                        let unavailable = spec.path == "credentials.oauth_type" && choice == "ai_studio" && !aiStudioEnabled
                        return LinearMenuEntry(
                            id: choice,
                            title: AccountCreationCatalog.choiceLabel(choice) + (unavailable ? "（未配置）" : ""),
                            checked: currentRaw == choice,
                            disabled: unavailable
                        ) {
                            draft.setValue(spec.path, .string(choice))
                        }
                    }
                )
            }

        case "number":
            CatalogNumberField(label: spec.label, value: draft.values[spec.path]) { updated in
                draft.setValue(spec.path, updated)
            }

        case "protocols":
            protocolsSection(title: spec.label)

        case "protocol-rules":
            protocolRulesSection(title: spec.label)

        case "pause-rules":
            pauseRulesSection(title: spec.label)

        default:
            CatalogStringOrNumbersField(
                label: spec.label,
                kind: spec.kind,
                value: draft.values[spec.path],
                presets: spec.path == "credentials.base_url" ? draft.urlPresets : []
            ) { updated in
                draft.setValue(spec.path, updated)
            }
        }
    }

    private struct CatalogNumberField: View {
        let label: String
        let value: JSONValue?
        let onCommit: (JSONValue) -> Void
        @State var text: String = ""

        private var formattedExternal: String {
            if let i = value?.intValue { return String(i) }
            if let d = value?.doubleValue { return String(d) }
            return ""
        }

        private static func parse(_ raw: String) -> JSONValue {
            let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
            if trimmed.isEmpty { return .null }
            if let i = Int64(trimmed) { return .int(i) }
            if let d = Double(trimmed) { return .double(d) }
            return .null
        }

        var body: some View {
            VStack(alignment: .leading, spacing: 4) {
                Text(label)
                    .font(DS.fontCompactMedium)
                    .foregroundStyle(DS.muted)
                TextField(text: $text, prompt: Text(verbatim: "未设置").foregroundStyle(DS.placeholder)) { EmptyView() }
                    .textFieldStyle(.plain)
                    .font(DS.fontBody)
                    .monospacedDigit()
                    .foregroundStyle(DS.ink)
                    .padding(.horizontal, 10)
                    .frame(height: DS.controlHeightCompact)
                    .background(DS.surface)
                    .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                    .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
                    .onAppear { text = formattedExternal }
                    .onChange(of: value) { _, _ in
                        if Self.parse(text) != (value ?? .null) {
                            text = formattedExternal
                        }
                    }
                    .onChange(of: text) { _, raw in
                        let parsed = Self.parse(raw)
                        if parsed != (value ?? .null) {
                            onCommit(parsed)
                        }
                    }
            }
        }
    }

    private struct CatalogStringOrNumbersField: View {
        let label: String
        let kind: String
        let value: JSONValue?
        var presets: [CreationURLPreset] = []
        let onCommit: (JSONValue) -> Void
        @State var text: String = ""

        /// Number lists stay as typed text; the draft validates and converts them on submit.
        private var formattedExternal: String {
            if let arr = value?.arrayValue {
                return arr.map(\.displayText).joined(separator: ", ")
            }
            return value?.stringValue ?? ""
        }

        var body: some View {
            VStack(alignment: .leading, spacing: 4) {
                Text(label)
                    .font(DS.fontCompactMedium)
                    .foregroundStyle(DS.muted)
                HStack(spacing: 8) {
                    TextField(
                        text: $text,
                        prompt: Text(verbatim: kind == "numbers" ? "逗号分隔" : "").foregroundStyle(DS.placeholder)
                    ) { EmptyView() }
                    .textFieldStyle(.plain)
                    .font(DS.fontBody)
                    .monospacedDigit()
                    .foregroundStyle(DS.ink)
                    .padding(.horizontal, 10)
                    .frame(height: DS.controlHeightCompact)
                    .background(DS.surface)
                    .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                    .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))

                    if !presets.isEmpty {
                        LinearSelect(
                            label: "预设地址",
                            accessibilityTitle: "\(label)预设",
                            height: DS.controlHeightCompact,
                            width: 132,
                            compactFont: true,
                            mutedLabel: true,
                            entries: presets.map { preset in
                                LinearMenuEntry(
                                    id: "\(preset.label):\(preset.url)",
                                    title: preset.label,
                                    checked: text == preset.url
                                ) {
                                    onCommit(.string(preset.url))
                                }
                            }
                        )
                    }
                }
                .onAppear { text = formattedExternal }
                .onChange(of: value) { _, _ in
                    if text != formattedExternal {
                        text = formattedExternal
                    }
                }
                .onChange(of: text) { _, next in
                    if next != formattedExternal {
                        onCommit(.string(next))
                    }
                }
            }
        }
    }

    func compactField(_ text: Binding<String>, prompt: String, width: CGFloat? = nil) -> some View {
        TextField(text: text, prompt: Text(verbatim: prompt).foregroundStyle(DS.placeholder)) { EmptyView() }
            .textFieldStyle(.plain)
            .font(DS.fontCompact)
            .monospacedDigit()
            .foregroundStyle(DS.ink)
            .padding(.horizontal, 8)
            .frame(height: DS.controlHeightDense)
            .frame(width: width)
            .background(DS.surface)
            .overlay(RoundedRectangle(cornerRadius: 5).strokeBorder(DS.line, lineWidth: 1))
            .clipShape(RoundedRectangle(cornerRadius: 5))
    }

    func captioned<Content: View>(_ caption: String, @ViewBuilder content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(caption)
                .font(DS.fontResetTime)
                .foregroundStyle(DS.muted)
            content()
        }
    }

    func collectionCard<Content: View>(@ViewBuilder content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            content()
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(DS.surface)
        .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(DS.line, lineWidth: 1))
        .clipShape(RoundedRectangle(cornerRadius: 6))
    }

    func ruleControls(index: Int, count: Int, move: @escaping (Int) -> Void, remove: @escaping () -> Void) -> some View {
        HStack(spacing: 2) {
            LinearIconButton(systemName: "arrow.up", label: "上移规则 \(index + 1)", size: 11, boxSize: 24, disabled: index == 0) {
                move(-1)
            }
            LinearIconButton(systemName: "arrow.down", label: "下移规则 \(index + 1)", size: 11, boxSize: 24, disabled: index == count - 1) {
                move(1)
            }
            LinearIconButton(systemName: "xmark", label: "移除规则 \(index + 1)", size: 11, boxSize: 24, action: remove)
        }
    }
}
