# 39 | Security results of the Rust build

Tested 2026-10-09 and 2026-10-10 against the Rust server (`rust/`, branch `rust-rewrite`). This is the security record of the Rust build in the same spirit as `docs/20_Security_Audit.md` (Phase 1) and `docs/32_Privacy_and_Security_Note.md` (Phase 2): what was attacked, how, what was found, what was fixed, and what was **not** tested. Every number below comes from a script in `rust/tools/` or a test in `rust/crates/`, so it can be re-run (section 8).

**Scope and authority.** The user's own application, run locally on 127.0.0.1 with generated demo accounts. No real data, no third-party system. The test of a deployment (TLS, firewall, container limits, database authentication) is separate and still to be done on the target server (section 7).

**Short answer.**
1. **Black-box penetration test: 83 checks, 83 pass** (after two fixes). It found two real weaknesses in the first run: connections that never completed their request were held open forever (slow-connection exhaustion, "slowloris"), and a 70 KB header line was accepted. Both are fixed and are now checks that pass.
2. **Fuzzing at the API boundary: 400,000 random requests, 0 server errors, 0 leaks.**
3. **Mutation test: 36 deliberate breakages of security rules, all caught by the tests after 8 gaps in the tests were closed.** Eight of the first 36 were *not* caught, which is the finding: eight security rules had no test that would have noticed them being switched off.
4. **No critical or high-severity weakness is open.** Five limits that remain are listed in section 6 so nobody is misled.

---

## 1. How it was tested

| Method | Tool | What it covers |
|---|---|---|
| Black-box penetration test | `rust/tools/pentest.py` | Real sockets against a running server: headers, malformed and smuggled HTTP, size limits, slow and many connections, login, sessions, CSRF, every route x every role, object-level access, injection, business logic, resource exhaustion, leakage |
| Fuzzing | `rust/crates/cg-api/tests/api_fuzz.rs` | Random method, path, query, headers and body (binary, control and bidirectional characters, injection-style JSON keys), with sessions of each role, through the same function the server calls; seeded, so a failure is replayable |
| Mutation test | `rust/tools/mutate.py` | The security rules broken one line at a time; the tests must fail for each |
| Behaviour comparison with the Python reference | `rust/tools/conformance.py` and the `compare_*` tools | The same requests as four roles give the same status codes and shapes on both builds (131 of 132; the one difference is a random-dealing artefact), the same rules, logs, tokens and timestamps |
| Unit and integration tests | `cargo test --workspace` | 176 tests, including the real-database contract suite when `MONGO_URI` and `REQUIRE_MONGO=1` are set |

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
| Logout does not revoke the session | LOGOUT_RESULT |
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

## 6. What remains, honestly

1. **Account lockout can be provoked.** Five wrong passwords lock an account for 15 minutes; anyone who knows a badge number can lock that account. This is the usual trade-off against password guessing; an administrator can unlock, and every lock is logged.
2. **The login throttle is per process and per address.** It lives in the server's memory (a restart clears it; two server processes would each count) and treats everyone behind one shared address as one client. Behind the proxy the address is the last hop the proxy adds.
3. **No hardware or write-once storage for the audit log.** The log is tamper-evident (hash chain, signed anchor, external RFC 3161 timestamp if configured), not immutable (see `docs/32`).
4. **Claim data is not encrypted at rest** in the database (identifiers are masked in what people see). Disk or database encryption is a deployment control.
5. **Dependency audit not repeated for the Rust crates.** `cargo audit` has not been run on the Rust dependency tree; the lock file pins versions, but known-vulnerability checking of those versions is still to do.

## 7. Not tested

- The deployed system: TLS configuration and certificates, the proxy's own limits, container isolation (read-only file system, dropped capabilities are set and checked as text by `tests/test_deploy_rust_files.py`, not attacked), the database's authentication and network exposure, the firewall of the target machine.
- A real language model: only the request/response handling was tested against fake servers; nothing about what a real model might return beyond the template-shape and grounding guards.
- Denial of service at network scale: the checks show that a few hundred slow connections and a login flood do not stop the server; they do not model a distributed attack.
- Browser-side attacks on the console (it is the unchanged Phase 2 JavaScript, covered by the Python tests for markup sinks and the content-security-policy that forbids inline script).
- Side channels beyond the login timing check.

## 8. Re-running it

```
cd rust && cargo test --release --workspace                              # 176 tests, includes 30,000 fuzzed requests
CG_FUZZ_N=400000 cargo test --release -p cg-api --test api_fuzz random   # the stress run
python rust/tools/pentest.py --json report.json                          # 83 checks, starts its own server on port 8491
python rust/tools/mutate.py                                              # about 90 minutes; edits and restores source files, never run beside a build
python rust/tools/conformance.py                                         # Python and Rust servers side by side
```
