# 18 | Contribution log

## Team roles

| Team member | Role | What they did | Evidence |
|---|---|---|---|
| Mohamed Mouelhi | Lead Systems Architect (team lead) | System and infrastructure design; the rule engine and its 15 rules; the bounded AI step and its guards; the audit log; the task queue; the Rust rebuild; security testing (penetration test, fuzzing, mutation tests, OWASP audits); evaluation and documentation. Directed the AI coding tool described below and decided what was kept | Git history of this repository (single author account), `docs/` |
| Hamza Tebbi | Product Design Lead | UI and UX design of the operator console: screens and reviewer flows | The operator console |
| Khalil Ben Youssef | Deployment and Operations | The GPU server (RTX 3080, Ubuntu): hardware, network, Cloudflare access and the physical deployment; ran the server installs and test runs | [docs/66](66_Server_Experiment_Results.md), server install reports |
| Ahmed Ayachi | AI Research Engineer | Phase 1 research into an agentic architecture. It was measured against the rules-first design and set aside; the current product does not use it | Phase 1 notes |

The git author configured on the build machine is a single account, so commits do not show
who did which part. The table above is the team's own statement.

**Human review.** Review is not recorded per rule or per module. What was checked, and how:
every rule against the organisers' answer key on all public splits and against an independent
oracle (0 disagreements over 107,635 generated claims); every module by its own tests (473 Rust
tests in CI); security by the tests listed in [docs/39](39_Rust_Security_Results.md). The team
lead read the results of each run and directed the fixes.

## How AI coding tools were used

**Tool:** Claude Code (Anthropic CLI). The latest sessions ran on Claude Sonnet 5; earlier
session models are not recorded in the repository. A separate "advisor" reviewer model was
consulted at planning time and before large design choices, and the `superpowers` workflow
skills (brainstorm -> written spec -> written plan -> execute) were used.

**What the AI wrote, under the team lead's direction (2026-09-21 to 2026-09-26):**

| Area | AI-authored | Team-lead decisions that steered it |
|---|---|---|
| Rule engine (`facts_extractor.py`, `rules/core.yar`, `yara_engine.py`, 15 rules, ~120 unit tests) | all code and tests, from `docs/04_Rulebook.md` | chose YARA-X facts-blob architecture; asked for all 15 rules and that "the exact rule he violated must show"; chose linear execution ("no subagents") |
| Rulebook/PDF gap review | read the handbook and produced `SUMMARY/03_REQUIREMENTS_CHECKLIST.md` | asked for the review against organizer requirements |
| Bounded AI adapter (`llm_adapter.py`, `claim_review.py`) | all code, tests, prompt v1.0.0 -> v1.1.0 | chose the model provider and supplied keys (NVIDIA first; replaced by Featherless when the NVIDIA key was declared untrusted); asked for "what the AI must do" per the rulebook |
| Audit log, review workflow (`audit_log.py`, `review_workflow.py`) | all code, tests, design note | asked to finish review workflow, audit trail and reports |
| Ingestion (`ingest.py`, `fhir_adapter.py`) | all code, tests, comparison script | supplied the four graded deliverables that ingestion must meet |
| Evaluation report, this log | drafted | to be reviewed and completed by the team |
| Stress testing (2026-09-24 to 2026-09-26): independent oracle, boundary table, differential and hostile-input suites | all code and tests | asked to recheck everything and stress the rules against what the judges score |
| Security audit and red team (2026-09-26): OWASP mapping, fixes, replay and advisory checks | all code, tests, `docs/20` | asked for a security audit against the AI and cyber Top 10 lists and for a red-team run, then to fix what it found |
| AI experiments (2026-09-26): runner, analysis, experiments in four rounds, garbled-output guard, model cascade, citation repair, closing gate, prompts v1.4.0 to v1.6.0, new default model, `docs/21` | all code, analysis and text; ran 2,136 live calls with the team's key | asked for temperature and optimization experiments with variables, figures and documentation; the design and decision rules were written before the runs |
| `README.md` | drafted | asked for a clean README; the team must fill in roles and names |

**How AI output was checked:** every module has offline unit tests; the engine is compared
with the organizers' labels on all three public splits and with the handbook's worked
cases; live model output was read by the AI and the failures it found (invented currency
symbol, "in the future" claims) were turned into a guard and regression tests. **These are
AI self-checks.** Human review of the code is not recorded anywhere in the repository;
the team must state in the table above what a person actually reviewed.

**AI at run time in the Rust build (what runs today):** a local model (`gemma3:4b` through Ollama, on the
hospital's own server) fills a fixed explanation template. It never sees claim values, a guard checks every
sentence, it cannot change a status, and the audit records `auto_correct_applied: false` for every AI action.
It falls back to a deterministic template on any failure. The copilot is deterministic and uses no model.

**AI at run time in the Python reference (phase 2, kept for comparison):** `mistralai/Mistral-Nemo-Instruct-2407` with prompt v1.6.0 and a closing gate
via Featherless.ai explains findings only (chosen in round two of `docs/21`; earlier runs used `Qwen/Qwen2.5-14B-Instruct`, and before that `mistralai/mistral-nemotron` via NVIDIA NIM).
It cannot change a status, rule id or review flag (pydantic schema + validator), has no tools beyond
read-only evidence lookup, its question and action type are written to the audit log before each call,
and it falls back to a deterministic template on any failure. See `docs/17_Evaluation_Report.md`.

## Disclosures

- API keys were pasted into chat sessions to configure the adapter (first NVIDIA, later Featherless).
  Neither is in the repository or its history (`.env` is ignored), but both exist in local session
  transcripts. The NVIDIA key was declared untrusted and removed from `.env`; **rotate the Featherless
  key too** before submission.
- The demo reviewer decisions and correction in `outputs/audit_demo/` are demonstration data
  written by a script under the actor `demo-reviewer`. They are not real human judgements.
- The perfect metrics on the public splits were measured on data the AI and team could see;
  see "Data discipline" in the evaluation report.
