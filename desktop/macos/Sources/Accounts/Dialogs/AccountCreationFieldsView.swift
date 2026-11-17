import SwiftUI

public struct AccountCreationFieldsView: View {
    @Binding public var draft: AccountCreationFormDraft

    public init(draft: Binding<AccountCreationFormDraft>) {
        self._draft = draft
    }

    private var groupedFields: [(group: String, items: [CreationFieldSpec])] {
        var order: [String] = []
        var map: [String: [CreationFieldSpec]] = [:]
        for field in draft.visibleFields where !["mapping", "headers"].contains(field.kind) {
            if map[field.group] == nil { order.append(field.group) }
            map[field.group, default: []].append(field)
        }
        return order.map { ($0, map[$0] ?? []) }
    }

    private var presets: [CreationModelPreset] {
        let key = draft.type == "bedrock" ? "bedrock" : draft.platform
        return AccountCreationCatalog.shared.mapping_presets[key] ?? []
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            ForEach(groupedFields, id: \.group) { section in
                VStack(alignment: .leading, spacing: 8) {
                    Text(section.group)
                        .font(DS.fontCompactSemibold)
                        .foregroundStyle(DS.muted)
                    ForEach(section.items) { spec in
                        fieldControl(spec)
                    }
                }
                .padding(12)
                .background(DS.surfaceSubtle)
                .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
            }

            if draft.visibleFields.contains(where: { $0.path == "credentials.model_mapping" }) {
                mappingSection(
                    title: "模型映射（可选）",
                    presets: presets,
                    rows: $draft.mappings
                )
            }

            if draft.visibleFields.contains(where: { $0.path == "credentials.header_overrides" }) {
                mappingSection(
                    title: "自定义请求头覆写",
                    presets: [],
                    rows: $draft.headers
                )
            }
        }
    }

    @ViewBuilder
    private func fieldControl(_ spec: CreationFieldSpec) -> some View {
        switch spec.kind {
        case "boolean", "stringbool":
            let isOn: Bool = {
                if spec.kind == "stringbool" {
                    return draft.values[spec.path]?.stringValue == "true"
                }
                return draft.values[spec.path]?.boolValue ?? false
            }()
            HStack {
                Text(spec.label)
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.ink)
                Spacer()
                LinearSwitch(isOn: isOn, label: spec.label) {
                    let next = !isOn
                    draft.values[spec.path] = spec.kind == "stringbool" ? .string(next ? "true" : "false") : .bool(next)
                }
            }

        case "select":
            let choices = spec.choices ?? []
            let binding = Binding<String>(
                get: { draft.values[spec.path]?.stringValue ?? choices.first ?? "" },
                set: { next in draft.values[spec.path] = .string(next) }
            )
            HStack {
                Text(spec.label)
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.ink)
                Spacer()
                Picker("", selection: binding) {
                    ForEach(choices, id: \.self) { choice in
                        Text(choice).tag(choice)
                    }
                }
                .labelsHidden()
                .frame(width: 190)
            }

        case "number":
            let binding = Binding<String>(
                get: {
                    if let i = draft.values[spec.path]?.intValue { return String(i) }
                    if let d = draft.values[spec.path]?.doubleValue { return String(d) }
                    return ""
                },
                set: { raw in
                    let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
                    if trimmed.isEmpty {
                        draft.values[spec.path] = .null
                    } else if let i = Int64(trimmed) {
                        draft.values[spec.path] = .int(i)
                    } else if let d = Double(trimmed) {
                        draft.values[spec.path] = .double(d)
                    }
                }
            )
            HStack {
                Text(spec.label)
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.ink)
                Spacer()
                TextField(text: binding, prompt: Text(verbatim: "未限制")) { EmptyView() }
                    .textFieldStyle(.plain)
                    .font(DS.fontMono12)
                    .monospacedDigit()
                    .padding(.horizontal, 8)
                    .frame(width: 190, height: DS.controlHeightCompact)
                    .background(DS.surface)
                    .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(DS.line, lineWidth: 1))
                    .clipShape(RoundedRectangle(cornerRadius: 6))
            }

        default:
            let binding = Binding<String>(
                get: { draft.values[spec.path]?.stringValue ?? "" },
                set: { next in draft.values[spec.path] = .string(next) }
            )
            VStack(alignment: .leading, spacing: 4) {
                Text(spec.label)
                    .font(DS.fontCompact)
                    .foregroundStyle(DS.ink)
                TextField(text: binding, prompt: Text(verbatim: spec.path)) { EmptyView() }
                    .textFieldStyle(.plain)
                    .font(DS.fontMono12)
                    .padding(.horizontal, 8)
                    .frame(height: DS.controlHeightCompact)
                    .background(DS.surface)
                    .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(DS.line, lineWidth: 1))
                    .clipShape(RoundedRectangle(cornerRadius: 6))
            }
        }
    }

    private func mappingSection(
        title: String,
        presets: [CreationModelPreset],
        rows: Binding<[KeyValuePairDraft]>
    ) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text(title)
                    .font(DS.fontCompactSemibold)
                    .foregroundStyle(DS.ink)
                Spacer()
                if !presets.isEmpty {
                    Menu("快速填入预设") {
                        ForEach(presets.prefix(24)) { preset in
                            Button("\(preset.label) (\(preset.from) -> \(preset.to))") {
                                rows.wrappedValue.append(KeyValuePairDraft(key: preset.from, value: preset.to))
                            }
                        }
                    }
                    .font(DS.fontCompact)
                    .frame(width: 120)
                }
                Button("添加一行") {
                    rows.wrappedValue.append(KeyValuePairDraft())
                }
                .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
            }

            ForEach(rows) { $item in
                HStack(spacing: 6) {
                    TextField(text: $item.key, prompt: Text(verbatim: "源名称")) { EmptyView() }
                        .textFieldStyle(.plain)
                        .font(DS.fontMono11)
                        .padding(.horizontal, 6)
                        .frame(height: 28)
                        .background(DS.surface)
                        .overlay(RoundedRectangle(cornerRadius: 5).strokeBorder(DS.line, lineWidth: 1))
                    Image(systemName: "arrow.right")
                        .font(DS.iconFont(10))
                        .foregroundStyle(DS.muted)
                    TextField(text: $item.value, prompt: Text(verbatim: "目标值")) { EmptyView() }
                        .textFieldStyle(.plain)
                        .font(DS.fontMono11)
                        .padding(.horizontal, 6)
                        .frame(height: 28)
                        .background(DS.surface)
                        .overlay(RoundedRectangle(cornerRadius: 5).strokeBorder(DS.line, lineWidth: 1))
                    LinearIconButton(systemName: "trash", label: "删除行", size: 11) {
                        rows.wrappedValue.removeAll { $0.id == item.id }
                    }
                }
            }
        }
        .padding(12)
        .background(DS.surfaceSubtle)
        .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
        .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
    }
}
