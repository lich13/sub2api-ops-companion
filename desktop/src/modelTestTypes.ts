import type {
  ModelFingerprintCandidateScore,
  ModelFingerprintReport,
  ModelTestClassification,
  ModelTestGroupStatus,
  ModelTestJob,
  ModelTestTokenizer,
  ModelTestTokenizerReport,
} from "./protocol";
import type { WireNull, WireReq } from "./types";

export const supportsModelDetection = (account: {
  platform: string;
  type: string;
}) =>
  (account.platform === "openai" &&
    ["oauth", "apikey"].includes(account.type)) ||
  (account.type === "apikey" &&
    ["anthropic", "grok", "deepseek"].includes(account.platform));

export type DetectionEngine = NonNullable<ModelTestJob["engine"]>;
export type VerdictMode = NonNullable<ModelTestJob["verdict_mode"]>;
export type ModelTestPreference = {
  engine: DetectionEngine;
  sample_count: 1 | 3;
};
export const engineName = (engine?: string | null) =>
  engine === "lm_detector" ? "LM Detector" : "ModelTrace";
export type FingerprintReport = Omit<
  WireNull<ModelFingerprintReport, never, "probability" | "ranking_score">,
  "results" | "bank_version"
> & {
  results?: WireNull<
    WireReq<ModelFingerprintCandidateScore, "display_name">,
    never,
    "score" | "probability"
  >[];
};
export type ModelTestResult = Omit<
  WireReq<
    ModelTestJob,
    | "account_id"
    | "account_name"
    | "requested_model"
    | "forwarded_model"
    | "returned_models"
    | "completed_groups"
    | "valid_groups"
    | "attempts"
    | "duration_ms"
  >,
  "groups" | "report" | "classification" | "tokenizer"
> & {
  groups?: (Omit<
    WireNull<
      WireReq<ModelTestGroupStatus, "attempts">,
      "ttft_ms" | "duration_ms"
    >,
    "report"
  > & {
    report?: FingerprintReport;
  })[];
  classification?: WireReq<
    ModelTestClassification,
    "reason" | "basis" | "group_indexes"
  >;
  tokenizer?: Omit<ModelTestTokenizer, "report"> & {
    report?: WireNull<ModelTestTokenizerReport, never, "claim"> | null;
  };
  report?: FingerprintReport | null;
};
