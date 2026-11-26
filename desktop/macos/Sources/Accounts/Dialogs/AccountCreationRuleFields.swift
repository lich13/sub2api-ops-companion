import SwiftUI

extension AccountCreationFieldsView {
    func protocolsSection(title: String) -> some View {
        collectionCard {
            HStack(spacing: 8) {
                Text(title)
                    .font(DS.fontCompactSemibold)
                    .foregroundStyle(DS.ink)
                Spacer()
                if !draft.urlPresets.isEmpty {
                    LinearSelect(
                        label: "填入预设地址",
                        accessibilityTitle: "\(title)预设",
                        height: DS.controlHeightDense,
                        width: 148,
                        compactFont: true,
                        mutedLabel: true,
                        entries: draft.urlPresets.map { preset in
                            LinearMenuEntry(
                                id: "\(preset.label):\(preset.protocolName ?? ""):\(preset.url)",
                                title: preset.label,
                                checked: draft.protocolURLs[preset.protocolName ?? ""] == preset.url
                            ) {
                                if let name = preset.protocolName, !name.isEmpty {
                                    draft.protocolURLs[name] = preset.url
                                }
                            }
                        }
                    )
                }
            }
            ForEach(draft.protocolNames, id: \.self) { name in
                HStack(spacing: 8) {
                    Text(AccountCreationCatalog.choiceLabel(name))
                        .font(DS.fontCompact)
                        .foregroundStyle(DS.muted)
                        .frame(width: 118, alignment: .leading)
                    ControlledCompactField(
                        value: draft.protocolURLs[name] ?? "",
                        prompt: "https://"
                    ) { next in
                        draft.protocolURLs[name] = next
                    }
                }
            }
        }
    }

    func protocolRulesSection(title: String) -> some View {
        let available = draft.protocolNames
        return collectionCard {
            HStack(spacing: 8) {
                Text(title)
                    .font(DS.fontCompactSemibold)
                    .foregroundStyle(DS.ink)
                Spacer()
                Button("+ 添加规则") {
                    draft.protocolRules.append(ProtocolRuleDraft(preferred: available.first ?? ""))
                }
                .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
            }
            ForEach(Array(draft.protocolRules.enumerated()), id: \.element.id) { index, rule in
                VStack(alignment: .leading, spacing: 8) {
                    HStack(alignment: .bottom, spacing: 8) {
                        captioned("模型匹配") {
                            ControlledCompactField(
                                value: rule.pattern,
                                prompt: "gpt-*"
                            ) { next in
                                updateProtocolRule(rule.id) { $0.pattern = next }
                            }
                        }
                        captioned("首选协议") {
                            LinearSelect(
                                label: AccountCreationCatalog.choiceLabel(rule.preferred),
                                accessibilityTitle: "规则协议 \(index + 1)",
                                height: DS.controlHeightDense,
                                width: 168,
                                compactFont: true,
                                entries: available.map { name in
                                    LinearMenuEntry(
                                        id: name,
                                        title: AccountCreationCatalog.choiceLabel(name),
                                        checked: rule.preferred == name
                                    ) {
                                        updateProtocolRule(rule.id) { item in
                                            item.also = item.also.filter { $0 != name }
                                            item.preferred = name
                                        }
                                    }
                                }
                            )
                        }
                        ruleControls(
                            index: index,
                            count: draft.protocolRules.count,
                            move: { draft.protocolRules.swapAt(index, index + $0) },
                            remove: { draft.protocolRules.removeAll { $0.id == rule.id } }
                        )
                        .frame(height: DS.controlHeightDense)
                    }
                    HStack(spacing: 12) {
                        Text("同时支持")
                            .font(DS.fontResetTime)
                            .foregroundStyle(DS.muted)
                        ForEach(available.filter { $0 != rule.preferred }, id: \.self) { name in
                            LinearCheckbox(
                                isChecked: rule.also.contains(name),
                                label: AccountCreationCatalog.choiceLabel(name)
                            ) {
                                updateProtocolRule(rule.id) { item in
                                    if item.also.contains(name) {
                                        item.also.removeAll { $0 == name }
                                    } else {
                                        item.also.append(name)
                                    }
                                }
                            }
                        }
                        Spacer(minLength: 0)
                    }
                }
                .padding(8)
                .background(DS.surfaceSubtle)
                .clipShape(RoundedRectangle(cornerRadius: 6))
            }
        }
    }

    private func updateProtocolRule(_ id: UUID, _ change: (inout ProtocolRuleDraft) -> Void) {
        guard let index = draft.protocolRules.firstIndex(where: { $0.id == id }) else { return }
        change(&draft.protocolRules[index])
    }

    private func updatePauseRule(_ id: UUID, _ change: (inout PauseRuleDraft) -> Void) {
        guard let index = draft.pauseRules.firstIndex(where: { $0.id == id }) else { return }
        change(&draft.pauseRules[index])
    }

    func pauseRulesSection(title: String) -> some View {
        collectionCard {
            HStack(spacing: 8) {
                Text(title)
                    .font(DS.fontCompactSemibold)
                    .foregroundStyle(DS.ink)
                Spacer()
                Button("+ 添加规则") {
                    draft.pauseRules.append(PauseRuleDraft())
                }
                .buttonStyle(LinearButtonStyle(variant: .secondary, compact: true))
            }
            if draft.pauseRules.isEmpty {
                Text("尚未添加规则；命中状态码与关键词时，按规则暂停调度。")
                    .font(DS.fontResetTime)
                    .foregroundStyle(DS.muted)
            }
            ForEach(Array(draft.pauseRules.enumerated()), id: \.element.id) { index, rule in
                VStack(alignment: .leading, spacing: 8) {
                    HStack(alignment: .bottom, spacing: 8) {
                        captioned("停用范围") {
                            LinearSelect(
                                label: AccountCreationCatalog.choiceLabel(rule.scope),
                                accessibilityTitle: "停用范围 \(index + 1)",
                                height: DS.controlHeightDense,
                                width: 104,
                                compactFont: true,
                                entries: ["model", "account"].map { scope in
                                    LinearMenuEntry(
                                        id: scope,
                                        title: AccountCreationCatalog.choiceLabel(scope),
                                        checked: rule.scope == scope
                                    ) {
                                        updatePauseRule(rule.id) { $0.scope = scope }
                                    }
                                }
                            )
                        }
                        captioned("状态码") {
                            ControlledCompactField(value: rule.errorCode, prompt: "429", width: 84) { next in
                                updatePauseRule(rule.id) { $0.errorCode = next }
                            }
                        }
                        captioned("停用分钟") {
                            ControlledCompactField(value: rule.durationMinutes, prompt: "30", width: 84) { next in
                                updatePauseRule(rule.id) { $0.durationMinutes = next }
                            }
                        }
                        Spacer(minLength: 0)
                        ruleControls(
                            index: index,
                            count: draft.pauseRules.count,
                            move: { draft.pauseRules.swapAt(index, index + $0) },
                            remove: { draft.pauseRules.removeAll { $0.id == rule.id } }
                        )
                        .frame(height: DS.controlHeightDense)
                    }
                    HStack(alignment: .bottom, spacing: 8) {
                        captioned("关键词（逗号分隔）") {
                            ControlledCompactField(value: rule.keywords, prompt: "rate limit, overloaded") { next in
                                updatePauseRule(rule.id) { $0.keywords = next }
                            }
                        }
                        captioned("说明") {
                            ControlledCompactField(value: rule.description, prompt: "可选") { next in
                                updatePauseRule(rule.id) { $0.description = next }
                            }
                        }
                    }
                }
                .padding(8)
                .background(DS.surfaceSubtle)
                .clipShape(RoundedRectangle(cornerRadius: 6))
            }
        }
    }

    func mappingSection(
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
            } else if draft.type == "apikey" {
                HStack(spacing: 6) {
                    Text("可用变量：")
                        .font(DS.fontResetTime)
                        .foregroundStyle(DS.muted)
                    ForEach(AccountCreationFormDraft.headerOverrideVariables, id: \.self) { variable in
                        let token = "{{\(variable)}}"
                        Button(token) {
                            AccountCreationFormDraft.applyHeaderOverrideVariable(
                                variable,
                                into: &rows.wrappedValue,
                                focusedId: focusedHeaderValueId
                            )
                        }
                        .buttonStyle(LinearButtonStyle(variant: .ghost, compact: true))
                    }
                    Spacer(minLength: 0)
                }
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
                    .focused($focusedHeaderValueId, equals: item.id)
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
