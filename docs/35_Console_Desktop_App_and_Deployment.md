# The reviewer console, the desktop app and deployment on one server

This document covers three things added after the reviewer API: the **web console** people actually use, the **desktop app** that shows the same console in its own window, and the **deployment** of the whole system on one server with Docker Compose. Each section says what was built, how it was checked, and what was not.

## 1. The reviewer console (web)

A single-page app served by the API itself at `/` (files in `src/access/ui/`, no build step, no CDN, no external request). It is one codebase for the browser and the desktop window.

| Person | Pages they see | Server permission behind it |
|---|---|---|
| Reviewer (level 2), senior (level 3) | My inbox, claim review, All claims | `claims.decide`, `claims.view` |
| Viewer (level 1) | All claims (read only) | `claims.view` |
| Administrator (level 4) | Queue overview, Submit claims, Routing settings, Users, Audit trail | `queue.view`, `routing.manage`, `users.manage`, `audit.view` |

What a reviewer can do on a claim: read the claim and its lines, read each flagged finding with its evidence and the rulebook's corrective action, read the AI draft explanation (labelled as a draft; the rule engine alone sets the status), and record one of four decisions with a written reason. A claim no rule flagged is shown as such and the reviewer verifies it or sends it up. The lease countdown is on screen and a heartbeat keeps the lease alive while the page is open. Level 2 and above may unmask identifiers for a stated reason; the act is logged.

What an administrator can do: see the queue (waiting, oldest wait, shortages, inbox sizes, what reviewers decided and the dismissal-rate interval per rule), change who is on shift and the formula's numbers (a new configuration version each time, never a per-claim assignment), upload claims (JSONL or a JSON list, sent in batches of 20 through the same intake as the command-line tool), manage users (create with a QR code for the authenticator app, unlock, reset the authenticator, deactivate), and read and verify the two audit logs. Level 4 still cannot decide a claim.

**Security properties, each tested (`tests/test_ui_console.py`, `tests/test_wq_submit.py`):**

- Pages are chosen by the permissions the server reports (hide, not disable), and the server checks every one again on each request.
- The console's Content-Security-Policy allows scripts and styles from its own origin only (no inline script, no inline style, no `eval`, no framing, no other origin for connections or images except `data:` for the QR code). Every API route keeps `default-src 'none'`.
- Text from the server (claim fields, notes, AI text, names) is only ever put on the page as text. A test scans every script for markup sinks (`innerHTML` and its relatives, `document.write`, `eval`, `new Function`, inline `style` attributes) and fails if one appears.
- Files are served from a table read once at start-up, so a request can only name a listed file (path traversal tests included).
- The session cookie stays `httpOnly`, `Secure` and `SameSite=Strict`; the CSRF token is sent as a header on every change.
- The authenticator QR code is drawn in the browser by a vendored library (qrcode-generator 1.4.4, MIT, hash recorded in `src/access/ui/vendor/README.txt`); the seed never goes to a third party.

**New route:** `POST /api/v1/queue/submit` (level 4, `routing.manage`): up to 25 claims per call, each checked, run through the 15 rules and given a triage receipt; one bad claim is reported and never stops the others; the same claim twice is stored once. Before this the only way in was `scripts/queue_admin.py`. The security log records `queue_admin` with `submit_http`.

**Not done / limits.** The console was driven by an automated browser (jsdom) against a live server and shown once in the desktop window; it has not been through a manual accessibility audit or been tested on a phone. The console has no live push: lists refresh every 15 to 20 seconds. There is no recheck screen (`recheck` is still not built, FR-18).

## 2. The desktop app

`desktop/claimguard_desktop.py` shows the console in a native window (Microsoft Edge WebView2 through pywebview).

```bash
python desktop/claimguard_desktop.py --server https://claims.example.org   # a real server; remembered for the next start
python desktop/claimguard_desktop.py --local-demo                          # a private demo on this PC
powershell -ExecutionPolicy Bypass -File desktop/build_windows.ps1         # builds desktop/dist/ClaimGuard.exe (about 14 MB)
```

- A remote server must be `https`; plain `http` is accepted only for this PC. The address check rejects credentials in the URL, other schemes and malformed ports (`tests/test_desktop.py`).
- The window holds no data and no secret. Sign-in, cookies and permissions are the server's, exactly as in a browser.
- The **local demo** starts the real server on a free port bound to this PC, with generated accounts, 60 sample claims, and the worker loop running inside the same process (`src/workqueue/demo.py`: the same relay, deal and expire functions the Celery worker runs, on a thread). The sign-in page offers one-click demo logins only inside this window: they come from a bridge object that exists only in the local demo, so a real server's page never has it.
- The packaged `ClaimGuard.exe` is the window only (server mode). The local demo needs the source checkout.
- Checked: the built program starts, connects and shows the sign-in page. **Not done:** code signing (Windows SmartScreen will warn on an unsigned program), an installer, auto-update, macOS and Linux builds.

For a demo in a browser instead: `python scripts/serve_access.py --demo`, open the printed address, and get a sign-in code with `python scripts/demo_code.py CG-2002`.

## 3. Deployment on one server

One machine, Docker Compose, everything on that machine (no cloud service). Files are in `deploy/`.

```
internet / LAN ──443──> proxy (Caddy, TLS) ──> api (console + REST) ──> mongo (truth)
                                               worker (Celery + scheduler) ──> redis (broker only)
                                               ollama (optional, local model)
```

Only the proxy publishes ports. The API, MongoDB and Redis are on the private compose network. The containers run as a non-root user, read-only, with all capabilities dropped and `no-new-privileges`.

**Install (Linux server with Docker and the compose plugin):**

```bash
git clone https://github.com/PublisherX02/Claim_Guard.git && cd Claim_Guard
bash deploy/install.sh 203.0.113.10 internal     # an IP address or internal name: certificate from Caddy's private authority
bash deploy/install.sh claims.example.org public # a real domain pointing here: Let's Encrypt certificate
```

`install.sh` writes `.env` with freshly generated secrets (mode 600, only if it does not already exist), builds and starts the stack, waits for the API to be healthy and asks you to create the first administrator (you choose the password; the authenticator seed is shown once). Then open `https://<address>/`, add reviewers under Users, put them on shift under Routing settings and upload claims under Submit claims.

**Keep `.env` safe and backed up.** Without `FERNET_KEY` the authenticator seeds cannot be read, and without `PII_KEY` pseudonyms change. Regenerating `.env` on an installed system locks everyone out.

**Optional local model.** Set `QUEUE_AI_PROVIDER=ollama` and `OLLAMA_MODEL=<model>` in `.env`, start with `--profile ai`, then `docker compose -f deploy/compose.yml --env-file .env exec ollama ollama pull <model>`. The model sees only the rule's own text and the failure's shape, never a claim value (`src/workqueue/model_adapter.py`). Without it each flagged finding keeps the engine's own explanation. A GPU needs the NVIDIA container toolkit and the commented block in `deploy/compose.yml`. Which model to choose is still an open question for the mentor (on-premises model under 15B).

**Backups and updates.** `bash deploy/backup.sh` writes `backup/<time>/mongo.archive.gz` and `audit-logs.tgz` with checksums (schedule it with cron and copy it off the machine). Restore: stop the stack, `docker compose ... exec -T mongo mongorestore --archive --gzip --drop -u ... -p ... --authenticationDatabase admin < mongo.archive.gz`, and untar the audit logs into the `claimguard_claimguard_data` volume. Update: `git pull` then `docker compose -f deploy/compose.yml --env-file .env up -d --build`. Run exactly one worker: it also runs the scheduler.

**Checked (2026-10-08, on a Windows PC with Docker Desktop):** the stack built and started with all five services; the API reported healthy through the proxy over HTTPS; a real Celery worker with Redis and MongoDB processed 40 uploaded claims, the dispatcher dealt 8 into a reviewer's inbox, a decision was recorded, and the security log verified. This run found a bug that is now fixed: audit verification reported a failure while no review decision had been written yet (`test_verify_treats_a_review_log_with_no_decisions_yet_as_intact`).

**Not done / limits.** Not run on the real server yet (that run is the next step: a Linux VM may differ in Docker version, firewall and certificate behaviour). Caddy's internal certificate needs trusting on each client, or the desktop app and browsers warn. The security log and the review log live on a Docker volume and are not append-only at the storage level (see `docs/32` section 4). Single machine: no high availability, no database replica set. No monitoring stack: logs are `docker compose logs`, health is the `healthz` route and compose healthchecks. The `TRUSTED_PROXY_IPS` default (`*`) is safe only because the API port is not published; pin it to the proxy's address if the network layout changes. MongoDB has authentication but no TLS between containers (private network).
