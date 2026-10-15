# 40 | Operating the Rust build: install, secrets, backup, key rotation, updates, incidents

For the person who runs the server. Everything here was exercised on this laptop with Docker (sections 4 and 5 end to end, with the test scripts named below); none of it has yet been run on the target virtual machine, so run the checks in section 9 there first.

## 1. Install

```
bash deploy/rust/install.sh <address-or-domain> internal|public [ai]
```
- Needs Linux with Docker and the compose plugin, and `openssl`. `internal` is for an address or internal name (a certificate from the proxy's own authority; browsers warn until you trust it); `public` is for a real domain with ports 80 and 443 reachable (a Let's Encrypt certificate). `ai` downloads the model once and hosts it with vLLM, then checks it with `cg model-check` (section 12).
- It writes `.env` with freshly generated secrets **only if `.env` does not exist** (mode 600), builds and starts the stack, waits for health, and asks you to create the first administrator (level 4). You choose the password; the authenticator seed is shown once.
- Only the proxy publishes ports (80, 443). The database and cache sit on a network with no route to the internet.
- Set `CG_SSH_FROM=<your address>/32` when you run the script and it also installs the host firewall (section 10). Without it the script says so and the server relies on Docker alone.

## 2. Settings (environment, in `.env`)

| Setting | Meaning | Default |
|---|---|---|
| `JWT_SECRET` | signs sign-in sessions | required |
| `AUDIT_ANCHOR_KEY` | signs the audit logs' anchors | required |
| `PII_KEY` | makes the stable pseudonyms shown instead of patient and member ids, and hashes client addresses | required |
| `FERNET_KEY` | encrypts the authenticator seeds in the user records | required |
| `DATA_KEY` | encrypts the claim data, the people's names and the audit log files on disk; a Fernet key | empty: derived from `FERNET_KEY` |
| `DATA_KEY_PREVIOUS` | older secrets (comma separated) that rows already written to the audit logs were encrypted under; read-only | empty |
| `DUAL_CONTROL`, `APPROVAL_TTL_SECONDS` | dangerous administrator actions need a second administrator (section 5b); how long a request stays open | true, 86400 |
| `MONGO_ROOT_USER`, `MONGO_ROOT_PASSWORD`, `REDIS_PASSWORD` | database and broker credentials | required |
| `CG_WORKER_CONCURRENCY` | claims processed at once by the worker, and so the most the AI model is asked at once (1 to 64). Keep it at or below the parallel requests the model server really runs; the Compose model server is given the same number (vLLM `--max-num-seqs`) | 4 |
| `CG_BROKER_MIN_IDLE_SECONDS` | how long a message may stay unacknowledged before another consumer takes it over; never below the 90 s explanation ceiling plus 10 s | 120 |
| `CG_REPUBLISH_AFTER_SECONDS` | a claim unfinished this long while the broker is empty is published again (never under 120) | 900 |
| `CG_BROKER_STREAM` | the Redis stream name | `cg:queue:claims` |
| `MONGO_CACHE_GB` | MongoDB's cache size; without a cap it takes half of the machine's RAM | 1 |
| `CG_SITE`, `CADDYFILE` | the address and which proxy configuration | required |
| `QUEUE_AI_PROVIDER=vllm`, `VLLM_BASE_URL`, `VLLM_MODEL`, `VLLM_API_KEY`, `QUEUE_AI_TWO_PASS=1` | the AI explanation step; blank = none. `ollama` (with the `OLLAMA_` variables) is accepted for a laptop | none |
| `AUDIT_MIRROR_DIR` (`/mirror`), `AUDIT_MIRROR_SECONDS` | the append-only second copy of the audit logs | on in the stack, 300 s |
| `AUDIT_VERIFY_SECONDS` | how often both log chains are verified | 3600 |
| `TSA_URL`, `TSA_CA_FILE`, `AUDIT_WITNESS_SECONDS` | an outside RFC 3161 timestamp of each log anchor | off, 3600 |
| `UNMASK_PER_HOUR` | how many claims one person may unmask per hour | 60 |
| `CG_ENCRYPT_AT_REST=false` | turns encryption of claim data, names and audit logs **off** (do not) | on |
| `TOKEN_TTL_SECONDS`, `MAX_FAILED_LOGINS`, `LOCKOUT_SECONDS`, `BCRYPT_ROUNDS` | session length (3600), failures before an account locks (5), lock time (900), password hash cost (12) | as shown |
| (no setting) | the server takes the client address from the last hop of `X-Forwarded-For`; that is safe only because nothing but the proxy can reach the API's port, which the network layout guarantees | |

## 3. The secrets: what each does, and what happens if it changes or is lost

| Secret | If it is **lost** | If it is **changed** |
|---|---|---|
| `JWT_SECRET` | nothing is lost | everyone is signed out once; safe, and the right response to a suspected stolen session |
| `AUDIT_ANCHOR_KEY` | the anchors can no longer be checked | old anchors fail verification: only change it with a plan (verify first, note the break) |
| `PII_KEY` | nothing stored is lost | the pseudonyms shown change (the same person gets a new pseudonym) and the unmask counts and client list restart |
| `FERNET_KEY` | **every authenticator seed is unreadable: nobody can sign in** (and the claim data, names and audit logs too if `DATA_KEY` is empty) | use `cg rotate-fernet-key` (section 5a); nobody re-enrols |
| `DATA_KEY` | **the claim data, the names and the audit log files are unreadable for good** | use `cg rotate-key` (section 5), never edit it by hand |
| `MONGO_ROOT_PASSWORD` | restore from backup | change it in the database and in `.env` together |

Keep a copy of `.env` somewhere other than the server and other than the backups. **The backups do not contain these secrets** and are useless without them.

## 4. Backup and restore (tested: `python rust/tools/backup_restore_test.py`, 23 checks)

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
`DATA_KEY` covers the claim data and the people's names (both are re-encrypted by the command above), and the audit log files on disk. **The audit logs are not rewritten**: that would break their hash chain, the mirror and every timestamp. New rows use the new key; the old rows stay readable only if you put the old secret in `DATA_KEY_PREVIOUS` (the old `DATA_KEY`, or the `FERNET_KEY` value if `DATA_KEY` had been empty). The command prints the exact line to add. Keep it for as long as the old rows exist, and add to the comma-separated list at the next rotation.

### 5a. Rotating FERNET_KEY (authenticator seeds)

`DATA_KEY` must be set first (if it is empty, everything above is derived from `FERNET_KEY`; the tool refuses and tells you to run section 5 first). Then, with the API and worker stopped:
`NEW_FERNET_KEY=<new Fernet key> cg rotate-fernet-key --dry-run`, then again without `--dry-run`, then set `FERNET_KEY` to the new key and start. Every authenticator seed is re-encrypted in place: **nobody has to re-enrol**. It can be repeated after an interruption, and stops with an error (changing nothing for that person) if a seed opens with neither key. Keep the old key until you have signed in once, then destroy it.

### 5b. Two-person approval (dual control)

Four things are never done on one administrator's word: replacing a person's authenticator, creating an administrator, and changing (promoting, demoting, deactivating, editing) an existing administrator. Asking for one gives a **request** (the console says so; the API answers 202), and **nothing changes** until a *different* administrator approves it in **Users → Waiting for a second administrator**, typing a **fresh authenticator code** of their own (so a stolen session alone cannot approve). Five wrong codes in 15 minutes stop that administrator from approving for a while. Requests expire after `APPROVAL_TTL_SECONDS` (a day); the person who asked can withdraw theirs; another administrator can reject it. The log records who asked, who approved, and what was done (`approval` events, and the usual `totp_reset` / `user_created` / `user_updated` events with an `approved_by`). When the request is to replace an authenticator, the person who asked receives the new setup link at once; it works only after approval.

- **It applies once two administrators are active.** A lone administrator acts alone (nobody could approve), and each such action is logged as `done_without_second_administrator`; the health of that is yours to watch: create a second administrator early. `DUAL_CONTROL=false` switches it off entirely.
- **It cannot know that two accounts are two people.** An administrator who controls both accounts defeats it; the protection is organisational (separate people) and the log.
- Break-glass: `cg admin` on the server is not subject to approval (it needs a shell on the machine) and is logged by the operating system, not the application.

## 6. Updating

- **The application:** pull the new source, `docker compose ... up -d --build`. The worker and API restart; claims in progress are safe (the queue is in the database).
- **The base images are pinned by digest** (`compose.yml`, `Dockerfile`, the backup scripts), so a rebuild gives the same bytes. That also means *you* take security updates: `docker buildx imagetools inspect <tag>` for the new digest, edit it in those three places, rebuild, run `python rust/tools/deploy_attack.py`. Do this at least monthly and whenever a vulnerability scan (`docker run aquasec/trivy image ...`) reports something in a package the image really uses.
- **Dependencies:** `cd rust && cargo audit` must stay clean apart from the reviewed list in `rust/.cargo/audit.toml`.

### Memory
Every container has a memory limit in `deploy/rust/compose.yml`, so one runaway cannot take the machine down: proxy 128 MB, API 768 MB, worker 768 MB (both measured at about 70 MB when idle), MongoDB 2 GB with its cache capped at `MONGO_CACHE_GB` (1), Redis 320 MB with `--maxmemory 200mb` and `noeviction` (at the limit a publish fails and the claim waits in the outbox, nothing is dropped), the model server 5 GB of host memory (the model itself sits in the graphics card). The worst case adds up to about 9 GB; the expected use is far lower. **Sizing:** about 2.5 GB without the AI model, about 4.5 GB with a 4B model on the GPU, 5 to 6 GB comfortable. **Measured** (2026-10-11, `docker stats` on the Compose stack right after the 50-check attack test, no model, little data): proxy 17 MB, API 39 MB, worker 10 MB, Redis 9 MB, MongoDB 290 MB: **about 370 MB in all**. Those are idle-ish figures; the worker and MongoDB grow with load and data, which is what the limits are for. The sizing above is a deliberately generous budget, not a measurement. Run `docker stats --no-stream` on the server once with the model loaded and write the real numbers here; the model server's host memory is the one figure estimated and not measured.

## 7. The health panel, component by component

| Component | If it is not green |
|---|---|
| Reviewer API, User database, Queue database | the container or MongoDB is down: `docker compose ps`, then the logs |
| Audit logs (hash chains) | **a chain does not verify: treat it as an incident (section 8.1)** |
| Audit log protection | a verification or mirror job failed or stopped: same, section 8.1 |
| Log anchor | the anchor is not signed or the key is not configured |
| External timestamp | optional; "unknown" means none configured |
| Scheduled jobs and workers | the worker is down or late: restart it; exactly one worker must run |
| Message broker (Redis) | down: claims wait safely in the outbox and are sent when it returns (section 8.5). Degraded: a message has been in progress for over five minutes. The detail shows how many messages wait, how many are in progress and how many consumers there are |
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

### 8.1b An audit log cannot be read ("cannot be decrypted")
The key does not match the rows. Check `DATA_KEY` (and `DATA_KEY_PREVIOUS` after a rotation, or `FERNET_KEY` if `DATA_KEY` is empty). Do not "fix" the log. The standalone `cg audit-verify <log>` also needs the key in the environment (`DATA_KEY` or `FERNET_KEY`) when the log is encrypted.

### 8.5 Claims are piling up, or Redis lost its data

1. Open the health panel, *Message broker*. **Many waiting, few in progress:** the model is the slow part. Check *Model*; raise `ai_per_minute` in the routing configuration if it is capping the explanations (default 30 a minute), or raise `CG_WORKER_CONCURRENCY` *and* the model server's parallel slots together. Claims never wait on the model forever: past the 90 s ceiling a claim keeps the engine's own text.
2. **Redis down:** nothing is lost. New claims keep their outbox marker in MongoDB; the worker's consumers retry with a growing pause and resume by themselves.
3. **Redis came back empty** (restarted without its data, or flushed): the claims it held are still in MongoDB in `triaged`. Within about 15 minutes the worker's `republish` job sends them again, but only when the stream is empty. To do it at once, restart the worker after lowering `CG_REPUBLISH_AFTER_SECONDS` to 120. Nothing needs to be re-submitted.
4. A claim that failed three deliveries is a dead letter: `cg queue deadletters`, then `cg queue deadletter-replay ID` once the cause is fixed.

### 8.2 A user is locked out
An administrator unlocks them in **Users**, or `cg admin unlock --badge CG-xxxx`. A lock never stops the person from signing in from a device they have used before. If it happens often to one person, someone is guessing their badge number: look at the failed sign-ins in the audit panel.

### 8.3 A stolen session is suspected
Rotate `JWT_SECRET` and restart: every session ends. Review the person's `unmask` and `decision` events. The unmask limit (60 an hour) bounds what one stolen session can take out.

### 8.4 A person's data was seen by someone who should not have
The security log records every unmask with who, which claim, and the reason; the review log records every decision. Reasons are refused if they contain the patient's identifier, so they do not repeat it.

## 9. What to run on the target machine, in this order

1. `python rust/tools/deploy_attack.py` (TLS, proxy, network, isolation, image): all checks must pass. Also run the same TLS checks from **another machine** against the real address, then `python3 deploy/rust/portcheck.py <address>` from a machine that is not on the SSH allow-list (only 80 and 443 may answer; section 10).
2. `python rust/tools/backup_restore_test.py`, then a real backup of the real data and a restore on a spare machine.
3. `cargo audit`, and a vulnerability scan of the running image.
4. A sign-in from a second device, an unmask, a decision and a sign-out; then `GET /api/v1/audit/verify` (the console's **Verify the logs**).
5. If `ai` was chosen: `cg model-check` on the server, and `python rust/tools/model_redteam.py` pointed at the server's model.

## 10. The host firewall (`deploy/rust/firewall.sh`, `deploy/rust/portcheck.py`)

**The trap.** Docker publishes a container port by rewriting packets (the `nat` table) and forwarding them. Those packets never reach the `INPUT` chain where ufw and most guides put their rules, so `ufw deny 6379` does **not** close a port that a compose file published. Docker reserves the `DOCKER-USER` chain for your rules on that traffic. This stack publishes only 80 and 443 (a test enforces it), so today nothing sensitive is exposed; the firewall is the second lock for the day someone adds a `ports:` line by mistake, and for the host itself (SSH, anything else installed on the machine).

**What the script does.** Two chains of its own, jumped to first from `DOCKER-USER` and `INPUT`, for IPv4 and IPv6:
- containers: replies, and traffic that arrived for port 80 or 443, pass; everything else coming in from the outside interface is dropped;
- the host: replies, loopback, container bridges, DHCP answers, limited ping (ICMPv6 for IPv6), 80, 443, and SSH **only from `CG_SSH_FROM`**; everything else is dropped. It does not touch ufw: ours run first, so a stray "allow 22 from anywhere" in ufw is shadowed.

```
CG_SSH_FROM=203.0.113.7/32 bash deploy/rust/firewall.sh apply --dry-run          # read what it will do first
CG_SSH_FROM=203.0.113.7/32 bash deploy/rust/firewall.sh apply --auto-revert 120  # then apply; open a SECOND ssh session, answer yes within 120 s or it undoes itself
CG_SSH_FROM=203.0.113.7/32 bash deploy/rust/firewall.sh persist                  # re-apply at every boot, after Docker is up (systemd)
bash deploy/rust/firewall.sh status | remove
```
It refuses to run without `CG_SSH_FROM` (it cannot guess where you connect from, and a wrong guess locks you out of a machine you cannot walk up to). `any` is accepted with a warning. Values are checked against exact address shapes before they reach a command.

**Verify from outside, every time you change the firewall or the compose file.** On a machine that is not allowed to use SSH: `python3 deploy/rust/portcheck.py <server address>` must print `PASS` (80 and 443 only). From the administration address: add `--ssh open`. Never run it on the server itself: traffic to its own address goes over loopback, which is always allowed, and proves nothing. A dropped port shows as *filtered*, a refused one as *closed*; only *open* counts against you. UDP cannot be scanned this way; `firewall.sh status` shows the rules.

**What was tested, and what was not.** `rust/tools/firewall_test.py` applies the script for real in throwaway Linux containers (a client, a router that forwards ports 80, 443, 6379 and 11434 the way Docker does, and a backend): before the script the database and model ports answer from outside, after it only 80 and 443 do, SSH answers only to the allowed address, a second apply adds no second jump, `remove` leaves nothing behind, and `--auto-revert` without an answer removes the rules (15 checks). `tests/test_deploy_firewall.py` checks the dry-run output and the input validation. **Not tested:** the real server's kernel, distribution, ufw/firewalld/nftables setup and cloud security groups (a provider firewall in front of the machine is a good third lock); the systemd unit on a real boot; a flood (the script does not rate-limit SYN floods or defend against distributed denial of service, which needs the provider's help); rule changes made by other software after the script ran.

## 11. Reviewing the console in a real browser (`rust/tools/console_review.py`)
Against a local demo server (`cg serve --demo`), it signs in as each demo role in Chrome, opens every page at desktop and phone width plus one claim in the work view, saves screenshots, and fails on a console error or warning, an HTTP error, an uncaught exception, or a page that scrolls sideways. Run on 2026-10-12 it found and the next commit fixed: pages wider than a phone screen (tables forced the column open), no gap between two cards on the Users page, an orphan seventh summary tile, the shift list cut off after three badges, an AI box that only repeated the line above it, and long decision forms (options now two per row on wide screens). It does not judge colour, wording or usability; it is a regression check, and the screenshots still need a person's eye. It must only be pointed at a local demo server.

## 12. The model server (vLLM)

**Decision (2026-10-15): in production the explanation model is served by vLLM.** Status: designed and scripted, **not run**. The reasons are about the load the system must carry, not fashion: the plan is hundreds of claims a minute against one GPU, vLLM batches concurrent requests continuously (a server that handles requests one after another would queue claims behind each other), it exposes the OpenAI-compatible API the client already speaks (so the application did not change), it is an open-source server that runs entirely on the hospital's own machine, and one pinned container image plus an offline weight download is a deployment someone else can repeat. What exists is the script (`deploy/rust/install.sh ... ai`, `deploy/rust/fetch_model.py`, the compose and Kubernetes files) and the configuration below; what does not exist is a measurement. The first real run on the target server, with `cg model-check`, is the step that turns this decision into evidence.

**What runs.** The AI explanation step asks an OpenAI-compatible server on the private network. In production that server is **vLLM** (`vllm/vllm-openai`, pinned by digest in `deploy/rust/compose.yml`); the Rust client sends the same `POST /v1/chat/completions` it always did, with the server's key as a bearer token. The model sees only the rulebook's own text, never a claim value, and never sets a status; if the server is slow, down or wrong, claims keep the engine's own explanation and nothing waits (the breaker, `docs/38`). Ollama is still accepted (`QUEUE_AI_PROVIDER=ollama`) because vLLM does not run natively on a Windows laptop.

**Why a change of server is not a rename.**
- vLLM serves **one model per server process**. The explanation model and the document reader's optional vision model therefore have two servers (`vllm` with `--profile ai`, `vllm-vision` with `--profile vision`), and they split the card's memory: `VLLM_GPU_UTIL` (default 0.55) plus `VLLM_VLM_GPU_UTIL` (0.30) must stay below 1. Ollama kept both models loaded in one process.
- vLLM names a model by its Hugging Face repository id (`google/gemma-3-4b-it`), not an Ollama tag (`gemma3:4b`), and takes its weights from a directory, not from a registry pull.
- vLLM batches requests continuously; `--max-num-seqs` is set to `CG_WORKER_CONCURRENCY`, so the worker's pool remains the one limit on how many claims ask at once.

**Offline by design.** The model servers sit on networks with no route out and run with `HF_HUB_OFFLINE=1`. The weights are fetched once by `vllm-fetch`, a one-shot container that is the only one of the model containers with internet access (`deploy/rust/fetch_model.py`; on Kubernetes `deploy/k8s/vllm-fetch-job.yaml`). `install.sh ... ai` does it first, before anything starts. Gemma is a gated model: accept its licence on huggingface.co with an account and run `HF_TOKEN=hf_... bash deploy/rust/install.sh <address> internal ai`; the token is used by the download only and is not stored in `.env`. A commit hash in `VLLM_MODEL_REVISION` pins the download. Another model: `CG_MODEL=<repository> bash deploy/rust/install.sh ... ai` (and `CG_VLM_MODEL=<repository> ... ai docs` for a vision model).

**The key.** `VLLM_API_KEY` (generated by `install.sh`, kept in `.env`) is required: the server refuses a request without it, and the client treats a wrong key (HTTP 401) or a wrong model name (404) as a configuration error that never opens the breaker, while 5xx, 429 and 503 (the server is still loading) count as outages and are retried.

**The card (RTX 3080, 10 GB).** The default is `google/gemma-3-4b-it` with `VLLM_EXTRA_ARGS=--quantization fp8`: vLLM quantizes the weights to FP8 as it loads them. On an Ampere card this runs as weight-only FP8 (vLLM's documentation: FP8 models run on compute capability above 8.0 as weight-only W8A16 through Marlin kernels), roughly half the size of the 16-bit weights, so the 4B model fits with room for the cache. `VLLM_MAX_LEN` is 2048 (an explanation prompt is about 600 tokens, the answer at most 500). On a card with more memory set `VLLM_EXTRA_ARGS` to a single space.

**The driver.** The image is the CUDA 12.9 build (`v0.31.0-cu129`). The release without the `-cu129` suffix targets CUDA 13 and needs a much newer NVIDIA driver. Check `nvidia-smi` on the server before choosing; to change the image set `VLLM_IMAGE` in `.env` to another tag with its digest (`docker buildx imagetools inspect vllm/vllm-openai:<tag>` prints it) and keep it pinned.

**Checks, in this order.**
1. `docker compose -f deploy/rust/compose.yml -f deploy/rust/compose.gpu.yml --env-file .env --profile ai ps`: `vllm` is `healthy` (the first start takes several minutes: loading, FP8 quantization, graph capture).
2. `nvidia-smi`: the card's memory in use is about the share you set; if the server died at start with an out-of-memory or "no available memory for the cache blocks" error, lower `VLLM_GPU_UTIL` or `VLLM_MAX_LEN` in `.env` (or raise them if the vision server is off) and `docker compose ... up -d vllm`.
3. `docker compose ... exec worker cg model-check`: asks the model for the template of each of the 15 rules and reports how many came back in the required shape and how long each took. Exit 0 only if all 15 were accepted.
4. The health panel's AI line says which model the worker uses and whether the breaker is closed.

**What has and has not been verified.** Verified: the Rust client against a fake OpenAI-compatible server (the key as a bearer token, the served model name, the error classes), the environment selection, `docker compose config` on the whole file with every profile, the structural tests of the compose and Kubernetes files, that the chosen image tag exists and its digest. **Not verified: no vLLM server has been started in this build, on any GPU.** The FP8 path on an RTX 3080, the memory split between the two servers, the start time, the throughput and the quality of the drafts are all unmeasured. Every earlier figure for this step (accepted drafts, latency, `docs/25`, README) was measured through Ollama with a 4-bit build of the same model and has **not** been repeated through vLLM: run `cg model-check`, `rust/tools/model_redteam.py` (`VLLM_API_KEY=... python rust/tools/model_redteam.py`) and the explanation benchmark on the real server before quoting a number.

**Not changed.** The Python reference build (`src/`, `deploy/compose.yml`) keeps its Ollama provider; it is not the deployed system.
