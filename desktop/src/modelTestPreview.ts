import type { ModelTestResult, FingerprintReport } from "./modelTestTypes";
import type { Account } from "./types";
export function modelTestCandidates(account: Account) {
  if (account.platform === "anthropic") return ["claude-opus-4-6", "claude-sonnet-4-6"].map(id => ({ id, display_name: id, type: "model" }));
  if (account.platform === "deepseek") return ["deepseek-v4-pro", "deepseek-v4-flash"].map(id => ({ id, display_name: id, type: "model" }));
  if (account.platform === "grok") return ["grok-4.5", "grok-imagine-image", "grok-imagine-video", "grok-search", "grok-tts"].map((id) => ({ id, display_name: id, type: "model" }));
  if (account.platform !== "openai") throw new Error("该平台没有模型目录");
  return (account.type === "apikey" ? ["gpt-6-sol", "gpt-6-luna"] : [
    "gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna", "gpt-6.1-sol", "gpt-6-sol", "gpt-6-luna", "gpt-6-astra", "codex-auto-review",
  ]).map((id) => ({ id, display_name: id, type: "model" }));
}
type Job = ModelTestResult & { request_id: string; started: number; concurrency: number; groups: NonNullable<ModelTestResult["groups"]> };
const jobs = new Map<number, Job>();
export function modelTestPreview(method: string, path: string, body: Record<string, unknown>, accounts: Account[]) {
  const aid = Number(path.split("/")[2]);
  if (path.startsWith("/accounts/")) {
    if (path.endsWith("/latest")) {
      const scenario = new URLSearchParams(location.search).get("scenario") ?? "";
      if (!jobs.has(aid) && ["lm-completed", "lm-running", "lm-failed", "lm-tokenizer", "lm-offline"].includes(scenario)) {
        const created = modelTestPreview("POST", `/accounts/${aid}/model-tests`, { model_id: "gpt-6-astra", request_id: "fixture-lm", engine: "lm_detector", sample_count: 3, concurrency: 3, tokenizer_probe: true }, accounts) as Job;
        const saved = jobs.get(aid)!; saved.started -= 15000; saved.request_id = "fixture-seeding";
        modelTestPreview("GET", `/model-tests/${created.id}`, {}, accounts); saved.request_id = "fixture-lm";
        if (scenario === "lm-failed") { saved.status = "failed"; saved.error = "上游连接失败，未取得完整样本。请检查账号连接与代理设置后重新测试。This is a synthetic long error body used to check wrapping and reachable actions."; saved.groups[2].status = "failed"; saved.tokenizer = undefined; }
        if (scenario === "lm-running") { saved.status = "running"; saved.phase = "sampling"; saved.completed_groups = 2; saved.groups[2].status = "receiving"; saved.tokenizer = undefined; }
        if (scenario === "lm-tokenizer") { saved.status = "running"; saved.phase = "tokenizer"; saved.tokenizer!.status = "running"; }
      }
      return structuredClone(jobs.get(aid) ?? null);
    }
    const old = jobs.get(aid);
    if (old?.request_id === body.request_id) return structuredClone(old);
    if (old?.status === "running") throw new Error("此账号已有测试正在执行");
    const lm = body.engine === "lm_detector", count = lm ? Number(body.sample_count) || 1 : 3;
    const job: Job = { id: crypto.randomUUID().replaceAll("-", ""), request_id: String(body.request_id), account_id: aid,
      account_name: accounts.find((a) => a.id === aid)?.name ?? "账号", requested_model: String(body.model_id), forwarded_model: String(body.model_id),
      returned_models: [], status: "running", completed_groups: 0, valid_groups: 0, attempts: 0, duration_ms: 0,
      engine: lm ? "lm_detector" : "modeltrace", planned_groups: count, tokenizer_probe: lm && body.tokenizer_probe === true,
      bank_version: { revision: "a".repeat(40), sha256: "b".repeat(64), built_at: "2026-10-24T00:00:00Z", analyzer_version: lm ? "lmfpd-0.2.0" : 1 },
      concurrency: Number(body.concurrency) || 1, started: Date.now(), error: "", groups: Array.from({ length: count }, (_, i) => i + 1).map((index) => ({ index, status: "queued", attempts: 0, ttft_ms: null, duration_ms: null })) };
    jobs.set(aid, job); return structuredClone(job);
  }
  const job = [...jobs.values()].find((value) => value.id === path.split("/")[2]);
  if (!job) throw new Error("测试结果不存在");
  if (method === "POST" && path.endsWith("/cancel")) { job.status = job.phase === "tokenizer" ? "completed" : "cancelled"; if (job.tokenizer) job.tokenizer.status = "cancelled"; else job.error = "测试已停止"; }
  if (job.status === "running" && (job.request_id === "fixture-seeding" || !new URLSearchParams(location.search).get("scenario")?.startsWith("lm-"))) {
    job.duration_ms = Date.now() - job.started;
    for (const group of job.groups) {
      const elapsed = job.duration_ms - Math.floor((group.index - 1) / job.concurrency) * 7000;
      group.status = elapsed >= 7000 ? "completed" : elapsed >= 0 ? "running" : "queued";
      group.attempts = elapsed >= 0 ? 1 : 0; group.ttft_ms = elapsed >= 1500 ? 1500 : null; group.duration_ms = elapsed >= 7000 ? 7000 : null;
      if (elapsed >= 7000) group.diagnostics = { protocol: 'responses', http_status: 200, content_type: 'text/event-stream',
        first_event_type: 'response.created', last_event_type: 'response.completed', first_event_ms: 300,
        output_tokens: 1500, reasoning_tokens: 100, max_output_tokens: 4096, bytes: 8500, end_reason: 'completed' };
    }
    job.attempts = job.groups.reduce((sum, g) => sum + g.attempts, 0);
    job.completed_groups = job.groups.filter((g) => g.status === "completed").length;
    job.valid_groups = job.completed_groups;
    if (job.completed_groups) { job.returned_models = [job.requested_model]; job.report = { prediction_name: "gpt-6-luna", probability: .86, used_outputs: job.completed_groups }; }
    if (job.engine === "lm_detector" && job.completed_groups) {
      const names = ["GPT-6 Astra / GPT-6.1 Sol", "Claude Opus 5", "GPT-6 Sol", "Grok 4", "DeepSeek V4", "Qwen3", "Gemini 3 Pro"];
      if (new URLSearchParams(location.search).get("scenario") === "lm-completed") names.push(...Array.from({ length: 55 }, (_, i) => `Long-model-name-with-multiple-merged-variants-for-layout-fixture-${i + 1}`));
      const report: FingerprintReport = { prediction: "gpt-6-astra", prediction_name: names[0], decision: job.completed_groups < 3 ? "partial" : "scored", merged_class: true, probability: job.completed_groups < 3 ? null : .81, used_outputs: job.completed_groups,
        results: names.map((name, i) => ({ model: `fixture-${i}`, display_name: name, score: .875123 - i * .05, probability: job.completed_groups < 3 ? null : i === 0 ? .81 : .03 })) };
      job.report = report; for (const g of job.groups) if (g.status === "completed") g.report = { ...report, decision: "partial", probability: null, used_outputs: 1 };
    }
    if (job.completed_groups === job.planned_groups) {
      job.status = "completed";
      if (job.tokenizer_probe) {
        const done = job.duration_ms >= Math.ceil(job.planned_groups / job.concurrency) * 7000 + 7000;
        job.phase = done ? "finished" : "tokenizer"; job.status = done ? "completed" : "running";
        job.tokenizer = { status: done ? "completed" : "running", requests: done ? 7 : 3, report: { kind: "known", name: "OpenAI o200k", lab: "OpenAI", confidence: .99, unknown: .01, claim: { status: "consistent", probability: .99 }, candidates: [{ id: "o200k", name: "OpenAI o200k", probability: .99 }] } };
      }
    }
  }
  return structuredClone(job);
}
