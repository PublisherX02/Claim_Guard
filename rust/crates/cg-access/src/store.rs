//! The user store: a trait, and an in-memory implementation for fast tests and the demo (`src/access/store.py`).
//!
//! The MongoDB implementation (`store_mongo`) must pass the same contract suite, so this twin is checked against the real database's
//! behaviour. Identifiers are plain Rust strings, so a query object such as `{"$ne": null}` cannot even be expressed.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Mutex;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum StoreError {
    #[error("a user with this badge already exists")]
    Duplicate,
    #[error("no such user")]
    NotFound,
    #[error("the database cannot be reached or failed")]
    Unavailable,
    #[error("{0}")]
    Invalid(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct User {
    pub badge_id: String,
    pub name: String,
    pub password_hash: String,
    pub totp_secret_enc: String,
    pub level: i64,
    pub grants: Vec<String>,
    pub revokes: Vec<String>,
    pub active: bool,
    pub failed_attempts: i64,
    pub locked_until: Option<f64>,
    pub must_change_password: bool,
    pub created_by: String,
    pub created_at: f64,
    pub last_login: Option<f64>,
}

/// A dangerous administrator action waiting for a second administrator (see `AccessService::approve`). `payload` holds what is needed to
/// carry the action out (it can include a password hash or an encrypted authenticator seed, never a plain secret) and is never shown;
/// `summary` is the plain description shown to the approver.
#[derive(Debug, Clone, PartialEq)]
pub struct Approval {
    pub id: String,
    pub action: String,
    pub target: String,
    pub requested_by: String,
    pub requested_at: f64,
    pub expires_at: f64,
    /// pending, approved, rejected, cancelled, expired or failed
    pub status: String,
    pub summary: String,
    pub payload: String,
    pub decided_by: Option<String>,
    pub decided_at: Option<f64>,
    pub note: Option<String>,
}

/// The fields an update may change; `None` leaves a field alone. (`locked_until: Some(None)` clears the lock.)
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UserUpdate {
    pub name: Option<String>,
    pub password_hash: Option<String>,
    pub totp_secret_enc: Option<String>,
    pub level: Option<i64>,
    pub grants: Option<Vec<String>>,
    pub revokes: Option<Vec<String>>,
    pub active: Option<bool>,
    pub must_change_password: Option<bool>,
    pub locked_until: Option<Option<f64>>,
    pub failed_attempts: Option<i64>,
}

/// Every stored field has exactly one acceptable shape.
pub fn validate_user(u: &User) -> Result<(), StoreError> {
    let bad = |m: &str| Err(StoreError::Invalid(format!("{m} has the wrong type or range")));
    if u.badge_id.is_empty() {
        return bad("badge_id");
    }
    if !(1..=4).contains(&u.level) {
        return bad("level");
    }
    if u.failed_attempts < 0 {
        return bad("failed_attempts");
    }
    if [u.created_at].iter().chain(u.locked_until.iter()).chain(u.last_login.iter()).any(|n| !n.is_finite()) {
        return bad("a time");
    }
    Ok(())
}

pub fn validate_update(f: &UserUpdate) -> Result<(), StoreError> {
    if f.level.is_some_and(|l| !(1..=4).contains(&l)) {
        return Err(StoreError::Invalid("level has the wrong type or range".into()));
    }
    if f.failed_attempts.is_some_and(|n| n < 0) {
        return Err(StoreError::Invalid("failed_attempts has the wrong type or range".into()));
    }
    if f.locked_until.flatten().is_some_and(|n| !n.is_finite()) {
        return Err(StoreError::Invalid("locked_until has the wrong type or range".into()));
    }
    Ok(())
}

pub fn apply(user: &mut User, f: &UserUpdate) {
    if let Some(v) = &f.name { user.name = v.clone(); }
    if let Some(v) = &f.password_hash { user.password_hash = v.clone(); }
    if let Some(v) = &f.totp_secret_enc { user.totp_secret_enc = v.clone(); }
    if let Some(v) = f.level { user.level = v; }
    if let Some(v) = &f.grants { user.grants = v.clone(); }
    if let Some(v) = &f.revokes { user.revokes = v.clone(); }
    if let Some(v) = f.active { user.active = v; }
    if let Some(v) = f.must_change_password { user.must_change_password = v; }
    if let Some(v) = f.locked_until { user.locked_until = v; }
    if let Some(v) = f.failed_attempts { user.failed_attempts = v; }
}

/// The lock logic shared by every implementation: a failure after a lock ran out starts a new count; a lock is never extended.
pub fn after_failure(user: &mut User, now: f64, max_failed: i64, lockout_seconds: f64) {
    if user.locked_until.is_some_and(|l| l <= now) {
        user.failed_attempts = 1;
        user.locked_until = None;
    } else {
        user.failed_attempts += 1;
    }
    if user.failed_attempts >= max_failed && user.locked_until.is_none() {
        user.locked_until = Some(now + lockout_seconds);
    }
}

pub trait UserStore: Send + Sync {
    fn ping(&self) -> bool;
    fn create_user(&self, user: &User) -> Result<(), StoreError>;
    fn get_user(&self, badge_id: &str) -> Result<Option<User>, StoreError>;
    fn list_users(&self) -> Result<Vec<User>, StoreError>;
    fn update_user(&self, badge_id: &str, fields: &UserUpdate) -> Result<User, StoreError>;
    fn record_failed_login(&self, badge_id: &str, now: f64, max_failed: i64, lockout_seconds: f64) -> Result<User, StoreError>;
    fn record_successful_login(&self, badge_id: &str, now: f64) -> Result<(), StoreError>;
    /// True the first time a (badge, step) is seen, false for a replay.
    fn mark_totp_used(&self, badge_id: &str, step: i64, expires_at: f64) -> Result<bool, StoreError>;
    fn revoke_token(&self, jti: &str, expires_at: f64) -> Result<(), StoreError>;
    fn is_revoked(&self, jti: &str, now: f64) -> Result<bool, StoreError>;
    fn count_active_admins(&self) -> Result<i64, StoreError>;
    /// A counter that restarts every `window_seconds` (fixed windows): how often has `key` been bumped in the window containing `now`?
    /// Shared by every server process that uses the database, so a restart or a second process does not reset a throttle.
    fn counter(&self, key: &str, window_seconds: u64, now: f64) -> Result<i64, StoreError>;
    /// Adds one to the counter and returns the new value. Atomic: parallel bumps never lose a count.
    fn bump_counter(&self, key: &str, window_seconds: u64, now: f64) -> Result<i64, StoreError>;
    /// Has this person signed in successfully from this client (a keyed hash of its address) within `KNOWN_CLIENT_SECONDS`?
    fn client_known(&self, badge_id: &str, client: &str, now: f64) -> Result<bool, StoreError>;
    fn mark_client(&self, badge_id: &str, client: &str, now: f64) -> Result<(), StoreError>;
    /// Stores a new pending approval. `Duplicate` if a pending, unexpired one already exists for the same (action, target); a pending one
    /// whose time has run out is marked expired first.
    fn create_approval(&self, approval: &Approval, now: f64) -> Result<(), StoreError>;
    fn get_approval(&self, id: &str) -> Result<Option<Approval>, StoreError>;
    /// The most recent approvals first.
    fn list_approvals(&self, limit: usize) -> Result<Vec<Approval>, StoreError>;
    /// Atomically moves an approval from status `from` to `to` and records who and when. When `from` is "pending" it also requires that
    /// the time has not run out. True if this call made the change; false if someone else got there first (or it expired).
    fn settle_approval(&self, id: &str, from: &str, to: &str, by: &str, now: f64, note: Option<&str>) -> Result<bool, StoreError>;
}

/// How long a client counts as known after the last successful sign-in from it (30 days).
pub const KNOWN_CLIENT_SECONDS: f64 = 30.0 * 86_400.0;

#[derive(Default)]
struct Inner {
    users: BTreeMap<String, User>,
    totp: BTreeSet<(String, i64)>,
    revoked: HashMap<String, f64>,
    counters: HashMap<String, (i64, i64)>,
    clients: HashMap<(String, String), f64>,
    approvals: Vec<Approval>,
}

#[derive(Default)]
pub struct MemoryStore {
    inner: Mutex<Inner>,
}

impl MemoryStore {
    pub fn new() -> MemoryStore {
        MemoryStore::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl UserStore for MemoryStore {
    fn ping(&self) -> bool {
        true
    }

    fn create_user(&self, user: &User) -> Result<(), StoreError> {
        validate_user(user)?;
        let mut g = self.lock();
        if g.users.contains_key(&user.badge_id) {
            return Err(StoreError::Duplicate);
        }
        g.users.insert(user.badge_id.clone(), user.clone());
        Ok(())
    }

    fn get_user(&self, badge_id: &str) -> Result<Option<User>, StoreError> {
        Ok(self.lock().users.get(badge_id).cloned())
    }

    fn list_users(&self) -> Result<Vec<User>, StoreError> {
        Ok(self.lock().users.values().cloned().collect())
    }

    fn update_user(&self, badge_id: &str, fields: &UserUpdate) -> Result<User, StoreError> {
        validate_update(fields)?;
        let mut g = self.lock();
        let user = g.users.get_mut(badge_id).ok_or(StoreError::NotFound)?;
        apply(user, fields);
        Ok(user.clone())
    }

    fn record_failed_login(&self, badge_id: &str, now: f64, max_failed: i64, lockout_seconds: f64) -> Result<User, StoreError> {
        let mut g = self.lock();
        let user = g.users.get_mut(badge_id).ok_or(StoreError::NotFound)?;
        after_failure(user, now, max_failed, lockout_seconds);
        Ok(user.clone())
    }

    fn record_successful_login(&self, badge_id: &str, now: f64) -> Result<(), StoreError> {
        let mut g = self.lock();
        let user = g.users.get_mut(badge_id).ok_or(StoreError::NotFound)?;
        user.failed_attempts = 0;
        user.locked_until = None;
        user.last_login = Some(now);
        Ok(())
    }

    fn mark_totp_used(&self, badge_id: &str, step: i64, _expires_at: f64) -> Result<bool, StoreError> {
        Ok(self.lock().totp.insert((badge_id.to_string(), step)))
    }

    fn revoke_token(&self, jti: &str, expires_at: f64) -> Result<(), StoreError> {
        let mut g = self.lock();
        let e = g.revoked.entry(jti.to_string()).or_insert(0.0);
        *e = e.max(expires_at);
        Ok(())
    }

    fn is_revoked(&self, jti: &str, now: f64) -> Result<bool, StoreError> {
        Ok(self.lock().revoked.get(jti).is_some_and(|e| *e > now))
    }

    fn count_active_admins(&self) -> Result<i64, StoreError> {
        Ok(self.lock().users.values().filter(|u| u.level == 4 && u.active).count() as i64)
    }

    fn counter(&self, key: &str, window_seconds: u64, now: f64) -> Result<i64, StoreError> {
        let idx = (now / window_seconds.max(1) as f64).floor() as i64;
        Ok(self.lock().counters.get(key).filter(|(w, _)| *w == idx).map_or(0, |(_, n)| *n))
    }

    fn bump_counter(&self, key: &str, window_seconds: u64, now: f64) -> Result<i64, StoreError> {
        let idx = (now / window_seconds.max(1) as f64).floor() as i64;
        let mut g = self.lock();
        if g.counters.len() > 200_000 {
            g.counters.retain(|_, (w, _)| *w >= idx); // old windows are worthless; never let the table grow without bound
        }
        let e = g.counters.entry(key.to_string()).or_insert((idx, 0));
        if e.0 != idx {
            *e = (idx, 0);
        }
        e.1 += 1;
        Ok(e.1)
    }

    fn client_known(&self, badge_id: &str, client: &str, now: f64) -> Result<bool, StoreError> {
        Ok(self.lock().clients.get(&(badge_id.to_string(), client.to_string())).is_some_and(|seen| now - *seen < KNOWN_CLIENT_SECONDS))
    }

    fn mark_client(&self, badge_id: &str, client: &str, now: f64) -> Result<(), StoreError> {
        self.lock().clients.insert((badge_id.to_string(), client.to_string()), now);
        Ok(())
    }

    fn create_approval(&self, approval: &Approval, now: f64) -> Result<(), StoreError> {
        let mut g = self.lock();
        for a in g.approvals.iter_mut().filter(|a| a.status == "pending" && a.expires_at <= now) {
            a.status = "expired".into();
        }
        if g.approvals.iter().any(|a| a.status == "pending" && a.action == approval.action && a.target == approval.target) {
            return Err(StoreError::Duplicate);
        }
        g.approvals.push(approval.clone());
        Ok(())
    }

    fn get_approval(&self, id: &str) -> Result<Option<Approval>, StoreError> {
        Ok(self.lock().approvals.iter().find(|a| a.id == id).cloned())
    }

    fn list_approvals(&self, limit: usize) -> Result<Vec<Approval>, StoreError> {
        Ok(self.lock().approvals.iter().rev().take(limit).cloned().collect())
    }

    fn settle_approval(&self, id: &str, from: &str, to: &str, by: &str, now: f64, note: Option<&str>) -> Result<bool, StoreError> {
        let mut g = self.lock();
        let Some(a) = g.approvals.iter_mut().find(|a| a.id == id && a.status == from && (from != "pending" || a.expires_at > now)) else { return Ok(false) };
        a.status = to.into();
        a.decided_by = Some(by.into());
        a.decided_at = Some(now);
        a.note = note.map(str::to_string);
        Ok(true)
    }
}

#[cfg(test)]
pub fn sample_user(badge: &str, level: i64) -> User {
    User {
        badge_id: badge.into(), name: "Sam".into(), password_hash: "h".into(), totp_secret_enc: "t".into(), level, grants: vec![], revokes: vec![],
        active: true, failed_attempts: 0, locked_until: None, must_change_password: false, created_by: "test".into(), created_at: 1.0, last_login: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_memory_store_meets_the_contract() {
        crate::contract::run(&|| Box::new(MemoryStore::new()));
    }

    #[test]
    fn create_get_update_list() {
        let s = MemoryStore::new();
        s.create_user(&sample_user("CG-1001", 1)).unwrap();
        assert_eq!(s.create_user(&sample_user("CG-1001", 2)), Err(StoreError::Duplicate));
        assert!(s.get_user("CG-9999").unwrap().is_none());
        let u = s.update_user("CG-1001", &UserUpdate { level: Some(3), grants: Some(vec!["audit.view".into()]), ..Default::default() }).unwrap();
        assert_eq!((u.level, u.grants.len()), (3, 1));
        assert_eq!(s.update_user("nope", &UserUpdate::default()), Err(StoreError::NotFound));
        assert_eq!(s.list_users().unwrap().len(), 1);
    }

    #[test]
    fn invalid_values_are_refused_before_storage() {
        let s = MemoryStore::new();
        assert!(s.create_user(&sample_user("CG-1", 9)).is_err());
        assert!(s.create_user(&sample_user("", 1)).is_err());
        s.create_user(&sample_user("CG-1001", 1)).unwrap();
        assert!(s.update_user("CG-1001", &UserUpdate { level: Some(0), ..Default::default() }).is_err());
        assert!(s.update_user("CG-1001", &UserUpdate { failed_attempts: Some(-1), ..Default::default() }).is_err());
        assert!(s.update_user("CG-1001", &UserUpdate { locked_until: Some(Some(f64::NAN)), ..Default::default() }).is_err());
    }

    #[test]
    fn lockout_counts_locks_and_restarts() {
        let s = MemoryStore::new();
        s.create_user(&sample_user("CG-1001", 1)).unwrap();
        for i in 1..=2 {
            let u = s.record_failed_login("CG-1001", 100.0, 3, 60.0).unwrap();
            assert_eq!((u.failed_attempts, u.locked_until), (i, None));
        }
        let u = s.record_failed_login("CG-1001", 100.0, 3, 60.0).unwrap();
        assert_eq!(u.locked_until, Some(160.0));
        let u = s.record_failed_login("CG-1001", 120.0, 3, 60.0).unwrap();
        assert_eq!(u.locked_until, Some(160.0), "a lock is never extended");
        let u = s.record_failed_login("CG-1001", 170.0, 3, 60.0).unwrap();
        assert_eq!((u.failed_attempts, u.locked_until), (1, None), "the lock ran out: a new count starts");
        s.record_successful_login("CG-1001", 200.0).unwrap();
        let u = s.get_user("CG-1001").unwrap().unwrap();
        assert_eq!((u.failed_attempts, u.last_login), (0, Some(200.0)));
    }

    #[test]
    fn a_one_time_code_step_is_used_once() {
        let s = MemoryStore::new();
        assert!(s.mark_totp_used("CG-1", 5, 0.0).unwrap());
        assert!(!s.mark_totp_used("CG-1", 5, 0.0).unwrap());
        assert!(s.mark_totp_used("CG-1", 6, 0.0).unwrap());
        assert!(s.mark_totp_used("CG-2", 5, 0.0).unwrap());
    }

    #[test]
    fn revocation_lasts_until_the_token_would_have_expired() {
        let s = MemoryStore::new();
        s.revoke_token("j", 100.0).unwrap();
        s.revoke_token("j", 50.0).unwrap();
        assert!(s.is_revoked("j", 99.0).unwrap() && !s.is_revoked("j", 100.0).unwrap() && !s.is_revoked("other", 1.0).unwrap());
    }

    #[test]
    fn admins_are_counted_only_when_active() {
        let s = MemoryStore::new();
        s.create_user(&sample_user("CG-4001", 4)).unwrap();
        s.create_user(&sample_user("CG-4002", 4)).unwrap();
        s.update_user("CG-4002", &UserUpdate { active: Some(false), ..Default::default() }).unwrap();
        s.create_user(&sample_user("CG-1001", 1)).unwrap();
        assert_eq!(s.count_active_admins().unwrap(), 1);
    }
}
