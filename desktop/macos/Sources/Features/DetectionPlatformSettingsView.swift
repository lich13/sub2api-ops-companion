import SwiftUI

/// Matches `DetectionPlatformSettings` in `desktop/src/DetectionPlatformSettings.tsx` (`openai` supports ModelTrace/LM Detector; others lock LM Detector).
public struct DetectionPlatformSettingsView: View {
    public let value: JSONValue?
    public let online: Bool
    public let connectionKey: String
    public let onChange: (JSONValue) -> Void

    public static let platforms: [(key: String, label: String)] = [
        ("openai", "OpenAI"),
        ("anthropic", "Claude"),
        ("grok", "Grok"),
        ("deepseek", "DeepSeek"),
    ]

    public init(
        value: JSONValue?,
        online: Bool,
        connectionKey: String,
        onChange: @escaping (JSONValue) -> Void
    ) {
        self.value = value
        self.online = online
        self.connectionKey = connectionKey
        self.onChange = onChange
    }

    private func entry(for key: String) -> (engine: String, sampleCount: Int) {
        let obj = value?.objectValue?[key]?.objectValue
        let defaultEngine = (key == "openai") ? "modeltrace" : "lm_detector"
        let rawEngine = obj?["engine"]?.stringValue ?? defaultEngine
        let effectiveEngine = (key == "openai") ? (rawEngine == "lm_detector" ? "lm_detector" : "modeltrace") : "lm_detector"
        let rawSamples = obj?["sample_count"]?.intValue ?? 1
        return (effectiveEngine, rawSamples == 3 ? 3 : 1)
    }

    private func updatePlatform(_ key: String, engine: String, sampleCount: Int) {
        var nextMap: [String: JSONValue] = value?.objectValue ?? [:]
        for (pKey, _) in Self.platforms {
            let cur = entry(for: pKey)
            if pKey == key {
                nextMap[pKey] = .object([
                    "engine": .string(pKey == "openai" ? engine : "lm_detector"),
                    "sample_count": .int(Int64(sampleCount == 3 ? 3 : 1)),
                ])
            } else if nextMap[pKey] == nil {
                nextMap[pKey] = .object([
                    "engine": .string(cur.engine),
                    "sample_count": .int(Int64(cur.sampleCount)),
                ])
            }
        }
        onChange(.object(nextMap))
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            LazyVGrid(columns: [GridItem(.flexible(), spacing: 12), GridItem(.flexible(), spacing: 12)], spacing: 12) {
                ForEach(Self.platforms, id: \.key) { item in
                    let cur = entry(for: item.key)
                    let isOpenAI = (item.key == "openai")

                    VStack(alignment: .leading, spacing: 8) {
                        Text("\(item.label) 定时检测")
                            .font(DS.fontCompactSemibold)
                            .foregroundStyle(DS.ink)

                        HStack(spacing: 8) {
                            VStack(alignment: .leading, spacing: 4) {
                                Text("检测源")
                                    .font(DS.fontResetTime)
                                    .foregroundStyle(DS.muted)
                                LinearSelect(
                                    label: cur.engine == "lm_detector" ? "LM Detector" : "ModelTrace",
                                    accessibilityTitle: "\(item.label) 定时检测源",
                                    height: 32,
                                    expandWidth: true,
                                    compactFont: true,
                                    disabled: !isOpenAI,
                                    entries: (isOpenAI
                                        ? [("modeltrace", "ModelTrace"), ("lm_detector", "LM Detector")]
                                        : [("lm_detector", "LM Detector")]
                                    ).map { opt in
                                        LinearMenuEntry(id: opt.0, title: opt.1, checked: cur.engine == opt.0) {
                                            updatePlatform(item.key, engine: opt.0, sampleCount: cur.sampleCount)
                                        }
                                    }
                                )
                            }
                            .frame(maxWidth: .infinity)

                            if cur.engine == "lm_detector" {
                                VStack(alignment: .leading, spacing: 4) {
                                    Text("组数")
                                        .font(DS.fontResetTime)
                                        .foregroundStyle(DS.muted)
                                    LinearSelect(
                                        label: cur.sampleCount == 3 ? "3 组" : "1 组",
                                        accessibilityTitle: "\(item.label) 定时采样组数",
                                        height: 32,
                                        width: 84,
                                        compactFont: true,
                                        entries: [
                                            LinearMenuEntry(id: "1", title: "1 组", checked: cur.sampleCount == 1) {
                                                updatePlatform(item.key, engine: cur.engine, sampleCount: 1)
                                            },
                                            LinearMenuEntry(id: "3", title: "3 组", checked: cur.sampleCount == 3) {
                                                updatePlatform(item.key, engine: cur.engine, sampleCount: 3)
                                            },
                                        ]
                                    )
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

            FingerprintBankStatusView(online: online, connectionKey: connectionKey)
        }
    }
}
