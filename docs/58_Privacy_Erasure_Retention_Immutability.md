# Erasure, subject access, retention and append-only logs

Built on 2026-10-19 after the gap review of the Privacy and Security Note (docs/57). Everything here is tested; what is still not built is listed in section 6.

## 1. Subject access and erasure

**Where the code is:** `rust/crates/cg-queue/src/privacy.rs` (the logic), three new methods on the queue store (`docs_for_patient`, `erase_claim`, `closed_before`; memory and MongoDB), and the operator commands in `cg queue`.

| Command (level 4, badge + password + authenticator code) | What it does |
|---|---|
| `cg queue privacy-export PATIENT_ID` | Prints everything held about that patient in the queue store, decrypted: every version of every claim, with its request events. Needs `queue.view`. |
| `cg queue privacy-erase PATIENT_ID` | Counts only: "N claims would be erased". Nothing is deleted. |
| `cg queue privacy-erase PATIENT_ID --confirm N` | Deletes every version of the patient's claims, their request events and their dead letters, but only if N is the number just counted. Needs `routing.manage`. Final. |

Rules the code enforces:

- The patient is found by the keyed index (`patient_key`), so the lookup works on the encrypted store; an identifier that looks like a query matches nothing (tested).
- **A claim a person is working on is never erased from under them** (states `leased` and `awaiting_countersign`): the whole request is refused and nothing at all is deleted.
- Erasing twice is harmless; another patient's claims are untouched (tested).
- Every use writes a `privacy_action` event (actor, action, claim count, version count) to the security log. The event carries no patient identifier and no claim id.

**Why the audit logs are not rewritten.** They are chained, signed and mirrored, and they hold badge numbers, claim ids and hashes, never a name or an identifier (the data-minimisation check, docs/57 section 3.2). Once the claim is gone, a claim id names no one. This is the design that lets the log stay uneditable and the erasure still be real.

**What an erasure does not reach (said plainly):**

- **Backups.** The encrypted backups still hold the claim until they age out; how long backups are kept is the operator's decision (`deploy/rust/backup.sh`).
- **The insurer's own systems.** The product holds a copy; it is not the system of record.
- The claim id stays in the audit logs, as above.

## 2. Retention

`RETENTION_DAYS` (default 0, meaning keep everything, so nothing is deleted until the insurer decides). When it is set, the worker runs an hourly `retention` job that erases claims whose newest version was **decided** more than that many days ago (at most 500 per run). It never touches an undecided claim, and skips one that became active between the listing and the deletion. The job appears in the health panel like the other jobs.

By hand: `cg queue privacy-retention --days 365` shows what would be erased (a dry run); adding `--apply` does it and writes a `privacy_action` event.

**Logs.** The audit logs hold no personal data about patients, so they are not erased by the retention job; the insurer sets how long they are kept (an auditor will want years). Rotating them is a controlled operation: lift the append-only attribute (section 3), archive, start a new chain.

Tested (the `cg-queue` privacy tests, and the same contract run against MongoDB): off by default; a dry run deletes nothing; a claim decided 500 days ago goes, one decided 100 days ago stays, one never decided stays.

## 3. Append-only logs at the operating-system level

`deploy/rust/harden-audit.sh apply DIR...` sets the Linux append-only attribute (`chattr +a`) on every `*.jsonl` in the directories: the live security log, the review log, the ledger checkpoints, and the mirror directory. `status` reports any file still writable; `lift` removes the attribute (for rotation or a restore). Run as root.

With the attribute set, the kernel refuses to truncate, overwrite in place, rename or delete the file, even for root, until root lifts it. Appending still works, which is all the writer ever does. The anchor files are rewritten by rename on every append (and signed with the anchor key), so they stay writable; the chain, the signed anchor and the mirror still cover them.

**Proved in a throwaway container** (`deploy/rust/test-harden-audit.sh`, run with `--cap-add LINUX_IMMUTABLE`): append succeeded; truncate, overwrite, rename and delete were each refused with "Operation not permitted"; the file kept both rows; after `lift` it could be truncated again.

What this changes: someone who has the logs, the anchor and the keys *as the application account* can no longer rewrite history. To rewrite it now they need root on the host, and they must lift the attribute first, which a root-level audit (auditd, or the host's own log) should record. What it does not change: **root on the host can still lift it**. Truly immutable storage is a property of the storage (object lock with a retention period, or a write-once appliance), which this project does not have; the mirror directory is the thing to put there.

## 4. The embedding sidecar's weights

The model (bge-m3 at a pinned revision) ships only as a pickle file, and pickle files can run code when loaded. Two controls:

- The Dockerfile now checks the file's SHA-256 (`b5e0ce34...aad38`, the hash of the file the experiments used) and the build fails on any other file. The check was run against the local copy of the file and passes.
- `transformers` loads it with `weights_only=True` (confirmed in the installed version), so loading cannot execute code. The sidecar also runs read-only, as an unprivileged user, with no route out (compose.yml).

The image was **not rebuilt** (instruction: do not rebuild the image); the change applies on the next build.

## 5. Rate limit on "Ask the rulebook"

`GET /knowledge/search` is limited to 30 questions per minute per person (`ASK_PER_MINUTE`), counted in the database so it holds across copies; the 31st gets 429 with `Retry-After: 60` and a `forbidden` entry. It protects the embedding sidecar from one person's loop. Test: `rulebook_questions_are_rationed_per_person`.

## 6. Not built, said plainly

- **Erasure through the web API and the console.** The commands run on the server, by a level 4 operator. There is no button and no two-person approval for erasure yet; the safeguard today is the count to confirm and the log entry. A second administrator's approval would reuse the existing dual-control mechanism.
- **An INSERT-only database role for the audit log.** The logs are files, not database rows. Append-only files (section 3) are the control we built; a MongoDB sink with an insert-only writer role and a read-only reader role was not.
- **True WORM / object-lock storage and a live timestamp authority.**
- **Erasure from backups.**
- **The batch job table is still in memory** (a restart forgets running jobs; sending the batch again is the repair).
- **The pen test was not extended** to the new commands (they are not network routes). They are covered by unit and store-contract tests.

## 7. Evidence

| Check | Result |
|---|---|
| Privacy logic (export, erase, refuse active, retention) | 4 tests pass |
| Store contract for the three new methods, memory twin | pass |
| Same contract against MongoDB (including the encrypting store) | pass |
| Search rate limit | 1 test passes |
| Append-only script in a container | append ok; truncate, overwrite, rename, delete refused |
| Full run: cg-audit, cg-access, cg-queue, cg-api, cg-cli | all pass |
