# 32 | Privacy and security note

This is the privacy and security note the challenge asks for (`docs/01`, deliverables), written for Phase 2, where the system gains
people (the reviewer API, `SPECS 10d`) and a queue that hands them claims (`docs/31`). It says what is built, what is only designed,
and what a real deployment would still have to add. Every "built" has a test or a file you can run.

**Scope.** All data in this repository is synthetic. Having no real personal information is not a compliance assessment for a future
healthcare product (`docs/10`), and nothing here is a legal opinion.

## 1. What is stored, where, and who can read it

| Record | Where | Holds a patient or member identifier? | Who can read it | Protection |
|---|---|---|---|---|
| Claim body, as submitted | Queue store (MongoDB `claims`, or the in-memory twin) | **Yes**, in the clear | Reviewers L1 to L3 through the API, **masked** by default (stable pseudonyms such as `PAT-xxxx`); L2 and L3 may unmask one claim with a typed reason, which is logged | Masking at the API (`access/masking.py`, `RedactionError` if one survives); database access control |
| Rule results and their evidence | Same document | **Yes**, in the clear (evidence quotes the value that failed a rule) | As above, same masking | As above |
| Reviewer decisions (action, reason, badge, time) | Same document, and the review log | Not by design. The **reason is free text typed by a person** and may contain anything | Reviewers; L4 sees counts only | The decision is checked (non-empty reason, real finding, original status preserved); the reason is never copied into reports (`workqueue/feedback.py`) |
| Deal documents (who was dealt what, the seed) | Queue store | **No.** A snapshot keeps the claim id, version, score, eligibility, and the badges barred from that patient, not the patient | L4 (`queue.view`) | `tests/test_data_minimization.py` checks a real flow, and the exact snapshot keys |
| Security log (logins, lockouts, decisions, deals, sign-offs) | `security_audit.jsonl`, hash-chained | **No.** Badge ids and claim ids only; a field named like a secret (`password`, `token`, `key`, `otp`) is refused, and the event vocabulary is closed (`access/securitylog.py`) | L4 (`audit.view`) | HMAC-anchored chain, see section 4 |
| Review log and the main audit log | JSONL, hash-chained | No | L4 | Same |
| Users: badge, password hash (bcrypt, cost 12 in production), authenticator seed, level, grants | MongoDB `users` | Staff data, not patient data | L4 (`users.manage`) | The seed is encrypted at rest with a Fernet key from the environment; the password is never stored; sessions are cookie plus CSRF token with server-side revocation |
| Dismissal-reason counts, rates, review candidates | Computed on request | No | L4 (`queue.view`), `GET /api/v1/queue/feedback` | Counts only; no reason text, claim id or badge |

Two facts the table makes plain, because they are the honest limits of minimization here:

- Identifiers sit **unmasked at rest** in the claim and in each result's evidence. The protection is the API (masking, permissions,
  an audit event for every unmask) and the database's own access control, not encryption of those fields. A production system would
  encrypt the database and keep the pseudonym key apart from it.
- A reviewer can type a name into a reason. The reason is stored (it has to be, for an explainable dismissal, `docs/10`) but it is kept
  out of every report and every log we generate.

## 2. Data minimization: what was changed, and the check

| Where | Before | Now |
|---|---|---|
| Deal documents | Kept `patient_id` for every dealt claim, to match conflict-of-interest exclusions | Keep `excluded_for`, the badges barred from that patient. The plan is identical and `verify(deal_id)` still reproduces it (`test_the_conflict_of_interest_rule_survives_dropping_the_identifier`). Deals written before the change still replay |
| AI explanation inside the queue | n/a | The model receives a template request with `{value}` and `{line}` placeholders; **it never sees a claim value**. Values are filled in afterwards and the grounding guard runs on the filled text (`workqueue/explain.py`) |
| Feedback to rule owners | n/a | Counts and exact intervals only |
| Security and review logs | n/a | Scanned for the claim's real identifiers after a real decision and a real deal: none (`tests/test_data_minimization.py`, memory and MongoDB stores) |

Not minimized, on purpose: the offline file flow (`docs/10`) sends a finding and its evidence to the explanation model, and evidence
can include a member id (OWASP LLM02 in `docs/20`). With synthetic data that is acceptable; with real data it would need an
on-premise model or a data-processing agreement.

## 3. Access control (summary; details and 21 attacks in SPECS 10d)

Badge number plus password plus one-time code from any authenticator app; five tries a minute; a `SameSite=Strict` cookie with a CSRF
token on every state change; four clearance levels with per-user grants. Permissions are re-read from the user store at the moment of
a decision, so a demotion takes effect at once (nothing is cached). Hide-not-disable is enforced by the server: a viewer is not
offered an action, and asking for it is refused and audited. Separation of duties: level 4 runs the queue and the users but holds no
decision right, so whoever runs the queue cannot decide its claims. A claim with a high-severity finding needs two different seniors
(`SPECS 10f`); a person never countersigns their own decision, and a conflict-of-interest list bars a reviewer from a named patient.

## 4. The audit trail: what we built against what an immutable design needs

`docs/10` asks us to describe how authenticated actors, append-only writes, retention-locked storage, independent trusted timestamps,
backups and controlled exports would combine, and to say which we implemented.

| Control | Status | Evidence |
|---|---|---|
| Hash chain over every event, edits and reordering detected | **Built** | `tests/test_audit_log.py`, `scripts/verify_audit.py` |
| Head hash and count in an anchor file, keyed with `AUDIT_ANCHOR_KEY` (HMAC), strict check on verify | **Built** | `audit_log.verify_with_anchor(strict=True)`; the appended-forged-rows hole found in `docs/27` is closed by it |
| Truncation and whole-log replacement detected | **Built** (when the anchor is held apart from the log) | `tests/test_audit_log.py` |
| Authenticated actors (the reviewer is the session, never a field the client sends) | **Built** | `SPECS 10d`; the queue API takes the actor from the session |
| Concurrent writers do not corrupt the chain | **Built** (file lock, fsync) | `scripts/benchmark_audit_concurrency.py`, race suites |
| Run-level events (every rule run, every model call, every recommendation) | **Built** | `docs/16`, `audit_log.verify_ai_ordering` |
| Separate log for logins, lockouts, unmasks, decisions, deals | **Built** | `access/securitylog.py`, `SPECS 10d` |
| Append-only database privileges (INSERT-only role) | **Not built.** Designed: a MongoDB role with `insert` on the log collection only | `docs/16` |
| Retention-locked (WORM) storage | **Not built.** A local JSONL file is not immutable and we do not call it that | `docs/16` |
| Independent trusted timestamp, for example RFC 3161 | **Not built.** Designed: publish the head hash to a timestamp authority on a schedule, and keep the token beside the anchor. Left out because verifying a token needs an ASN.1/CMS library and a live authority, and the suite promises no network | `docs/16` |
| Off-host backups, controlled exports | **Not built** | n/a |
| Key custody (anchor key and Fernet key outside the writer's reach) | **Partly.** Keys come from the environment; development generates them into a git-ignored file | `access/bootstrap.py` |

Said plainly: the log is tamper-**evident** against anyone who cannot rewrite the chain, the anchor and the key together. It is not
tamper-proof against someone who controls all three, which is what the unbuilt rows are for.

## 5. The AI step and prompt injection (OWASP LLM01, LLM02)

The model can only add words. It never sets a status, a rule id, a citation or the review flag, and a model failure cannot remove a
deterministic finding. Layers, each tested in `tests/test_injection_owasp_llm01.py`:

1. Claim data: hostile text in any free-text field, or split across two fields, changes no verdict.
2. The prompt: untrusted text is fenced as data, bounded, and never in the instruction part.
3. The reply: approval or payment language, text in another alphabet (Arabic, Chinese, Russian and homoglyphs), garbled text, and now
   **invisible characters** (zero-width space and joiners, bidirectional controls, line and paragraph separators, variation
   selectors, tag characters, soft hyphen, combining grapheme joiner) are rejected. The last was an open limit in the earlier
   battery (a word split by a zero-width space matched no pattern). The byte-order mark and the Arabic letter mark were already
   rejected by the foreign-script rule. False-positive check, reproducible with `python scripts/scan_invisible.py` on a fresh clone: of 292,045
   strings in the 76 result files under `outputs/` and `experiments/` (about 18 million characters), 30 contain such a
   character. All 30 are raw model replies in experiments e1, e2, e3 and e7, mixed-language gibberish that the older guards already
   reject, so the new check rejects **no** text that was accepted before. The new test fails 50 times when the check is switched
   off. (An earlier draft of this note quoted a larger scan of an untracked folder; the figures above replace it.)
4. Whatever slips through: the verdict and the review flag cannot change. A model that obeys every payload and flips the flag is contained.

Still open, and recorded as such: approval wording in **non-English Latin text** (French, Spanish) and **Base64** passes the text
guards. It cannot change any status or flag, but a reviewer could read it, so it stays a reason that a person reviews every claim.
We left the two-pass explanation idea (free text, then constrained JSON) out: it would change the AI step behind the frozen
experiment numbers, and it is a quality measure more than a privacy one.

## 6. Dependencies (checked 2026-10-06)

`pip-audit` on every requirements file in the repository:

| File | Used by | Result |
|---|---|---|
| `requirements.txt` | the product and CI | no known vulnerabilities |
| `requirements-dev.txt` | tests, CI | no known vulnerabilities |
| `experiments/requirements-experiments.txt` | AI experiments | no known vulnerabilities |
| `experiments/requirements-local-models.txt` | the opt-in local-model run (torch, transformers, accelerate) | **vulnerabilities reported** (torch 2.5.1: 22; transformers 4.51.3: 28; accelerate 1.2.1: 1) |
| `comparison/architecture_b/requirements.txt` | a third party's system, installed only to run the comparison | **vulnerabilities reported** (Pillow 11.0.0: 33; langchain-core 0.3.28: 11; langchain 0.3.13: 3; langgraph 0.2.76: 3; langchain-openai 0.2.14: 2; python-dotenv 1.0.1: 2; sentence-transformers 3.3.1: 1) |
| `comparison/architecture_b/requirements-harness.txt` | the comparison harness | no known vulnerabilities |

`bandit -r src scripts -ll` exits 0. The two files with findings are not installed by the product, by CI or by the demo; they exist to
reproduce frozen experiments, and bumping them across major versions would change those results. They are **not** fixed. Anyone who
installs them should do so in a throwaway environment and keep it off a network that matters. GitHub's alert count for the
repository includes them.

## 7. Evidence, and how to re-run it

| Claim | Command |
|---|---|
| All tests (1641) | `REQUIRE_MONGO=1 REQUIRE_REDIS=1 python -m unittest discover -s tests` |
| No identifier in logs, deals, dead letters | `python -m unittest discover -s tests -p test_data_minimization.py` |
| Feedback report is counts only and read-only | `python -m unittest discover -s tests -p test_wq_feedback.py` |
| Prompt injection battery | `python -m unittest discover -s tests -p test_injection_owasp_llm01.py` |
| Invisible-character guard costs no accepted answer | `python scripts/scan_invisible.py` on a fresh clone (exit 0) |
| 77 queue mutants, none survive | `python tests/mutation_queue.py` |
| Static scan and dependency scan | `bandit -r src scripts -ll`; `pip-audit -r requirements.txt` |

The 3.10, 3.12 and 3.14 matrix result in the README comes from the earlier session's run; the full 1641 were run on the project
environment only.

## 8. What a real deployment would still need

Encryption at rest and in transit (TLS in front of the API, an encrypted database); the pseudonym and Fernet keys in a secrets manager;
WORM or INSERT-only storage for the logs and an external timestamp on the head hash; retention periods and deletion rules for claims,
reasons and logs; a password-reset and single-sign-on story; a data-processing agreement or an on-premise model before any real claim
reaches an AI provider; and a privacy impact assessment under the law that applies. None of these is claimed here.
