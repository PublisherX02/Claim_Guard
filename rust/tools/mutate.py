"""Mutation test of the security-critical Rust code: break one rule at a time and demand that the tests notice.

    python rust/tools/mutate.py [--only ID,ID] [--json out.json]

Each mutant is an exact text replacement in one source file (it must match exactly once). The crate's tests run against the mutated source;
a mutant is KILLED when they fail (or the build is rejected) and SURVIVED when they still pass, which means a gap in the tests. The file is
always restored. Takes a few minutes per mutant, dominated by compiling.
"""
import argparse, json, os, subprocess, sys, time
from pathlib import Path

RUST = Path(__file__).resolve().parents[1]
CR = RUST / 'crates'
API, ACC, QUE = 'cg-api/src/lib.rs', 'cg-access/src/', 'cg-queue/src/'

# (id, crate to test, file, old text, new text)
M = [
    ('api-csrf-off', 'cg-api', API, 'if !SAFE_METHODS.contains(&req.method.as_str()) && !constant_time_eq(', 'if false && !constant_time_eq('),
    ('api-password-change-gate-off', 'cg-api', API, 'if p.user.must_change_password && !PASSWORD_CHANGE_PATHS.contains(', 'if false && !PASSWORD_CHANGE_PATHS.contains('),
    ('api-need-always-true', 'cg-api', API, 'if p.permissions.contains(perm) { Ok(p) } else { Err(self.forbid(req, &p)) }', 'if true { Ok(p) } else { Err(self.forbid(req, &p)) }'),
    ('api-cookie-not-httponly', 'cg-api', API, 'if http_only { "; HttpOnly" } else { "" }', '""'),
    ('api-body-limit-huge', 'cg-api', API, 'pub const MAX_BODY_BYTES: usize = 65_536;', 'pub const MAX_BODY_BYTES: usize = 655_360_000;'),
    ('api-content-type-unchecked', 'cg-api', API, 'if has_body && ctype != "application/json" {', 'if false && ctype != "application/json" {'),
    ('api-high-severity-check-off', 'cg-api', API, 'if finding["severity"] == "high" && !p.permissions.contains("claims.decide_high") {\n            return Err(self.forbid(req, &p));', 'if false && !p.permissions.contains("claims.decide_high") {\n            return Err(self.forbid(req, &p));'),
    ('api-unmask-needs-only-view', 'cg-api', API, 'let p = self.need(req, "pii.unmask")?;', 'let p = self.need(req, "claims.view")?;'),
    ('api-audit-events-needs-only-view', 'cg-api', API, 'let p = self.need(req, "audit.view")?;\n        let q = query(req);', 'let p = self.need(req, "claims.view")?;\n        let q = query(req);'),
    ('api-users-list-needs-only-view', 'cg-api', API, 'let p = self.need(req, "users.manage")?;\n        let users = self.access.list_users', 'let p = self.need(req, "claims.view")?;\n        let users = self.access.list_users'),
    ('api-frame-header-dropped', 'cg-api', API, '            ("x-frame-options", "DENY"),\n', ''),
    ('api-logout-does-not-revoke', 'cg-api', API, 'self.access.logout(&cookie(req, SESSION_COOKIE).unwrap_or_default());', 'let _ = ();'),
    ('api-decision-actor-from-nowhere', 'cg-api', API, '"actor": p.badge(), "reason": b.reason,', '"actor": "CG-0000", "reason": b.reason,'),
    ('api-decision-body-not-strict', 'cg-api', API, '#[serde(deny_unknown_fields)]\nstruct DecisionBody', 'struct DecisionBody'),
    ('api-login-body-not-strict', 'cg-api', API, '#[serde(deny_unknown_fields)]\nstruct LoginBody', 'struct LoginBody'),
    ('api-finding-actions-for-anyone', 'cg-api', API, ' || !p.permissions.contains("claims.decide") {\n            return None;', ' {\n            return None;'),
    ('api-unmask-not-audited', 'cg-api', API, 'self.access.record("unmask", json!({"badge_id": p.badge(), "claim_id": id.chars().take(64).collect::<String>(), "reason": b.reason})).map_err(|e| self.serr(e.into(), req, &p))?;', '();'),
    ('api-unauthenticated-is-403', 'cg-api', API, 'let token = cookie(req, SESSION_COOKIE).ok_or_else(|| ApiError::new(401, "unauthenticated"))?;', 'let token = cookie(req, SESSION_COOKIE).ok_or_else(|| ApiError::new(403, "unauthenticated"))?;'),
    ('api-forbidden-not-noted', 'cg-api', API, 'self.access.note_forbidden(p.badge(), &req.method, &req.path);', '();'),
    ('api-xff-ignored-check', 'cg-api', 'cg-api/src/server.rs', 'if behind_proxy {\n        if let Some(last)', 'if true {\n        if let Some(last)'),
    ('api-xff-first-hop', 'cg-api', 'cg-api/src/server.rs', 'v.rsplit(\',\').next()', 'v.split(\',\').next()'),
    ('acc-token-expiry-ignored', 'cg-access', ACC + 'tokens.rs', 'if exp as f64 <= now || iat as f64 > now {', 'if iat as f64 > now {'),
    ('acc-token-future-iat-allowed', 'cg-access', ACC + 'tokens.rs', 'if exp as f64 <= now || iat as f64 > now {', 'if exp as f64 <= now {'),
    ('acc-revocation-ignored', 'cg-access', ACC + 'service.rs', 'if self.store.is_revoked(jti, now).map_err(|_| AuthError::Unavailable)? {', 'if false && self.store.is_revoked(jti, now).map_err(|_| AuthError::Unavailable)? {'),
    ('acc-totp-replay-allowed', 'cg-access', ACC + 'service.rs', 'let fresh = self.store.mark_totp_used(badge_id, step, ((step + 2) * self.settings.totp_step) as f64).map_err(unavailable)?;', 'let _ = self.store.mark_totp_used(badge_id, step, ((step + 2) * self.settings.totp_step) as f64).map_err(unavailable)?; let fresh = true;'),
    ('acc-viewer-can-unmask', 'cg-access', ACC + 'permissions.rs', 'let l1: Perms = ["claims.view"].into_iter().collect();', 'let l1: Perms = ["claims.view", "pii.unmask"].into_iter().collect();'),
    ('acc-reviewer-can-decide-high', 'cg-access', ACC + 'permissions.rs', 'let mut l3 = l2.clone();\n    l3.insert("claims.decide_high");', 'let mut l3 = l2.clone();\n    l3.insert("claims.decide_high"); l2.insert("claims.decide_high");'),
    ('acc-notes-always-visible', 'cg-access', ACC + 'masking.rs', 'if !permissions.contains("claims.view_notes") {', 'if false && !permissions.contains("claims.view_notes") {'),
    ('svc-lock-binds-the-owner-too', 'cg-access', ACC + 'service.rs', 'if user.locked_until.is_some_and(|l| l > now) && !known {', 'if user.locked_until.is_some_and(|l| l > now) {'),
    ('svc-lock-ignored', 'cg-access', ACC + 'service.rs', 'if user.locked_until.is_some_and(|l| l > now) && !known {', 'if false && !known {'),
    ('svc-pair-limit-off', 'cg-access', ACC + 'service.rs', 'self.store.counter(&pair, PAIR_WINDOW, now).map_err(unavailable)? >= PAIR_LIMIT;', 'self.store.counter(&pair, PAIR_WINDOW, now).map_err(unavailable)? >= 1_000_000;'),
    ('svc-refusal-not-counted', 'cg-access', ACC + 'service.rs', '            self.store.bump_counter(&pair, PAIR_WINDOW, now).map_err(unavailable)?;\n', ''),
    ('svc-device-never-remembered', 'cg-access', ACC + 'service.rs', '        self.store.mark_client(badge_id, &ck, now).map_err(unavailable)?;\n', ''),
    ('svc-all-clients-look-alike', 'cg-access', ACC + 'service.rs', 'mac.update(format!("client|{client}").as_bytes());', 'mac.update(b"client|x");'),
    ('api-address-limit-huge', 'cg-api', API, 'pub const LOGIN_FAILURE_LIMIT: i64 = 100;', 'pub const LOGIN_FAILURE_LIMIT: i64 = 100_000;'),
    ('api-address-failures-not-counted', 'cg-api', API, 'let _ = self.access.store.bump_counter(&self.ip_key(client), LOGIN_FAILURE_WINDOW as u64, now);', 'let _ = ();'),
    ('guard-trusts-claim-values', 'cg-queue', QUE + 'guard.rs', '.filter(|(k, _)| WRITTEN_BY_THE_ENGINE.contains(&k.as_str()))', '.filter(|(_k, _)| true)'),
    ('fill-does-not-quote', 'cg-queue', QUE + 'explain.rs', 'if capped.is_empty() || plain { capped } else {', 'if true { capped } else {'),
    ('fill-keeps-direction-characters', 'cg-queue', QUE + 'explain.rs', 'GeneralCategory::Format | GeneralCategory::Control', 'GeneralCategory::Control'),
    ('crypt-field-not-bound-to-its-place', 'cg-mongo', 'cg-mongo/src/crypt.rs', 'let here_ok = ids.0.as_deref().map_or(true, |id| env["c"] == id) && ids.1.map_or(true, |ver| env["v"] == ver) && (field.is_empty() || env["f"] == field);', 'let here_ok = true;'),
    ('crypt-patient-index-in-clear', 'cg-mongo', 'cg-mongo/src/crypt.rs', 'mac.finalize().into_bytes().iter().take(16).map(|b| format!("{b:02x}")).collect()', 'patient_id.to_string()'),
    ('mirror-follows-an-edited-log', 'cg-audit', 'cg-audit/src/mirror.rs', 'if prefix_hash(mirror, have)?.as_deref() != Some(src.as_str()) {', 'if false {'),
    ('guard-job-failure-shown-as-ok', 'cg-api', 'cg-api/src/ops.rs', '(DOWN, format!("FAILED {} s ago: {}", age as i64,', '(OK, format!("FAILED {} s ago: {}", age as i64,'),
    ('api-cross-site-check-off', 'cg-api', API, 'if !SAFE_METHODS.contains(&req.method.as_str()) && Self::cross_site(req) {', 'if false && Self::cross_site(req) {'),
    ('api-origin-never-compared-to-host', 'cg-api', API, 'host.is_empty() || !req.headers.get("host").is_some_and(|h| h.eq_ignore_ascii_case(host))', 'host.is_empty()'),
    ('api-fetch-metadata-ignored', 'cg-api', API, 'if req.headers.get("sec-fetch-site").is_some_and(|v| v.eq_ignore_ascii_case("cross-site")) {', 'if false {'),
    ('api-unmask-not-rationed', 'cg-api', API, 'if used > self.unmask_per_hour {', 'if false && used > self.unmask_per_hour {'),
    ('api-unmask-reason-may-hold-the-id', 'cg-api', API, 'if cg_access::masking::text_leaks_identifier(&b.reason, &c, &self.settings.pii_key) {', 'if false && cg_access::masking::text_leaks_identifier(&b.reason, &c, &self.settings.pii_key) {'),
    ('api-decision-reason-may-hold-the-id', 'cg-api', API, 'if cg_access::masking::text_leaks_identifier(&b.reason, &claim, &self.settings.pii_key) {', 'if false && cg_access::masking::text_leaks_identifier(&b.reason, &claim, &self.settings.pii_key) {'),
    ('api-opener-policy-dropped', 'cg-api', API, '            ("cross-origin-opener-policy", "same-origin"),\n', ''),
    ('que-decision-reason-may-hold-the-id', 'cg-queue', QUE + 'service.rs', 'if masking::text_leaks_identifier(reason, &doc["claim"], &self.access.settings.pii_key) {', 'if false && masking::text_leaks_identifier(reason, &doc["claim"], &self.access.settings.pii_key) {'),
    ('rotation-leaves-decisions-readable', 'cg-mongo', 'cg-mongo/src/crypt.rs', 'self.seal(&id, version, "decisions", d)', 'd.clone()'),
    ('rotation-keeps-the-old-patient-index', 'cg-mongo', 'cg-mongo/src/crypt.rs', 'self.seal_new_doc(&mut doc);\n        if let Some(list)', 'if let Some(list)'),
    ('que-same-person-countersign', 'cg-queue', QUE + 'service.rs', 'if sign["first_by"] == principal.badge() || sign["second_by"] == principal.badge() {', 'if false && (sign["first_by"] == principal.badge() || sign["second_by"] == principal.badge()) {'),
    ('que-lease-owner-unchecked', 'cg-queue', QUE + 'service.rs', 'if doc["state"] == "leased" && doc["lease"]["badge_id"] == principal.badge() {', 'if doc["state"] == "leased" {'),
    ('que-expired-lease-usable', 'cg-queue', QUE + 'service.rs', 'if doc["lease"]["expires_at"].as_f64().unwrap_or(0.0) <= now {', 'if false && doc["lease"]["expires_at"].as_f64().unwrap_or(0.0) <= now {'),
    ('que-unreviewable-decidable', 'cg-queue', QUE + 'service.rs', 'if !finding["status"].as_str().is_some_and(|s| REVIEWABLE.contains(&s)) {\n            return Err(QueueError::Invalid("only a flagged', 'if false && !finding["status"].as_str().is_some_and(|s| REVIEWABLE.contains(&s)) {\n            return Err(QueueError::Invalid("only a flagged'),
    # round 4: two-person approval, encrypted audit logs and names, key rotation
    ('acc-dual-control-never-applies', 'cg-access', ACC + 'service.rs', 'Ok(self.settings.dual_control && self.store.count_active_admins()? >= 2)', 'Ok(false)'),
    ('acc-dual-control-asks-a-lone-admin', 'cg-access', ACC + 'service.rs', 'self.store.count_active_admins()? >= 2)', 'self.store.count_active_admins()? >= 1)'),
    ('acc-approve-own-request', 'cg-access', ACC + 'service.rs', 'if a.requested_by == actor.badge() {\n            return Err(PermissionError::Denied("the person who asked cannot approve', 'if false && a.requested_by == actor.badge() {\n            return Err(PermissionError::Denied("the person who asked cannot approve'),
    ('acc-approve-without-authenticator-code', 'cg-access', ACC + 'service.rs', 'self.confirm_with_authenticator(actor, totp_code)?;', 'let _ = totp_code;'),
    ('acc-wrong-approval-codes-not-counted', 'cg-access', ACC + 'service.rs', 'if self.store.counter(&key, 900, now)? >= 5 {', 'if false && self.store.counter(&key, 900, now)? >= 5 {'),
    ('acc-approval-code-replayable', 'cg-access', ACC + 'service.rs', 'if !self.store.mark_totp_used(actor.badge(), step, ((step + 2) * self.settings.totp_step) as f64)? {', 'if false && !self.store.mark_totp_used(actor.badge(), step, ((step + 2) * self.settings.totp_step) as f64)? {'),
    ('acc-approval-never-expires', 'cg-access', ACC + 'service.rs', 'if a.expires_at <= self.now() {', 'if false && a.expires_at <= self.now() {'),
    ('acc-authenticator-reset-is-direct', 'cg-access', ACC + 'service.rs', 'if !self.dual_control()? {\n            let uri = self.reset_totp(actor, badge_id)?;', 'if true {\n            let uri = self.reset_totp(actor, badge_id)?;'),
    ('acc-admin-changes-are-direct', 'cg-access', ACC + 'service.rs', 'let touches_an_admin = target.level == permissions::ADMIN_LEVEL || changes.level == Some(permissions::ADMIN_LEVEL);', 'let touches_an_admin = false;'),
    ('acc-admin-creation-is-direct', 'cg-access', ACC + 'service.rs', 'if level != permissions::ADMIN_LEVEL {\n            return Ok(Gate::Done(self.create_user(', 'if true {\n            return Ok(Gate::Done(self.create_user('),
    ('acc-anyone-can-withdraw-a-request', 'cg-access', ACC + 'service.rs', 'if a.requested_by != actor.badge() {\n            return Err(PermissionError::Denied("only the person who asked can withdraw', 'if false && a.requested_by != actor.badge() {\n            return Err(PermissionError::Denied("only the person who asked can withdraw'),
    ('acc-approved-request-skips-the-rules', 'cg-access', ACC + 'service.rs', 'self.update_user(approver, &a.target, &changes)?;', 'self.store.update_user(&a.target, &UserUpdate { level: changes.level, active: changes.active, ..Default::default() })?;'),
    ('audit-rows-written-in-clear', 'cg-audit', 'cg-audit/src/atrest.rs', 'Some(f) => format!("{PREFIX}{}", f.encrypt(plain.as_bytes())),', 'Some(_) => plain.to_string(),'),
    ('audit-previous-keys-ignored', 'cg-audit', 'cg-audit/src/atrest.rs', '.chain(guard.previous.iter())', ''),
    ('mongo-names-stored-in-clear', 'cg-mongo', 'cg-mongo/src/users.rs', 'self.crypt.as_ref().map_or_else(|| name.to_string(), |c| c.seal_text(badge_id, "name", name))', 'name.to_string()'),
    ('mongo-name-moved-between-people-accepted', 'cg-mongo', 'cg-mongo/src/crypt.rs', 'if env["s"] != scope || env["f"] != field {', 'if false {'),
    ('rotate-seeds-not-rewritten', 'cg-mongo', 'cg-mongo/src/users.rs', '"$set": {"totp_secret_enc": n.encrypt(&plain)}', '"$set": {"totp_secret_enc": stored}'),
    ('rotate-names-not-rewritten', 'cg-mongo', 'cg-mongo/src/users.rs', '"$set": {"name": new.seal_text(badge, "name", &plain)}', '"$set": {"name": stored}'),
]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--only')
    ap.add_argument('--json')
    a = ap.parse_args()
    only = set(a.only.split(',')) if a.only else None
    tools = os.pathsep.join([str(Path.home() / '.cargo' / 'bin'), 'C:/msys64/ucrt64/bin'])
    env = dict(os.environ, PATH=tools + os.pathsep + os.environ['PATH'])
    out = []
    for mid, crate, rel, old, new in M:
        if only and mid not in only:
            continue
        path = CR / rel
        raw = path.read_bytes().decode('utf8')
        crlf = '\r\n' in raw
        src = raw.replace('\r\n', '\n')
        if src.count(old) != 1:
            out.append({'id': mid, 'result': f'BAD-PATTERN({src.count(old)} matches)'})
            print(f'{mid:36} BAD PATTERN ({src.count(old)} matches)', flush=True)
            continue
        mutated = src.replace(old, new)
        path.write_bytes((mutated.replace('\n', '\r\n') if crlf else mutated).encode('utf8'))
        t0 = time.time()
        try:
            p = subprocess.run([str(Path.home() / '.cargo' / 'bin' / 'cargo.exe'), 'test', '--release', '-p', crate, '--quiet'], cwd=RUST, env=env, capture_output=True, text=True, encoding='utf-8', errors='replace', timeout=1500)
            built = 'could not compile' not in p.stderr and 'error[' not in p.stderr
            result = 'SURVIVED' if p.returncode == 0 else ('KILLED' if built else 'KILLED(build error: fix the mutant)')
        except subprocess.TimeoutExpired:
            result = 'KILLED(timeout)'
        finally:
            path.write_bytes(raw.encode('utf8'))
        out.append({'id': mid, 'crate': crate, 'result': result, 'seconds': round(time.time() - t0)})
        print(f'{mid:36} {result}  ({round(time.time() - t0)} s)', flush=True)
    alive = [o for o in out if o['result'] == 'SURVIVED' or o['result'].startswith('BAD')]
    print(f'\n{len(out)} mutants, {sum(o["result"].startswith("KILLED") for o in out)} killed, {len(alive)} survived or invalid')
    if a.json:
        Path(a.json).write_text(json.dumps(out, indent=1))
    return 1 if alive else 0


if __name__ == '__main__':
    sys.exit(main())
