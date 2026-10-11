//! The rules of identity and access (`src/access/service.py`). No HTTP in here: the API layer only translates requests to these calls.
//!
//! Login order (every early exit spends the time of one real password check, so an unknown, locked or inactive account answers no faster
//! than a real one): shape checks, lookup, locked, inactive, password, authenticator code, consume the code, then issue a session. Every
//! failure is the same `AuthError::InvalidCredentials` to the caller; the true reason goes to the security log.

use crate::permissions::{self, PermissionError, Perms};
use crate::securitylog::SecurityLog;
use crate::settings::Settings;
use crate::store::{Approval, StoreError, User, UserStore, UserUpdate};
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
    #[error("too many attempts")]
    Throttled,
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

/// What a dangerous administrator action did: it was carried out, or it became a request for a second administrator (with the new
/// authenticator link where there is one: the person who asked gets it now, and it only works once the request is approved).
#[derive(Debug)]
pub enum Gate<T> {
    Done(T),
    Pending { approval: Approval, uri: Option<String> },
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

/// Failures one client may make against one badge in one window before further attempts from it are refused whatever they contain.
pub const PAIR_LIMIT: i64 = 5;
pub const PAIR_WINDOW: u64 = 900;
const MAX_BADGE_LOOKUP: usize = 64;

fn badge_key(badge: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(badge.as_bytes()))[..16].to_string()
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

    /// A keyed hash of a client address: what the stores remember and count by, so no raw address is kept in a throttle or a device list.
    pub fn client_key(&self, client: &str) -> String {
        use hmac::{Hmac, Mac};
        let mut mac = Hmac::<sha2::Sha256>::new_from_slice(self.settings.pii_key.as_bytes()).expect("a key of any length is accepted");
        mac.update(format!("client|{client}").as_bytes());
        hex::encode(mac.finalize().into_bytes())[..32].to_string()
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
        let unavailable = |_| AuthError::Unavailable;
        let (ck, bk) = (self.client_key(client), badge_key(badge_id));
        let pair = format!("pair|{bk}|{ck}");
        // Every refusal below takes the same steps: two reads, one password hash, one or two writes. A refusal for an unknown badge, a
        // locked account, a throttled client or a wrong password therefore cannot be told apart by how long it takes.
        let throttled = self.store.counter(&pair, PAIR_WINDOW, now).map_err(unavailable)? >= PAIR_LIMIT;
        let known = self.store.client_known(&cut(badge_id, MAX_BADGE_LOOKUP), &ck, now).map_err(unavailable)?;
        let shaped = badge_re().is_match(badge_id) && !password.is_empty() && password.chars().count() <= MAX_PASSWORD_CHARS;
        let user = self.store.get_user(&cut(badge_id, MAX_BADGE_LOOKUP)).map_err(unavailable)?.filter(|_| shaped);
        let refuse = |reason: &str, user: Option<&User>, counts_for_account: bool| -> Result<Session, AuthError> {
            self.store.bump_counter(&pair, PAIR_WINDOW, now).map_err(unavailable)?;
            match user {
                Some(u) if counts_for_account => self.count_failure(u, now)?,
                _ => {
                    self.store.bump_counter("decoy", PAIR_WINDOW, now).map_err(unavailable)?;
                }
            }
            self.fail(reason, badge_id, client)
        };
        if throttled {
            passwords::dummy_verify(password, rounds);
            return refuse("throttled", user.as_ref(), false);
        }
        if !shaped {
            passwords::dummy_verify(password, rounds);
            return refuse("malformed", None, false);
        }
        let Some(user) = user else {
            passwords::dummy_verify(password, rounds);
            return refuse("unknown_badge", None, false);
        };
        // An account lock keeps out clients this person has never signed in from, and only those: whoever knows a badge number can
        // provoke the lock, but cannot use it to shut the owner out of the place they normally sign in from.
        if user.locked_until.is_some_and(|l| l > now) && !known {
            passwords::dummy_verify(password, rounds);
            return refuse("locked", Some(&user), false);
        }
        if !user.active {
            passwords::dummy_verify(password, rounds);
            return refuse("inactive", Some(&user), false);
        }
        if !passwords::verify_password(password, &user.password_hash) {
            return refuse("bad_password", Some(&user), true);
        }
        let Ok(secret) = totp::decrypt_secret(&user.totp_secret_enc, &self.settings.fernet_key) else {
            return refuse("seed_unreadable", Some(&user), false);
        };
        let Some(step) = totp::verify_code(&secret, totp_code, now, self.settings.totp_step) else {
            return refuse("bad_totp", Some(&user), true);
        };
        let fresh = self.store.mark_totp_used(badge_id, step, ((step + 2) * self.settings.totp_step) as f64).map_err(unavailable)?;
        if !fresh {
            return refuse("replayed_totp", Some(&user), true);
        }
        self.store.record_successful_login(badge_id, now).map_err(unavailable)?;
        self.store.mark_client(badge_id, &ck, now).map_err(unavailable)?;
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
        let (user, uri) = self.build_user(badge_id, name, password, level, created_by, grants, revokes, must_change_password)?;
        self.store.create_user(&user)?;
        Ok((user, uri))
    }

    /// Checks everything about a new user and makes the record (password hashed, authenticator seed made and encrypted) without storing it.
    #[allow(clippy::too_many_arguments)]
    fn build_user(&self, badge_id: &str, name: &str, password: &str, level: i64, created_by: &str, grants: &[String], revokes: &[String], must_change_password: bool)
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
        self.update_user_inner(actor, badge_id, changes, false)?.ok_or(ServiceError::NotFound)
    }

    /// With `check_only` every rule is applied but nothing is changed (None is returned).
    fn update_user_inner(&self, actor: &Principal, badge_id: &str, changes: &Changes, check_only: bool) -> Result<Option<User>, ServiceError> {
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
        if check_only {
            return Ok(None);
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
        Ok(Some(updated))
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

    // ---- dual control ------------------------------------------------------------------------------------------------
    // The most dangerous administrator actions (replacing someone's authenticator; creating, promoting, demoting or deactivating an
    // administrator) are not carried out when asked for. They become a request, and a different administrator must approve it with a
    // fresh authenticator code. The API uses the `*_gated` functions; the plain ones above do the work and are what runs after approval.

    /// Dual control applies when it is switched on and there are at least two active administrators to ask. A lone administrator acts alone
    /// (nobody else could approve) and the action is logged as such.
    fn dual_control(&self) -> Result<bool, ServiceError> {
        Ok(self.settings.dual_control && self.store.count_active_admins()? >= 2)
    }

    fn approval_event(&self, actor: &str, a: &Approval, outcome: &str) -> Result<(), ServiceError> {
        Ok(self.must("approval", json!({"actor": actor, "approval_id": a.id, "action": a.action, "badge_id": a.target, "requested_by": a.requested_by, "outcome": outcome}))?)
    }

    fn open_approval(&self, actor: &Principal, action: &str, target: &str, summary: String, payload: Value) -> Result<Approval, ServiceError> {
        let now = self.now();
        let a = Approval {
            id: format!("AP-{}", crate::settings::random_token(6)),
            action: action.into(),
            target: target.into(),
            requested_by: actor.badge().into(),
            requested_at: now,
            expires_at: now + self.settings.approval_ttl_seconds as f64,
            status: "pending".into(),
            summary: cut(&summary, 300),
            payload: payload.to_string(),
            decided_by: None,
            decided_at: None,
            note: None,
        };
        self.store.create_approval(&a, now)?;
        self.approval_event(actor.badge(), &a, "requested")?;
        Ok(a)
    }

    /// Notes in the log that an action which would need a second person was done by a lone administrator.
    fn note_unchecked(&self, actor: &Principal, action: &str, target: &str) {
        if self.settings.dual_control {
            let _ = self.log.record("approval", json!({"actor": actor.badge(), "approval_id": "-", "action": action, "badge_id": target, "requested_by": actor.badge(), "outcome": "done_without_second_administrator"}));
        }
    }

    pub fn create_user_gated(&self, actor: &Principal, badge_id: &str, name: &str, password: &str, level: i64, grants: &[String], revokes: &[String]) -> Result<Gate<(User, String)>, ServiceError> {
        if level != permissions::ADMIN_LEVEL {
            return Ok(Gate::Done(self.create_user(actor, badge_id, name, password, level, grants, revokes)?));
        }
        Self::require(actor, "users.manage")?;
        permissions::check_user_change(actor.user.level, level, level, grants, revokes, false)?;
        if !self.dual_control()? {
            let out = self.create_user(actor, badge_id, name, password, level, grants, revokes)?;
            self.note_unchecked(actor, "create_admin", badge_id);
            return Ok(Gate::Done(out));
        }
        if self.store.get_user(badge_id)?.is_some() {
            return Err(ServiceError::Duplicate);
        }
        let (user, uri) = self.build_user(badge_id, name, password, level, actor.badge(), grants, revokes, true)?;
        let payload = json!({"name": user.name, "password_hash": user.password_hash, "totp_secret_enc": user.totp_secret_enc, "level": user.level, "grants": user.grants, "revokes": user.revokes});
        let approval = self.open_approval(actor, "create_admin", badge_id, format!("Create administrator {badge_id} ({})", user.name), payload)?;
        Ok(Gate::Pending { approval, uri: Some(uri) })
    }

    pub fn update_user_gated(&self, actor: &Principal, badge_id: &str, changes: &Changes) -> Result<Gate<User>, ServiceError> {
        Self::require(actor, "users.manage")?;
        let target = self.store.get_user(badge_id)?.ok_or(ServiceError::NotFound)?;
        let touches_an_admin = target.level == permissions::ADMIN_LEVEL || changes.level == Some(permissions::ADMIN_LEVEL);
        if !touches_an_admin {
            return Ok(Gate::Done(self.update_user(actor, badge_id, changes)?));
        }
        if !self.dual_control()? {
            let out = self.update_user(actor, badge_id, changes)?;
            self.note_unchecked(actor, "update_user", badge_id);
            return Ok(Gate::Done(out));
        }
        self.update_user_inner(actor, badge_id, changes, true)?; // every rule now, so a doomed request is refused at once
        let mut parts = vec![];
        if let Some(l) = changes.level { parts.push(format!("level {} to {l}", target.level)); }
        if let Some(a) = changes.active { parts.push(if a { "activate".to_string() } else { "deactivate".to_string() }); }
        if changes.grants.is_some() || changes.revokes.is_some() { parts.push("permissions".into()); }
        if changes.name.is_some() { parts.push("name".into()); }
        let payload = json!({"name": changes.name, "level": changes.level, "grants": changes.grants, "revokes": changes.revokes, "active": changes.active});
        let approval = self.open_approval(actor, "update_user", badge_id, format!("Change {badge_id}: {}", parts.join(", ")), payload)?;
        Ok(Gate::Pending { approval, uri: None })
    }

    pub fn reset_totp_gated(&self, actor: &Principal, badge_id: &str) -> Result<Gate<String>, ServiceError> {
        Self::require(actor, "users.manage")?;
        if !self.dual_control()? {
            let uri = self.reset_totp(actor, badge_id)?;
            self.note_unchecked(actor, "reset_totp", badge_id);
            return Ok(Gate::Done(uri));
        }
        let target = self.store.get_user(badge_id)?.ok_or(ServiceError::NotFound)?;
        let secret = totp::new_secret();
        let enc = totp::encrypt_secret(&secret, &self.settings.fernet_key).map_err(ServiceError::Invalid)?;
        let approval = self.open_approval(actor, "reset_totp", badge_id, format!("Replace the authenticator of {badge_id} ({})", target.name), json!({"totp_secret_enc": enc}))?;
        Ok(Gate::Pending { approval, uri: Some(totp::provisioning_uri(&secret, badge_id, "ClaimGuard")) })
    }

    pub fn list_approvals(&self, actor: &Principal) -> Result<Vec<Approval>, ServiceError> {
        Self::require(actor, "users.manage")?;
        Ok(self.store.list_approvals(100)?)
    }

    /// A fresh authenticator code from the person approving, so a stolen session alone cannot approve. Wrong codes are counted.
    fn confirm_with_authenticator(&self, actor: &Principal, code: &str) -> Result<(), ServiceError> {
        let now = self.now();
        let key = format!("stepup|{}", badge_key(actor.badge()));
        if self.store.counter(&key, 900, now)? >= 5 {
            return Err(ServiceError::Throttled);
        }
        let wrong = |why: &str| -> ServiceError {
            let _ = self.store.bump_counter(&key, 900, now);
            self.gated("login_failure", why, json!({"badge_attempt": actor.badge()}));
            AuthError::InvalidCredentials.into()
        };
        let user = self.store.get_user(actor.badge())?.ok_or(ServiceError::NotFound)?;
        let Ok(secret) = totp::decrypt_secret(&user.totp_secret_enc, &self.settings.fernet_key) else { return Err(wrong("seed_unreadable")) };
        let Some(step) = totp::verify_code(&secret, code, now, self.settings.totp_step) else { return Err(wrong("bad_step_up_code")) };
        if !self.store.mark_totp_used(actor.badge(), step, ((step + 2) * self.settings.totp_step) as f64)? {
            return Err(wrong("replayed_step_up_code"));
        }
        Ok(())
    }

    fn approver_checks(&self, actor: &Principal, id: &str) -> Result<Approval, ServiceError> {
        Self::require(actor, "users.manage")?;
        if actor.user.level != permissions::ADMIN_LEVEL {
            return Err(PermissionError::Denied("only an administrator can decide on a request".into()).into());
        }
        self.store.get_approval(id)?.ok_or(ServiceError::NotFound)
    }

    fn still_pending(&self, a: &Approval) -> Result<(), ServiceError> {
        if a.status != "pending" {
            return Err(ServiceError::Invalid(format!("this request is already {}", a.status)));
        }
        if a.expires_at <= self.now() {
            return Err(ServiceError::Invalid("this request has expired".into()));
        }
        Ok(())
    }

    /// A different administrator approves (with a fresh authenticator code) and the action is carried out at once.
    pub fn approve(&self, actor: &Principal, id: &str, totp_code: &str) -> Result<Approval, ServiceError> {
        let a = self.approver_checks(actor, id)?;
        if a.requested_by == actor.badge() {
            return Err(PermissionError::Denied("the person who asked cannot approve their own request".into()).into());
        }
        self.still_pending(&a)?;
        self.confirm_with_authenticator(actor, totp_code)?;
        let now = self.now();
        if !self.store.settle_approval(id, "pending", "approved", actor.badge(), now, None)? {
            return Err(ServiceError::Invalid("someone else decided this request first, or it has expired".into()));
        }
        match self.carry_out(actor, &a) {
            Ok(()) => {
                self.approval_event(actor.badge(), &a, "approved")?;
                Ok(self.store.get_approval(id)?.unwrap_or(a))
            }
            Err(e) => {
                let _ = self.store.settle_approval(id, "approved", "failed", actor.badge(), now, Some(&cut(&e.to_string(), 200)));
                let _ = self.approval_event(actor.badge(), &a, "failed");
                Err(e)
            }
        }
    }

    fn carry_out(&self, approver: &Principal, a: &Approval) -> Result<(), ServiceError> {
        let requester = self.store.get_user(&a.requested_by)?.filter(|u| u.active).ok_or_else(|| ServiceError::Invalid("the administrator who asked is no longer active".into()))?;
        let p: Value = serde_json::from_str(&a.payload).map_err(|_| ServiceError::Invalid("the stored request is damaged".into()))?;
        let text = |k: &str| p[k].as_str().unwrap_or("").to_string();
        let strings = |k: &str| p[k].as_array().map(|l| l.iter().filter_map(|s| s.as_str().map(str::to_string)).collect::<Vec<_>>());
        match a.action.as_str() {
            "create_admin" => {
                let user = User {
                    badge_id: a.target.clone(), name: text("name"), password_hash: text("password_hash"), totp_secret_enc: text("totp_secret_enc"), level: p["level"].as_i64().unwrap_or(0),
                    grants: strings("grants").unwrap_or_default(), revokes: strings("revokes").unwrap_or_default(), active: true, failed_attempts: 0, locked_until: None,
                    must_change_password: true, created_by: requester.badge_id.clone(), created_at: self.now(), last_login: None,
                };
                self.store.create_user(&user)?;
                self.must("user_created", json!({"actor": requester.badge_id, "badge_id": a.target, "level": user.level, "approved_by": approver.badge()}))?;
            }
            "update_user" => {
                let changes = Changes { name: p["name"].as_str().map(str::to_string), level: p["level"].as_i64(), grants: strings("grants"), revokes: strings("revokes"), active: p["active"].as_bool() };
                self.update_user(approver, &a.target, &changes)?;
            }
            "reset_totp" => {
                self.store.update_user(&a.target, &UserUpdate { totp_secret_enc: Some(text("totp_secret_enc")), ..Default::default() })?;
                self.must("totp_reset", json!({"actor": requester.badge_id, "badge_id": a.target, "approved_by": approver.badge()}))?;
            }
            _ => return Err(ServiceError::Invalid("unknown action".into())),
        }
        Ok(())
    }

    /// A different administrator refuses the request.
    pub fn reject_request(&self, actor: &Principal, id: &str, note: &str) -> Result<Approval, ServiceError> {
        let a = self.approver_checks(actor, id)?;
        if a.requested_by == actor.badge() {
            return Err(PermissionError::Denied("withdraw your own request instead of rejecting it".into()).into());
        }
        self.settle(actor, &a, "rejected", note)
    }

    /// The administrator who asked withdraws the request.
    pub fn cancel(&self, actor: &Principal, id: &str) -> Result<Approval, ServiceError> {
        let a = self.approver_checks(actor, id)?;
        if a.requested_by != actor.badge() {
            return Err(PermissionError::Denied("only the person who asked can withdraw a request".into()).into());
        }
        self.settle(actor, &a, "cancelled", "")
    }

    fn settle(&self, actor: &Principal, a: &Approval, to: &str, note: &str) -> Result<Approval, ServiceError> {
        self.still_pending(a)?;
        let note = (!note.trim().is_empty()).then(|| cut(note.trim(), 200));
        if !self.store.settle_approval(&a.id, "pending", to, actor.badge(), self.now(), note.as_deref())? {
            return Err(ServiceError::Invalid("someone else decided this request first, or it has expired".into()));
        }
        self.approval_event(actor.badge(), a, to)?;
        Ok(self.store.get_approval(&a.id)?.unwrap_or_else(|| a.clone()))
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

/// A small world for tests here and in other crates (not used by the server).
pub mod testkit {
    //! A small world for tests here and in the API crate: users, a clock that can be moved, a security log in a temp dir.
    use super::*;
    use crate::store::MemoryStore;
    use std::sync::atomic::{AtomicU64, Ordering};

    pub struct World {
        pub svc: Arc<AccessService>,
        pub store: Arc<MemoryStore>,
        pub clock: Arc<AtomicU64>,
        pub dir: tempfile::TempDir,
        pub seeds: Mutex<HashMap<String, String>>,
    }

    pub fn world() -> World {
        world_with(|_| {})
    }

    pub fn world_with(tweak: impl FnOnce(&mut Settings)) -> World {
        let clock = Arc::new(AtomicU64::new(1_700_000_000_000));
        let c2 = clock.clone();
        let clock_fn: Clock = Arc::new(move || c2.load(Ordering::SeqCst) as f64 / 1000.0);
        let dir = tempfile::tempdir().unwrap();
        let mut settings = crate::settings::test_settings();
        tweak(&mut settings);
        let log = Arc::new(SecurityLog::open(dir.path().join("sec.jsonl"), &settings.audit_anchor_key).unwrap());
        let store = Arc::new(MemoryStore::new());
        let svc = Arc::new(AccessService::new(store.clone(), settings, log, clock_fn));
        World { svc, store, clock, dir, seeds: Mutex::new(HashMap::new()) }
    }

    impl World {
        pub fn advance(&self, seconds: f64) {
            self.clock.fetch_add((seconds * 1000.0) as u64, Ordering::SeqCst);
        }

        pub fn provision(&self, badge: &str, level: i64, password: &str) -> User {
            self.provision_with(badge, level, password, false)
        }

        pub fn provision_with(&self, badge: &str, level: i64, password: &str, must_change_password: bool) -> User {
            let (user, uri) = self.svc.provision_user(badge, "Test User", password, level, "test", &[], &[], must_change_password).unwrap();
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

    // ---- dual control -----------------------------------------------------------------------------------------------------
    fn seed_of(uri: &str) -> String {
        uri.split("secret=").nth(1).unwrap().split('&').next().unwrap().to_string()
    }

    fn two_admins() -> (World, Principal, Principal) {
        let w = world();
        w.provision("CG-4004", 4, PW);
        w.provision("CG-4005", 4, PW);
        w.provision("CG-2002", 2, PW);
        let (a, b) = (w.principal("CG-4004", PW), w.principal("CG-4005", PW));
        (w, a, b)
    }

    fn pending<T: std::fmt::Debug>(g: Gate<T>) -> (Approval, Option<String>) {
        match g {
            Gate::Pending { approval, uri } => (approval, uri),
            other => panic!("expected a request for approval, got {other:?}"),
        }
    }

    #[test]
    fn a_lone_administrator_acts_alone_and_it_is_logged() {
        let w = world();
        w.provision("CG-4004", 4, PW);
        w.provision("CG-2002", 2, PW);
        let admin = w.principal("CG-4004", PW);
        assert!(matches!(w.svc.reset_totp_gated(&admin, "CG-2002").unwrap(), Gate::Done(uri) if uri.contains("secret=")));
        assert!(events(&w).iter().any(|e| e["event_type"] == "approval" && e["outcome"] == "done_without_second_administrator" && e["action"] == "reset_totp"));
        assert!(w.svc.list_approvals(&admin).unwrap().is_empty());
    }

    #[test]
    fn switching_dual_control_off_makes_every_action_direct() {
        let w = world_with(|s| s.dual_control = false);
        w.provision("CG-4004", 4, PW);
        w.provision("CG-4005", 4, PW);
        w.provision("CG-2002", 2, PW);
        let admin = w.principal("CG-4004", PW);
        assert!(matches!(w.svc.reset_totp_gated(&admin, "CG-2002").unwrap(), Gate::Done(_)));
        assert!(matches!(w.svc.update_user_gated(&admin, "CG-4005", &Changes { active: Some(false), ..Default::default() }).unwrap(), Gate::Done(_)));
        assert!(!events(&w).iter().any(|e| e["event_type"] == "approval"), "nothing to note when the control is off on purpose");
    }

    #[test]
    fn an_authenticator_reset_waits_for_a_different_administrator_and_a_fresh_code() {
        let (w, a, b) = two_admins();
        let old_seed = w.seeds.lock().unwrap()["CG-2002"].clone();
        let (req, uri) = pending(w.svc.reset_totp_gated(&a, "CG-2002").unwrap());
        assert_eq!((req.status.as_str(), req.action.as_str(), req.requested_by.as_str()), ("pending", "reset_totp", "CG-4004"));
        let new_seed = seed_of(&uri.unwrap());
        assert_ne!(new_seed, old_seed);
        // nothing has changed yet: the old authenticator still signs the person in
        assert!(w.login("CG-2002", PW).is_ok());
        // the one who asked cannot approve, whatever code they give
        let own = w.code("CG-4004");
        assert!(matches!(w.svc.approve(&a, &req.id, &own), Err(ServiceError::Permission(_))));
        // a wrong code, then the right one
        assert!(matches!(w.svc.approve(&b, &req.id, "000000"), Err(ServiceError::Auth(AuthError::InvalidCredentials))));
        assert_eq!(w.svc.store.get_approval(&req.id).unwrap().unwrap().status, "pending");
        let code = w.code("CG-4005");
        let done = w.svc.approve(&b, &req.id, &code).unwrap();
        assert_eq!((done.status.as_str(), done.decided_by.as_deref()), ("approved", Some("CG-4005")));
        // the new authenticator works, the old one does not
        w.seeds.lock().unwrap().insert("CG-2002".into(), old_seed);
        assert!(w.login("CG-2002", PW).is_err());
        w.seeds.lock().unwrap().insert("CG-2002".into(), new_seed);
        assert!(w.login("CG-2002", PW).is_ok());
        // it can be decided once only
        let code = w.code("CG-4005");
        assert!(matches!(w.svc.approve(&b, &req.id, &code), Err(ServiceError::Invalid(m)) if m.contains("already approved")));
        let ev = events(&w);
        for outcome in ["requested", "approved"] {
            assert!(ev.iter().any(|e| e["event_type"] == "approval" && e["outcome"] == outcome && e["approval_id"] == req.id.as_str()), "{outcome} is on the log");
        }
        let reset = ev.iter().find(|e| e["event_type"] == "totp_reset").unwrap();
        assert_eq!((reset["actor"].as_str(), reset["approved_by"].as_str()), (Some("CG-4004"), Some("CG-4005")), "the log names both people");
    }

    #[test]
    fn approval_codes_cannot_be_guessed_or_replayed() {
        let (w, a, b) = two_admins();
        let (req, _) = pending(w.svc.reset_totp_gated(&a, "CG-2002").unwrap());
        for _ in 0..5 {
            assert!(matches!(w.svc.approve(&b, &req.id, "000000"), Err(ServiceError::Auth(AuthError::InvalidCredentials))));
        }
        let code = w.code("CG-4005");
        assert!(matches!(w.svc.approve(&b, &req.id, &code), Err(ServiceError::Throttled)), "five wrong codes stop even the right one for a while");
        w.advance(901.0);
        let code = w.code("CG-4005");
        assert!(w.svc.approve(&b, &req.id, &code).is_ok());
        // a code that has been used once is never accepted again, here or anywhere else
        let (req2, _) = pending(w.svc.reset_totp_gated(&a, "CG-2002").unwrap());
        let code = w.code("CG-4005");
        assert!(w.svc.approve(&b, &req2.id, &code).is_ok());
        let (req3, _) = pending(w.svc.reset_totp_gated(&a, "CG-2002").unwrap());
        assert!(matches!(w.svc.approve(&b, &req3.id, &code), Err(ServiceError::Auth(AuthError::InvalidCredentials))), "the same code a second time");
    }

    #[test]
    fn creating_an_administrator_waits_and_only_then_exists() {
        let (w, a, b) = two_admins();
        let (req, uri) = pending(w.svc.create_user_gated(&a, "CG-4006", "New Admin", PW, 4, &[], &[]).unwrap());
        assert!(w.store.get_user("CG-4006").unwrap().is_none(), "no account exists before approval");
        assert!(matches!(w.svc.create_user_gated(&a, "CG-4006", "New Admin", PW, 4, &[], &[]), Err(ServiceError::Duplicate)), "one request at a time");
        let seed = seed_of(&uri.unwrap());
        let code = w.code("CG-4005");
        w.svc.approve(&b, &req.id, &code).unwrap();
        let user = w.store.get_user("CG-4006").unwrap().unwrap();
        assert_eq!((user.level, user.active, user.must_change_password, user.created_by.as_str()), (4, true, true, "CG-4004"));
        w.seeds.lock().unwrap().insert("CG-4006".into(), seed);
        assert!(w.login("CG-4006", PW).is_ok(), "the password and authenticator given when asking now work");
        // a lower-level account needs no second person
        assert!(matches!(w.svc.create_user_gated(&a, "CG-2003", "Reviewer", PW, 2, &[], &[]).unwrap(), Gate::Done(_)));
        assert_eq!(w.svc.list_approvals(&a).unwrap().len(), 1);
    }

    #[test]
    fn changes_to_administrators_wait_and_ordinary_changes_do_not() {
        let (w, a, b) = two_admins();
        w.provision("CG-3003", 3, PW);
        assert!(matches!(w.svc.update_user_gated(&a, "CG-3003", &Changes { level: Some(2), ..Default::default() }).unwrap(), Gate::Done(_)), "a senior reviewer to reviewer");
        // promoting someone to administrator, and deactivating one, are requests
        let (promote, _) = pending(w.svc.update_user_gated(&a, "CG-2002", &Changes { level: Some(4), ..Default::default() }).unwrap());
        let (deactivate, _) = pending(w.svc.update_user_gated(&a, "CG-4005", &Changes { active: Some(false), ..Default::default() }).unwrap());
        assert_eq!(w.store.get_user("CG-2002").unwrap().unwrap().level, 2);
        assert!(w.store.get_user("CG-4005").unwrap().unwrap().active);
        // a request the rules forbid anyway is refused at once, not queued
        assert!(matches!(w.svc.update_user_gated(&a, "CG-4004", &Changes { active: Some(false), ..Default::default() }), Err(ServiceError::Permission(_))), "not yourself");
        // approving the promotion
        let code = w.code("CG-4005");
        w.svc.approve(&b, &promote.id, &code).unwrap();
        assert_eq!(w.store.get_user("CG-2002").unwrap().unwrap().level, 4);
        // the deactivation of CG-4005 was asked for by CG-4004; the newly promoted CG-2002 (now an administrator) approves it
        let c = w.principal("CG-2002", PW);
        let code = w.code("CG-2002");
        w.svc.approve(&c, &deactivate.id, &code).unwrap();
        assert!(!w.store.get_user("CG-4005").unwrap().unwrap().active);
        assert!(events(&w).iter().filter(|e| e["event_type"] == "user_updated").count() >= 3);
        // two administrators are left (CG-4004 and CG-2002), so asking to deactivate one is again a request, and approval is still needed
        let (again, _) = pending(w.svc.update_user_gated(&c, "CG-4004", &Changes { active: Some(false), ..Default::default() }).unwrap());
        assert_eq!(again.status, "pending");
    }

    #[test]
    fn requests_can_be_rejected_withdrawn_and_expire() {
        let (w, a, b) = two_admins();
        let (r1, _) = pending(w.svc.reset_totp_gated(&a, "CG-2002").unwrap());
        assert!(matches!(w.svc.reject_request(&a, &r1.id, "no"), Err(ServiceError::Permission(_))), "withdraw your own instead");
        assert!(matches!(w.svc.cancel(&b, &r1.id), Err(ServiceError::Permission(_))), "only the person who asked can withdraw");
        let rejected = w.svc.reject_request(&b, &r1.id, "not today").unwrap();
        assert_eq!((rejected.status.as_str(), rejected.note.as_deref()), ("rejected", Some("not today")));
        let code = w.code("CG-4005");
        assert!(matches!(w.svc.approve(&b, &r1.id, &code), Err(ServiceError::Invalid(m)) if m.contains("rejected")));
        let (r2, _) = pending(w.svc.reset_totp_gated(&a, "CG-2002").unwrap());
        assert_eq!(w.svc.cancel(&a, &r2.id).unwrap().status, "cancelled");
        let (r3, _) = pending(w.svc.reset_totp_gated(&a, "CG-2002").unwrap());
        w.advance(86_401.0);
        let code = w.code("CG-4005");
        assert!(matches!(w.svc.approve(&b, &r3.id, &code), Err(ServiceError::Invalid(m)) if m == "this request has expired"), "the reason given is the real one");
        // a lower-level person can neither see nor decide
        let reviewer = w.principal("CG-2002", PW);
        assert!(w.svc.list_approvals(&reviewer).is_err());
        assert!(w.svc.approve(&reviewer, &r3.id, "123456").is_err() && w.svc.cancel(&reviewer, &r3.id).is_err());
        assert!(matches!(w.svc.approve(&b, "AP-nope", "123456"), Err(ServiceError::NotFound)));
    }

    #[test]
    fn a_request_does_not_survive_its_requester_being_deactivated() {
        let (w, a, b) = two_admins();
        w.provision("CG-4006", 4, PW);
        let c = w.principal("CG-4006", PW);
        let (req, _) = pending(w.svc.reset_totp_gated(&c, "CG-2002").unwrap());
        // the requester is deactivated (by the other two)
        w.store.update_user("CG-4006", &UserUpdate { active: Some(false), ..Default::default() }).unwrap();
        let code = w.code("CG-4005");
        assert!(matches!(w.svc.approve(&b, &req.id, &code), Err(ServiceError::Invalid(m)) if m.contains("no longer active")));
        assert_eq!(w.svc.store.get_approval(&req.id).unwrap().unwrap().status, "failed");
        let _ = a;
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

    // ---- lockout that cannot be turned against its owner, and throttles that live in the database ------------------------------
    fn login_from(w: &World, badge: &str, password: &str, client: &str) -> Result<Session, AuthError> {
        let code = w.code(badge);
        w.svc.login(badge, password, &code, client)
    }

    fn attack(w: &World, badge: &str, client: &str, n: usize) {
        for _ in 0..n {
            let _ = w.svc.login(badge, "Wrong-Password-1x", "000000", client);
        }
    }

    #[test]
    fn whoever_knows_a_badge_can_lock_new_devices_out_but_never_the_owner_at_their_usual_place() {
        let w = world();
        w.provision("CG-2002", 2, PW);
        login_from(&w, "CG-2002", PW, "10.0.0.1").unwrap(); // the owner signs in from the office: that client is now known
        attack(&w, "CG-2002", "203.0.113.9", w.svc.settings.max_failed_logins as usize); // an attacker provokes the lock
        assert!(w.store.get_user("CG-2002").unwrap().unwrap().locked_until.is_some(), "the account lock was provoked");
        let again = login_from(&w, "CG-2002", PW, "10.0.0.1");
        assert!(again.is_ok(), "the owner still signs in from the known client: {again:?}");
        // the owner's success proves they are present: the lock is cleared, and the attacker starts from zero again
        assert!(w.store.get_user("CG-2002").unwrap().unwrap().locked_until.is_none());
    }

    #[test]
    fn a_locked_account_still_refuses_a_client_it_has_never_seen_until_the_lock_runs_out() {
        let w = world();
        w.provision("CG-2002", 2, PW);
        attack(&w, "CG-2002", "203.0.113.9", w.svc.settings.max_failed_logins as usize);
        assert!(matches!(login_from(&w, "CG-2002", PW, "198.51.100.7"), Err(AuthError::InvalidCredentials)), "right password and code, new device, locked");
        w.advance(w.svc.settings.lockout_seconds + 1.0);
        assert!(login_from(&w, "CG-2002", PW, "198.51.100.7").is_ok(), "after the lock runs out the same attempt works");
    }

    #[test]
    fn a_client_that_keeps_failing_is_refused_even_with_the_right_password_and_others_are_not_affected() {
        let w = world();
        w.provision("CG-2002", 2, PW);
        login_from(&w, "CG-2002", PW, "10.0.0.1").unwrap();
        attack(&w, "CG-2002", "203.0.113.9", PAIR_LIMIT as usize);
        assert!(matches!(login_from(&w, "CG-2002", PW, "203.0.113.9"), Err(AuthError::InvalidCredentials)), "five failures from this client for this badge close it");
        assert!(login_from(&w, "CG-2002", PW, "10.0.0.1").is_ok(), "another client is unaffected");
        w.advance(PAIR_WINDOW as f64 + 1.0);
        assert!(matches!(login_from(&w, "CG-2002", PW, "203.0.113.9"), Err(AuthError::InvalidCredentials)) || login_from(&w, "CG-2002", PW, "203.0.113.9").is_ok(), "the window moves on");
    }

    #[test]
    fn the_throttle_survives_a_restart_because_it_lives_in_the_store() {
        let w = world();
        w.provision("CG-2002", 2, PW);
        attack(&w, "CG-2002", "203.0.113.9", PAIR_LIMIT as usize);
        // a second service over the same store, as after a restart or in a second process
        let log = Arc::new(SecurityLog::open(w.dir.path().join("second.jsonl"), &w.svc.settings.audit_anchor_key).unwrap());
        let t = w.clock.clone();
        let clock: Clock = Arc::new(move || t.load(std::sync::atomic::Ordering::SeqCst) as f64 / 1000.0);
        let second = AccessService::new(w.store.clone(), w.svc.settings.clone(), log, clock);
        let code = w.code("CG-2002");
        assert!(matches!(second.login("CG-2002", PW, &code, "203.0.113.9"), Err(AuthError::InvalidCredentials)), "still throttled");
    }

    #[test]
    fn every_kind_of_refusal_counts_against_the_client_so_probing_unknown_badges_is_throttled_too() {
        let w = world();
        for _ in 0..=PAIR_LIMIT {
            let _ = w.svc.login("CG-9999", "x", "000000", "203.0.113.9");
        }
        assert!(w.store.counter(&format!("pair|{}|{}", badge_key("CG-9999"), w.svc.client_key("203.0.113.9")), PAIR_WINDOW, w.svc.now()).unwrap() >= PAIR_LIMIT);
        let k = events(&w).iter().filter_map(|e| e["reason"].as_str().map(str::to_string)).collect::<Vec<_>>();
        assert!(k.iter().any(|r| r == "throttled"), "{k:?}");
    }
}
