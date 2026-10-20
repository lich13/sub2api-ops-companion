import type { DetectionLog } from "./useDetectionLogFeed";
const records: DetectionLog[] = Array.from({ length: 54 }, (_, index) => {
  const status = ["completed", "completed", "failed", "cancelled", "interrupted", "needs_confirmation"][index % 6];
  const degraded = index % 6 === 1;
  const at = new Date(Date.UTC(2026, 9, 20, 12) - index * 60_000).toISOString();
  return { job_id: (index + 1).toString(16).padStart(32, "0"), account_id: 101, account_name: "Codex · 主力", model_id: "gpt-6-luna",
    status, created_at: at, started_at: at, completed_at: at, triggers: [index % 3 ? "scheduled" : "recovery"], error: "", details_available: false,
    summary: { outcome: status === "completed" ? degraded ? "degraded" : "normal" : status,
      analysis_available: status === "completed", first_prediction: status === "completed" ? "预览模型" : "", prediction: "",
      probability: status === "completed" ? 0.92 : null, used_outputs: status === "completed" ? 3 : null,
      first_downgrade: status === "completed" ? degraded : null, aggregate_downgrade: status === "completed" ? degraded : null,
      disposition: degraded ? { status: "completed", marked: true, schedule_verified: true, mark_cleared: null, reason: "", completed_at: at } : null, recovery: null },
    notification: degraded ? { status: "delivered", merged: false, delivered_at: at, reason: "" } : null };
});
export function detectionLogPreview(path: string) {
  const params = new URLSearchParams(path.split("?")[1]);
  const start = Number(params.get("before") || 0), limit = Math.min(Number(params.get("limit") || 50), 100);
  return structuredClone({ items: records.slice(start, start + limit), next_cursor: start + limit < records.length ? String(start + limit) : null });
}
