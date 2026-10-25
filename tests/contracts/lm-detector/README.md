# LM Detector reference fixtures

`analyze.json` uses the upstream SDK conformance inputs at `Ikaleio/lm-detector@567e11d65a48d4fbba2713cc4dfbe685cdd05a73`, under the MIT license. Original input file SHA-256: `16e97c9b7c18a002827307400edb2f974b0493dfb1b567cfd33b0cf77384728f`. These are upstream reference samples, with no account data. Upstream stored expectations still refer to the earlier 03:33 bank; the expectations here were recomputed with the fixed commit's official TypeScript SDK `analyzeOutputs` and the bundled 14:34 bank and detector. Rust must match within the upstream relative tolerance of 1e-9.

`tokenizer.json` records the same commit's `shared/tokenizer-posterior.ts` on fixed input-token counts derived from the bundled reference bank. It covers initial adaptive selection, exact and noisy classes, unknown tokenizers, baseline drift and a missing baseline. Rust posterior values, verdicts, stop conditions and next probes must match the TypeScript reference.
