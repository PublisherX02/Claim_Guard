//! The rules of identity and access (`src/access/service.py`). No HTTP in here: the API layer only translates requests to these calls.
//!
//! Login order (every early exit spends the time of one real password check, so an unknown, locked or inactive account answers no faster
//! than a real one): shape checks, lookup, locked, inactive, password, authenticator code, consume the code, then issue a session. Every
//! failure is the same `AuthError::InvalidCredentials` to the caller; the true reason goes to the security log.

use crate::permissions::{self, PermissionError, Perms};
use crate::securitylog::SecurityLog;
use crate::settings::Settings;
use crate::store::{StoreError, User, UserStore, UserUpdate};
use crate::{passwords, tokens, totp, Clock};
use regex::Regex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

pub const MAX_PASSWORD_CHARS: usize = 128;
pub const MAX_NAME_CHARS: usize = 100;
pub const MAX_CLIENT_CHARS: usize = 64;
const AUDIT_GATE_LIMIT: u32 = 200; // failure and rejection events logged per window before they are summarised
const AUDIT_GATE_WINDOW: f64 = 60.0;

#[derive(Debug, thiserror::Error, PartialEq, Clone, Copy)]
pub enum AuthError {
    #[error("invalid credentials")]
    InvalidCredentials,
    #[error("token invalid")]
    TokenInvalid,
    #[error("unavailable")]
    Unavailable,
}

#[derive(Debug, thiserror::Error)]
#[error("forbidden: {0}")]
pub struct Forbidden(pub String);

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error(transparent)]
    Auth(#[from] AuthError),
    #[error(transparent)]
    Forbidden(#[from] Forbidden),
    #[error(transparent)]
    Permission(#[from] PermissionError),
    #[error("that badge already exists")]
    Duplicate,
    #[error("not found")]
    NotFound,
    #[error("{0}")]
    Invalid(String),
}

impl From<StoreError> for ServiceError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::Unavailable => AuthError::Unavailable.into(),
            StoreError::Duplicate => ServiceError::Duplicate,
            StoreError::NotFound => ServiceError::NotFound,
            StoreError::Invalid(m) => ServiceError::Invalid(m),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Principal {
    pub user: User,
    pub permissions: Perms,
    pub jti: String,
    pub csrf: String,
}

impl Principal {
    pub fn badge(&self) -> &str {
        &self.user.badge_id
    }
}

#[derive(Debug, Clone)]
pub struct Session {
    pub token: String,
    pub csrf: String,
    pub expires_at: i64,
    pub must_change_password: bool,
}

/// Fields of a user change request; `None` means "leave it".
#[derive(Debug, Clone, Default)]
pub struct Changes {
    pub name: Option<String>,
    pub level: Option<i64>,
    pub grants: Option<Vec<String>>,
    pub revokes: Option<Vec<String>>,
    pub active: Option<bool>,
}

fn badge_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^CG-\d{4,8}\z").expect("static pattern"))
}

fn cut(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Caps how many failure events one window can add, so a flood of bad logins or bad tokens cannot fill the disk. The first event of the
/// next window is preceded by a summary saying how many were left out.
struct AuditGate {
    state: Mutex<HashMap<String, (f64, u32, u32)>>,
}

impl AuditGate {
    fn allow(&self, key: &str, now: f64) -> (bool, u32) {
        let mut g = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let (start, count, suppressed) = g.get(key).copied().unwrap_or((now, 0, 0));
        if now - start >= AUDIT_GATE_WINDOW {
            g.insert(key.to_string(), (now, 1, 0));
            return (true, suppressed);
        }
        if count < AUDIT_GATE_LIMIT {
            g.insert(key.to_string(), (start, count + 1, suppressed));
            return (true, 0);
        }
        g.insert(key.to_string(), (start, count, suppressed + 1));
        (false, 0)
    }
}

pub struct AccessService {
    pub store: Arc<dyn UserStore>,
    pub settings: Settings,
    pub log: Arc<SecurityLog>,
    clock: Clock,
    gate: AuditGate,
}

impl AccessService {
    pub fn new(store: Arc<dyn UserStore>, settings: Settings, log: Arc<SecurityLog>, clock: Clock) -> AccessService {
        passwords::dummy_verify("warm-up", settings.bcrypt_rounds); // build the cached hash before the first real request
        AccessService { store, settings, log, clock, gate: AuditGate { state: Mutex::new(HashMap::new()) } }
    }

    pub fn now(&self) -> f64 {
        (self.clock)()
    }

    // ---- audit helpers -----------------------------------------------------------------------------------------------
    fn gated(&self, event_type: &str, reason: &str, mut fields: Value) {
        let (allowed, suppressed) = self.gate.allow(event_type, self.now());
        if suppressed > 0 {
            let _ = self.log.record(event_type, json!({"reason": format!("suppressed_{suppressed}_events")}));
        }
        if allowed {
            fields["reason"] = json!(reason);
            let _ = self.log.record(event_type, fields); // the request is already being refused; do not turn that into an error
        }
    }

    fn fail<T>(&self, reason: &str, attempt: &str, client: &str) -> Result<T, AuthError> {
        self.gated("login_failure", reason, json!({"badge_attempt": cut(attempt, 64), "client": cut(client, MAX_CLIENT_CHARS)}));
        Err(AuthError::InvalidCredentials)
    }

    fn count_failure(&self, user: &User, now: f64) -> Result<(), AuthError> {
        let after = self.store.record_failed_login(&user.badge_id, now, self.settings.max_failed_logins, self.settings.lockout_seconds).map_err(|_| AuthError::Unavailable)?;
        if after.locked_until.is_some() && user.locked_until.is_none() {
            let _ = self.log.record("lockout", json!({"badge_id": user.badge_id, "until": after.locked_until, "attempts": after.failed_attempts}));
        }
        Ok(())
    }

    /// Records an event whose loss would hide a privileged action; if it cannot be written the action is refused.
    fn must(&self, event_type: &str, fields: Value) -> Result<(), AuthError> {
        self.log.record(event_type, fields).map_err(|_| AuthError::Unavailable)
    }

    /// Audits a refused request. Capped per window like other failure events. Never fails: a refusal stays a refusal.
    pub fn note_forbidden(&self, badge_id: &str, method: &str, path: &str) {
        let (allowed, suppressed) = self.gate.allow("forbidden", self.now());
        if !allowed {
            return;
        }
        let mut fields = json!({"badge_id": badge_id, "method": cut(method, 10), "path": cut(path, 200)});
        if suppressed > 0 {
            fields["suppressed_before"] = json!(suppressed);
        }
        let _ = self.log.record("forbidden", fields);
    }

    /// Writes a security event that must not be lost: if it cannot be written the caller's action is refused.
    pub fn record(&self, event_type: &str, fields: Value) -> Result<(), AuthError> {
        self.must(event_type, fields)
    }

    // ---- sessions ----------------------------------------------------------------------------------------------------
    pub fn login(&self, badge_id: &str, password: &str, totp_code: &str, client: &str) -> Result<Session, AuthError> {
        let now = self.now();
        let rounds = self.settings.bcrypt_rounds;
        let shaped = badge_re().is_match(badge_id) && !password.is_empty() && password.chars().count() <= MAX_PASSWORD_CHARS;
        if !shaped {
            passwords::dummy_verify(password, rounds);
            return self.fail("malformed", badge_id, client);
        }
        let user = self.store.get_user(badge_id).map_err(|_| AuthError::Unavailable)?;
        let Some(user) = user else {
            passwords::dummy_verify(password, rounds);
            return self.fail("unknown_badge", badge_id, client);
        };
        if user.locked_until.is_some_and(|l| l > now) {
            passwords::dummy_verify(password, rounds);
            return self.fail("locked", badge_id, client);
        }
        if !user.active {
            passwords::dummy_verify(password, rounds);
            return self.fail("inactive", badge_id, client);
        }
        if !passwords::verify_password(password, &user.password_hash) {
            self.count_failure(&user, now)?;
            return self.fail("bad_password", badge_id, client);
        }
        let Ok(secret) = totp::decrypt_secret(&user.totp_secret_enc, &self.settings.fernet_key) else {
            return self.fail("seed_unreadable", badge_id, client);
        };
        let Some(step) = totp::verify_code(&secret, totp_code, now, self.settings.totp_step) else {
            self.count_failure(&user, now)?;
            return self.fail("bad_totp", badge_id, client);
        };
        let fresh = self.store.mark_totp_used(badge_id, step, ((step + 2) * self.settings.totp_step) as f64).map_err(|_| AuthError::Unavailable)?;
        if !fresh {
            self.count_failure(&user, now)?;
            return self.fail("replayed_totp", badge_id, client);
        }
        self.store.record_successful_login(badge_id, now).map_err(|_| AuthError::Unavailable)?;
        let issued = tokens::issue(&self.settings, badge_id, now);
        self.must("login_success", json!({"badge_id": badge_id, "client": cut(client, MAX_CLIENT_CHARS)}))?;
        Ok(Session { token: issued.token, csrf: issued.csrf, expires_at: issued.expires_at, must_change_password: user.must_change_password })
    }

    fn reject<T>(&self, reason: &str) -> Result<T, AuthError> {
        self.gated("token_rejected", reason, json!({}));
        Err(AuthError::TokenInvalid)
    }

    pub fn authenticate(&self, token: &str) -> Result<Principal, AuthError> {
        let now = self.now();
        let Ok(payload) = tokens::decode(&self.settings, token, now) else { return self.reject("invalid") };
        let (sub, jti, csrf) = (payload["sub"].as_str().unwrap_or(""), payload["jti"].as_str().unwrap_or(""), payload["csrf"].as_str().unwrap_or(""));
        if self.store.is_revoked(jti, now).map_err(|_| AuthError::Unavailable)? {
            return self.reject("revoked");
        }
        let user = self.store.get_user(sub).map_err(|_| AuthError::Unavailable)?;
        let Some(user) = user.filter(|u| u.active) else { return self.reject("user_gone_or_inactive") };
        let Ok(perms) = permissions::effective_permissions(user.level, &user.grants, &user.revokes) else { return self.reject("corrupt_user_record") };
        Ok(Principal { user, permissions: perms, jti: jti.to_string(), csrf: csrf.to_string() })
    }

    pub fn logout(&self, token: &str) {
        let Ok(payload) = tokens::decode(&self.settings, token, self.now()) else { return };
        if self.store.revoke_token(payload["jti"].as_str().unwrap_or(""), payload["exp"].as_f64().unwrap_or(0.0)).is_err() {
            return;
        }
        let _ = self.log.record("logout", json!({"badge_id": payload["sub"]}));
    }

    pub fn require(principal: &Principal, permission: &str) -> Result<(), ServiceError> {
        if !permissions::PERMISSIONS.contains(&permission) {
            return Err(ServiceError::Invalid("unknown permission".into()));
        }
        if !principal.permissions.contains(permission) {
            return Err(Forbidden(permission.to_string()).into());
        }
        Ok(())
    }

    // ---- user management ---------------------------------------------------------------------------------------------
    /// Creates a user without an acting administrator. For trusted local administration only (the admin tool and tests); the API never
    /// calls this. Returns (user, provisioning URI); the URI carries the authenticator seed and is shown once.
    pub fn provision_user(&self, badge_id: &str, name: &str, password: &str, level: i64, created_by: &str, grants: &[String], revokes: &[String], must_change_password: bool)
        -> Result<(User, String), ServiceError> {
        if !badge_re().is_match(badge_id) {
            return Err(ServiceError::Invalid("badge must look like CG-1234 (4 to 8 digits)".into()));
        }
        if name.trim().is_empty() || name.chars().count() > MAX_NAME_CHARS {
            return Err(ServiceError::Invalid("name must be 1 to 100 characters".into()));
        }
        permissions::effective_permissions(level, grants, revokes)?; // validates level and names
        let problems = passwords::check_policy(password, badge_id);
        if !problems.is_empty() {
            return Err(ServiceError::Invalid(problems.join("; ")));
        }
        let secret = totp::new_secret();
        let user = User {
            badge_id: badge_id.into(),
            name: name.trim().into(),
            password_hash: passwords::hash_password(password, self.settings.bcrypt_rounds).map_err(ServiceError::Invalid)?,
            totp_secret_enc: totp::encrypt_secret(&secret, &self.settings.fernet_key).map_err(ServiceError::Invalid)?,
            level,
            grants: grants.to_vec(),
            revokes: revokes.to_vec(),
            active: true,
            failed_attempts: 0,
            locked_until: None,
            must_change_password,
            created_by: created_by.into(),
            created_at: self.now(),
            last_login: None,
        };
        self.store.create_user(&user)?;
        Ok((user, totp::provisioning_uri(&secret, badge_id, "ClaimGuard")))
    }

    pub fn create_user(&self, actor: &Principal, badge_id: &str, name: &str, password: &str, level: i64, grants: &[String], revokes: &[String]) -> Result<(User, String), ServiceError> {
        Self::require(actor, "users.manage")?;
        permissions::check_user_change(actor.user.level, level, level, grants, revokes, false)?;
        let out = self.provision_user(badge_id, name, password, level, actor.badge(), grants, revokes, true)?;
        self.must("user_created", json!({"actor": actor.badge(), "badge_id": badge_id, "level": level}))?;
        Ok(out)
    }

    pub fn list_users(&self, actor: &Principal) -> Result<Vec<User>, ServiceError> {
        Self::require(actor, "users.manage")?;
        Ok(self.store.list_users()?)
    }

    pub fn update_user(&self, actor: &Principal, badge_id: &str, changes: &Changes) -> Result<User, ServiceError> {
        Self::require(actor, "users.manage")?;
        if changes.name.is_none() && changes.level.is_none() && changes.grants.is_none() && changes.revokes.is_none() && changes.active.is_none() {
            return Err(ServiceError::Invalid("changes must be a non-empty object".into()));
        }
        let target = self.store.get_user(badge_id)?.ok_or(ServiceError::NotFound)?;
        let new_level = changes.level.unwrap_or(target.level);
        let grants = changes.grants.clone().unwrap_or_else(|| target.grants.clone());
        let revokes = changes.revokes.clone().unwrap_or_else(|| target.revokes.clone());
        let self_change = actor.badge() == badge_id;
        permissions::check_user_change(actor.user.level, target.level, new_level, &grants, &revokes, self_change)?;
        let new_active = changes.active.unwrap_or(target.active);
        if self_change && !new_active {
            return Err(PermissionError::Denied("cannot deactivate yourself".into()).into());
        }
        let stops_being_admin = target.level == permissions::ADMIN_LEVEL && target.active && (new_level != permissions::ADMIN_LEVEL || !new_active);
        if stops_being_admin && self.store.count_active_admins()? <= 1 {
            return Err(PermissionError::Denied("cannot remove the last active administrator".into()).into());
        }
        let updated = self.store.update_user(
            badge_id,
            &UserUpdate { name: changes.name.clone(), level: changes.level, grants: changes.grants.clone(), revokes: changes.revokes.clone(), active: changes.active, ..Default::default() },
        )?;
        let mut diff = serde_json::Map::new();
        if changes.level.is_some() {
            diff.insert("level".into(), json!([target.level, updated.level]));
        }
        if changes.active.is_some() {
            diff.insert("active".into(), json!([target.active, updated.active]));
        }
        if changes.grants.is_some() {
            diff.insert("grants".into(), json!([target.grants, updated.grants]));
        }
        if changes.revokes.is_some() {
            diff.insert("revokes".into(), json!([target.revokes, updated.revokes]));
        }
        if changes.name.is_some() {
            diff.insert("name".into(), json!(["changed"]));
        }
        self.must("user_updated", json!({"actor": actor.badge(), "badge_id": badge_id, "changes": Value::Object(diff)}))?;
        Ok(updated)
    }

    pub fn unlock_user(&self, actor: &Principal, badge_id: &str) -> Result<(), ServiceError> {
        Self::require(actor, "users.manage")?;
        self.store.update_user(badge_id, &UserUpdate { failed_attempts: Some(0), locked_until: Some(None), ..Default::default() })?;
        self.must("user_unlocked", json!({"actor": actor.badge(), "badge_id": badge_id}))?;
        Ok(())
    }

    pub fn reset_totp(&self, actor: &Principal, badge_id: &str) -> Result<String, ServiceError> {
        Self::require(actor, "users.manage")?;
        let secret = totp::new_secret();
        let enc = totp::encrypt_secret(&secret, &self.settings.fernet_key).map_err(ServiceError::Invalid)?;
        self.store.update_user(badge_id, &UserUpdate { totp_secret_enc: Some(enc), ..Default::default() })?;
        self.must("totp_reset", json!({"actor": actor.badge(), "badge_id": badge_id}))?;
        Ok(totp::provisioning_uri(&secret, badge_id, "ClaimGuard"))
    }

    pub fn change_password(&self, principal: &Principal, old_password: &str, new_password: &str) -> Result<(), ServiceError> {
        let fresh = self.store.get_user(principal.badge())?;
        let matches = fresh.as_ref().is_some_and(|u| passwords::verify_password(old_password, &u.password_hash));
        if !matches {
            passwords::dummy_verify(old_password, self.settings.bcrypt_rounds);
            self.gated("login_failure", "bad_old_password", json!({"badge_attempt": principal.badge()}));
            return Err(AuthError::InvalidCredentials.into());
        }
        let problems = passwords::check_policy(new_password, principal.badge());
        if !problems.is_empty() {
            return Err(ServiceError::Invalid(problems.join("; ")));
        }
        if new_password == old_password {
            return Err(ServiceError::Invalid("the new password must be different".into()));
        }
        let hash = passwords::hash_password(new_password, self.settings.bcrypt_rounds).map_err(ServiceError::Invalid)?;
        self.store.update_user(principal.badge(), &UserUpdate { password_hash: Some(hash), must_change_password: Some(false), ..Default::default() })?;
        self.store.revoke_token(&principal.jti, self.now() + self.settings.token_ttl_seconds as f64)?;
        self.must("password_changed", json!({"badge_id": principal.badge()}))?;
        Ok(())
    }
}

#[cfg(test)]
pub mod testkit {
    //! A small world for tests here and in the API crate: users, a clock that can be moved, a security log in a temp dir.
    use super::*;
    use crate::store::MemoryStore;
    use std::sync::atomic::{AtomicU64, Ordering};

    pub struct World {
        pub svc: AccessService,
        pub store: Arc<MemoryStore>,
        pub clock: Arc<AtomicU64>,
        pub dir: tempfile::TempDir,
        pub seeds: Mutex<HashMap<String, String>>,
    }

    pub fn world() -> World {
        let clock = Arc::new(AtomicU64::new(1_700_000_000_000));
        let c2 = clock.clone();
        let clock_fn: Clock = Arc::new(move || c2.load(Ordering::SeqCst) as f64 / 1000.0);
        let dir = tempfile::tempdir().unwrap();
        let settings = crate::settings::test_settings();
        let log = Arc::new(SecurityLog::open(dir.path().join("sec.jsonl"), &settings.audit_anchor_key).unwrap());
        let store = Arc::new(MemoryStore::new());
        let svc = AccessService::new(store.clone(), settings, log, clock_fn);
        World { svc, store, clock, dir, seeds: Mutex::new(HashMap::new()) }
    }

    impl World {
        pub fn advance(&self, seconds: f64) {
            self.clock.fetch_add((seconds * 1000.0) as u64, Ordering::SeqCst);
        }

        pub fn provision(&self, badge: &str, level: i64, password: &str) -> User {
            let (user, uri) = self.svc.provision_user(badge, "Test User", password, level, "test", &[], &[], false).unwrap();
            let secret = uri.split("secret=").nth(1).unwrap().split('&').next().unwrap().to_string();
            self.seeds.lock().unwrap().insert(badge.to_string(), secret);
            user
        }

        /// The current code for this badge; moves the clock on one step first so every call gives a fresh code.
        pub fn code(&self, badge: &str) -> String {
            self.advance(31.0);
            let secret = self.seeds.lock().unwrap()[badge].clone();
            totp::code_at(&secret, totp::current_step(self.svc.now(), 30)).unwrap()
        }

        pub fn login(&self, badge: &str, password: &str) -> Result<Session, AuthError> {
            let code = self.code(badge);
            self.svc.login(badge, password, &code, "127.0.0.1")
        }

        pub fn principal(&self, badge: &str, password: &str) -> Principal {
            let s = self.login(badge, password).unwrap();
            self.svc.authenticate(&s.token).unwrap()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testkit::*;
    use super::*;

    const PW: &str = "Correct-Horse-9";

    fn events(w: &World) -> Vec<Value> {
        w.svc.log.scan(1000).unwrap().into_iter().map(|r| r["event"].clone()).collect()
    }

    fn kinds(w: &World) -> Vec<String> {
        events(w).iter().map(|e| e["event_type"].as_str().unwrap().to_string()).collect()
    }

    #[test]
    fn a_full_login_issues_a_session_and_authenticates() {
        let w = world();
        w.provision("CG-2002", 2, PW);
        let s = w.login("CG-2002", PW).unwrap();
        let p = w.svc.authenticate(&s.token).unwrap();
        assert_eq!(p.badge(), "CG-2002");
        assert!(p.permissions.contains("claims.decide") && !p.permissions.contains("audit.view"));
        assert_eq!(kinds(&w), ["login_success"]);
    }

    #[test]
    fn every_failure_looks_the_same_and_is_logged_with_its_true_reason() {
        let w = world();
        w.provision("CG-2002", 2, PW);
        let code = w.code("CG-2002");
        for (badge, pw, expect) in [("nope", PW, "malformed"), ("CG-9999", PW, "unknown_badge"), ("CG-2002", "wrong-Password-1", "bad_password"), ("CG-2002", "", "malformed")] {
            assert_eq!(w.svc.login(badge, pw, &code, "c").unwrap_err(), AuthError::InvalidCredentials);
            let last = events(&w).pop().unwrap();
            assert_eq!(last["reason"], expect, "{badge}");
        }
        assert_eq!(w.svc.login("CG-2002", PW, "000000", "c").unwrap_err(), AuthError::InvalidCredentials);
        assert_eq!(events(&w).pop().unwrap()["reason"], "bad_totp");
    }

    #[test]
    fn a_code_cannot_be_used_twice() {
        let w = world();
        w.provision("CG-2002", 2, PW);
        let code = w.code("CG-2002");
        assert!(w.svc.login("CG-2002", PW, &code, "c").is_ok());
        assert_eq!(w.svc.login("CG-2002", PW, &code, "c").unwrap_err(), AuthError::InvalidCredentials);
        assert_eq!(events(&w).pop().unwrap()["reason"], "replayed_totp");
    }

    #[test]
    fn repeated_failures_lock_the_account_even_for_the_right_password() {
        let w = world();
        w.provision("CG-2002", 2, PW);
        for _ in 0..w.svc.settings.max_failed_logins {
            let _ = w.svc.login("CG-2002", "wrong-Password-1", "000000", "c");
        }
        assert!(kinds(&w).contains(&"lockout".to_string()));
        assert!(w.login("CG-2002", PW).is_err(), "locked");
        assert_eq!(events(&w).pop().unwrap()["reason"], "locked");
        w.advance(w.svc.settings.lockout_seconds + 1.0);
        assert!(w.login("CG-2002", PW).is_ok(), "the lock ran out");
    }

    #[test]
    fn inactive_users_and_revoked_or_expired_sessions_are_refused() {
        let w = world();
        let admin = {
            w.provision("CG-4004", 4, PW);
            w.principal("CG-4004", PW)
        };
        w.provision("CG-2002", 2, PW);
        let s = w.login("CG-2002", PW).unwrap();
        w.svc.update_user(&admin, "CG-2002", &Changes { active: Some(false), ..Default::default() }).unwrap();
        assert_eq!(w.svc.authenticate(&s.token).unwrap_err(), AuthError::TokenInvalid, "deactivation applies at once");
        assert!(w.login("CG-2002", PW).is_err());
        w.svc.update_user(&admin, "CG-2002", &Changes { active: Some(true), ..Default::default() }).unwrap();
        let s = w.login("CG-2002", PW).unwrap();
        w.svc.logout(&s.token);
        assert_eq!(w.svc.authenticate(&s.token).unwrap_err(), AuthError::TokenInvalid, "revoked");
        let s = w.login("CG-2002", PW).unwrap();
        w.advance(w.svc.settings.token_ttl_seconds as f64 + 1.0);
        assert_eq!(w.svc.authenticate(&s.token).unwrap_err(), AuthError::TokenInvalid, "expired");
    }

    #[test]
    fn a_demotion_applies_to_a_live_session_immediately() {
        let w = world();
        w.provision("CG-4004", 4, PW);
        let admin = w.principal("CG-4004", PW);
        w.provision("CG-3003", 3, PW);
        let s = w.login("CG-3003", PW).unwrap();
        assert!(w.svc.authenticate(&s.token).unwrap().permissions.contains("claims.decide_high"));
        w.svc.update_user(&admin, "CG-3003", &Changes { level: Some(2), ..Default::default() }).unwrap();
        assert!(!w.svc.authenticate(&s.token).unwrap().permissions.contains("claims.decide_high"));
    }

    #[test]
    fn only_administrators_manage_users_and_never_the_last_one_away() {
        let w = world();
        w.provision("CG-4004", 4, PW);
        w.provision("CG-3003", 3, PW);
        let admin = w.principal("CG-4004", PW);
        let senior = w.principal("CG-3003", PW);
        assert!(matches!(w.svc.list_users(&senior), Err(ServiceError::Forbidden(_))));
        assert!(matches!(w.svc.update_user(&admin, "CG-4004", &Changes { active: Some(false), ..Default::default() }), Err(ServiceError::Permission(_))), "not yourself");
        assert!(matches!(w.svc.update_user(&admin, "CG-4004", &Changes { level: Some(3), ..Default::default() }), Err(ServiceError::Permission(_))));
        w.provision("CG-4005", 4, PW);
        let other = w.principal("CG-4005", PW);
        w.svc.update_user(&other, "CG-4004", &Changes { active: Some(false), ..Default::default() }).unwrap();
        assert!(matches!(w.svc.update_user(&other, "CG-4004", &Changes::default()), Err(ServiceError::Invalid(_))), "empty change refused");
        assert!(kinds(&w).contains(&"user_updated".to_string()));
    }

    #[test]
    fn creating_users_checks_policy_badge_and_duplicates_and_logs() {
        let w = world();
        w.provision("CG-4004", 4, PW);
        let admin = w.principal("CG-4004", PW);
        assert!(matches!(w.svc.create_user(&admin, "bad", "N", PW, 2, &[], &[]), Err(ServiceError::Invalid(_))));
        assert!(matches!(w.svc.create_user(&admin, "CG-5005", "N", "short", 2, &[], &[]), Err(ServiceError::Invalid(_))));
        assert!(matches!(w.svc.create_user(&admin, "CG-5005", "N", PW, 5, &[], &[]), Err(ServiceError::Permission(_)) | Err(ServiceError::Invalid(_))));
        let (user, uri) = w.svc.create_user(&admin, "CG-5005", "New Person", PW, 2, &[], &[]).unwrap();
        assert!(user.must_change_password && uri.starts_with("otpauth://totp/ClaimGuard:CG-5005?secret="));
        assert!(matches!(w.svc.create_user(&admin, "CG-5005", "Again", PW, 2, &[], &[]), Err(ServiceError::Duplicate)));
        assert!(kinds(&w).contains(&"user_created".to_string()));
    }

    #[test]
    fn changing_a_password_revokes_the_session_and_enforces_policy() {
        let w = world();
        w.provision("CG-2002", 2, PW);
        let s = w.login("CG-2002", PW).unwrap();
        let p = w.svc.authenticate(&s.token).unwrap();
        assert!(matches!(w.svc.change_password(&p, "wrong-Password-1", "Another-Good-9x"), Err(ServiceError::Auth(AuthError::InvalidCredentials))));
        assert!(matches!(w.svc.change_password(&p, PW, PW), Err(ServiceError::Invalid(_))));
        assert!(matches!(w.svc.change_password(&p, PW, "short"), Err(ServiceError::Invalid(_))));
        w.svc.change_password(&p, PW, "Another-Good-9x").unwrap();
        assert_eq!(w.svc.authenticate(&s.token).unwrap_err(), AuthError::TokenInvalid);
        assert!(w.login("CG-2002", PW).is_err() && w.login("CG-2002", "Another-Good-9x").is_ok());
    }

    #[test]
    fn a_flood_of_failures_cannot_fill_the_log() {
        let w = world();
        for _ in 0..(AUDIT_GATE_LIMIT + 50) {
            let _ = w.svc.login("CG-9999", "x", "000000", "c");
        }
        let n = events(&w).len() as u32;
        assert!(n <= AUDIT_GATE_LIMIT, "{n}");
        w.advance(AUDIT_GATE_WINDOW + 1.0);
        let _ = w.svc.login("CG-9999", "x", "000000", "c");
        let evs = events(&w);
        assert!(evs.iter().any(|e| e["reason"].as_str().is_some_and(|r| r.starts_with("suppressed_"))), "the gap is summarised");
    }

    #[test]
    fn totp_reset_and_unlock_need_the_permission_and_are_logged() {
        let w = world();
        w.provision("CG-4004", 4, PW);
        w.provision("CG-2002", 2, PW);
        let admin = w.principal("CG-4004", PW);
        let user = w.principal("CG-2002", PW);
        assert!(w.svc.reset_totp(&user, "CG-2002").is_err() && w.svc.unlock_user(&user, "CG-2002").is_err());
        assert!(w.svc.reset_totp(&admin, "CG-2002").unwrap().contains("secret="));
        w.svc.unlock_user(&admin, "CG-2002").unwrap();
        assert!(matches!(w.svc.unlock_user(&admin, "CG-0000"), Err(ServiceError::NotFound)));
        let k = kinds(&w);
        assert!(k.contains(&"totp_reset".to_string()) && k.contains(&"user_unlocked".to_string()));
    }
}
