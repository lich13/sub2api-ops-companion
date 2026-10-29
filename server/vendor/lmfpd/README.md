# lmfpd (offline Rust subset)

Pinned to `Ikaleio/lm-detector@fd9b4caf6feafbec64d45a1552432515bf4efa39`, SDK 0.2.0, under the MIT license. Source checksums are recorded in the server resource manifest.

This vendored subset retains upstream scoring, challenge generation, number parsing and paired-data validation. The 0.2.0 upstream changes concern network request timeouts and sample progress; its scoring, codec, ranking, features and challenge sources are unchanged from the previous pinned revision.

Sub2Ops owns all networking, cancellation, request limits, verified downloads and resource activation. The SDK's network loaders and live sampling modules are deliberately omitted.

- `Detector::load_bytes(bank, detector)` loads verified data from byte slices.
- `Detector::load_dir(path)` loads a local paired bank and detector.
- `Detector::analyze(outputs, strict)` scores one to three saved replies.
- `Detector::models()` and `Detector::built_at()` expose the reference metadata.
- `generate_challenges(count)` and `parse_numbers(text)` retain upstream behavior.

The server build verifies the full source resources and embeds only the bank fields used by the scorer, together with the unchanged detector and tokenizer data. Full-bank and compact-bank scoring are tested against the pinned official TypeScript implementation with the upstream relative tolerance of 1e-9. Reports retain the full upstream files' identity and hashes.
