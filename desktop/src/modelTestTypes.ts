import type { BankVersion } from "./FingerprintBankStatus";

export type DetectionEngine = "modeltrace" | "lm_detector";
export type ModelTestPreference = { engine: DetectionEngine; sample_count: 1 | 3 };
export const engineName = (engine?: string | null) => engine === "lm_detector" ? "LM Detector" : "ModelTrace";
export type FingerprintReport = {
  prediction?: string; prediction_name?: string; probability?: number | null; used_outputs?: number;
  decision?: string; merged_class?: boolean; ranking_score?: number | null;
  results?: { model: string; display_name: string; score: number; probability?: number | null }[];
};
export type ModelTestResult = {
  id: string; account_id: number; account_name: string; requested_model: string;
  forwarded_model: string; returned_models: string[]; status: string; phase?: string; automatic?: boolean;
  engine?: DetectionEngine; tokenizer_probe?: boolean; planned_groups?: number; completed_groups: number; valid_groups: number; attempts: number;
  concurrency?: number; groups?: { index: number; status: string; attempts: number; ttft_ms: number | null; duration_ms: number | null; error?: string; report?: FingerprintReport;
    diagnostics?: { protocol?: string; response_protocol?: string; http_status?: number; content_type?: string; first_event_ms?: number; first_event_type?: string; last_event_type?: string; headers_ms?: number; bytes?: number; end_reason?: string; input_tokens?: number; output_tokens?: number; reasoning_tokens?: number; max_output_tokens?: number } }[];
  bank_version?: BankVersion; automatic_disposition?: { basis?: "first_group" | "aggregate" | "sample_family"; group_indexes?: number[]; kind?: string; status?: string; marked?: boolean; schedule_verified?: boolean; reason?: string };
  classification?: { outcome: string; reason: string; basis: string; group_indexes: number[] };
  tokenizer?: { status: string; requests?: number; baseline_drift?: boolean; error?: string; report?: {
    kind: string; name: string; lab: string; confidence: number; unknown: number;
    claim?: { status: string; probability: number } | null;
    candidates: { id: string; name: string; probability: number }[];
  } | null };
  can_retry?: boolean; completion_reason?: string; duration_ms: number; error?: string; report?: FingerprintReport | null;
};
