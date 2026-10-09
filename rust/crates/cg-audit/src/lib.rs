//! The hash-chained audit log: the Rust counterpart of `src/audit.py` and `src/audit_log.py`.
//!
//! Same file format and same hash function as the Python build, so each build verifies the other's logs. A row is
//! `{sequence, recorded_at, previous_hash, event, hash}`; the hash covers the row without `hash`. Next to the log an anchor file
//! (`<log>.head.json`) holds the head hash and the event count (HMAC-signed when `AUDIT_ANCHOR_KEY` is set) so truncation and whole-log
//! replacement are detected, which a chain alone cannot do. This is tamper-EVIDENT, not immutable.

pub mod canonical;

use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";
pub const ANCHOR_KEY_ENV: &str = "AUDIT_ANCHOR_KEY";
const LOCK_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub enum AuditError {
    #[error("{0}")]
    Invalid(String),
    #[error("audit log i/o: {0}")]
    Io(String),
}

impl From<std::io::Error> for AuditError {
    fn from(e: std::io::Error) -> Self {
        AuditError::Io(e.to_string())
    }
}

fn invalid<T>(msg: impl Into<String>) -> Result<T, AuditError> {
    Err(AuditError::Invalid(msg.into()))
}

// ---- what an event must carry --------------------------------------------------------------------------------------------

pub const SYSTEM_DECISIONS: [&str; 3] = ["route_to_human_review", "no_findings_for_review", "quarantine_claim"];
const REVIEW_ACTIONS: [&str; 4] = ["confirm_issue", "dismiss_with_reason", "request_information", "mark_corrected_for_recheck"];

fn required() -> &'static BTreeMap<&'static str, BTreeSet<&'static str>> {
    static T: OnceLock<BTreeMap<&'static str, BTreeSet<&'static str>>> = OnceLock::new();
    T.get_or_init(|| {
        let rows: &[(&str, &[&str])] = &[
            ("ingestion", &["claim_id", "source_format", "outcome"]),
            ("run_started", &["run_id", "claim_id", "input_hash"]),
            ("rule_check", &["run_id", "claim_id", "rule_id", "rule_version", "status", "severity", "result_hash", "confidence", "confidence_kind", "method"]),
            ("ai_request", &["run_id", "claim_id", "rule_id", "request_id", "question", "deterministic_status", "verdict", "requires_human_review", "finding_hash", "prompt_version", "prompt_hash", "action_type"]),
            ("ai_recommendation", &["run_id", "claim_id", "rule_id", "request_id", "model", "prompt_version", "used_fallback", "source", "output_hash", "action_type", "auto_correct_applied", "escalated_to", "confidence", "confidence_kind"]),
            ("ai_failure", &["run_id", "claim_id", "rule_id", "request_id", "error", "action_type"]),
            ("system_decision", &["run_id", "claim_id", "decision", "reason"]),
            ("run_finished", &["run_id", "claim_id", "rule_pack_hash", "tool_errors"]),
            ("recheck_run", &["claim_id", "prior_run_id", "new_run_id", "prior_input_hash", "new_input_hash"]),
            ("duplicate_submission", &["run_id", "claim_id", "prior_run_ids", "same_input"]),
            ("advisory_check", &["run_id", "claim_id", "check_id", "detail"]),
            // the reviewer API's security log
            ("login_success", &["badge_id"]),
            ("login_failure", &["reason"]),
            ("lockout", &["badge_id"]),
            ("logout", &["badge_id"]),
            ("token_rejected", &["reason"]),
            ("forbidden", &["badge_id", "method", "path"]),
            ("unmask", &["badge_id", "claim_id", "reason"]),
            ("user_created", &["actor", "badge_id", "level"]),
            ("user_updated", &["actor", "badge_id", "changes"]),
            ("user_unlocked", &["actor", "badge_id"]),
            ("totp_reset", &["actor", "badge_id"]),
            ("password_changed", &["badge_id"]),
            ("audit_read", &["badge_id"]),
            ("audit_verify", &["badge_id", "ok"]),
            ("decision", &["badge_id", "claim_id", "rule_id", "action"]),
            ("routing_config_changed", &["actor", "version", "before", "after"]),
            ("lease_expired", &["claim_id", "badge_id"]),
            ("lease_reclaimed", &["claim_id", "badge_id", "reason"]),
            ("claim_dealt", &["deal_id", "claim_id", "badge_id"]),
            ("claim_decided_green", &["badge_id", "claim_id", "action"]),
            ("queue_admin", &["actor", "command"]),
            ("claim_signoff", &["claim_id", "badge_id", "stage", "outcome"]),
            ("triage_receipt", &["claim_id", "input_hash", "result_hash", "lane", "score", "config_version"]),
        ];
        rows.iter().map(|(k, fields)| (*k, fields.iter().copied().collect())).collect()
    })
}

/// `audit_log._validate_system_event`
pub fn validate_system_event(event: &Value) -> Result<(), AuditError> {
    let obj = event.as_object().ok_or_else(|| AuditError::Invalid("an event must be an object".into()))?;
    let kind = obj.get("event_type").and_then(Value::as_str).unwrap_or("");
    let Some(fields) = required().get(kind) else { return invalid(format!("Unknown system event type: {kind:?}")) };
    let missing: Vec<&str> = fields.iter().copied().filter(|f| !obj.contains_key(*f)).collect();
    if !missing.is_empty() {
        return invalid(format!("{kind} event missing fields: {missing:?}"));
    }
    let text = |k: &str| obj.get(k).and_then(Value::as_str).unwrap_or("");
    if kind == "system_decision" && !SYSTEM_DECISIONS.contains(&text("decision")) {
        return invalid(format!("System decision {:?} is not permitted", text("decision")));
    }
    if kind == "rule_check" || kind == "ai_recommendation" {
        let ck = text("confidence_kind");
        if ck == "not_probabilistic" && !obj["confidence"].is_null() {
            return invalid("not_probabilistic events must have null confidence");
        }
        if !["not_probabilistic", "uncalibrated", "calibrated"].contains(&ck) {
            return invalid("Invalid confidence_kind");
        }
    }
    if ["ai_request", "ai_recommendation", "ai_failure"].contains(&kind) {
        match text("action_type") {
            "auto_correct" => return invalid("AI auto-correction is not permitted: AI actions must be human_escalation"),
            "human_escalation" => {}
            other => return invalid(format!("Unknown AI action type: {other:?}")),
        }
    }
    if kind == "ai_recommendation" && obj["auto_correct_applied"] != Value::Bool(false) {
        return invalid("auto_correct_applied must be false: the AI never changes a claim or result");
    }
    Ok(())
}

/// `audit.append`'s checks on a human review decision.
pub fn validate_review_event(event: &Value) -> Result<(), AuditError> {
    let text = |k: &str| event.get(k).and_then(Value::as_str).unwrap_or("");
    let action_ok = REVIEW_ACTIONS.contains(&text("action"));
    if !action_ok || text("actor").is_empty() || text("claim_id").is_empty() || text("rule_id").is_empty() {
        return invalid("Invalid review event");
    }
    if cg_core::text::blank(text("reason")) {
        return invalid("Review reason is required");
    }
    Ok(())
}

// ---- reading and verifying -----------------------------------------------------------------------------------------------

fn rows_of(path: &Path) -> Result<Vec<String>, AuditError> {
    let text = std::fs::read(path)?;
    let text = String::from_utf8(text).map_err(|e| AuditError::Invalid(format!("Audit log is not valid UTF-8: {e}")))?;
    // split on \n only: a U+2028 inside a reviewer's reason must not split a record
    Ok(text.split('\n').filter(|l| !l.trim().is_empty()).map(str::to_string).collect())
}

/// `audit.verify`: walks the chain, returns (head, count).
pub fn verify(path: &Path) -> Result<(String, u64), AuditError> {
    let mut previous = GENESIS.to_string();
    let mut count = 0u64;
    if !path.exists() {
        return Ok((previous, count));
    }
    for line in rows_of(path)? {
        let mut row: Value = serde_json::from_str(&line).map_err(|_| AuditError::Invalid(format!("Audit chain invalid at event {}: malformed record", count + 1)))?;
        let malformed = || AuditError::Invalid(format!("Audit chain invalid at event {}: malformed record", count + 1));
        let obj = row.as_object_mut().ok_or_else(malformed)?;
        let claimed = obj.remove("hash").and_then(|h| h.as_str().map(str::to_string)).ok_or_else(malformed)?;
        let linked = obj.get("previous_hash").ok_or_else(malformed)?.as_str() == Some(previous.as_str());
        if !linked || canonical::digest(&row) != claimed {
            return invalid(format!("Audit chain invalid at event {}", count + 1));
        }
        previous = claimed;
        count += 1;
    }
    Ok((previous, count))
}

fn anchor_path_of(log: &Path) -> PathBuf {
    let mut name = log.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".head.json");
    log.with_file_name(name)
}

fn lock_path_of(log: &Path) -> PathBuf {
    let mut name = log.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".lock");
    log.with_file_name(name)
}

/// HMAC-SHA256 over `head|count`, or None when no key is configured.
pub fn anchor_mac(head: &str, count: u64) -> Option<String> {
    let key = std::env::var(ANCHOR_KEY_ENV).ok().filter(|k| !k.is_empty())?;
    let mut mac = Hmac::<Sha256>::new_from_slice(key.as_bytes()).ok()?;
    mac.update(format!("{head}|{count}").as_bytes());
    Some(hex::encode(mac.finalize().into_bytes()))
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// `verify_with_anchor`: the chain plus the anchor. `strict` also rejects rows after the anchored position.
pub fn verify_with_anchor(path: &Path, anchor: Option<&Path>, strict: bool) -> Result<(String, u64), AuditError> {
    let anchor_path = anchor.map(Path::to_path_buf).unwrap_or_else(|| anchor_path_of(path));
    let (head, count) = verify(path)?;
    if !anchor_path.exists() {
        return invalid("No anchor file: chain is internally consistent but cannot be checked against truncation");
    }
    let parsed: Result<(Value, u64, String), String> = (|| {
        let a: Value = serde_json::from_str(&std::fs::read_to_string(&anchor_path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let c = a.get("count").and_then(|c| c.as_u64().or_else(|| c.as_str().and_then(|s| s.parse().ok()))).ok_or("count")?;
        let h = a.get("head").and_then(Value::as_str).ok_or("head")?.to_string();
        Ok((a, c, h))
    })();
    let (anchor, anchor_count, anchor_head) = parsed.map_err(|e| AuditError::Invalid(format!("Anchor file {} is unreadable or malformed: {e}", anchor_path.display())))?;
    if let Some(expected) = anchor_mac(&anchor_head, anchor_count) {
        let given = anchor.get("mac").and_then(Value::as_str).unwrap_or("");
        if !constant_time_eq(given, &expected) {
            return invalid("Anchor MAC missing or wrong: the anchor was not written with the configured key");
        }
    }
    if count < anchor_count {
        return invalid(format!("Log truncated: {count} events, anchor recorded {anchor_count}"));
    }
    if strict && count > anchor_count {
        return invalid(format!("{} unanchored row(s): the log has {count} events but the anchor recorded {anchor_count}", count - anchor_count));
    }
    if anchor_count == 0 {
        return Ok((head, count));
    }
    let rows = rows_of(path)?;
    let at: Value = serde_json::from_str(&rows[(anchor_count - 1) as usize]).map_err(|_| AuditError::Invalid("Log replaced".into()))?;
    if at.get("hash").and_then(Value::as_str) != Some(anchor_head.as_str()) {
        return invalid("Log replaced: hash at anchored position differs from the anchor");
    }
    Ok((head, count))
}

/// `anchor_status`: reports whether the anchor is signed and a key is configured; never fails.
pub fn anchor_status(path: &Path) -> Value {
    let signed = std::fs::read_to_string(anchor_path_of(path))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .map(|a| a.get("mac").is_some_and(|m| !m.is_null() && m != ""))
        .unwrap_or(false);
    json!({"key_configured": anchor_mac("", 0).is_some(), "anchor_signed": signed})
}

// ---- time ------------------------------------------------------------------------------------------------------------------

/// `datetime.now(timezone.utc).isoformat()`: microseconds are omitted when they are zero, exactly as Python does.
pub fn iso_now() -> String {
    iso_of(SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default())
}

pub fn iso_of(since_epoch: Duration) -> String {
    let secs = since_epoch.as_secs() as i64;
    let micros = since_epoch.subsec_micros();
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // civil from days (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let base = format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}", rem / 3600, (rem % 3600) / 60, rem % 60);
    if micros == 0 {
        format!("{base}+00:00")
    } else {
        format!("{base}.{micros:06}+00:00")
    }
}

// ---- writing ---------------------------------------------------------------------------------------------------------------

fn process_lock(path: &Path) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<BTreeMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    let key = std::fs::canonicalize(path.parent().unwrap_or(Path::new("."))).unwrap_or_default().join(path.file_name().unwrap_or_default());
    let map = LOCKS.get_or_init(|| Mutex::new(BTreeMap::new()));
    map.lock().unwrap_or_else(|e| e.into_inner()).entry(key).or_default().clone()
}

/// An exclusive cross-process lock on the log's sidecar file, released when dropped.
struct FileLock(File);

impl FileLock {
    fn acquire(path: &Path) -> Result<FileLock, AuditError> {
        let file = OpenOptions::new().create(true).read(true).append(true).open(path)?;
        let deadline = Instant::now() + LOCK_TIMEOUT;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(FileLock(file)),
                Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
                Err(std::fs::TryLockError::WouldBlock) => return Err(AuditError::Io(format!("Could not lock {} within {}s", path.display(), LOCK_TIMEOUT.as_secs()))),
                Err(std::fs::TryLockError::Error(e)) => return Err(e.into()),
            }
        }
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// The append-only chain writer, safe for several threads and several processes: every append takes both locks and re-reads the log's real
/// last row to learn the current head and count, so two writers can never fork the chain.
pub struct AuditLog {
    path: PathBuf,
    anchor_path: PathBuf,
    lock_path: PathBuf,
    guard: Arc<Mutex<()>>,
}

impl AuditLog {
    /// Opens (creating the directory) and, if an anchor exists, checks the log against it first: appending to a truncated log would
    /// re-anchor the truncated state and erase the evidence.
    pub fn open(path: impl AsRef<Path>) -> Result<AuditLog, AuditError> {
        let path = path.as_ref().to_path_buf();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let log = AuditLog { anchor_path: anchor_path_of(&path), lock_path: lock_path_of(&path), guard: process_lock(&path), path };
        let _g = log.guard.lock().unwrap_or_else(|e| e.into_inner());
        let _f = FileLock::acquire(&log.lock_path)?;
        if log.anchor_path.exists() {
            verify_with_anchor(&log.path, Some(&log.anchor_path), false)?;
        } else {
            verify(&log.path)?;
        }
        drop(_f);
        drop(_g);
        Ok(log)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The real last row: (head, count). Refuses a torn or altered tail.
    fn sync_tail(&self) -> Result<(String, u64), AuditError> {
        let size = std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
        if size == 0 {
            return Ok((GENESIS.to_string(), 0));
        }
        let mut f = File::open(&self.path)?;
        let mut chunk: u64 = 1 << 16;
        let last: Vec<u8> = loop {
            let start = size.saturating_sub(chunk);
            f.seek(SeekFrom::Start(start))?;
            let mut data = vec![0u8; (size - start) as usize];
            f.read_exact(&mut data)?;
            while data.last() == Some(&b'\n') {
                data.pop();
            }
            if data.contains(&b'\n') || start == 0 {
                let cut = data.iter().rposition(|b| *b == b'\n').map(|i| i + 1).unwrap_or(0);
                break data[cut..].to_vec();
            }
            chunk *= 2;
        };
        let torn = || AuditError::Invalid("Audit log tail is torn or altered; refusing to append".into());
        let mut row: Value = serde_json::from_slice(&last).map_err(|_| torn())?;
        let claimed = row.as_object_mut().and_then(|o| o.remove("hash")).and_then(|h| h.as_str().map(str::to_string)).ok_or_else(torn)?;
        if canonical::digest(&row) != claimed {
            return Err(torn());
        }
        let seq = row.get("sequence").and_then(Value::as_u64).ok_or_else(torn)?;
        Ok((claimed, seq))
    }

    fn write_anchor(&self, head: &str, count: u64) -> Result<(), AuditError> {
        let mut anchor = json!({"head": head, "count": count, "written_at": iso_now(),
            "note": "Keep a copy of this file somewhere the log writer cannot modify."});
        if let Some(mac) = anchor_mac(head, count) {
            anchor["mac"] = Value::String(mac);
        }
        let tmp = self.anchor_path.with_file_name(format!("{}.{}.tmp", self.anchor_path.file_name().and_then(|n| n.to_str()).unwrap_or("anchor"), std::process::id()));
        std::fs::write(&tmp, serde_json::to_string_pretty(&anchor).unwrap_or_default())?;
        for attempt in 0..40u64 {
            match std::fs::rename(&tmp, &self.anchor_path) {
                Ok(()) => return Ok(()),
                Err(e) if attempt == 39 => {
                    let _ = std::fs::remove_file(&tmp);
                    return Err(e.into());
                }
                Err(_) => std::thread::sleep(Duration::from_millis(5 * (attempt + 1))),
            }
        }
        Ok(())
    }

    fn append(&self, events: &[Value], durable: bool) -> Result<(String, u64), AuditError> {
        let _g = self.guard.lock().unwrap_or_else(|e| e.into_inner());
        let _f = FileLock::acquire(&self.lock_path)?;
        let (mut head, mut count) = self.sync_tail()?;
        if events.is_empty() {
            return Ok((head, count));
        }
        let mut file = OpenOptions::new().create(true).append(true).open(&self.path)?;
        for event in events {
            let row = json!({"sequence": count + 1, "recorded_at": iso_now(), "previous_hash": head, "event": event});
            head = canonical::digest(&row);
            let mut full = row;
            full["hash"] = Value::String(head.clone());
            file.write_all(canonical::line(&full).as_bytes())?;
            file.write_all(b"\n")?;
            count += 1;
        }
        file.flush()?;
        if durable {
            file.sync_all()?;
        }
        self.write_anchor(&head, count)?;
        Ok((head, count))
    }

    /// System events (security log, run flow). Validated first; nothing is written if any one is invalid.
    pub fn append_system_events(&self, events: &[Value]) -> Result<(String, u64), AuditError> {
        events.iter().try_for_each(validate_system_event)?;
        let durable = events.iter().any(|e| e.get("event_type").and_then(Value::as_str) == Some("ai_request"));
        self.append(events, durable)
    }

    /// Human review decisions: stricter rules, always fsync'd.
    pub fn append_review_decisions(&self, events: &[Value]) -> Result<(String, u64), AuditError> {
        events.iter().try_for_each(validate_review_event)?;
        self.append(events, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn login(badge: &str) -> Value {
        json!({"event_type": "login_success", "badge_id": badge})
    }

    #[test]
    fn iso_time_matches_python_isoformat() {
        assert_eq!(iso_of(Duration::new(1_700_000_000, 0)), "2023-11-14T22:13:20+00:00");
        assert_eq!(iso_of(Duration::new(1_700_000_000, 176_630_000)), "2023-11-14T22:13:20.176630+00:00");
        assert_eq!(iso_of(Duration::new(951_782_400, 5_000)), "2000-02-29T00:00:00.000005+00:00");
    }

    #[test]
    fn a_log_chains_verifies_and_anchors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.jsonl");
        let log = AuditLog::open(&path).unwrap();
        let (h1, n1) = log.append_system_events(&[login("CG-1"), login("CG-2")]).unwrap();
        assert_eq!(n1, 2);
        assert_eq!(verify(&path).unwrap(), (h1.clone(), 2));
        assert_eq!(verify_with_anchor(&path, None, true).unwrap(), (h1, 2));
        assert_eq!(AuditLog::open(&path).unwrap().append_system_events(&[login("CG-3")]).unwrap().1, 3);
    }

    #[test]
    fn editing_a_row_breaks_the_chain() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.jsonl");
        let log = AuditLog::open(&path).unwrap();
        log.append_system_events(&[login("CG-1"), login("CG-2")]).unwrap();
        let text = std::fs::read_to_string(&path).unwrap().replacen("CG-1", "CG-9", 1);
        std::fs::write(&path, text).unwrap();
        assert!(verify(&path).unwrap_err().to_string().contains("invalid at event 1"));
        assert!(AuditLog::open(&path).is_err(), "a tampered log is refused at open");
    }

    #[test]
    fn truncation_and_forged_appends_are_caught_by_the_anchor() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.jsonl");
        let log = AuditLog::open(&path).unwrap();
        log.append_system_events(&[login("CG-1"), login("CG-2"), login("CG-3")]).unwrap();
        let lines: Vec<String> = std::fs::read_to_string(&path).unwrap().lines().map(str::to_string).collect();
        std::fs::write(&path, format!("{}\n{}\n", lines[0], lines[1])).unwrap();
        assert!(verify_with_anchor(&path, None, false).unwrap_err().to_string().contains("truncated"));
        // a valid extra row appended after the anchored position is only visible in strict mode
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        let (head, count) = verify(&path).unwrap();
        let forged = json!({"sequence": count + 1, "recorded_at": "x", "previous_hash": head, "event": login("EVIL")});
        let mut row = forged.clone();
        row["hash"] = Value::String(canonical::digest(&forged));
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, "{}", canonical::line(&row)).unwrap();
        assert!(verify_with_anchor(&path, None, false).is_ok());
        assert!(verify_with_anchor(&path, None, true).unwrap_err().to_string().contains("unanchored"));
    }

    #[test]
    fn invalid_events_are_refused_before_anything_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let log = AuditLog::open(dir.path().join("a.jsonl")).unwrap();
        assert!(log.append_system_events(&[login("CG-1"), json!({"event_type": "nonsense"})]).is_err());
        assert!(log.append_system_events(&[json!({"event_type": "login_success"})]).is_err());
        assert_eq!(log.append_system_events(&[]).unwrap().1, 0);
        assert!(log.append_review_decisions(&[json!({"action": "confirm_issue", "actor": "a", "claim_id": "c", "rule_id": "R001", "reason": "  "})]).is_err());
        assert!(log.append_review_decisions(&[json!({"action": "approve", "actor": "a", "claim_id": "c", "rule_id": "R001", "reason": "x"})]).is_err());
        let ok = json!({"action": "confirm_issue", "actor": "a", "claim_id": "c", "rule_id": "R001", "reason": "checked"});
        assert_eq!(log.append_review_decisions(&[ok]).unwrap().1, 1);
    }

    #[test]
    fn two_writers_never_fork_the_chain() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.jsonl");
        let a = AuditLog::open(&path).unwrap();
        let b = AuditLog::open(&path).unwrap();
        std::thread::scope(|s| {
            s.spawn(|| (0..40).for_each(|i| { a.append_system_events(&[login(&format!("A{i}"))]).unwrap(); }));
            s.spawn(|| (0..40).for_each(|i| { b.append_system_events(&[login(&format!("B{i}"))]).unwrap(); }));
        });
        assert_eq!(verify_with_anchor(&path, None, true).unwrap().1, 80);
    }

    #[test]
    fn unicode_line_separators_do_not_split_a_row() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.jsonl");
        let log = AuditLog::open(&path).unwrap();
        log.append_system_events(&[json!({"event_type": "login_failure", "reason": "a\u{2028}b\u{85}c é"})]).unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().is_ascii());
        assert_eq!(verify(&path).unwrap().1, 1);
    }
}
