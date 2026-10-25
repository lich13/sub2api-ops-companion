import type { ModelTestPreference } from "./modelTestTypes";

export default function DetectionPlatformSettings({ value, set }: { value: unknown; set: (value: unknown) => void }) {
  const selected = (value as { openai?: ModelTestPreference })?.openai ?? { engine: "modeltrace", sample_count: 1 };
  return <div className="detection-platform-settings">
    <div><strong>OpenAI 定时检测</strong><p className="muted">用于定时检测及降智复查，新建任务生效。</p></div>
    <label>检测源<select aria-label="OpenAI 定时检测源" value={selected.engine} onChange={e => set({ openai: { ...selected, engine: e.target.value } })}><option value="modeltrace">ModelTrace</option><option value="lm_detector">LM Detector</option></select></label>
    {selected.engine === "lm_detector" && <label>组数<select aria-label="OpenAI 定时采样组数" value={selected.sample_count} onChange={e => set({ openai: { ...selected, sample_count: Number(e.target.value) } })}><option value={1}>1 组</option><option value={3}>3 组</option></select></label>}
  </div>;
}
