# Spike working notes (historical)

These are the chronological working notes from the `ast` spike — kept for the
record, not as current documentation. They were written as the investigation
progressed, so **earlier files report intermediate numbers that later ones
supersede**. Read them as a timeline, not as a spec.

Authoritative current results live in the PR description and are reproduced by the
gates/benchmarks in `harness/` (`gate.mjs`, `gate_*.mjs`, `perf_*.mjs`):

- **mdast compatibility:** `COMPAT_GATE_FINDINGS.md` — the final 652/652 result
  (deep-equal incl. position vs `mdast-util-from-markdown`) is the headline; it
  supersedes the partial shape/position percentages in `HARNESS_FINDINGS.md`,
  `POSITION_FINDINGS.md`, and `PLUGIN_FINDINGS.md`.
- **boundary / integration / overview:** `WASM_BOUNDARY_FINDINGS.md`,
  `INTEGRATION_FINDINGS.md`, `SPIKE_FINDINGS.md`.
