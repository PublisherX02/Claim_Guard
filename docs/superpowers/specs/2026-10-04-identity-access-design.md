# Phase 2, sub-project 2: identity and access (full version)

Closes security finding "no identity behind review decisions" (docs/20, A07) and serves the Phase 2 rubric lines
"access control", "least privilege", "data minimization" and, with the severity gate, the "explicit human approval" half of
the human-in-the-loop line. Ported from the team's Attijari project (`src/database.py`, `src/routers/auth.py`) with the
improvements listed under "Differences from Attijari".

## Goal
A reviewer logs in with an **ID badge number, a password and an authenticator-app code**. What they can see and do depends
on their **clearance level**. Every review decision is bound to the authenticated badge, never to text the client sends.
Every security-relevant event lands in a hash-chained security audit log.

## Non-goals
Web front end; moving claims and results into the database (they stay in the existing JSONL outputs behind a `ClaimStore`
interface; the move belongs to the queue sub-project); human-in-the-loop routing and the work dispatcher (next
sub-projects, they consume the permissions defined here); single sign-on; self-service sign-up or e-mail password reset.

## Levels and permissions
Permissions are flags. A level is a default set; an admin may grant or revoke single flags per user, but never above their
own level. Separation of duties: **L4 does not decide claims**.

| Permission | L1 viewer | L2 reviewer | L3 senior | L4 admin |
|---|---|---|---|---|
| `claims.view` (findings, evidence, explanation) | yes | yes | yes | no |
| `pii.unmask` (real identifiers for one claim; reason required, logged). Identifiers are masked for everyone otherwise | no | yes | yes | no |
| `claims.view_notes` (free-text notes, attachment text) | no | yes | yes | no |
| `claims.decide` (confirm, dismiss with reason, request info, mark corrected) on medium-severity findings | no | yes | yes | no |
| `claims.decide_high` (the same on high-severity findings) | no | no | yes | no |
| `claims.recheck` | no | yes | yes | no |
| `audit.view`, `audit.verify` | no | no | no | yes |
| `users.manage` | no | no | no | yes |

An L4 who also needs to review claims gets a second, lower-level badge.

## Data model (MongoDB collections)
- `users`: `badge_id` (unique index, format `CG-` plus 4 to 8 digits), `name`, `password_hash` (bcrypt), `totp_secret_enc`
  (Fernet), `level` (1 to 4), `grants`/`revokes` (permission lists), `active`, `failed_attempts`, `locked_until`,
  `must_change_password`, `created_by`, `created_at`, `last_login`.
- `used_totp`: `(badge_id, window)` with a **unique index** and a TTL, so a code can be used once even under parallel
  requests. `revoked_tokens`: `jti` unique, TTL at token expiry.
- Storage sits behind a `UserStore` interface with two implementations: MongoDB (the deployment store, as the mentor
  asked for NoSQL) and an in-memory one for fast tests. **One contract test suite runs against both**, so the fake is
  checked against real behaviour. MongoDB runs from `docker-compose.yml` (image `mongo:7`, bound to localhost, auth on)
  and as a service container in CI. If neither is reachable the Mongo contract tests report as skipped, loudly, and the
  deployment does not start.

## Login and sessions
1. `POST /auth/login` with `badge_id`, `password`, `totp`. Inputs must be strings of bounded length; anything else
   (including `{"$ne": null}`) is rejected before reaching the database.
2. Unknown badge, wrong password and inactive account give the **same response and take the same time** (a dummy bcrypt
   check runs for unknown badges). The TOTP step runs only after the password is right.
3. Five consecutive failures lock the account for 15 minutes (counter updated atomically in the database). Lockout and
   every failure are audited.
4. TOTP: 30-second step, window of exactly one step, each `(badge, step)` accepted once via the unique index. **If the
   store is unreachable, login fails closed.**
5. Success sets a JWT (HS256, 60-minute lifetime, `jti`) in a `HttpOnly; Secure; SameSite=Strict` cookie and a CSRF token
   cookie. State-changing requests must echo the CSRF token in a header.
6. The token carries only `badge_id` and `jti`. **Level and permissions are read from the database on every request**, so
   a demotion or deactivation takes effect immediately; a forged `role` claim has nothing to forge. Logout revokes the
   `jti`.
7. New users get a one-time provisioning URI for the authenticator app and `must_change_password`; no password or TOTP
   seed is ever logged or returned again.

## API (FastAPI, `/api/v1`)
| Endpoint | Permission |
|---|---|
| `GET /healthz` | none |
| `POST /auth/login`, `POST /auth/logout`, `GET /auth/me` | none / session |
| `GET /claims`, `GET /claims/{id}` | `claims.view` |
| `POST /claims/{id}/unmask` | `pii.unmask` |
| `POST /claims/{id}/findings/{rule_id}/decision` | `claims.decide`, plus `claims.decide_high` for a high-severity finding |
| `POST /claims/{id}/recheck` | `claims.recheck` |
| `GET /audit/events`, `GET /audit/verify` | `audit.view` / `audit.verify` |
| `POST /users`, `PATCH /users/{badge}`, `POST /users/{badge}/unlock`, `POST /users/{badge}/reset-totp` | `users.manage` |

**Hide, not disable.** The server shapes each response to the caller: fields and actions the caller may not use are absent,
not flagged. `GET /claims/{id}` returns `allowed_actions` computed server-side. A request for something forbidden gets 403
and an audit event; a request for a claim that does not exist and one the caller cannot see are indistinguishable (404).

## Data minimization
Patient and member identifiers are masked for everyone by default (stable pseudonym, for example `PAT-7F3A`, derived with a
keyed hash so it is consistent but not reversible). `claims.view_notes` gates notes and attachment text. `unmask` returns
the real identifiers for one claim, needs a reason, and writes an audit event. The AI explanation request already carries
only the finding; the API never forwards raw identifiers to it.

## Decision binding
The decision endpoint sets `actor` from the session and ignores any `actor` in the body, validates through the existing
`review_workflow` code, and writes through the existing review audit. The old file-based CLI flow stays for offline
demos and is labelled as unauthenticated in its help text and in the docs.

## Security audit log
A second hash-chained log, `security_audit.jsonl`, records login success and failure, lockout, logout, token rejection,
every 403, unmask, user changes and verification calls. In server mode the anchor must be HMAC-signed: startup refuses to run
without `AUDIT_ANCHOR_KEY`.

## Configuration and secrets
`JWT_SECRET` (at least 32 bytes), `FERNET_KEY`, `AUDIT_ANCHOR_KEY`, `MONGO_URI` come from the environment. The server
refuses to start with a missing or default value. A `--dev` flag generates ephemeral keys and prints a banner; it cannot be
combined with a non-localhost bind. `scripts/access_admin.py` creates the first admin and seeds demo users for the demo.

## Differences from Attijari
Fails closed when the replay store is down (Attijari fails open). Replay protection and lockout live in the database, so no
Redis is needed. No blanket admin bypass: each permission is checked. CSRF token added. Uniform timing for unknown users.
Level changes apply instantly because permissions are not cached in the token.

## Dependencies
Runtime, pinned and covered by `pip-audit`: `fastapi`, `uvicorn`, `bcrypt`, `PyJWT`, `pyotp`, `cryptography`, `pymongo`.
Dev only: `httpx` for the test client. This ends the README's "everything else is standard library" statement for the
server; the offline engine and the evaluation keep working without these packages installed.

## Layout
`src/access/` with `config`, `permissions`, `passwords`, `totp`, `tokens`, `masking`, `store` (interface and in-memory),
`store_mongo`, `service`, `securitylog`, `api`; `scripts/access_admin.py`; tests `tests/test_access_*.py`.

## Stress and security testing (part of this sub-project, as for the earlier ones)
- **Matrix test:** every endpoint against every level and against no session, expecting exactly the table above.
- **Hide-not-disable:** for each level, search every response body for forbidden keys and raw identifiers.
- **Attacks:** brute force and lockout; TOTP replay, including 50 parallel submissions of one code (exactly one succeeds);
  tampered, expired, `alg: none`, wrongly signed and revoked tokens; forged role claims; CSRF; NoSQL operator injection
  in every string field; mass assignment on user updates (a non-admin sending `level`); privilege escalation by an admin
  above their own level; IDOR on claims; user enumeration by response and by timing; session reuse after logout and after
  deactivation; oversized and malformed bodies.
- **Concurrency:** parallel logins, parallel lockout counting, parallel user edits; the in-memory and MongoDB stores must
  behave identically.
- **Property tests** (Hypothesis) on the login and decision inputs, added to the fuzz campaign.
- **Load:** login and read latency at 1, 8 and 32 concurrent clients, recorded with the machine description.
- **Evidence** in `outputs/defense/access.json` and a row in `docs/27`; findings fixed with regression tests, as before.

## Success criteria
A decision cannot be recorded under a name other than the logged-in badge; an L2 cannot decide a high-severity finding; an L1
never receives raw identifiers or notes; one TOTP code authenticates once; the matrix and attack suites pass on Python 3.10,
3.12 and 3.14 against both stores; the existing 618 tests still pass; the README's dependency statement and `docs/20` A07
row are updated.

## Risks and open items
- **Docker Desktop is installed but its engine is not running here, and MongoDB is not installed.** To run the MongoDB
  contract tests locally, start Docker Desktop (or install MongoDB Community); CI runs them regardless. Until then the
  in-memory store carries the local runs and the skipped Mongo tests are reported, not hidden.
- bcrypt cost makes tests slow, so tests use a low cost factor configured only in test settings.
- Wheels for `cryptography`, `bcrypt` and `pymongo` on Python 3.14 are assumed and will be verified in the first task.
