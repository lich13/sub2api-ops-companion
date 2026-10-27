import type { ModelTestPreference } from "./modelTestTypes";

const platforms = [["openai", "OpenAI"], ["anthropic", "Claude"], ["grok", "Grok"], ["deepseek", "DeepSeek"]] as const;
export default function DetectionPlatformSettings({ value, set }: { value: unknown; set: (value: unknown) => void }) {
  const saved = (value ?? {}) as Record<string, ModelTestPreference>;
  return <>{platforms.map(([platform, label]) => {
    const selected = saved[platform] ?? { engine: platform === "openai" ? "modeltrace" : "lm_detector", sample_count: 1 };
    const update = (change: Partial<ModelTestPreference>) => set({ ...saved, [platform]: { ...selected, ...change } });
    return <div className="detection-platform-settings" key={platform}>
      <div><strong>{label} 定时检测</strong></div>
      <label>检测源<select aria-label={`${label} 定时检测源`} value={selected.engine} disabled={platform !== "openai"} onChange={e => update({ engine: e.target.value as ModelTestPreference["engine"] })}>{platform === "openai" && <option value="modeltrace">ModelTrace</option>}<option value="lm_detector">LM Detector</option></select></label>
      {selected.engine === "lm_detector" && <label>组数<select aria-label={`${label} 定时采样组数`} value={selected.sample_count} onChange={e => update({ sample_count: Number(e.target.value) as 1 | 3 })}><option value={1}>1 组</option><option value={3}>3 组</option></select></label>}
    </div>;
  })}</>;
}
