import SwiftUI

/// Matches `DetectionPlatformSettings` in `desktop/src/DetectionPlatformSettings.tsx`.
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
        let engine = obj?["engine"]?.stringValue ?? "modeltrace"
        let rawSamples = obj?["sample_count"]?.intValue ?? 3
        return (engine == "lm_detector" ? "lm_detector" : "modeltrace", rawSamples == 1 ? 1 : 3)
    }

    private func updatePlatform(_ key: String, engine: String, sampleCount: Int) {
        var nextMap: [String: JSONValue] = [:]
        for (pKey, _) in Self.platforms {
            let cur = entry(for: pKey)
            if pKey == key {
                nextMap[pKey] = .object([
                    "engine": .string(engine),
                    "sample_count": .int(Int64(sampleCount)),
                ])
            } else {
                nextMap[pKey] = .object([
                    "engine": .string(cur.engine),
                    "sample_count": .int(Int64(cur.sampleCount)),
                ])
            }
        }
        onChange(.object(nextMap))
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            VStack(alignment: .leading, spacing: 2) {
                Text("平台自动检测源与采样")
                    .font(DS.fontBodySemibold)
                    .foregroundStyle(DS.ink)
                Text("按平台分别选择指纹检测引擎与采样组数（3 组完整核验或 1 组快速抽检）。")
                    .font(DS.fontResetTime)
                    .foregroundStyle(DS.muted)
            }

            LazyVGrid(columns: [GridItem(.flexible(), spacing: 10), GridItem(.flexible(), spacing: 10)], spacing: 10) {
                ForEach(Self.platforms, id: \.key) { item in
                    let cur = entry(for: item.key)
                    VStack(alignment: .leading, spacing: 8) {
                        HStack {
                            Text(item.label)
                                .font(DS.fontCompactSemibold)
                                .foregroundStyle(DS.ink)
                            Spacer()
                            Text(cur.engine == "lm_detector" ? "LM Detector" : "ModelTrace")
                                .font(DS.fontResetTimeSemibold)
                                .foregroundStyle(DS.accent)
                                .padding(.horizontal, 6)
                                .padding(.vertical, 2)
                                .background(DS.accentBg)
                                .clipShape(RoundedRectangle(cornerRadius: 4))
                        }

                        HStack(spacing: 6) {
                            Text("检测源")
                                .font(DS.fontResetTime)
                                .foregroundStyle(DS.muted)
                            Spacer()
                            Button("ModelTrace") {
                                updatePlatform(item.key, engine: "modeltrace", sampleCount: cur.sampleCount)
                            }
                            .buttonStyle(LinearButtonStyle(variant: cur.engine == "modeltrace" ? .selectedGhost : .ghost, compact: true))
                            Button("LM Detector") {
                                updatePlatform(item.key, engine: "lm_detector", sampleCount: cur.sampleCount)
                            }
                            .buttonStyle(LinearButtonStyle(variant: cur.engine == "lm_detector" ? .selectedGhost : .ghost, compact: true))
                        }

                        HStack(spacing: 6) {
                            Text("采样组数")
                                .font(DS.fontResetTime)
                                .foregroundStyle(DS.muted)
                            Spacer()
                            Button("3 组（完整）") {
                                updatePlatform(item.key, engine: cur.engine, sampleCount: 3)
                            }
                            .buttonStyle(LinearButtonStyle(variant: cur.sampleCount == 3 ? .selectedGhost : .ghost, compact: true))
                            Button("1 组（快速）") {
                                updatePlatform(item.key, engine: cur.engine, sampleCount: 1)
                            }
                            .buttonStyle(LinearButtonStyle(variant: cur.sampleCount == 1 ? .selectedGhost : .ghost, compact: true))
                        }
                    }
                    .padding(10)
                    .background(DS.surfaceSubtle)
                    .overlay(RoundedRectangle(cornerRadius: DS.cornerRadius).strokeBorder(DS.line, lineWidth: 1))
                    .clipShape(RoundedRectangle(cornerRadius: DS.cornerRadius))
                }
            }

            FingerprintBankStatusView(online: online, connectionKey: connectionKey)
        }
        .padding(.top, 4)
    }
}
