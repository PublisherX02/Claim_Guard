//! The erasure ledger (docs/59): an append-only file that remembers *what was erased*, outside the database.
//!
//! A backup taken before an erasure still holds the claim. The law (and the regulators' guidance) accept that if the copy is put beyond use
//! and the erasure is applied again to anything restored from it. This file is how it is applied again: every erasure writes a line here
//! first, and after a restore `replay` deletes whatever the restored database brings back.
//!
//! * A line holds claim ids (which name no one once the claim is gone), the keyed `patient_key` index of the patient (a keyed hash, not the
//!   identifier), who did it and when. No name, no identifier, no health data.
//! * It lives with the audit logs, so the append-only hardening (`deploy/rust/harden-audit.sh`) and the backup of the log volume cover it,
//!   and a restore must never roll it back (`restore.sh` keeps the longer of the live file and the backup's).
//! * Each line carries a SHA-256 of its own content; a line that does not match makes `read` fail loudly rather than skip it, because a
//!   silently skipped line is an erasure that is not re-applied.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};

pub struct Ledger {
    path: PathBuf,
}

fn digest(body: &Value) -> String {
    hex::encode(Sha256::digest(body.to_string().as_bytes()))
}

impl Ledger {
    pub fn open(path: impl AsRef<Path>) -> Ledger {
        Ledger { path: path.as_ref().to_path_buf() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Writes one erasure down and flushes it to disk. Call it BEFORE deleting anything: if the deletion is cut short, the line is still
    /// there and the next replay finishes the job.
    pub fn append(&self, kind: &str, actor: &str, at: f64, claim_ids: &[String], patient_keys: &[String]) -> Result<(), String> {
        let body = json!({"at": at, "kind": kind, "actor": actor, "claim_ids": claim_ids, "patient_keys": patient_keys});
        let line = json!({"entry": body, "sha": digest(&body)}).to_string();
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&self.path).map_err(|e| format!("the erasure ledger cannot be written: {e}"))?;
        f.write_all(format!("{line}\n").as_bytes()).and_then(|_| f.sync_all()).map_err(|e| format!("the erasure ledger cannot be written: {e}"))
    }

    /// Every entry, oldest first. A line that is damaged or whose hash does not match is an error.
    pub fn read(&self) -> Result<Vec<Value>, String> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(format!("the erasure ledger cannot be read: {e}")),
        };
        let mut out = vec![];
        for (i, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
            let v: Value = serde_json::from_str(line).map_err(|_| format!("erasure ledger line {} is damaged", i + 1))?;
            if v["sha"].as_str() != Some(digest(&v["entry"]).as_str()) {
                return Err(format!("erasure ledger line {} does not match its hash: the file was changed", i + 1));
            }
            out.push(v["entry"].clone());
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn entries_are_kept_in_order_and_hold_no_identifier() {
        let d = tempfile::tempdir().unwrap();
        let l = Ledger::open(d.path().join("erasure_ledger.jsonl"));
        assert!(l.read().unwrap().is_empty(), "no file yet means nothing erased");
        l.append("erase", "CG-4004", 10.0, &ids(&["C1", "C2"]), &ids(&["abc"])).unwrap();
        l.append("retention", "system:worker", 20.0, &ids(&["C3"]), &[]).unwrap();
        let all = l.read().unwrap();
        assert_eq!((all.len(), all[0]["claim_ids"][1].as_str(), all[1]["kind"].as_str()), (2, Some("C2"), Some("retention")));
        assert!(!std::fs::read_to_string(l.path()).unwrap().contains("PAT-"));
    }

    #[test]
    fn an_edited_line_is_an_error_not_a_skipped_erasure() {
        let d = tempfile::tempdir().unwrap();
        let l = Ledger::open(d.path().join("erasure_ledger.jsonl"));
        l.append("erase", "CG-4004", 10.0, &ids(&["C1"]), &[]).unwrap();
        let text = std::fs::read_to_string(l.path()).unwrap();
        std::fs::write(l.path(), text.replace("C1", "C9")).unwrap();
        assert!(l.read().unwrap_err().contains("does not match its hash"));
        std::fs::write(l.path(), "not json\n").unwrap();
        assert!(l.read().unwrap_err().contains("damaged"));
    }
}
