//! The security audit log of the reviewer API: logins, failures, lockouts, forbidden requests, unmasking, user changes
//! (`src/access/securitylog.py`). The same hash-chained, anchored log as the review audit, with two rules of its own: the anchor must be
//! HMAC-signed, and no event may carry anything that looks like a secret. Fields are bounded and JSON-safe, so a hostile badge or path
//! typed by an attacker can neither bloat the log nor smuggle structure into it.

use cg_audit::{AuditError, AuditLog, SECURITY_EVENT_TYPES};
use serde_json::{json, Map, Value};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

const FORBIDDEN_NAME_PARTS: [&str; 6] = ["password", "passwd", "totp", "otp", "secret", "token"];
const MAX_VALUE_CHARS: usize = 500;
const MAX_DEPTH: usize = 3;
const MAX_EVENT_CHARS: usize = 4000;
pub const MIN_KEY_CHARS: usize = 32;
pub const MAX_PAGE: usize = 1000;

fn check_name(name: &str) -> Result<(), AuditError> {
    let lowered = name.to_lowercase();
    if name.is_empty() || FORBIDDEN_NAME_PARTS.iter().any(|p| lowered.contains(p)) || lowered.contains("key") {
        return Err(AuditError::Invalid("a field name suggests a secret and may not be logged".into()));
    }
    Ok(())
}

fn check_value(v: &Value, depth: usize) -> Result<(), AuditError> {
    match v {
        Value::Null | Value::Bool(_) => Ok(()),
        Value::Number(n) => {
            if n.as_f64().is_some_and(|f| !f.is_finite()) {
                return Err(AuditError::Invalid("numbers must be finite".into()));
            }
            Ok(())
        }
        Value::String(s) => {
            if s.chars().count() > MAX_VALUE_CHARS {
                return Err(AuditError::Invalid(format!("text values are limited to {MAX_VALUE_CHARS} characters")));
            }
            Ok(())
        }
        Value::Array(items) => {
            if depth + 1 > MAX_DEPTH {
                return Err(AuditError::Invalid("values are nested too deeply".into()));
            }
            items.iter().try_for_each(|i| check_value(i, depth + 1))
        }
        Value::Object(map) => {
            if depth + 1 > MAX_DEPTH {
                return Err(AuditError::Invalid("values are nested too deeply".into()));
            }
            map.iter().try_for_each(|(k, i)| {
                check_name(k)?;
                check_value(i, depth + 1)
            })
        }
    }
}

pub struct SecurityLog {
    path: PathBuf,
    audit: AuditLog,
}

impl SecurityLog {
    pub fn open(path: impl AsRef<Path>, anchor_key: &str) -> Result<SecurityLog, AuditError> {
        if anchor_key.trim().chars().count() < MIN_KEY_CHARS {
            return Err(AuditError::Invalid(format!("the anchor key must be at least {MIN_KEY_CHARS} characters")));
        }
        if let Ok(current) = std::env::var(cg_audit::ANCHOR_KEY_ENV) {
            if !current.is_empty() && current != anchor_key {
                return Err(AuditError::Invalid("a different anchor key is already set for this process".into()));
            }
        }
        // The audit writer signs anchors with the process-wide key, so this log and the review log share it.
        std::env::set_var(cg_audit::ANCHOR_KEY_ENV, anchor_key);
        Ok(SecurityLog { path: path.as_ref().to_path_buf(), audit: AuditLog::open(path)? })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Records one event. `fields` must be a JSON object; nothing is written if any field is unsafe.
    pub fn record(&self, event_type: &str, fields: Value) -> Result<(), AuditError> {
        if !SECURITY_EVENT_TYPES.contains(&event_type) {
            return Err(AuditError::Invalid("unknown security event type".into()));
        }
        let Value::Object(map) = fields else { return Err(AuditError::Invalid("fields must be an object".into())) };
        for (k, v) in &map {
            check_name(k)?;
            check_value(v, 0)?;
        }
        let mut event = Map::new();
        event.insert("event_type".into(), json!(event_type));
        event.extend(map);
        let event = Value::Object(event);
        if event.to_string().len() > MAX_EVENT_CHARS {
            return Err(AuditError::Invalid("event is too large".into()));
        }
        self.audit.append_system_events(&[event]).map(|_| ())
    }

    fn rows(&self, max_rows: usize) -> Result<Vec<Value>, AuditError> {
        if !self.path.exists() {
            return Ok(vec![]);
        }
        let text = std::fs::read_to_string(&self.path).map_err(|e| AuditError::Io(e.to_string()))?;
        let mut recent: VecDeque<Value> = VecDeque::new();
        for line in text.split('\n').filter(|l| !l.trim().is_empty()) {
            let row: Value = serde_json::from_str(line).map_err(|e| AuditError::Invalid(e.to_string()))?;
            if recent.len() == max_rows {
                recent.pop_front();
            }
            recent.push_back(json!({"sequence": row["sequence"], "recorded_at": row["recorded_at"], "event": row["event"]}));
        }
        Ok(recent.into_iter().collect())
    }

    /// A page of rows, oldest first.
    pub fn events(&self, limit: usize, offset: usize) -> Result<Vec<Value>, AuditError> {
        if !(1..=MAX_PAGE).contains(&limit) {
            return Err(AuditError::Invalid(format!("limit must be 1 to {MAX_PAGE} and offset 0 or more")));
        }
        Ok(self.rows(usize::MAX)?.into_iter().skip(offset).take(limit).collect())
    }

    /// The most recent `max_rows` rows, oldest first, in one pass (the audit panel's filters and counts work on this).
    pub fn scan(&self, max_rows: usize) -> Result<Vec<Value>, AuditError> {
        if max_rows == 0 {
            return Err(AuditError::Invalid("max_rows must be a positive whole number".into()));
        }
        self.rows(max_rows)
    }

    pub fn verify(&self) -> Value {
        let status = cg_audit::anchor_status(&self.path);
        let mut out = match cg_audit::verify_with_anchor(&self.path, None, true) {
            Ok((_, count)) => json!({"ok": true, "events": count}),
            Err(_) if !self.path.exists() => json!({"ok": true, "events": 0}),
            Err(e) => json!({"ok": false, "error": e.to_string().chars().take(300).collect::<String>()}),
        };
        if let (Some(o), Some(s)) = (out.as_object_mut(), status.as_object()) {
            o.extend(s.clone());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn log() -> (tempfile::TempDir, SecurityLog) {
        let dir = tempfile::tempdir().unwrap();
        let l = SecurityLog::open(dir.path().join("sec.jsonl"), &"t".repeat(40)).unwrap();
        (dir, l)
    }

    #[test]
    fn records_pages_and_verifies() {
        let (_d, l) = log();
        for i in 0..5 {
            l.record("login_success", json!({"badge_id": format!("CG-{i}")})).unwrap();
        }
        assert_eq!(l.events(2, 1).unwrap().len(), 2);
        assert_eq!(l.events(2, 1).unwrap()[0]["event"]["badge_id"], "CG-1");
        assert_eq!(l.scan(3).unwrap()[0]["event"]["badge_id"], "CG-2");
        let v = l.verify();
        assert_eq!((v["ok"].clone(), v["events"].clone(), v["anchor_signed"].clone()), (json!(true), json!(5), json!(true)));
        assert!(l.events(0, 0).is_err() && l.events(1001, 0).is_err());
    }

    #[test]
    fn unknown_types_and_secret_looking_fields_are_refused() {
        let (_d, l) = log();
        assert!(l.record("nonsense", json!({})).is_err());
        assert!(l.record("login_failure", json!({"reason": "x", "password": "hunter2"})).is_err());
        assert!(l.record("login_failure", json!({"reason": "x", "api_key": "k"})).is_err());
        assert!(l.record("login_failure", json!({"reason": "x", "nested": {"totp_seed": "k"}})).is_err());
        assert!(l.record("login_failure", json!({"reason": "x".repeat(501)})).is_err());
        assert!(l.record("login_failure", json!({"reason": "x", "deep": [[[[1]]]]})).is_err());
        assert!(l.record("login_failure", json!("not an object")).is_err());
        assert!(l.record("login_failure", json!({"reason": "x", "a": "y".repeat(500), "b": "y".repeat(500), "c": "y".repeat(500), "d": "y".repeat(500),
            "e": "y".repeat(500), "f": "y".repeat(500), "g": "y".repeat(500), "h": "y".repeat(500), "i": "y".repeat(500)})).is_err(), "too large");
        assert_eq!(l.scan(10).unwrap().len(), 0, "nothing was written");
    }

    #[test]
    fn a_short_anchor_key_is_refused_and_tampering_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        assert!(SecurityLog::open(dir.path().join("x.jsonl"), "short").is_err());
        let (_d, l) = log();
        l.record("login_success", json!({"badge_id": "CG-1"})).unwrap();
        let text = std::fs::read_to_string(l.path()).unwrap().replace("CG-1", "CG-9");
        std::fs::write(l.path(), text).unwrap();
        assert_eq!(l.verify()["ok"], json!(false));
    }
}
