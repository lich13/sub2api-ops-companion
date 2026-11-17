import SwiftUI

public struct AccountCreationFieldsView: View {
    @Binding public var draft: AccountCreationFormDraft
    public var tlsProfiles: [CreationTLSProfileOption]
    public var quotaNotifyEnabled: Bool
    public var webSearchEnabled: Bool

    @State private var expandedGroups: Set<String> = ["连接", "调度"]

    public init(
        draft: Binding<AccountCreationFormDraft>,
        tlsProfiles: [CreationTLSProfileOption] = [],
        quotaNotifyEnabled: Bool = false,
        webSearchEnabled: Bool = false
    ) {
        self._draft = draft
        self.tlsProfiles = tlsProfiles
        self.quotaNotifyEnabled = quotaNotifyEnabled
        self.webSearchEnabled = webSearchEnabled
    }

    private var shownFields: [CreationFieldSpec] {
        draft.visibleFields.filter { field in
            if field.group == "额度通知" && !quotaNotifyEnabled { return false }
            if field.path == "extra.web_search_emulation" && !webSearchEnabled { return false }
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

    private var presets: [CreationModelPreset] {
        let key = draft.type == "bedrock" ? "bedrock" : draft.platform
        return AccountCreationCatalog.shared.mapping_presets[key] ?? []
    }

    private func isWide(_ spec: CreationFieldSpec) -> Bool {
        ["mapping", "headers", "pause-rules", "protocols", "protocol-rules", "strings"].contains(spec.kind)
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
                    .buttonStyle(.plain)

                    if isExpanded {
                        let scalars = section.items.filter { !isWide($0) }
                        let wides = section.items.filter { isWide($0) }

                        if !scalars.isEmpty {
                            LazyVGrid(
                                columns: [GridItem(.flexible(), spacing: 12), GridItem(.flexible(), spacing: 12)],
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
                presets: spec.path == "credentials.model_mapping" ? presets : [],
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
                    draft.values[spec.path] = spec.kind == "stringbool" ? .string(next ? "true" : "false") : .bool(next)
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
                            draft.values[spec.path] = .array(ordered.map(JSONValue.string))
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
                            draft.values[spec.path] = .null
                        },
                    ] + tlsProfiles.map { p in
                        LinearMenuEntry(id: "\(p.id)", title: p.name, checked: currentId == p.id) {
                            draft.values[spec.path] = .int(Int64(p.id))
                        }
                    }
                )
            }

        case "select", "tier":
            let choices = spec.kind == "tier" ? draft.tierChoices : (spec.choices ?? [])
            let currentRaw = draft.values[spec.path]?.stringValue ?? spec.defaultValue?.stringValue ?? choices.first ?? ""
            let allowUnset = !choices.contains("") && spec.defaultValue == nil && spec.kind != "tier"
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
                            draft.values[spec.path] = .null
                        },
                    ] : []) + choices.map { choice in
                        LinearMenuEntry(
                            id: choice,
                            title: AccountCreationCatalog.choiceLabel(choice),
                            checked: currentRaw == choice
                        ) {
                            draft.values[spec.path] = .string(choice)
                            if spec.path == "credentials.oauth_type" {
                                draft.values["credentials.tier_id"] = .string(draft.tierChoices[0])
                            }
                        }
                    }
                )
            }

        case "number":
            CatalogNumberField(label: spec.label, value: draft.values[spec.path]) { updated in
                draft.values[spec.path] = updated
            }

        default:
            CatalogStringOrNumbersField(
                label: spec.label,
                kind: spec.kind,
                value: draft.values[spec.path]
            ) { updated in
                draft.values[spec.path] = updated
            }
        }
    }

    private struct CatalogNumberField: View {
        let label: String
        let value: JSONValue?
        let onCommit: (JSONValue) -> Void
        @State private var text: String = ""

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
                TextField(text: $text, prompt: Text(verbatim: "未设置")) { EmptyView() }
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
        let onCommit: (JSONValue) -> Void
        @State private var text: String = ""

        private var formattedExternal: String {
            if let arr = value?.arrayValue {
                return arr.map(\.displayText).joined(separator: ", ")
            }
            return value?.stringValue ?? ""
        }

        private func parse(_ raw: String) -> JSONValue {
            if kind == "numbers" {
                let nums = raw
                    .components(separatedBy: CharacterSet(charactersIn: ",， "))
                    .compactMap { Int64($0.trimmingCharacters(in: .whitespacesAndNewlines)) }
                return .array(nums.map(JSONValue.int))
            }
            return .string(raw)
        }

        var body: some View {
            VStack(alignment: .leading, spacing: 4) {
                Text(label)
                    .font(DS.fontCompactMedium)
                    .foregroundStyle(DS.muted)
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
                .onAppear { text = formattedExternal }
                .onChange(of: value) { _, _ in
                    if parse(text) != (value ?? (kind == "numbers" ? .array([]) : .string(""))) {
                        text = formattedExternal
                    }
                }
                .onChange(of: text) { _, next in
                    let parsed = parse(next)
                    if parsed != value {
                        onCommit(parsed)
                    }
                }
            }
        }
    }

    private func mappingSection(
        title: String,
        isHeaders: Bool,
        isPrimaryModelMapping: Bool,
        presets: [CreationModelPreset],
        rows: Binding<[KeyValuePairDraft]>
    ) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                Text(title)
                    .font(DS.fontCompactSemibold)
                    .foregroundStyle(DS.ink)
                Spacer()
                if isPrimaryModelMapping {
                    Button("填入默认白名单") {
                        let defaults = AccountCreationCatalog.shared.models?[draft.platform] ?? []
                        rows.wrappedValue = defaults.map { KeyValuePairDraft(key: $0, value: $0) }
                    }
                    .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))

                    Button("不限制模型") {
                        rows.wrappedValue = []
                    }
                    .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
                }
                Button(isHeaders ? "+ 添加请求头" : "+ 添加映射") {
                    rows.wrappedValue.append(KeyValuePairDraft())
                }
                .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
            }

            if !isHeaders {
                Text("相同的请求与转发模型表示白名单；留空表示不限制。")
                    .font(DS.fontResetTime)
                    .foregroundStyle(DS.muted)
            }

            if !presets.isEmpty {
                LinearSelect(
                    label: "添加预设映射…",
                    accessibilityTitle: "添加预设映射",
                    height: 30,
                    expandWidth: true,
                    compactFont: true,
                    mutedLabel: true,
                    entries: presets.map { preset in
                        LinearMenuEntry(id: preset.id, title: preset.label) {
                            var next = rows.wrappedValue.filter { $0.key != preset.from }
                            next.append(KeyValuePairDraft(key: preset.from, value: preset.to))
                            rows.wrappedValue = next
                        }
                    }
                )
            }

            ForEach(rows) { $item in
                HStack(spacing: 6) {
                    TextField(
                        text: $item.key,
                        prompt: Text(verbatim: isHeaders ? "请求头名称" : "请求模型").foregroundStyle(DS.placeholder)
                    ) { EmptyView() }
                    .textFieldStyle(.plain)
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.ink)
                    .padding(.horizontal, 8)
                    .frame(height: 28)
                    .background(DS.surface)
                    .overlay(RoundedRectangle(cornerRadius: 5).strokeBorder(DS.line, lineWidth: 1))

                    Image(systemName: "arrow.right")
                        .font(DS.iconFont(10))
                        .foregroundStyle(DS.muted)

                    TextField(
                        text: $item.value,
                        prompt: Text(verbatim: isHeaders ? "请求头值" : "转发模型").foregroundStyle(DS.placeholder)
                    ) { EmptyView() }
                    .textFieldStyle(.plain)
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.ink)
                    .padding(.horizontal, 8)
                    .frame(height: 28)
                    .background(DS.surface)
                    .overlay(RoundedRectangle(cornerRadius: 5).strokeBorder(DS.line, lineWidth: 1))

                    LinearIconButton(systemName: "xmark", label: "删除行", size: 11, boxSize: 24) {
                        rows.wrappedValue.removeAll { $0.id == item.id }
                    }
                }
            }
        }
        .padding(10)
        .background(DS.surface)
        .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(DS.line, lineWidth: 1))
        .clipShape(RoundedRectangle(cornerRadius: 6))
    }
}
