# Fuzz testing for ClaimGuard (Phase 2 tests)

## Goal
Show that ClaimGuard (a) never crashes or hangs on hostile input, (b) never emits output that breaks its own
contract, and (c) fails closed under attack. Results are recorded as evidence in `outputs/defense/fuzz.json`
and also run in CI.

## Non-goals
Coverage-guided fuzzing (Atheris is Linux-only and does not build on the team's Windows machines), live
LLM calls (the model is mocked, so everything runs offline), and fuzzing the Phase 2 web app/RBAC, which is not built yet.

## Tooling
- `hypothesis` as a dev-only dependency in `requirements-dev.txt`; runtime requirements stay unchanged.
- CI profile: fixed seed (`derandomize=True`), small `max_examples`, deterministic and fast.
- Deep profile: `scripts/fuzz_campaign.py --minutes N` runs each surface for N minutes and writes
  `outputs/defense/fuzz.json` (use `git add -f`, `outputs/` is gitignored).
- Any failing input is minimized by Hypothesis and saved under `tests/fuzz_corpus/` as a permanent regression case.

## Surfaces and invariants
| # | Surface | Fuzz input | Invariant |
|---|---|---|---|
| 1 | Ingest (`src/ingest.py`, `jsonl_reader.py`, `fhir_adapter.py`) | mutated bytes, huge fields, bad encodings, deep nesting, BOM, NUL | no uncaught exception; bad rows quarantined; input limits enforced |
| 2 | LLM output parsing + closing gate (`src/llm_adapter.py`) | garbled, truncated, injected, schema-violating replies from a mocked provider | output always passes the pydantic schema or falls back to the template; engine verdict never changed by model text |
| 3 | Audit log (`src/audit_log.py`, `scripts/verify_audit.py`) | mutate, reorder, delete, append rows | strict verification always detects tampering |
| 4 | Rule engine (R001-R015) | random and boundary claims, mutated fields | verdict equals `tests/oracle.py`; no crash; garbage fails closed |
| 5 | Prompt injection | payloads in claim free-text fields | no forged fact, no verdict flip |
| 6 | Review page (`src/make_review.py`) | HTML/JS payloads in claim fields | all output escaped; no raw `<script>` |

## Build order
Surfaces 1 to 6 in that order, one commit each, tests under `tests/fuzz/`, shared strategies in
`tests/fuzz/strategies.py`. Finish with `scripts/fuzz_campaign.py`, a CI step, and one row in
`docs/27_Decisions_Proofs_and_Defense.md` with the findings.

## Success criteria
- Existing 481 tests still pass and the CI-profile fuzz suite adds under 60 s.
- Every invariant above is exercised, and any real bug found is fixed with its minimized case kept as a regression test.
- A deep run's results are recorded and cited in docs/27.

## Risks
Hypothesis can find genuine bugs in Phase 1 code; fixing them is in scope, redesigning is not. The engine's
precedence order is untested by data (docs/27), so surface 4 reports it rather than asserting on it.
