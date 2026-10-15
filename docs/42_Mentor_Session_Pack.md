# 42 | Mentor session pack

One page to open, one table per question the mentor is likely to ask, a demo script that takes twelve minutes, and the honest list of what is not done. Every number below comes from a command listed next to it; run it before the meeting rather than trusting this page.

## 1. The pitch (thirty seconds)
ClaimGuard reads a health-insurance claim, checks it against the fifteen rules of the rulebook (and eight extension rules of our own, kept apart), and shows a human reviewer exactly which rule failed, on which line, with the evidence and the corrective action. An AI model drafts the explanation but can never remove a finding, and its text is checked against the engine's facts before anyone sees it. Everything runs on the hospital's own servers. Reviewers see pseudonyms, not identifiers; every decision and every access is in a tamper-evident log. The whole system is now written in Rust, and the Python original is kept as the reference it is tested against.

## 2. The mentor's points of 2026-09-27: where each one stands

| # | Mentor point | State | Evidence / how to show it |
|---|---|---|---|
| 1 | Benchmark YARA-X against plain Python rules | **Done** | `SPECS.md` section 5a (speed on 107,635 claims, maintainability, safety); the rust engine uses the same YARA-X rules and agrees with the Python one on 10,600 claims (`rust/tools/compare_engine.py`) |
| 2 | Add Qwen3 and MedGemma as comparison models | **Done for the 4B sizes** | `SPECS.md` 6a, `docs/figures/model_comparison.png`. MedGemma **27B was not tested** (too large for the 8 GB laptop GPU): say so before being asked. gemma3:4b is the model the project uses on premises |
| 3 | Web app first, mobile second | **Done (web console)** | the reviewer console is served by the server itself (`src/access/ui`); the desktop window is the same console in a native frame; the mobile prototype is separate |
| 4 | "Claims accepted as JSONL, with a confidence score" | **Unclear: ask** | what exists: a submit route/CLI accepting JSONL or a list, a triage receipt with a score and lane per claim, and a per-finding confidence kind. We do not know which of these the mentor meant (section 6, question 1) |
| 5 | Task queue: Celery or ARQ | **Partly** | the Python build ran Celery on Redis. The Rust build has the same safe design (an outbox in the database, a relay, idempotent tasks, retries, dead letters, leases) **on a Redis Stream with a consumer group** (`docs/38` section 9): a pool of consumers, a hard cap on how many claims are processed (and so asked of the model) at once, takeover of a dead consumer's work, repair of lost messages. It is not Celery or ARQ, which are Python libraries (section 7, question 2) |
| 6 | Respect Middle East data-protection law (PDPL) | **Mapped, two gaps** | `docs/41`: each obligation against what the system does. Not built: subject access/erasure and retention |
| 7 | Near-production, not a proof of concept (K8s, Redis) | **Compose tested; Kubernetes written, not run** | `deploy/rust/` (50-check attack test, encrypted backup/restore tested end to end, runbook `docs/40`); `deploy/k8s/` (manifests with network policies and the restricted pod profile, structural test, **no cluster to apply them to**) |
| 8 | Fully on premises | **Done by design** | local model (vLLM), no outside call in the deployed system, data network with no route out (`deploy_attack.py`), the Rust build refuses a cloud model provider |
| 9 | NoSQL, not SQL | **Done** | MongoDB for users, claims, queue, approvals; the same contract suite runs against the in-memory twin and the real database |

## 3. The judging weights, and what proves each one

| Weight | Criterion | What we can show |
|---|---|---|
| 35 | Rule correctness | F1 1.0 and status accuracy 1.0 on the three public splits, 0 false positives on valid claims (`docs/29`); the Rust engine identical to the Python one on 10,600 claims and the extension rules on 6,000 (`compare_engine.py`, `compare_extensions.py`) |
| 20 | Grounded AI explanations | every AI sentence is checked against the engine's own fields; the model never sees claim values (placeholders are filled in afterwards); a real-model red team with gemma3:4b found and closed one hole (`model_redteam.py`, `docs/39` section 7) |
| 15 | Human review and usability | the console (inbox, lease, decision with reason, senior sign-off for high severity, four-eyes), routing lanes by a severity-weighted score, feedback report; approvals panel for administrators |
| 15 | Uncertainty and security | `docs/39` (rounds 1 to 4): 116-check black-box attack test, 93-mutant test of the security and queue rules, 400,000-request fuzz, deployment attack, browser attack in Chrome, side-channel timing test, dependency audit; `docs/32` privacy note; `docs/41` |
| 10 | Audit and reproducibility | hash-chained, signed, encrypted-at-rest logs; an append-only mirror that refuses to follow an edited log; optional outside time stamp; replay of any dealing; `audit_guard_demo.py` shows a rewrite being caught |
| 5 | Communication | `README.md` briefing, `SPECIFICATION.md`, this pack |

Non-negotiables from the brief, and where each is guaranteed: no unknown check shown as PASS (engine returns UNABLE_TO_ASSESS, tested); every finding links to evidence and a rule; a model failure cannot remove a finding (the AI step only adds text; its failure leaves the engine's own explanation); the original input is kept and a correction is a new version; no real patient data or live payer submission in the demo (generated claims, local only).

## 4. The demo (twelve minutes)

Start: `desktop\dist\ClaimGuard.exe --local-demo` (a private server starts on this PC; the sign-in page has one-click demo logins; accounts are listed in `demo_accounts.json` in the temp folder named in the first line of the server log).

1. **The reviewer's day (3 min).** Sign in as *Reviewer* → *My inbox*. Open a claim: the rule failures, the evidence value, the line, the corrective action, the AI draft marked as a draft. Identifiers are pseudonyms. Decide a medium finding with a reason. Try a high-severity finding: refused; it needs a senior.
2. **The senior and four eyes (2 min).** Sign in as *Senior reviewer*: confirm the high finding; the second person's sign-off is required and the first cannot countersign themselves.
3. **Privacy (1 min).** Unmask one claim with a reason; show that the reason cannot contain the identifier, and the count limit.
4. **The administrator (3 min).** Sign in as *Administrator*: *System health* (every component, including the audit protection and disk), *Audit and traceability* (filters, one claim's trace). Then *Users*: reset someone's authenticator → it becomes a **request**, nothing changes. Sign in as the second administrator (CG-4005): approve with a fresh code → carried out. Say what the log now shows.
5. **Tamper evidence (2 min).** `python rust/tools/audit_guard_demo.py`: it rewrites a row of the log; the health panel goes red within seconds; the mirror still holds the original.
6. **Close (1 min).** `python rust/tools/pentest.py` summary line (116 checks), the data-protection table (`docs/41`), and the list of what is not done (section 5).

Have ready, in case of questions: `docs/39` (security results), `docs/40` (operations), `docs/36` (why Rust), `deploy/k8s/README.md`.

## 5. What is not done (say it before being asked)
- **Not run on the target virtual machine**, and no Kubernetes cluster was available to apply the manifests to.
- **Subject access and erasure, and a retention period** are designed, not built (`docs/41`).
- **MedGemma 27B** was not tested; gemma3:4b is the model.
- The **host firewall** is written and tested in throwaway containers (`docs/40` section 10) but has not been run on the real server; a **distributed denial-of-service** and a public certificate authority are untested; the system has not been attacked by anyone outside the team.
- **Dual control** cannot know two administrator accounts belong to two people, and a lone administrator is not protected (logged).
- **One API pod and one MongoDB member** in the Kubernetes files (the audit logs are files with locks).
- The recheck route (`POST /claims/{id}/recheck`) is 501 in both builds; corrections are new versions through resubmission.
- Cloud model providers are not in the Rust build (on-premises only).

## 6. Hard questions, with the honest answer

- **"Why did you rewrite it in Rust three weeks before the deadline?"** The measured benchmark (`docs/36`) showed the rule engine is not the bottleneck (about 4 microseconds a claim against about 3 seconds for the AI step), so speed was not the reason. The reasons were memory safety for a system that handles health data, one small static binary instead of an interpreter and its dependencies (83 MB image against 362 MB, no shell in the container), and removing whole classes of bugs the Python build needed tests for. The Python tree was kept untouched as the reference, and every layer of the Rust build was tested against it (identical results on thousands of claims, the audit logs verified by both builds, the API compared request by request). The cost: three days of the schedule.
- **"Is the AI deciding anything?"** No. The engine decides; the model drafts an explanation from the engine's facts; its text is rejected if it is not grounded, and a rejected or failed draft leaves the engine's explanation in place. A model cannot remove or add a finding.
- **"What if the model server is down?"** Claims keep flowing with the engine's explanation (a circuit breaker stops retrying a failing model). The health panel shows it.
- **"What stops an insider reading patient data?"** Identifiers are pseudonyms by default; unmasking needs its own permission, a written reason that cannot contain the identifier, and is capped per hour; every unmask is in the log; the data is encrypted at rest; an administrator cannot read claims at all.
- **"What stops one bad administrator?"** The most dangerous actions need a second administrator with a fresh authenticator code; the log records both. (And honestly: not two accounts held by one person, and not a lone administrator.)
- **"How do you know the tests test anything?"** A mutation test: 75 deliberate breakages of the security rules, each must make a test fail. Twice it showed a hole in our own tests, and twice an old test pattern that had silently stopped testing anything; all fixed and listed in `docs/39`.
- **"Does it scale?"** One API pod and one worker handle the demo and a hospital department comfortably (the engine does hundreds of thousands of claims a second; the explanation step is the slow part and runs in the background). Beyond that, the next steps are written down: several API pods need the logs in the database, MongoDB needs a replica set.
- **"Is it PDPL-compliant?"** No software is; the law binds the organisation. `docs/41` lists each obligation, what the system does about it, and the two things still to build.

## 7. Questions to ask the mentor
1. **"Claims accepted as JSONL with a confidence score" (his point 4):** is the output a batch of accepted claims for the payer, each with a score? Or the input format?
2. **Task queue:** he named Celery or ARQ (Python). The system is now Rust, and the queue is built on Redis Streams with consumer groups, acknowledgement after work, retries, dead letters and an outbox in the database. Is that acceptable in place of Celery or ARQ?
3. **May a trained model influence routing** (who reviews what, in what order), never the verdict?
4. **May green claims ever skip a human?** (Today every claim goes to a person.)
5. **"More than 15 rules":** which kinds? We built eight extension rules (E001 to E005, E101 to E103) and kept them apart from the official fifteen.
6. **Is an open model under 15B on premises mandatory?** (gemma3:4b today.)
7. **Retention and erasure:** what retention period would the insurer expect, and is erasure on request in scope for the demo?
8. **Kubernetes:** is a written, tested set of manifests enough, or does he expect to see it running on a cluster?
9. The R006 / EDU-SEPARATE policy question: the rulebook exempts that modifier from the per-line limit; 3 public claims are affected.
