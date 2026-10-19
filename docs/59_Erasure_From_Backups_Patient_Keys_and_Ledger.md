# Erasure from backups: a key per patient, and the erasure ledger

Built on 2026-10-19. docs/58 built erasure from the live database; this closes the gap it left open, that a backup taken before an erasure still holds the patient.

## 1. The problem

A health-data holder who erases a person on request is also expected to stop holding them in backups. Rewriting every backup is not practical (they are encrypted archives, and some are on media nobody should open). The regulators' own guidance names two accepted ways:

1. **Make the copy unreadable** (crypto-shredding): the data was encrypted under a key that belonged to that person, and the key is destroyed.
2. **Put the copy beyond use and apply the erasure again** to anything restored from it (a suppression list, kept outside the database).

We built both, because each covers the other's weak point: the key makes an old backup useless on its own; the ledger makes a restore safe even if the key somehow comes back.

## 2. A key per patient

**What changed.** Before, every claim's sealed fields were encrypted under one master key (`DATA_KEY`). Now each patient has their own random key. A claim's fields are encrypted under the key of the patient it belongs to; the patient is found by the same keyed index as before (`patient_key`, a keyed hash of the identifier, never the identifier).

- The patient's key is stored **wrapped by the master key**, in a **separate database** (`<queue db>_keys`, or `KEYS_DB`), one small document per patient. The wrapper names its patient, so swapping one patient's key under another is refused (tested).
- Sealed tokens made this way start with `p1:`. Older tokens (no prefix) still open with the master key, so nothing written before is lost.
- The other protections are unchanged: each field is still bound to its claim, version and field name, so moving data between claims is refused.
- **Erasing a patient deletes their key** (when none of their claims remains stored). Every copy of their data that survives anywhere, a backup included, is then undecryptable: the master key alone cannot open it, because the master key only unwraps keys that are no longer there.
- A patient who comes back gets a new key; the old data stays unreadable (tested).
- A process keeps an unwrapped key in memory for 30 seconds. The process that erases forgets it at once; another process may still read that patient for up to 30 seconds.
- Default on; `CG_PATIENT_KEYS=false` returns to one master key (nothing can then be shredded).

**Moving existing data.** `cg adopt-patient-keys [--dry-run]` (server, API and worker stopped) moves every claim still sealed under the master key onto its patient's key. Safe to repeat. **Backups made before this run still hold the old form and cannot be shredded**; they have to age out.

**Key rotation** (`cg rotate-key`) keeps working: the patient index depends on the data key, so each patient's key is carried to its new index, the documents are pointed at it, and the old entry is deleted once nothing points at it (tested, including repeating after an interruption and shredding a patient afterwards).

## 3. Backups: the keys travel separately

`deploy/rust/backup.sh` now writes **two files**: the data backup as before, and `<name>.keys`, the key database, encrypted the same way. The keys are never inside the data backup.

- Keep the **`.keys` files for a short time** (30 days is the suggestion) and the data backups for as long as the insurer needs.
- When the last `.keys` file that held a patient's key is deleted, that patient is unreadable in **every** older data backup. That is the moment the erasure is complete everywhere. During the short window the key can in principle still be restored, which is what the ledger is for.
- A data backup without its `.keys` file restores but cannot be read: keep the pair together for the short period.

## 4. The erasure ledger

`erasure_ledger.jsonl`, next to the audit logs (so `harden-audit.sh` makes it append-only and the log backup carries it). One line per erasure: claim ids, the keyed patient index, who, when, and a SHA-256 of the line. **No name, no identifier, no health data.** The patient index is a keyed hash and counts as pseudonymous data; treat the file as confidential.

- The line is written **before** anything is deleted, so a cut-short erasure is finished by the next replay.
- A line that is damaged or whose hash does not match makes the replay **fail loudly**; it never skips one (a skipped line would be an erasure that is not re-applied). Tested.
- `cg replay-erasures` applies every line again: deletes whatever a restore brought back and destroys the keys of those patients if the restore brought them back too. Safe to repeat.
- `restore.sh` restores the keys file, **keeps the live ledger if it is longer than the backup's** (the ledger only grows, and a restore must never roll it back), and runs the replay **before the API and the worker start**. If the replay fails, the script stops and says not to start the system.

## 5. What was tested (against real MongoDB)

| Test | Result |
|---|---|
| Data under a patient key dies with that key; another patient is untouched; forgetting twice is harmless; a cold second process cannot read it | pass |
| A returning patient gets a new key that cannot open the old data | pass |
| Master-key documents from before still open; a store without keys cannot open patient data | pass |
| A key moved to another patient, or a wrong master key, is refused | pass |
| Full store contract with a key per patient (same contract as the memory twin) | pass |
| **Erase, then restore the old claims backup: unreadable; replay deletes it** | pass |
| **Worst case: the old keys backup is restored too, so the data reads again; replay deletes the claims and destroys the resurrected key** | pass |
| The database holds no identifier or note in the clear | pass |
| Existing claims move onto patient keys; rotation carries the keys; a rotated patient can still be shredded | pass |
| Ledger: order, no identifier, an edited or damaged line is an error | pass |
| Replay deletes what a restored backup brings back, twice is harmless, a damaged ledger stops it | pass |

## 6. Not built / not tested, said plainly

- **`backup.sh` and `restore.sh` were changed and syntax-checked, but not run end to end** through Docker: that needs a rebuilt application image, which was not rebuilt. The logic they call (replay, keys) is tested at the database level above. Run `rust/tools/backup_restore_test.py` after the next image build.
- **Backups made before `adopt-patient-keys` cannot be shredded.** They are the old single-key form and must age out.
- **Whoever holds the master key, the key database and an old data backup at the same time can read an erased patient** until the old `.keys` file is gone. That is why the keys file has a short life and why the ledger is replayed on every restore.
- **Key custody is organisational:** the master key (`DATA_KEY`) is still in `.env`; losing it loses everything, and the `.env` backup must be kept apart from the data backups.
- **The ledger holds the keyed patient index.** Anyone with the data key could test a guessed identifier against it. It is pseudonymous, not anonymous.
- **No second-person approval and no web route for erasure yet** (docs/58 section 6), and the legal question whether destroying a key counts as erasure under the Middle East laws is for the insurer's counsel; the ledger-and-replay route is the one regulators describe in their guidance.
- **Dev mode (in-memory store) has no keys;** only the MongoDB store shreds.
