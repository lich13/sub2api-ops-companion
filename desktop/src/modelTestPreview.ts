import type { Account } from "./types";
type Job = { id: string; request_id: string; account_id: number; account_name: string; requested_model: string; forwarded_model: string; returned_models: string[]; status: string; completed_groups: number; valid_groups: number; attempts: number; duration_ms: number; concurrency: number; started: number; error: string; groups: { index: number; status: string; attempts: number; ttft_ms: number | null; duration_ms: number | null; diagnostics?: Record<string, string | number> }[]; report?: { prediction_name: string; probability: number; used_outputs: number } };
const jobs = new Map<number, Job>();
export function modelTestPreview(method: string, path: string, body: Record<string, unknown>, accounts: Account[]) {
  const aid = Number(path.split("/")[2]);
  if (path.startsWith("/accounts/")) {
    if (path.endsWith("/latest")) return structuredClone(jobs.get(aid) ?? null);
    const old = jobs.get(aid);
    if (old?.request_id === body.request_id) return structuredClone(old);
    if (old?.status === "running") throw new Error("此账号已有测试正在执行");
    const job: Job = { id: crypto.randomUUID().replaceAll("-", ""), request_id: String(body.request_id), account_id: aid,
      account_name: accounts.find((a) => a.id === aid)?.name ?? "账号", requested_model: String(body.model_id), forwarded_model: String(body.model_id),
      returned_models: [], status: "running", completed_groups: 0, valid_groups: 0, attempts: 0, duration_ms: 0,
      concurrency: Number(body.concurrency) || 1, started: Date.now(), error: "", groups: [1, 2, 3].map((index) => ({ index, status: "queued", attempts: 0, ttft_ms: null, duration_ms: null })) };
    jobs.set(aid, job); return structuredClone(job);
  }
  const job = [...jobs.values()].find((value) => value.id === path.split("/")[2]);
  if (!job) throw new Error("测试结果不存在");
  if (method === "POST" && path.endsWith("/cancel")) { job.status = "cancelled"; job.error = "测试已停止"; }
  if (job.status === "running") {
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
    if (job.completed_groups === 3) job.status = "completed";
  }
  return structuredClone(job);
}
