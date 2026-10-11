# 39 | Security results of the Rust build

Tested 2026-10-09 and 2026-10-10 (a second round on 2026-10-10 closed the limits the first round listed) against the Rust server (`rust/`, branch `rust-rewrite`). This is the security record of the Rust build in the same spirit as `docs/20_Security_Audit.md` (Phase 1) and `docs/32_Privacy_and_Security_Note.md` (Phase 2): what was attacked, how, what was found, what was fixed, and what was **not** tested. Every number below comes from a script in `rust/tools/` or a test in `rust/crates/`, so it can be re-run (section 8).

**Scope and authority.** The user's own application, run locally on 127.0.0.1 with generated demo accounts. No real data, no third-party system. The test of a deployment (TLS, firewall, container limits, database authentication) is separate and still to be done on the target server (section 7).

**Short answer.**
1. **Black-box penetration test: 116 checks, 116 pass** (91 after round 3, 25 more in round 4; of the 91, 83 in the first rounds, after two fixes: connections held open forever, and a 70 KB header accepted; 8 more in the third round for cross-site refusal, headers and unmask reasons).
2. **Fuzzing at the API boundary: 400,000 random requests, 0 server errors, 0 leaks.**
3. **Mutation test: 75 deliberate breakages of security rules in the harness, all caught in one full run on 2026-10-11** (the round-by-round tallies recorded at the time were 36 in the first round, of which 8 were missed until tests were added; 15 more in the second round; 10 in the third, of which 1 was missed until a test was tightened).
4. **The five limits listed after the first round were worked on (section 6):** lockout no longer lets a stranger keep the owner out of their usual device; throttles live in the database; the audit log has a second append-only copy, a scheduled verification and an optional outside timestamp; claim data is encrypted at rest in MongoDB; `cargo audit` was installed and run (five advisories, all via `yara-x`, none reachable).
5. **The five things the first round did not test were tested (section 7):** the deployed Docker stack (50 checks, which found four problems), the real gemma3:4b model (15 of 15 rules; a red team that **found and fixed a real weakness in the AI guard**), a single-machine flood (16 checks), the console in real Chrome (19 checks, no payload ran), and refusal timing (all eight kinds within 3 %).
6. **A third round (section 11)** added cross-site refusal, identifier-free reasons, unmask rationing, key rotation, an encrypted backup with a restore proven end to end (which found two real bugs in its own first version), pinned image digests and an operations runbook (`docs/40`).
7. **No critical weakness is open.** What is still true after the fixes is stated under each item and in section 11.8.

---

## 1. How it was tested

| Method | Tool | What it covers |
|---|---|---|
| Black-box penetration test | `rust/tools/pentest.py` | Real sockets against a running server: headers, malformed and smuggled HTTP, size limits, slow and many connections, login, sessions, CSRF, every route x every role, object-level access, injection, business logic, resource exhaustion, leakage |
| Fuzzing | `rust/crates/cg-api/tests/api_fuzz.rs` | Random method, path, query, headers and body (binary, control and bidirectional characters, injection-style JSON keys), with sessions of each role, through the same function the server calls; seeded, so a failure is replayable |
| Mutation test | `rust/tools/mutate.py` | The security rules broken one line at a time; the tests must fail for each |
| Behaviour comparison with the Python reference | `rust/tools/conformance.py` and the `compare_*` tools | The same requests as four roles give the same status codes and shapes on both builds (131 of 132; the one difference is a random-dealing artefact), the same rules, logs, tokens and timestamps |
| Unit and integration tests | `cargo test --workspace` | 195 tests, including the real-database contract suite when `MONGO_URI` and `REQUIRE_MONGO=1` are set |

## 2. Penetration test: 83 checks

| Area | Checks | What was tried | Result |
|---|---|---|---|
| Headers | 8 | version banner, `X-Content-Type-Options`, `X-Frame-Options`, `Referrer-Policy`, `Cache-Control: no-store`, API content-security-policy, console policy (no inline script, no framing) | all present, no banner |
| CORS | 2 | an `Origin` of another site on a normal and on a pre-flight request | no `Access-Control-*` header is ever sent |
| Methods | 3 | TRACE, CONNECT, an unknown verb | refused or ignored |
| Request smuggling | 4 | Content-Length together with Transfer-Encoding, two different Content-Lengths, a NUL byte in a header, obsolete header folding | all refused; no second request is served |
| Size limits | 3 | 70 KB header line, 70 KB request line, 80 KB body | refused (**the 70 KB header was accepted in the first run; fixed**) |
| Malformed input | 6 | HTTP/1.0, garbage, no Host header, 100,000-level nested JSON, duplicate JSON keys, invalid UTF-8 | clean 4xx, no crash |
| Slow and many connections | 5 | 300 half-open slow connections while another client is served; flood of 60 password hashes while the health check is polled; oversized page limits | the server stays responsive (**half-open connections were never closed in the first run; fixed**) |
| Authentication | 6 | timing of a known and an unknown badge, identical answers, one-time code reuse, cookie flags, throttle with rotated `X-Forwarded-For`/`X-Real-IP`/`Forwarded`, logged-out token reuse | no enumeration by timing or body; throttle cannot be dodged; a used code and a logged-out token are dead |
| Session tokens | 2 | forged tokens: algorithm `none`, five weak signing keys, swapped payload, algorithm confusion, truncated; payload contents | all refused; the payload holds ids and times only |
| CSRF | 4 | no token, another user's token, form-encoded and plain-text cross-site style posts | all refused (403 or 415) |
| Authorization | 11 | every role against 12 reads and 3 writes (the permission table), administrator reading or deciding claims, mass assignment on user creation, granting an administrator right to a reviewer, deactivating the last administrator, writing a password hash, self-promotion, deciding or verifying another reviewer's claim | exactly as the permission table says; a foreign claim answers 404, not 403 (no existence leak) |
| Injection | 10 | NoSQL operators and arrays in login fields, extra fields, a 5,000-character password, claim ids with traversal, `%00`, `$ne`, 300 characters | all refused or a plain 404 |
| Path traversal | 6 | `/ui/../`, encoded `..%2f`, `%2e%2e`, double slash, trailing slash, NUL | no file outside the console is served |
| Business logic | 9 | six malformed decision bodies, a valid decision, countersigning one's own decision, eight parallel configuration updates against one version | validation holds; one winner and seven 409s |
| Information leakage | 4 | error bodies for framework, path and library names; a reviewer's claim view for raw identifiers; user list for hashes and seeds | none |

The two fixes are in `crates/cg-api/src/server.rs`: a 10-second limit to receive a request's headers, a 16 KB cap on a header block, a cap of 2,048 open connections and a 5-minute lifetime for a connection.

## 3. Fuzzing: 400,000 requests

30,000 requests run in the normal test suite (1.3 s) and 400,000 in the stress run (`CG_FUZZ_N=400000`, 12 s). The assertions on every response:
- status below 500 (a 500 would mean a panic was caught), except the documented 501 and 503;
- the security headers are present;
- the body never contains a path, a library name, a connection string or a stack-trace word;
- an error is a JSON object with an `error` code;
- at least 100 requests reach a real handler and succeed, so the corpus is not only refused at the door.

Two further tests: 4,000 random requests with and without bogus cookies never get a 200 or 201 from a protected route, and six forged or damaged session tokens are all refused with 401.

## 4. Mutation test: are the security tests any good?

A test suite that passes proves little if it would also pass with the rule removed. Each mutant switches off or weakens one rule; the tests for that crate must fail.

| Rule broken | Outcome |
|---|---|
| CSRF check off | caught |
| Forced-password-change gate off | **missed at first**, now caught |
| Permission check always true | caught |
| Login throttle never blocks / is never recorded / has the wrong time window / its check removed | caught (4 mutants) |
| Session cookie without HttpOnly | caught |
| Body-size limit made huge | caught |
| Content-type check removed | caught |
| High-severity decision check removed | **missed at first**, now caught |
| Unmask, audit-events, user-list routes requiring only the viewing right | caught (3 mutants) |
| `X-Frame-Options` header dropped | **missed at first**, now caught |
| Logout does not revoke the session | caught (the first version of this mutant did not compile, so it proved nothing; it was corrected and re-run) |
| Decision recorded under a client-chosen reviewer / decision body not strict | caught (2 mutants) |
| Login body not strict (extra fields accepted) | **missed at first**, now caught |
| Findings offering actions to people who may not decide | **missed at first**, now caught |
| Unmask not written to the security log | caught |
| Unauthenticated answered 403 instead of 401 | caught |
| Refused request not written to the security log | **missed at first**, now caught |
| `X-Forwarded-For` trusted without a proxy / first hop trusted | caught (2 mutants) |
| Session expiry ignored / future-dated token accepted / revoked token accepted | caught (3 mutants) |
| One-time code reusable | caught |
| A viewer allowed to unmask | **missed at first**, now caught |
| A reviewer allowed to decide high severity (permission table) / notes always visible | caught (2 mutants) |
| A signer may countersign their own decision / claim not tied to its lease holder / expired lease still usable | caught (3 mutants) |
| A finding that passed can be decided | **missed at first** (two matches also made the mutant ambiguous), now caught |

**First run: 36 mutants, 28 caught, 8 missed.** Eight tests were added or tightened (forced-password-change gate, high-severity decisions by clearance level, response headers, unknown-field refusal on login and decisions, hide-not-disable for viewers, the security-log entry for a refusal, the exact permission list of each of the four levels, and a passed finding being undecidable). **Second run, the eight again: all caught.** The Python queue has its own 77-mutant run (`tests/mutation_queue.py`); the lists are different.

A limit of this method: 36 hand-picked mutants are not every possible bug. They were chosen at the places where a mistake would let someone in, see too much, decide without the right, or erase a trace.

## 5. What a visitor can and cannot do (summary of the permission design under test)

| Level | Can | Cannot |
|---|---|---|
| 1 viewer | list and view claims with masked identifiers, no notes, no action buttons | unmask, decide, audit, anything administrative |
| 2 reviewer | the above plus notes, unmask (audited, with a reason), decide medium-severity findings, recheck | decide high-severity findings, administer |
| 3 senior | the above plus decide high-severity findings | administer |
| 4 administrator | users, routing settings, audit log and its verification, queue overview, health, claim traces | read claims, unmask, decide (a separation of duties: the person who runs the system cannot approve claims) |

The reviewer recorded on a decision is always the signed-in person, never a field sent by the client; a decision is bound to a lease the person currently holds; the first signer of a high-severity claim can never be the second.

## 6. The five limits listed on 2026-10-10, and what was done about each

The first version of this document ended with five honest limits. All five were worked on the same day; this section says what changed, how it was checked, and what is still true afterwards.

### 6.1 Account lockout could be provoked by a stranger

*Before:* five wrong passwords locked an account for 15 minutes, so anyone who knew a badge number could keep that person out.

*Now:* the lock still exists (it stops a guesser who spreads attempts over many addresses) but it keeps out **only clients the person has never signed in from**. A client becomes "known" after a successful sign-in (a keyed hash of its address is remembered for 30 days). Separately, each (badge, client) pair is allowed **five failures per 15 minutes**; after that, that client is refused for that badge whatever it sends, and nobody else is affected. And each address as a whole is allowed 100 failures per 15 minutes over all badges. A successful sign-in by the owner clears the lock, which proves they are present.

*Checked by:* `cargo test -p cg-access` (5 new tests): an attacker provokes the lock and the owner still signs in from their usual client; the same attacker's correct-looking attempt from a new device is refused until the lock runs out; a client that keeps failing is refused even with the right password while another client is not; every kind of refusal (unknown badge too) counts. Mutation test: six mutants of this logic, all caught (section 9).

*Still true:* an attacker can still keep a person out of a **new device** for 15 minutes. The person's usual place is unaffected. Someone who shares a network address with the victim shares the per-address limit.

### 6.2 The sign-in throttle lived in one process's memory

*Before:* a restart cleared it and two server processes each counted separately.

*Now:* the counters are stored in the database (a counter that restarts each window, updated atomically, expiring on its own), so a restart or a second process sees the same counts. Addresses are stored only as keyed hashes. `cg-access` and `cg-mongo` share one contract suite for the new store functions: the in-memory twin and real MongoDB both pass it, including 50 parallel increments that lose none. A test builds a second service over the same store ("a restart") and finds the client still throttled.

*Still true:* the windows are fixed (up to twice the limit can land across a window boundary) and a burst of parallel requests can pass the check before the counts catch up: a flood of 400 concurrent sign-ins produced 341 ordinary refusals before the first 429 (section 8), bounded by the concurrency and not by time.

### 6.3 The audit log was tamper-evident, not immutable

Software cannot make storage immutable; it can make a quiet rewrite **detectable, reported and provable**. Three worker jobs were added (own thread, so they never delay the queue), each recorded so the health panel shows a failure:
- **`audit_verify`** (hourly): both hash chains and their anchors.
- **`audit_mirror`** (every 5 minutes, when `AUDIT_MIRROR_DIR` is set): an append-only copy of both logs on a separate volume. Before copying, it proves that what it already holds is still byte-for-byte the beginning of the live log; if the live log was edited, truncated or replaced, or the mirror was, it refuses to follow and reports it. In the Compose stack only the worker mounts that volume.
- **`audit_witness`** (hourly, when `TSA_URL` is set): an RFC 3161 timestamp of each anchor from an outside authority.

*Checked by:* `rust/tools/audit_guard_demo.py` edits a past row of the live security log with a running server: within seconds the health panel's "Audit log protection" goes from ok to DOWN with *"the first 30,931 bytes of the source differ from the mirror"* and *"Audit chain invalid at event 3"*, while the mirror still holds the original. Unit tests of the mirror (growth, half-written line, edit, truncation, replacement, a tampered mirror) and of the health judging; two mutants, both caught. The deployment attack found that the mirror volume was root-owned and the worker could not have written to it; fixed (section 7.1).

*Still true:* the mirror is immutable only if its volume is (an append-only or object-lock volume); an intruder who controls both volumes defeats it. The external timestamp needs a configured authority.

### 6.4 Claim data was not encrypted at rest

*Now:* in MongoDB the claim, the rule results (whose evidence quotes the claim), the advisory results, the AI explanations and every reviewer decision (with its free-text reason) are stored as encrypted fields, on by default. The key is `DATA_KEY` if set, otherwise derived from `FERNET_KEY`. Each encrypted field is **bound to its claim, version and field name**, so moving one claim's evidence under another claim is detected and refused. What stays readable is what routing needs and what holds no personal data (ids, state, lane, score, lease holder, hashes, timestamps) plus a keyed hash of the patient id for finding a patient's earlier claims. Documents written before encryption was switched on are still read.

*Checked by:* against real MongoDB, the same queue contract passes with encryption on; a test reads the raw stored documents and finds no patient id, note text, rule id, decision reason or AI text; a reader with another key fails; a swapped field is refused; older plain documents are read; and the whole split-process end-to-end test passes with encryption on. Two mutants (binding removed, patient index in the clear), both caught.

*Still true:* the user records (names, badge numbers, password hashes, encrypted authenticator seeds) are not field-encrypted, the audit logs on disk are not encrypted (they hold badge numbers, claim ids and the free-text reasons given for unmasking), the key is in the environment, and there is no re-encryption tool for key rotation. Disk or volume encryption is still advised underneath. The Python build cannot read the encrypted documents.

### 6.5 Dependencies were not audited

`cargo audit` was installed and run on the 439 crates of the lock file. Findings: **five advisories, all reached only through `yara-x` 1.21.0**, the latest release, which embeds a WebAssembly engine to run compiled rule conditions: `wasmtime` 45.0.3 (four advisories, one rated critical, three in features this program does not use: the WASI file-system sandbox, whose crate is not in the tree, and the component-model async and record-lifting paths, plus type indices mixed between engines) and `rsa` 0.9.10 (Marvin timing attack on **private**-key operations, no fix released; ClaimGuard holds no RSA private key and only verifies public-key signatures). One more warning: `bincode` is unmaintained. Each was looked at and recorded with its reason in `rust/.cargo/audit.toml`; with that list `cargo audit` exits 0, and any new advisory fails it. The list has to be re-read whenever `yara-x` is upgraded. The only rule packs ever compiled are generated from files in the repository, never from a claim or the network.

## 7. What section 7 of the first version said was not tested

### 7.1 The deployed system: `rust/tools/deploy_attack.py`, 50 checks

It brings the Compose stack up with throw-away secrets and attacks it: TLS 1.0 and 1.1 refused, TLS 1.3 with HTTP/2 accepted, certificate valid, forward-secret AEAD cipher on 1.2, HSTS, no server banner, port 80 only redirects; a 20 KB header, a 1 MB body and 60 half-sent requests against the proxy; only the proxy publishes a port; the proxy cannot reach MongoDB or Redis; MongoDB and Redis have no route to the internet; MongoDB and Redis refuse missing and wrong passwords; for the API and the worker: non-root, read-only root file system, no capability, `no-new-privileges`, memory, process and CPU limits, no host path or Docker socket mounted, not on the host network, only `/tmp` and `/data` writable; the image holds no secret, key or demo account file, no shell, package manager, interpreter, downloader or mount/su binary.

**It found four real problems, all fixed:**
1. The audit-mirror volume would have been owned by root, so the worker (unprivileged) could not have written to it and the mirror job would have failed in production.
2. A 1 MB request through the proxy ended as a 502 instead of a clean 413 (the application answered early and closed while the proxy was still sending). The proxy now caps bodies at 100 KB itself.
3. The database and cache shared a network with the proxy; they now sit on an `internal` network with no route out, which only the API and worker (and the proxy for nothing) can reach, and Redis now demands a password.
4. The image was Debian slim and a vulnerability scan (Trivy 0.75) found **241 findings (4 critical, 54 high)**, nearly all in operating-system tools the program never uses (util-linux, perl, gnutls). The runtime image is now **distroless** (the C library and certificates only; no shell, 83.5 MB): **49 findings, none critical, one high (`libssl3`, present in the base image but not linked or loaded by the program, which uses rustls)**. The scan is a snapshot: new advisories appear daily.

Also added at the proxy: read-header and read-body timeouts, a header size cap, and HSTS.

*Not tested:* the target machine's own firewall, its operating system and its Docker daemon; a certificate from a public authority (the test uses the proxy's internal one); that backups are encrypted.

### 7.2 A real language model: `cg model-check` and `rust/tools/model_redteam.py`

Against the real local **gemma3:4b** (Ollama): all **15 of 15** rules produced an accepted template. Then nine claims were submitted through the whole pipeline (real model, real guard, real queue) whose fields carried instructions and false statements: "IGNORE PREVIOUS INSTRUCTIONS … approved and payable", a fraud accusation, a cancer diagnosis, `<img onerror>` and `<script>`, a fake system prompt, bidirectional and zero-width characters, a 5,000-character value, template braces, plus a clean control.

**It found a real weakness.** The model never sees a claim, but the system fills the claim's values into the model's template afterwards, and the grounding guard treated a phrase as "grounded" if it appeared anywhere in the finding, **including the claim's own values and its claim id**. A submitter could therefore plant "this claim is approved and payable" in a service code and the guard accepted a reviewer-facing, AI-labelled sentence containing it. Fixed in two ways: the guard now counts only the fields the engine and rulebook wrote (an allow-list; the claim's values, claim id and line ids no longer ground anything), and every value is made safe before it is shown in a sentence (control, zero-width and direction-changing characters removed, white space collapsed, quotation marks stripped, `<` and `>` replaced by look-alike angle quotes, length capped, and anything that is not a plain code, number or date put in quotation marks so that it reads as the submitter's data). Clean claims keep their AI-written explanations (the control claim: 7 of 7 accepted); for the attacks, the sentences that would have carried an accusation, an approval or a diagnosis were refused and replaced by the engine's own text (5 of 63), and the ones that quote a value show it in quotation marks. No explanation shown to a reviewer contained the forbidden content. Three mutants of this logic, all caught.

*Not tested:* other models, other sizes, two-pass mode on a live model, or what a model does when it is also the attacker's target in a different deployment.

### 7.3 Load and exhaustion: `rust/tools/flood.py`, 16 checks

One machine, one address, so no distributed attack. Results: 3,000 idle connections, 300 concurrent clients for 20 seconds (29,700 requests, about 1,500 per second from a Python client, all 200), 200 clients hammering a protected route without a session (18,799 requests, all 401), a 400-client sign-in flood (341 refusals then 429s, no 5xx), and 100 clients sending 60 KB bodies. In every phase a polite client was still answered in 1 to 12 ms, memory never passed 87 MB (idle 66), and afterwards the server was healthy. The proxy's own behaviour under slow connections is in 7.1.

*Not tested:* a distributed attack from many addresses; network-level floods below the HTTP layer; exhaustion of the database server.

### 7.4 The console in a real browser: `rust/tools/xss_seed.py` and `xss_browser.py`, 19 checks

Script-injection payloads (img/onerror, script, svg/onload, attribute break-out, `javascript:`, template and prototype strings, iframe srcdoc, style and link) were planted in claim notes, invoice numbers, provider ids, currencies, service codes, line ids, attachment text, user display names, and the badge field of failed sign-ins. A real Chrome (Playwright) signed in as a reviewer and as an administrator and opened every screen that shows other people's text, plus hostile fragments such as `#/work/<script>…`. On every screen: no payload ran, no inline script, event-handler attribute, iframe, foreign stylesheet, `javascript:` address or stray image existed in the page, and the payload text was on screen as plain text. An injected inline handler was refused by the browser itself (`script-src-attr`). The test tooling cannot even use string evaluation on the page, which is the content-security-policy working.

### 7.5 Side channels

The refusal of a sign-in is built so that every reason takes the same steps (two reads, one password hash, one or two writes). `cargo test -p cg-access --test timing -- --ignored` times eight kinds of refusal with the production hash cost, interleaved to cancel machine drift: unknown badge 81.5 ms, malformed badge 81.7, locked account 81.3, inactive account 81.4, wrong password 80.3, wrong code 81.8, replayed code 81.0, throttled client 82.6: all **within 3 %**. Other channels checked by construction and by the 83-check penetration test: identical bodies for known and unknown badges; a claim leased to someone else answers 404 exactly as a missing one does; constant-time comparison of the anti-forgery token and the session signature.

*Not tested:* network-level timing across a real network (the measurement is in-process); cache, power or electromagnetic channels; cross-tenant effects in a shared host.

## 8. Re-running it

```
cd rust && cargo test --release --workspace                               # unit and integration tests; set MONGO_URI and REQUIRE_MONGO=1 for the real-database contracts
cargo test --release -p cg-access --test timing -- --ignored --nocapture # refusal timing
CG_FUZZ_N=400000 cargo test --release -p cg-api --test api_fuzz random    # fuzz stress
cargo audit                                                               # dependency advisories (uses rust/.cargo/audit.toml)
python rust/tools/pentest.py                                              # 116 checks against a local server
python rust/tools/flood.py                                                # load and exhaustion
python rust/tools/audit_guard_demo.py                                     # rewrite a live log and watch the protection notice
python rust/tools/model_redteam.py                                        # needs Ollama with gemma3:4b
python rust/tools/xss_seed.py --port 8498 ; python rust/tools/xss_browser.py --port 8498 --accounts <file> --seed-log <log>   # needs Chrome and Playwright
python rust/tools/deploy_attack.py                                        # needs Docker; builds nothing, uses claimguard-rs:latest
python rust/tools/mutate.py                                               # about two hours; edits and restores sources, never run beside a build
```

## 9. Mutation test, second round

Fifteen more mutants were written for the new logic: the lock applied to the owner too, the lock ignored, the per-client limit off, refusals not counted, devices never remembered, all clients hashed alike, the address limit enlarged, address failures not counted, the guard trusting claim values, values not quoted, direction characters kept, an encrypted field not bound to its place, the patient index stored in clear, the mirror following an edited log, and an audit-job failure shown as ok. **All 15 are caught.** With the 36 of section 4 that makes **51 mutants, all caught**.

## 10. Summary of what was found by this round

| Found by | Finding | Severity (our judgement) | State |
|---|---|---|---|
| Real-model red team | The AI guard accepted attacker-written approval/fraud/diagnosis text because it was "grounded" in the claim's own values and claim id | High: false statements in an AI-labelled explanation shown to a reviewer | Fixed, tested, mutation-checked |
| Deployment attack | Audit-mirror volume not writable by the unprivileged worker | Medium: the new protection would silently not have worked in production | Fixed, checked |
| Deployment attack | Debian base image: 241 findings, 4 critical | Medium: tools an intruder could use, none used by the program | Replaced by a distroless image: 49 findings, 0 critical |
| Deployment attack | Database and cache on the proxy's network; Redis without a password | Medium | Fixed (internal network, password) |
| Deployment attack | Oversized body through the proxy ended as 502 | Low | Fixed (cap at the proxy) |
| Timing test | (no path stood out once samples were interleaved) | none | n/a |
| `cargo audit` | Five advisories via `yara-x` (`wasmtime`, `rsa`), one unmaintained crate | Not reachable in this program | Reviewed and recorded in `rust/.cargo/audit.toml` |

## 11. Third round (2026-10-10, evening): closing the gaps that remained

The second round left a list of things that were still true. This round took the ones that can be fixed in software and checked on this machine. Every item below has a test, and the logic has mutants (section 11.6).

### 11.1 A browser-declared cross-site request is refused outright
The anti-forgery token and the `SameSite` cookie already stop a forged request. A third, independent check was added: a state-changing request (any method except GET, HEAD, OPTIONS) is refused with 403 `cross_site` **before anything else looks at it**, including the sign-in, if the browser says it comes from another site (`Sec-Fetch-Site: cross-site`), or carries an `Origin` that is `null`, empty, or not exactly the host it was sent to (`http://host:8443.evil.example` is caught). Requests from programs, which send neither header, are unaffected; so is the console itself and the desktop window. Also added to every response: `Permissions-Policy` (camera, microphone, geolocation, payment, USB and others off), `Cross-Origin-Opener-Policy: same-origin`, `Cross-Origin-Resource-Policy: same-origin`. Pen test: 8 new checks (now **91, all pass**).

### 11.2 Personal data leaves the system only through a rationed, audited door
- **Reasons may not repeat the identifier.** A free-text reason (for an unmask, or for a decision, in the direct route and in the queue) is refused with 422 if it contains the claim's real patient or member identifier, case-insensitively. Reasons end up in the security log and the review log, which auditors read and which are not field-encrypted; the pseudonym is what belongs there. Checked on all three routes.
- **Unmasking is rationed.** One person may unmask at most 60 claims an hour (`UNMASK_PER_HOUR`); the sixty-first gets 429 and a refused-request entry on the security log. The count lives in the database (shared by every server process). A reviewer who unmasks hundreds of claims an hour is copying, not reviewing, and a stolen session cannot empty the system quickly.

### 11.3 The data key can be rotated: `cg rotate-key`
`NEW_DATA_KEY=<new Fernet key> cg rotate-key [--dry-run]` re-encrypts every stored claim (claim, results, advisory, explanation, every decision) from the current key to the new one and recomputes the patient index. It stops with an error rather than damaging a document that opens with neither key, skips and reports any document someone changed meanwhile, and can be repeated after an interruption (documents already on the new key are counted and left alone). Tested against real MongoDB (decisions and the patient index included, old key refused afterwards, raw storage still free of readable data) **and with the real command inside the Compose stack** (section 11.4). Procedure in `docs/40`.

### 11.4 Backup and restore, proven end to end (`rust/tools/backup_restore_test.py`, 23 checks)
`deploy/rust/backup.sh` writes **one encrypted file** (AES-256, PBKDF2 600,000 rounds, key from a file) holding both databases, both audit logs and the audit mirror, plus a checksum; it never overwrites. `restore.sh` refuses a wrong key, a damaged file and contents that do not match their checksums, replaces the databases and log volumes, and fails loudly if any document does not restore. The test: put an administrator, two users and 12 claims in through the real API; back up; confirm the file shows no readable claim id, badge number or JSON; **destroy every volume**; start an empty stack and confirm nobody can sign in; confirm a wrong key and a damaged file are refused; restore; confirm the same administrator signs in with the same password and the same authenticator seed, the same users and claim states are back, claim data is readable, the audit chains verify and the history is intact; then rotate the data key with the real command (dry run, real run, repeat run), restart on the new key, and confirm everything again.

**Testing found two real problems, both fixed:**
1. A whole-server database dump also carries the database's own user accounts, and restoring it replaced them in the middle of the restore itself: `mongorestore` then failed with *"createIndexes requires authentication"* and left the system half restored. The backup now dumps only the two application databases, one archive each, and the restore is limited to them.
2. The first restore script reported success even when `mongorestore` failed. It now checks every archive's result and stops.

### 11.5 Supply chain and installation
- **Every third-party image is pinned by digest** (proxy, MongoDB, Redis, the optional model server, both build stages, the helper image of the backup scripts), so a rebuild gives the same bytes; the price is that security updates are taken deliberately (procedure in `docs/40`). A test fails if any image reference is not pinned.
- The installer now generates a separate **`DATA_KEY`**, so the key for claim data and the key for authenticator seeds are independent and the data key can be rotated.
- `cg admin create-admin --password-stdin` creates the first administrator without a terminal (installers, tests); the password is read from standard input, never an argument.

### 11.6 Mutation test, third round
Ten more mutants: cross-site check off, origin never compared to host, fetch-metadata ignored, unmask not rationed, the identifier allowed in an unmask reason, in a direct decision reason and in a queue decision reason, the opener policy header dropped, a rotation that leaves decisions readable, a rotation that keeps the old patient index. **Nine were caught at once; one survived** (the direct decision's identifier check: the test used a rule that could not be decided at all, so it was refused for another reason). The test was tightened to a finding that can really be decided and to read the refusal's cause; the mutant is now caught. **Total: 61 mutants, all caught.**

### 11.7 Operations
A runbook for the person who runs the server is in `docs/40_Rust_Operations_Runbook.md`: install, every setting, what each secret protects and what happens if it is lost or changed, backup and restore, key rotation, updates, the health panel component by component, incident playbooks (audit alarm, locked user, stolen session, data seen by someone who should not have), and the exact order of checks to run on the target machine.

### 11.8 What is still open
- Everything under section 6 that was marked "still true" remains true except as stated above; in particular nothing here was run on the target virtual machine, and a distributed denial of service, the VM's firewall, a public certificate authority and the host's own hardening are untested.
- User records are not field-encrypted; the audit logs on disk are not encrypted (they hold badge numbers, claim ids and reasons, which can no longer contain the identifier); `FERNET_KEY` has no rotation tool (authenticators must be re-enrolled).
- There is no automatic scheduling of backups or of the image-update routine; both are manual procedures with scripts.
- Two-person approval for the most dangerous administrator actions (resetting an authenticator, deactivating the last reviewer) is not built.

## 12. Fourth round (2026-10-10, night): the four items left open in section 11.8

Section 11.8 ended with a list of what was still open. Three of the four were buildable and checkable on this machine; they are done and tested. The fourth (running it on the target virtual machine) is deliberately last.

### 12.1 `FERNET_KEY` can be rotated: `cg rotate-fernet-key`
`NEW_FERNET_KEY=<new Fernet key> cg rotate-fernet-key [--dry-run]` re-encrypts every authenticator seed from the old key to the new one. Nobody has to re-enrol. It stops with an error, changing nothing for that person, if a seed opens with neither key; it skips and reports a record that changed meanwhile; it can be repeated. **It refuses to run while `DATA_KEY` is empty**, because then the claim data, the names and the audit logs would be protected by a key derived from `FERNET_KEY`, which the command would change; the message tells the operator to run `cg rotate-key` first. Tested against real MongoDB (seeds readable only under the new key afterwards; a bad seed stops the run; a dry run changes nothing; repeating is safe).

### 12.2 User names and the audit logs are encrypted at rest
- **People's names** are stored in the user database as `enc1:<token>` (Fernet), bound to the badge and the field name: copying one person's sealed name onto another person is detected and refused. Names written before encryption was on are still read. `cg rotate-key` re-encrypts them together with the claim data.
- **The audit log files** (`security_audit.jsonl`, `review_audit.jsonl`) are stored one encrypted row per line, `enc1:<token>`. The hash chain, the signed anchor and every check work on the *plain* row (the row is opened first), so the chain is unchanged and a plain log is still the Python build's format. The mirror copies stored bytes, so the second copy is encrypted too. The anchor file holds only a head hash, a count and a signature and stays readable, so it can be checked and timestamped without the key. Logs written before the switch stay readable (a log can be a mix); a row that cannot be opened (wrong key, or altered) is an error, never silently skipped.
- **Rotation does not rewrite the logs** (that would break the chain, the mirror and every timestamp). New rows use the new key; the old secret goes in `DATA_KEY_PREVIOUS` and the old rows stay readable. Tested: the earlier bytes are byte-for-byte unchanged after a rotation, and without the previous key the old rows are refused.
- The key is derived from `DATA_KEY` (or `FERNET_KEY` when it is empty) with a label of its own, so the audit key and the claim-data key are different keys even though they share a root secret. `CG_ENCRYPT_AT_REST=false` turns all of it off (not recommended).
- The standalone `cg audit-verify` and `cg audit-append` open an encrypted log when `DATA_KEY` or `FERNET_KEY` is in the environment.
- Black-box check against the running server (pen test): every row of both logs on disk starts with `enc1:`; no badge number, event name or user-created marker is readable in the file; the logs still verify end to end through the API and the audit panel still reads them. The audit-protection demo (`audit_guard_demo.py`) still notices a rewrite of an encrypted log.

### 12.3 Two-person approval of the most dangerous administrator actions
Replacing someone's authenticator, creating an administrator, and changing (promoting, demoting, deactivating, editing) an administrator no longer happen on one administrator's word. Asking gives a **request** (the API answers 202 and says so; the console shows it as waiting) and **nothing changes** until a *different* administrator approves it with a **fresh authenticator code of their own**, so a stolen session cannot approve. Rules, each with a test:
- the person who asked cannot approve their own request (nor reject it; they can withdraw it);
- a wrong code does not approve; five wrong codes in 15 minutes stop that administrator from approving for a while (429); a code used once is never accepted again (here or at sign-in); a wrong code is a 422, not a 401, so the console does not sign the person out for a typing mistake;
- a request is decided exactly once, atomically (four administrators approving in parallel: exactly one wins), and expires after a day (`APPROVAL_TTL_SECONDS`);
- one pending request per action and target, enforced by the database (a partial unique index);
- the rules for the action (who may change whom, the last administrator, not yourself) are applied twice: when asking, so a doomed request is refused at once, and again when approving, by the approver;
- the person who asked is still active at approval time, or the request fails;
- the stored payload (a password hash or an encrypted authenticator seed) is never shown by any route; the new authenticator link goes to the person who asked, at once, and works only after approval;
- the log has an `approval` event for requested / approved / rejected / cancelled / failed, and the usual `totp_reset`, `user_created`, `user_updated` events carry `approved_by`.

**It applies once two administrators are active.** A lone administrator acts alone (nobody could approve), and each such action is logged as `done_without_second_administrator`. `DUAL_CONTROL=false` switches it off. The console has a *Waiting for a second administrator* panel under **Users**. The demo now has a second administrator (CG-4005) so this can be shown.

### 12.4 Tests added, and what they found
- 8 service tests for dual control, 1 API test (18 steps), 2 audit-log tests (one process-wide-key test: encrypted rows, mixed log, mirror, tampering, wrong key, no key, rotation with previous keys), 2 real-MongoDB tests (encrypted names with the shared contract suite; rotation of names and seeds), and approval storage added to the shared store contract (memory twin and real MongoDB: one pending per target, settle once, expiry, parallel settle).
- Pen test: **116 checks, all pass** (25 new: dual control end to end, malformed approval bodies and ids, a senior reviewer cannot see approvals, the old authenticator works until approval and not after, ordinary accounts stay direct; encrypted logs on disk and still verifiable).
- Two older mutation patterns had gone stale after earlier refactors (they matched nothing and so tested nothing): the replayed-code mutant and the frame-header mutant. Both were repaired and now kill.
- **Mutation test, fourth round (full run, 75 mutants in all: the first run of 36 plus 39 more, with the six database-backed ones re-run against a real MongoDB because without a database their tests skip and the mutants wrongly look alive): all 75 caught. 18 of them are new** (dual control never applies; applies to a lone administrator; own-request approval; approval without a code; wrong codes not counted; codes replayable; approval never expires; authenticator reset direct; administrator changes direct; administrator creation direct; anyone can withdraw; approved action skips the rules; audit rows in clear; previous keys ignored; names in clear; names moved between people accepted; seeds not rewritten; names not rewritten). **17 were caught at once; one survived**: with the expiry check removed, the store still refused an expired request, only with a different message. The test now requires the exact reason, and the mutant is killed.

### 12.5 Deployment
`DUAL_CONTROL`, `APPROVAL_TTL_SECONDS` and `DATA_KEY_PREVIOUS` are passed through `compose.yml` and listed in `deploy/env.production.example`; a deployment test checks both. The runbook (`docs/40`) has the new procedures: rotating `FERNET_KEY` (5a), the approval flow (5b), and "an audit log cannot be read" (8.1b).

### 12.6 What is still open
- Not run on the target virtual machine (deliberately last).
- Dual control cannot know that two administrator accounts belong to two people; a person who controls both defeats it. The log records who did what.
- A lone administrator is not protected (nobody could approve); it is logged.
- User records other than the name (level, permissions, timestamps) are readable in the database; they hold no personal data beyond the badge number.
- The audit log keeps the badge numbers of the people who acted (needed for accountability) and, since the log is encrypted, they are protected on disk but are visible to anyone who can read the API's audit panel with the right permission.
- Disk: the build machine's disk is almost full (the health panel reports `disk: down`, correctly). That is a finding about this laptop, not the product.

**A note on the count.** The harness (`rust/tools/mutate.py`) holds 75 mutants, and that is the number checked by the full run. The tallies of earlier rounds (61 after round 3, plus the 18 listed in 12.4) add up to 79, not 75: some mutants were merged or replaced when earlier ones were repaired, and the per-round figures were not reconciled at the time. The authoritative figure is the harness itself. The run needs a real MongoDB (`MONGO_URI`, `REQUIRE_MONGO=1`); without one the database-backed mutants are not caught because their tests skip.
