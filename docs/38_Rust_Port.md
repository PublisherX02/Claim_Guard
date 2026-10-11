# The Rust build of ClaimGuard

**What this is.** After the language comparison (`docs/36`) the team chose Rust for the whole system. This document describes the Rust build that now sits beside the Python one: what it is made of, how it was proved to behave like the Python reference, how it is run, how it was attacked, and what it still does not do. The Python tree is kept unchanged as the reference and as the oracle for every comparison below.

**Short answer.**
1. **Same behaviour, checked by machine.** Every layer was compared with the Python build on the same inputs: the 15 rules on 10,600 claims, the 8 extension rules on 6,000 claims, the audit log, passwords, session tokens, authenticator codes and pseudonyms value by value, the AI step's prompts (181 of 181 byte-identical), and the HTTP API on 132 requests as four different roles. The only API difference left is explained in section 4.
2. **Smaller and lighter.** The container image is 159 MB (Python: 362 MB), the server starts in 3 s instead of 5 and uses about 25 MB less memory; request speed is about the same (section 6).
3. **Attacked, not just tested.** An 83-check black-box penetration test over real sockets, a 400,000-request fuzz of the API boundary, a mutation test of 51 security-rule breakages (all caught), a 50-check attack on the deployed Docker stack, a red team of the AI step with the real gemma3:4b, a browser-side check in Chrome and a dependency audit (`docs/39`); how to operate it is in `docs/40`. The pen test found two real weaknesses (slow-connection exhaustion and an oversized-header allowance); both are fixed and covered.
4. **Not finished:** cloud model providers and the recheck route. The Redis Streams broker is built (section 9). Section 7 lists everything that is missing.

---

## 1. What it is made of

One Cargo workspace in `rust/`, nine crates. A crate never reaches "up": the engine knows nothing of the web, the web layer knows nothing of the database.

| Crate | Job | Python counterpart |
|---|---|---|
| `cg-core` | numbers that behave like Python's (exact integers and decimals, float text), strict dates, the claim as the rules see it, transport checks | `engine_core.py`, `facts_extractor.rule_view` |
| `cg-engine` | the 15 rules and the 8 extension rules; YARA-X rule packs; extension history | `facts_extractor.py`, `yara_engine.py`, `extension_rules.py`, `claim_history.py` |
| `cg-audit` | the hash-chained audit log, canonical JSON, anchor file | `audit_log.py` |
| `cg-timestamp` | the RFC 3161 trusted timestamp on the log's anchor: request, HTTP exchange with a Time-Stamp Authority, full verification (CMS signature, certificate chain to a trusted CA, validity at the stamped time, timeStamping usage, nonce), sidecar and status | `timestamp_anchor.py` |
| `cg-access` | badge, password, authenticator code, session tokens, four clearance levels, masking, security log | `src/access/` |
| `cg-mongo` | MongoDB stores for users and for the queue | `store_mongo.py`, `workqueue/store_mongo.py` |
| `cg-queue` | triage lanes, dealing, leases, two-person sign-off, AI step (guard, breaker, cache, model client), replay | `src/workqueue/` |
| `cg-api` | the reviewer API as a plain function from request to response, health and audit analytics, the hardened HTTP server | `access/api.py`, `workqueue/api.py`, `access/ops.py` |
| `cg-cli` | the `cg` program: `serve`, `worker`, `admin`, `queue`, `evaluate`, `extensions`, `health`, `audit-verify` | `scripts/` |

About 13,900 lines of Rust including tests, against about 10,200 lines of Python for the same source (without the console's JavaScript, which is shared and unchanged: the Rust server embeds `src/access/ui/` in the binary).

**Design decisions that matter.**
- **The API is a function.** `Api::handle(request) -> response` has no web framework in it, so every rule (CSRF, permissions, masking, throttling) is tested directly, and the HTTP layer is a thin adapter that can be replaced.
- **Documents are JSON values.** A claim is carried as `serde_json::Value`, the way Python carries a dict; numbers keep their exact text (`arbitrary_precision`), and money uses a decimal type, so `2.5` and `2.50` and very large integers behave as they do in Python.
- **Two stores, one contract.** The in-memory twin and MongoDB pass the same contract test suite.
- **Failure closes.** A rule that panics becomes UNABLE_TO_ASSESS for that rule only; a claim that cannot be read gets 15 UNABLE_TO_ASSESS; an unreachable user database refuses logins.

## 2. Same data, both ways

The two builds read and write each other's files and database records. That is a stronger promise than "same answers": an operator can run the Python tools on a Rust database, or the reverse, and the audit chain stays valid.

| Shared format | Checked by |
|---|---|
| Audit log lines, hash chain, anchor file | `rust/tools/compare_audit.py`: 24 checks, a log written by one side verifies on the other |
| bcrypt hashes, JWT session tokens, Fernet-encrypted authenticator seeds, TOTP codes, identifier pseudonyms, provisioning URIs | `rust/tools/compare_access.py`: 25 checks in both directions |
| Rule results for a claim | `rust/tools/compare_engine.py`: public splits + 3,000 generated + 3,000 damaged claims, every field equal |
| Extension results | `rust/tools/compare_extensions.py`: 6,000 claims with earlier-claim history, every rule fires, passes, is not applicable and is unable to assess in the corpus; 0 differences |
| The AI step's prompts | `rust/tools/compare_model.py`: 181 prompts (15 rules x statuses x severities x line counts), byte-identical |
| RFC 3161 requests, tokens and sidecars | `crates/cg-timestamp` tests: the request is byte-identical to Python's, and 19 tokens signed by the Python test authority (valid, wrong head or count or nonce, forged signature, untrusted issuer, no or non-critical timeStamping usage, expired certificate, garbage, trailing bytes, flipped bytes) get the verdict Python gives; `rust/tools/compare_timestamp.py`: each build stamps over real HTTP and the other verifies, stale and rewritten logs, a TSA that is down (10 checks) |

**One deliberate difference.** The dealing of claims to reviewers is seeded and replayable, but the random generator is different (ChaCha8 instead of Python's Mersenne Twister), so a deal recorded by one build can be replayed and verified only by the same build. Verification of a stored deal never crosses languages.

## 3. How to run it

```
cd rust && cargo build --release -p cg-cli          # builds rust/target/release/cg(.exe)
cg serve --demo                                     # a private demo: sample accounts, 60 claims, workers in-process, console at http://127.0.0.1:8443/ui/
cg serve --queue --behind-proxy --host 0.0.0.0      # production API (secrets and MONGO_URI from the environment); TLS is the proxy's job
cg worker                                           # production queue worker (relay, deal, expire, reconcile); exactly one instance
cg admin create-admin --badge CG-0001 --name "..."  # accounts (also: list, unlock, reset-totp)
cg queue --badge CG-4004 reconcile                  # operator tools (also: submit, replay, rerun, deadletters, deadletter-replay, verify-deal)
cg evaluate claims.jsonl                            # the 15 results per claim as JSON lines
cg timestamp stamp --log audit.jsonl --tsa-url https://tsa.example/tsr   # RFC 3161 stamp of the log's anchor (also: verify --tsa-ca ca.pem)
cg extensions claims.jsonl                          # the 8 advisory results, history taken from the same file
```

- **Container stack:** `bash deploy/rust/install.sh <address> internal|public [ai]` (generates the secrets, starts the stack, waits for health, creates the first administrator, and with `ai` hosts and checks the Ollama model with `cg model-check`), or by hand `docker compose -f deploy/rust/compose.yml --env-file .env up -d --build`. The stack was brought up from the built image with throw-away secrets on 2026-10-09: all five containers healthy, the console and `/healthz` answered through the TLS proxy with the HSTS header (same proxy, MongoDB, Redis and optional Ollama as `deploy/compose.yml`, with `cg serve` and `cg worker` in place of the Python server and Celery). The image has a health check that needs no extra tools (`cg health`).
- **Desktop:** `desktop/dist/ClaimGuard.exe` contains the window, the Rust server, the rule files and the sample claims, so "local demo" runs from the exe alone; the server stops with the window, even if the window is killed.
- **Model:** set `QUEUE_AI_PROVIDER=ollama` and `OLLAMA_BASE_URL=http://ollama:11434` (plain HTTP on the private network, by design). Without it, every claim keeps the engine's own explanation.

## 4. Behaviour compared at the HTTP boundary

`rust/tools/conformance.py` starts both servers in demo mode and sends 132 requests as the four roles (viewer, reviewer, senior, administrator): listings with good and bad parameters, claim views, unmask, decisions with good and bad bodies, audit queries, health, users, queue routes, console files, wrong methods. Status codes and the JSON shape of every answer are compared (volatile values ignored, fixed vocabularies such as error codes, review states, allowed actions and permission lists compared exactly).

**Result: 131 of 132 identical.** The differences found, and the one left:
- *Decision rejection detail* (found by this test, fixed): the Rust API did not return the explanatory `detail` text with `decision_rejected`.
- *Extension results in the inbox* (found by this test before the extension rules were ported, now fixed): the Rust inbox carried no advisory results.
- *Senior inbox* (still differs, by design of the test): each server deals different claims to the reviewers, so the first claim in the list is a different claim with a different optional-field shape; the shape of every claim and advisory field is equal in the other requests.

`rust/tools/e2e_split.py` runs the production topology on a real MongoDB: `cg serve --queue` and `cg worker` as two processes, an administrator who puts reviewers on shift and submits 25 claims, the worker process triaging and dealing them, a reviewer deciding through the queue routes, the claim trace, the health panel (showing the worker alive) and the audit chains verifying. All steps pass.

## 5. Attack and stress results

### 5.1 Black-box penetration test (`rust/tools/pentest.py`, 83 checks)

Authorized scope: this application on 127.0.0.1. The script starts a demo server and attacks it over real sockets.

| Area | Attempts | Result |
|---|---|---|
| Headers, CORS, methods | missing security headers, version banner, foreign `Origin`, TRACE/CONNECT/unknown verbs | all refused or absent |
| Smuggling and malformed HTTP | Content-Length with Transfer-Encoding, two different Content-Lengths, NUL in a header, obsolete header folding, garbage, HTTP/1.0, no Host | all refused |
| Limits | 70 KB header, 70 KB request line, 80 KB body, 100,000-deep JSON, duplicate JSON keys, invalid UTF-8 | refused cleanly (**the 70 KB header was accepted until this test; fixed**) |
| Slow and many connections | 300 half-open slow connections while another client is served; are they cut off? | server stays responsive (**connections were held open forever until this test; fixed with a 10-second header timeout, a 16 KB header cap and a 2,048-connection cap**) |
| Authentication | timing of known and unknown badge, identical answers, throttle with rotated `X-Forwarded-For`/`X-Real-IP`/`Forwarded`, NoSQL operators and arrays in login fields, mass fields, password 5,000 characters, one-time code reuse | no enumeration; throttle cannot be dodged; all refused |
| Sessions | forged tokens (algorithm none, five weak signing keys, swapped payload, algorithm confusion), reuse after logout, cookie flags | all refused |
| CSRF | no token, another user's token, form-encoded and plain-text cross-site style posts | all refused |
| Authorization | every role against 15 routes (180 combinations), administrator reading or deciding claims, mass assignment, self-promotion, granting an administrator right to a reviewer, deactivating the last administrator, writing a password hash, deciding another reviewer's lease | exactly as the permission table says |
| Business logic | invalid decision bodies, countersigning one's own decision, eight parallel configuration updates against one version | one winner, seven 409s |
| Resource exhaustion | oversized page limits, a 60-request login flood while the health check is polled | refused; health answers within 1.5 s |
| Information leakage | error bodies for framework, path and library names; user list for password hashes or seeds | none |

### 5.2 Fuzzing at the API boundary (`rust/crates/cg-api/tests/api_fuzz.rs`)

Random method, path, query, headers and body (binary, text with control and bidirectional characters, random JSON with injection-style keys), with and without sessions of each role: **400,000 requests, no 5xx**, no leaked internal text, every error a JSON object with an `error` code, nothing protected reachable without a session, and forged or truncated tokens refused. 30,000 of these run in the normal test suite.

### 5.3 Mutation test of the security rules (`rust/tools/mutate.py`)

Each mutant breaks one security rule in one line (CSRF check off, permission check always true, throttle never blocks, cookie not HttpOnly, body limit enlarged, high-severity check off, forwarded-for trusted without a proxy, token expiry ignored, revoked tokens accepted, one-time code reusable, a viewer allowed to unmask, notes always visible, a signer allowed to countersign their own decision, a lease not tied to its owner, an expired lease still usable, an unflagged finding decidable, and others). The tests must fail for each one.

**First run: 36 mutants, 28 caught, 8 survived** (one pattern also matched twice and was made specific). Each survivor was a real gap: no test checked the forced-password-change gate, that a mid-level reviewer cannot decide a high-severity finding, the framing header, that login and decision bodies refuse unknown fields, that a viewer sees no action buttons, that a refused request is written to the security log, the exact permission list of each level, or that a passed finding cannot be decided. Tests were added for each (and one loose assertion tightened). **After the fix: all 36 mutants are caught.** The Python queue has its own 77-mutant run (`tests/mutation_queue.py`); the two are not the same list.

### 5.4 What the pen test cannot show

It attacks the server process, not the deployment. TLS, the proxy's header rules, the container's file-system and capability limits, the database's own authentication and the firewall of the target machine are configured by the deployment (`deploy/`) and have to be checked on the target. An account lockout is deliberate and can itself be provoked by someone who knows a badge number (five wrong passwords lock the account for 15 minutes; an administrator can unlock). The login throttle counts per client address in the memory of one process.

## 6. Footprint

Same laptop, both servers in demo mode with the same 60 sample claims and the in-process workers, one signed-in reviewer sending requests one after another over a new connection each time (`rust/tools/footprint.py`; one afternoon, one noisy Windows machine, so read these as an order of magnitude):

| | Python (FastAPI + uvicorn) | Rust |
|---|---|---|
| Time until the server answers | 5.1 s | 3.1 s |
| Memory, idle | 92 MB | 67 MB |
| Memory, after 800 requests | 94 MB | 66 MB |
| `GET /claims?limit=50`, median / 95th percentile | 16.7 / 35 ms | 15.3 / 30 ms |
| `GET /work/inbox`, median / 95th percentile | 43.7 / 66 ms | 32.3 / 50 ms |
| Container image | 362 MB | 159 MB |

Most of a request here is opening a connection on Windows, not the work; the rules themselves cost microseconds (`docs/36`), and a person waits seconds for the AI step either way. The measurement found one real problem: the first Rust claim listing was *slower* than Python (37 ms) because it copied every claim's results on each request; the listing summaries are now computed once when the store is built. The honest summary is that Rust is a little lighter and quicker, not an order of magnitude, for this workload; the reasons to prefer it are memory safety, the small image, no interpreter to patch, and one binary to ship.

## 7. What is not done

- **Cloud model providers.** Only an on-premises OpenAI-compatible server (Ollama) is supported, over plain HTTP on the private network. Sending even rule text to a cloud service is not wanted for a hospital.
- **Recheck route.** Answers 501 in both builds; a correction is resubmitted as a new version of the claim.
- **Redis.** Now a real broker (section 9). Without `REDIS_URL` the worker processes claims inline as before. Whether Redis Streams is acceptable in place of Celery or ARQ is an open question for the mentor.
- **TLS inside the server.** Terminated by the proxy only (`--behind-proxy`).
- **Not run on the target server yet.**

## 8. Checking it again

```
cd rust && cargo test --release --workspace                           # 195 tests (set MONGO_URI and REQUIRE_MONGO=1 to include the real-database contracts)
python rust/tools/compare_engine.py && python rust/tools/compare_extensions.py
python rust/tools/compare_audit.py && python rust/tools/compare_access.py && python rust/tools/compare_model.py
python rust/tools/conformance.py                                       # starts both servers
python rust/tools/e2e_split.py --mongo "mongodb://user:pass@host:27017/?authSource=admin"
python rust/tools/pentest.py                                           # 83 checks
python rust/tools/mutate.py                                            # about an hour; edits and restores source files
python rust/tools/footprint.py
```

## 9. The Redis Streams broker (built 2026-10-11)

**What it is.** With `REDIS_URL` set, the worker no longer processes claims inside its own relay loop. The relay *publishes* a message ("run claim X, version V") to a Redis Stream, and a pool of consumer threads takes the messages and runs each claim up to `ready`. Without `REDIS_URL` (and always in the demo) the old behaviour is unchanged: the claim is processed inline in the relay sweep.

**Why.** Inline, the explanation step (the AI model) was called for one claim at a time inside the relay tick, which also delayed dealing, lease expiry and reconciliation. Now the number of claims processed at the same moment is a setting (`CG_WORKER_CONCURRENCY`, default 4, 1 to 64), and it is also the most the model is ever asked at once. A burst waits in the stream, not in front of the model.

**The guarantees**, each with a test (`rust/crates/cg-queue/src/consumer_tests.rs`, `broker.rs`; real Redis in `rust/crates/cg-redis`):
- The outbox marker in MongoDB is cleared only after the message is in Redis, so a Redis outage leaves the markers and nothing is lost.
- A message is acknowledged only after the claim was processed. A thread that panics leaves it pending; it comes back after `CG_BROKER_MIN_IDLE_SECONDS` (default: the 90 s explanation ceiling plus 30 s, never below the ceiling plus 10 s, or a slow but live consumer would have its message stolen and the model would be asked twice).
- A consumer that died holding a message: another consumer takes it over (`XPENDING ... IDLE`, then `XCLAIM ... MINIDLE`, which refuses if someone else got there first and counts the delivery).
- A message delivered more than three times is parked as a dead letter and acknowledged, so one poison claim cannot loop.
- A forged or stale message (wrong hash, unknown claim, unknown version) changes nothing and is acknowledged.
- The same claim published twice runs once inside a process (an in-flight set). Across two worker processes a duplicate can cost one extra model call; the result is the same, and the worker is meant to run as a single instance.
- Redis down: the consumer threads back off (up to 5 s) and keep trying; they never stop. Redis returns, work continues.
- The stream holds only unfinished work: acknowledging is `XACK` plus `XDEL`, so it needs no trimming (a trim could silently drop pending work).
- **Lost messages are repaired.** If the stream is completely empty yet a claim has sat in a pipeline state for more than 15 minutes (`CG_REPUBLISH_AFTER_SECONDS`, never under two minutes), the worker publishes it again (job `republish`, every five minutes). It never does this while the stream still holds messages (a long backlog is not loss), so it cannot pile up copies. The Compose Redis now keeps an append-only file (about one second of messages can be lost in a crash) and the repair covers the rest.
- The address and password never appear in an error, a log line or the health panel.

**Where the model limit really is.** The routing configuration has `ai_per_minute` (default **30**) and `ai_daily_budget` (default 2000). They cap how many explanations are *requested* per minute; the rest keep the engine's own text. For a site at 200 claims a minute an administrator has to raise them (the limits are 600 and 100000). The pool size caps how many run *at once*; these cap how many run *per minute*. Both are needed.

**Evidence.** Broker contract (6 behaviours) passes on the in-memory twin and on real Redis 7.4; 10 pool tests (a burst of 40 claims with pools of 1, 3 and 4 threads never shows the model more than that many at once and calls it exactly once per flagged claim; green claims never reach it; a dead consumer's message is finished; a poison message is parked after exactly three tries; forged, duplicate and lost messages; Redis outage); 3 repair tests; 2 settings tests. The two-process end to end (`rust/tools/e2e_split.py`, API and worker as separate processes on real MongoDB and Redis) passes with `REDIS_URL` set: Redis then shows the `cg-workers` group with the consumers, 25 messages read for 25 claims, 0 pending, stream length 0. Mutation test: 14 new mutants (acknowledging too early or on a panic, never reclaiming, ignoring the attempt limit, ignoring the cap, no duplicate guard, no takeover floor, reclaim not counted, a pending message handed to two consumers, acknowledging without removing, no self-healing of a vanished stream, republishing during a backlog, ignoring a claim's age), all killed, run with a real Redis (without one the Redis mutants would look alive because their tests skip).

**Not measured.** Throughput. No load test was run (it was set aside by the owner); any claim about claims per minute on the target hardware (an RTX 3080 with 10 GB) is an estimate until one is.

**Celery or ARQ.** The mentor named Celery or ARQ, which are Python libraries. This is the same pattern (broker, consumer group, acknowledgement after work, retries, dead letters, an outbox) built in Rust on Redis Streams. Whether that is acceptable is question 2 in `docs/42`.
