# 40 | Operating the Rust build: install, secrets, backup, key rotation, updates, incidents

For the person who runs the server. Everything here was exercised on this laptop with Docker (sections 4 and 5 end to end, with the test scripts named below); none of it has yet been run on the target virtual machine, so run the checks in section 9 there first.

## 1. Install

```
bash deploy/rust/install.sh <address-or-domain> internal|public [ai]
```
- Needs Linux with Docker and the compose plugin, and `openssl`. `internal` is for an address or internal name (a certificate from the proxy's own authority; browsers warn until you trust it); `public` is for a real domain with ports 80 and 443 reachable (a Let's Encrypt certificate). `ai` also hosts the local model (Ollama) and checks it with `cg model-check`.
- It writes `.env` with freshly generated secrets **only if `.env` does not exist** (mode 600), builds and starts the stack, waits for health, and asks you to create the first administrator (level 4). You choose the password; the authenticator seed is shown once.
- Only the proxy publishes ports (80, 443). The database and cache sit on a network with no route to the internet.

## 2. Settings (environment, in `.env`)

| Setting | Meaning | Default |
|---|---|---|
| `JWT_SECRET` | signs sign-in sessions | required |
| `AUDIT_ANCHOR_KEY` | signs the audit logs' anchors | required |
| `PII_KEY` | makes the stable pseudonyms shown instead of patient and member ids, and hashes client addresses | required |
| `FERNET_KEY` | encrypts the authenticator seeds in the user records | required |
| `DATA_KEY` | encrypts the claim data in the database; a Fernet key | empty: derived from `FERNET_KEY` |
| `MONGO_ROOT_USER`, `MONGO_ROOT_PASSWORD`, `REDIS_PASSWORD` | database and cache credentials | required |
| `CG_SITE`, `CADDYFILE` | the address and which proxy configuration | required |
| `QUEUE_AI_PROVIDER=ollama`, `OLLAMA_BASE_URL`, `OLLAMA_MODEL`, `QUEUE_AI_TWO_PASS=1` | the AI explanation step; blank = none | none |
| `AUDIT_MIRROR_DIR` (`/mirror`), `AUDIT_MIRROR_SECONDS` | the append-only second copy of the audit logs | on in the stack, 300 s |
| `AUDIT_VERIFY_SECONDS` | how often both log chains are verified | 3600 |
| `TSA_URL`, `TSA_CA_FILE`, `AUDIT_WITNESS_SECONDS` | an outside RFC 3161 timestamp of each log anchor | off, 3600 |
| `UNMASK_PER_HOUR` | how many claims one person may unmask per hour | 60 |
| `CG_ENCRYPT_AT_REST=false` | turns claim-data encryption **off** (do not) | on |
| `TOKEN_TTL_SECONDS`, `MAX_FAILED_LOGINS`, `LOCKOUT_SECONDS`, `BCRYPT_ROUNDS` | session length (3600), failures before an account locks (5), lock time (900), password hash cost (12) | as shown |
| (no setting) | the server takes the client address from the last hop of `X-Forwarded-For`; that is safe only because nothing but the proxy can reach the API's port, which the network layout guarantees | |

## 3. The secrets: what each does, and what happens if it changes or is lost

| Secret | If it is **lost** | If it is **changed** |
|---|---|---|
| `JWT_SECRET` | nothing is lost | everyone is signed out once; safe, and the right response to a suspected stolen session |
| `AUDIT_ANCHOR_KEY` | the anchors can no longer be checked | old anchors fail verification: only change it with a plan (verify first, note the break) |
| `PII_KEY` | nothing stored is lost | the pseudonyms shown change (the same person gets a new pseudonym) and the unmask counts and client list restart |
| `FERNET_KEY` | **every authenticator seed is unreadable: nobody can sign in** (and the claim data too if `DATA_KEY` is empty) | same |
| `DATA_KEY` | **the claim data is unreadable for good** | use `cg rotate-key` (section 5), never edit it by hand |
| `MONGO_ROOT_PASSWORD` | restore from backup | change it in the database and in `.env` together |

Keep a copy of `.env` somewhere other than the server and other than the backups. **The backups do not contain these secrets** and are useless without them.

## 4. Backup and restore (tested: `python rust/tools/backup_restore_test.py`, 18 checks)

```
openssl rand -base64 48 > /root/claimguard-backup.key ; chmod 600 /root/claimguard-backup.key     # once; keep a copy elsewhere
BACKUP_KEY_FILE=/root/claimguard-backup.key bash deploy/rust/backup.sh /srv/backups/claimguard-$(date -u +%Y%m%d).tar.enc
BACKUP_KEY_FILE=/root/claimguard-backup.key bash deploy/rust/restore.sh /srv/backups/claimguard-20261010.tar.enc
```
- One encrypted file (AES-256, PBKDF2) holds both databases, both audit logs and the audit mirror, with a checksum beside it. It never overwrites an existing file.
- Restore refuses a wrong key, a damaged file and a file whose contents do not match their checksums; it replaces the two application databases and the log volumes, restarts the API and worker, and fails loudly if any document does not restore. Use the same `.env`.
- The test destroys every volume, restores, and checks that the same people sign in with the same passwords and authenticator seeds, the same claims are in the same states and readable, and the audit chains verify.
- Schedule it (cron), copy the files off the machine, and **practise a restore** on a spare machine every quarter. A backup that has never been restored is a hope.

## 5. Rotating the data key (tested against real MongoDB)

1. `bash deploy/rust/backup.sh ...` (a fresh backup first).
2. Stop the API and the worker: `docker compose -f deploy/rust/compose.yml --env-file .env stop api worker`.
3. Make the new key and run the rotation with the old environment: `NEW_DATA_KEY=$(python -c "from cryptography.fernet import Fernet; print(Fernet.generate_key().decode())") docker compose ... run --rm -e NEW_DATA_KEY api cg rotate-key --dry-run`, then again without `--dry-run`. It re-encrypts every claim document, reports how many it rewrote, and can be repeated after an interruption.
4. Put the new key in `.env` as `DATA_KEY`, start the API and the worker, check the console and the health panel, keep the old key for one backup cycle, then destroy it.
Only claim data is covered by `DATA_KEY`. `FERNET_KEY` (authenticator seeds) has no rotation tool: to change it every user's authenticator must be re-enrolled with `cg admin reset-totp`.

## 6. Updating

- **The application:** pull the new source, `docker compose ... up -d --build`. The worker and API restart; claims in progress are safe (the queue is in the database).
- **The base images are pinned by digest** (`compose.yml`, `Dockerfile`, the backup scripts), so a rebuild gives the same bytes. That also means *you* take security updates: `docker buildx imagetools inspect <tag>` for the new digest, edit it in those three places, rebuild, run `python rust/tools/deploy_attack.py`. Do this at least monthly and whenever a vulnerability scan (`docker run aquasec/trivy image ...`) reports something in a package the image really uses.
- **Dependencies:** `cd rust && cargo audit` must stay clean apart from the reviewed list in `rust/.cargo/audit.toml`.

## 7. The health panel, component by component

| Component | If it is not green |
|---|---|
| Reviewer API, User database, Queue database | the container or MongoDB is down: `docker compose ps`, then the logs |
| Audit logs (hash chains) | **a chain does not verify: treat it as an incident (section 8.1)** |
| Audit log protection | a verification or mirror job failed or stopped: same, section 8.1 |
| Log anchor | the anchor is not signed or the key is not configured |
| External timestamp | optional; "unknown" means none configured |
| Scheduled jobs and workers | the worker is down or late: restart it; exactly one worker must run |
| Publishing of new claims, Claim flow | claims are stuck: look for dead letters (`cg queue deadletters`) |
| Disk space | the logs cannot grow: free space before anything else, because a full disk refuses privileged actions |
| Model | the AI step is failing: claims keep the engine's own explanation; unset `QUEUE_AI_PROVIDER` to stop trying |

## 8. Incidents

### 8.1 The audit protection alarm (a chain fails, or a log no longer matches its mirror)
1. Do not restart or "fix" anything yet. Note the time and the message.
2. Copy the live logs, the mirror and the anchors to a safe place (`/data/*.jsonl*`, `/mirror`).
3. Compare the log and its mirror: the message says how many leading bytes differ; the first differing row is the first altered row. Every row after the break is untrusted.
4. Decide who had access to the machine and the volumes. Rotate `JWT_SECRET` (signs everyone out) and the administrators' passwords and authenticators.
5. Restore from the last good backup if the data is also in doubt; the mirror and the backup are the evidence.

### 8.2 A user is locked out
An administrator unlocks them in **Users**, or `cg admin unlock --badge CG-xxxx`. A lock never stops the person from signing in from a device they have used before. If it happens often to one person, someone is guessing their badge number: look at the failed sign-ins in the audit panel.

### 8.3 A stolen session is suspected
Rotate `JWT_SECRET` and restart: every session ends. Review the person's `unmask` and `decision` events. The unmask limit (60 an hour) bounds what one stolen session can take out.

### 8.4 A person's data was seen by someone who should not have
The security log records every unmask with who, which claim, and the reason; the review log records every decision. Reasons are refused if they contain the patient's identifier, so they do not repeat it.

## 9. What to run on the target machine, in this order

1. `python rust/tools/deploy_attack.py` (TLS, proxy, network, isolation, image): all checks must pass. Also run the same TLS checks from **another machine** against the real address, and port-scan the VM from outside: only 80 and 443 should answer.
2. `python rust/tools/backup_restore_test.py`, then a real backup of the real data and a restore on a spare machine.
3. `cargo audit`, and a vulnerability scan of the running image.
4. A sign-in from a second device, an unmask, a decision and a sign-out; then `GET /api/v1/audit/verify` (the console's **Verify the logs**).
5. If `ai` was chosen: `cg model-check` on the server, and `python rust/tools/model_redteam.py` pointed at the server's model.
