// Imported only by the development preview transport.
import { mergeFields } from "./ModelConfig";

const descriptor = (slug: string) => ({
  slug,
  display_name: slug,
  description: "",
  context_window: 272000,
  max_context_window: 400000,
  supported_reasoning_levels: [
    { effort: "low", description: "" },
    { effort: "high", description: "" },
  ],
  default_reasoning_level: "high",
  input_modalities: ["text", "image"],
  supports_parallel_tool_calls: true,
});
type PreviewState = {
  baseline: { models: ReturnType<typeof descriptor>[] };
  version: string;
  revision: string;
  allowlist: { enabled: boolean; models: string[] };
  overrides: Record<string, Record<string, unknown>>;
};
const states: Record<number, PreviewState> = Object.fromEntries(
  [
    [1, ["gpt-6-astra", "gpt-6-sol", "gpt-5.6-luna"]],
    [2, ["grok-4.6", "grok-4.6-fast"]],
  ].map(([id, models]) => {
    const baseline = { models: (models as string[]).map(descriptor) };
    return [
      id,
      {
        baseline,
        version: "a".repeat(64),
        revision: "b".repeat(64),
        allowlist: {
          enabled: true,
          models: baseline.models.map((m) => m.slug),
        },
        overrides: {} as Record<string, Record<string, unknown>>,
      },
    ];
  }),
);
export function modelPreview(
  method: string,
  path: string,
  payload: Record<string, unknown>,
) {
  if (path === "/model-groups")
    return {
      groups: [
        { id: 1, name: "Codex · 主力", platform: "openai" },
        { id: 2, name: "Grok · 开发", platform: "grok" },
      ],
      status: { state: "ready", message: "" },
    };
  if (path === "/model-catalog")
    return {
      items: [
        {
          provider: "openai",
          provider_name: "OpenAI",
          id: "gpt-6-astra",
          name: "GPT-6 Astra",
          fields: { context_window: 1000000, max_context_window: 1000000 },
        },
        {
          provider: "xai",
          provider_name: "xAI",
          id: "grok-4.6",
          name: "Grok 4.6",
          fields: { context_window: 256000, max_context_window: 256000 },
        },
      ],
    };
  const groupId = Number(path.split("/")[2]);
  const state = states[groupId] || states[1];
  const { baseline, version, revision, allowlist, overrides } = state;
  const effective = (draft = allowlist, patches = overrides) => ({
    models: baseline.models
      .filter((m) => !draft.enabled || draft.models.includes(m.slug))
      .map((m) => mergeFields(m, patches[m.slug] || {})),
  });
  if (path.endsWith("/preview"))
    return {
      effective: effective(
        payload.allowlist as typeof allowlist,
        payload.overrides as typeof overrides,
      ),
      pending_models: [],
      baseline_status: "native",
    };
  if (path.endsWith("/upstream-import"))
    return {
      fields: {
        description: "Imported upstream model",
        supports_parallel_tool_calls: false,
      },
      source: "upstream",
    };
  if (path.endsWith("/allowlist") && method === "PUT") {
    state.allowlist = payload.allowlist as typeof allowlist;
    state.version = "c".repeat(64);
    return { version: state.version, model_allowlist: state.allowlist };
  }
  if (path.endsWith("/overrides") && method === "PUT") {
    state.overrides = payload.overrides as typeof overrides;
    state.revision = "d".repeat(64);
    return {
      revision: state.revision,
      overrides: state.overrides,
      updated_at: new Date().toISOString(),
    };
  }
  return {
    group: {
      id: groupId,
      name: groupId === 1 ? "Codex · 主力" : "Grok · 开发",
      platform: groupId === 1 ? "openai" : "grok",
      version,
      model_allowlist: allowlist,
    },
    revision,
    overrides,
    candidates: baseline.models.map((m) => m.slug),
    baseline,
    effective: effective(),
    baseline_status: "native",
    status: { state: "ready", message: "" },
  };
}
