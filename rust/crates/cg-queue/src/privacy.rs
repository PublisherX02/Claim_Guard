//! Subject access, erasure and retention (docs/58). The three things a data-protection law asks of the holder of personal data that the first
//! versions of the system did not do.
//!
//! * **Subject access.** Everything held about one patient: every version of every claim of theirs, decrypted, with the request events attached.
//! * **Erasure.** The erasure is written to the erasure ledger first (docs/59). Then every version of the claim is deleted from the queue store, with
//!   its request events and its dead letters, and the patient's own encryption key is deleted when none of their claims is left, which makes any copy
//!   of their data that survives elsewhere (a backup) unreadable. The audit logs are *not*
//!   rewritten, because they are chained and signed and because they hold no personal data about the patient: they carry badge numbers, claim ids and
//!   hashes only (checked by `tests/test_data_minimization.py` and, for the Rust build, by the pen test). A claim id on its own names no one once the claim is gone.
//! * **Retention.** A claim that was decided long enough ago is erased in the same way, by a daily job, when `RETENTION_DAYS` is set.
//!
//! Nothing here decides who may ask: the callers (the CLI on the server, and the API routes behind a permission, a fresh authenticator code and, for
//! erasure, a second administrator) do. Every function takes the store, so the same code runs against the in-memory twin and against MongoDB.

use crate::erasure_ledger::Ledger;
use crate::store::QueueStore;
use cg_access::store::StoreError;
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// States in which a person is working on the claim right now: erasing it from under them would lose their work and leave a lease behind.
const ACTIVE: [&str; 2] = ["leased", "awaiting_countersign"];

#[derive(Debug, thiserror::Error)]
pub enum PrivacyError {
    #[error("{0}")]
    Refused(String),
    #[error(transparent)]
    Store(#[from] StoreError),
}

type R<T> = Result<T, PrivacyError>;

/// The ids of every claim of one patient, sorted.
pub fn claim_ids_of(store: &dyn QueueStore, patient_id: &str) -> R<Vec<String>> {
    if patient_id.trim().is_empty() || patient_id.chars().count() > 120 {
        return Err(PrivacyError::Refused("the patient identifier must be 1 to 120 characters".into()));
    }
    let mut ids: Vec<String> = store.docs_for_patient(patient_id)?.iter().filter_map(|d| d["claim_id"].as_str().map(str::to_string)).collect();
    ids.sort();
    ids.dedup();
    Ok(ids)
}

/// Subject access: everything held about one patient in the queue store, readable.
pub fn export_subject(store: &dyn QueueStore, patient_id: &str, now: f64) -> R<Value> {
    let docs = store.docs_for_patient(patient_id)?;
    let mut by_claim: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for d in docs {
        by_claim.entry(d["claim_id"].as_str().unwrap_or("").to_string()).or_default().push(d);
    }
    let mut claims = vec![];
    let mut versions = 0;
    for (id, mut docs) in by_claim {
        docs.sort_by_key(|d| d["version"].as_i64().unwrap_or(0));
        versions += docs.len();
        let requests = store.request_events(&id, 1000)?;
        claims.push(json!({"claim_id": id, "versions": docs, "request_events": requests}));
    }
    Ok(json!({
        "kind": "subject_access_export",
        "generated_at": now,
        "claims": claims,
        "totals": {"claims": claims.len(), "versions": versions},
        "not_included": "The audit logs hold badge numbers, claim ids and hashes, never the patient's name or identifier; the rows that mention these claim ids are listed separately by `cg privacy export`."
    }))
}

/// What erasing these claims would do, without doing it: the count to confirm. Refuses a claim somebody is working on.
pub fn erase_preview(store: &dyn QueueStore, claim_ids: &[String]) -> R<Value> {
    let mut versions = 0;
    let mut active = vec![];
    for id in claim_ids {
        let mut v = 1;
        // count versions by walking from the newest down; a gap-free sequence is how versions are made
        if let Some(newest) = store.get(id, None)? {
            let top = newest["version"].as_i64().unwrap_or(1);
            versions += top;
            while v <= top {
                if let Some(d) = store.get(id, Some(v))? {
                    if d["state"].as_str().is_some_and(|s| ACTIVE.contains(&s)) {
                        active.push(id.clone());
                        break;
                    }
                }
                v += 1;
            }
        }
    }
    if !active.is_empty() {
        return Err(PrivacyError::Refused(format!("{} claim(s) are being worked on right now ({}): wait until they are decided or handed back", active.len(), active.iter().take(5).cloned().collect::<Vec<_>>().join(", "))));
    }
    Ok(json!({"claims": claim_ids.len(), "versions_at_most": versions}))
}

/// Erasure: deletes every version of each claim, then the patient's key if no claim of theirs is left. Final. The erasure is written to the
/// ledger BEFORE anything is deleted, so a cut-short run is finished by the next replay. The result lists the claim ids, the number of
/// versions deleted and the number of patient keys destroyed.
pub fn erase_claims(store: &dyn QueueStore, ledger: &Ledger, claim_ids: &[String], kind: &str, actor: &str, now: f64) -> R<Value> {
    erase_preview(store, claim_ids)?;
    let mut keys: Vec<String> = vec![];
    for id in claim_ids {
        if let Some(k) = store.patient_key_of_claim(id)? {
            if !keys.contains(&k) {
                keys.push(k);
            }
        }
    }
    if !claim_ids.is_empty() {
        ledger.append(kind, actor, now, claim_ids, &keys).map_err(PrivacyError::Refused)?;
    }
    let mut erased = vec![];
    let mut versions = 0;
    for id in claim_ids {
        let n = store.erase_claim(id)?;
        if n > 0 {
            erased.push(id.clone());
            versions += n;
        }
    }
    let mut destroyed = 0;
    for k in &keys {
        if store.forget_patient_key_if_orphaned(k)? {
            destroyed += 1;
        }
    }
    Ok(json!({"claims_erased": erased.len(), "versions_erased": versions, "patient_keys_destroyed": destroyed, "claim_ids": erased}))
}

/// After a restore: applies every erasure in the ledger again. Whatever a backup brought back is deleted, and the keys of those patients are
/// destroyed if the restore brought them back too. Safe to run any number of times; run it before the system serves anyone.
pub fn replay_ledger(store: &dyn QueueStore, ledger: &Ledger) -> R<Value> {
    let entries = ledger.read().map_err(PrivacyError::Refused)?;
    let (mut claims, mut versions, mut destroyed) = (0, 0, 0);
    let mut keys: Vec<String> = vec![];
    for e in &entries {
        for id in e["claim_ids"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            let n = store.erase_claim(id)?;
            if n > 0 {
                claims += 1;
                versions += n;
            }
        }
        for k in e["patient_keys"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            if !keys.iter().any(|x| x == k) {
                keys.push(k.to_string());
            }
        }
    }
    for k in &keys {
        if store.forget_patient_key_if_orphaned(k)? {
            destroyed += 1;
        }
    }
    Ok(json!({"ledger_entries": entries.len(), "claims_erased_again": claims, "versions_erased_again": versions, "patient_keys_destroyed": destroyed}))
}

/// Retention: erase claims decided more than `days` days ago (at most `limit` per run). `days == 0` means retention is off and nothing is touched.
pub fn run_retention(store: &dyn QueueStore, ledger: &Ledger, actor: &str, now: f64, days: u32, limit: usize, dry_run: bool) -> R<Value> {
    if days == 0 {
        return Ok(json!({"retention": "off", "claims": 0, "versions": 0, "dry_run": dry_run}));
    }
    let cutoff = now - f64::from(days) * 86_400.0;
    let ids = store.closed_before(cutoff, limit)?;
    if dry_run {
        return Ok(json!({"retention_days": days, "cutoff": cutoff, "claims": ids.len(), "versions": 0, "dry_run": true, "claim_ids": ids}));
    }
    // a claim that became active between the listing and now is skipped, not an error: the next run sees it again
    let ready: Vec<String> = ids.into_iter().filter(|id| erase_preview(store, std::slice::from_ref(id)).is_ok()).collect();
    let r = erase_claims(store, ledger, &ready, "retention", actor, now)?;
    Ok(json!({"retention_days": days, "cutoff": cutoff, "claims": r["claims_erased"], "versions": r["versions_erased"], "patient_keys_destroyed": r["patient_keys_destroyed"], "dry_run": false, "claim_ids": r["claim_ids"]}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{make_doc, NOW};
    use crate::store::MemoryQueueStore;

    fn ledger() -> (tempfile::TempDir, Ledger) {
        let d = tempfile::tempdir().unwrap();
        let l = Ledger::open(d.path().join("erasure_ledger.jsonl"));
        (d, l)
    }

    fn store() -> MemoryQueueStore {
        let s = MemoryQueueStore::new();
        for (id, p) in [("A1", "PAT-A"), ("A2", "PAT-A"), ("B1", "PAT-B")] {
            assert!(s.put_triaged(&make_doc(id, 1, &format!("h-{id}"), "A", "decide", 2, p, NOW, None)).unwrap());
        }
        s
    }

    #[test]
    fn subject_access_returns_the_patients_claims_and_nobody_elses() {
        let s = store();
        s.append_request_event(&json!({"request_id": "Q1", "claim_id": "A1", "kind": "sent", "at": NOW})).unwrap();
        let e = export_subject(&s, "PAT-A", NOW + 5.0).unwrap();
        assert_eq!((e["totals"]["claims"].as_i64(), e["totals"]["versions"].as_i64()), (Some(2), Some(2)));
        let text = e.to_string();
        assert!(text.contains("A1") && text.contains("A2") && !text.contains("B1") && !text.contains("PAT-B"));
        assert_eq!(e["claims"][0]["request_events"].as_array().unwrap().len(), 1, "the request events come with their claim");
        assert_eq!(export_subject(&s, "PAT-NOBODY", NOW).unwrap()["totals"]["claims"], 0);
        assert!(claim_ids_of(&s, "").is_err() && claim_ids_of(&s, &"x".repeat(121)).is_err());
    }

    #[test]
    fn erasure_deletes_every_version_and_leaves_other_people_alone() {
        let s = store();
        assert!(s.put_triaged(&make_doc("A1", 2, "h-A1-2", "A", "decide", 2, "PAT-A", NOW + 1.0, None)).unwrap());
        let (_d, l) = ledger();
        let ids = claim_ids_of(&s, "PAT-A").unwrap();
        assert_eq!(ids, ["A1", "A2"]);
        assert_eq!(erase_preview(&s, &ids).unwrap()["claims"], 2);
        let r = erase_claims(&s, &l, &ids, "erase", "CG-4004", NOW).unwrap();
        assert_eq!((r["claims_erased"].as_i64(), r["versions_erased"].as_i64()), (Some(2), Some(3)));
        assert!(export_subject(&s, "PAT-A", NOW).unwrap()["claims"].as_array().unwrap().is_empty(), "nothing of the patient is left to export");
        assert!(s.get("B1", None).unwrap().is_some(), "another patient's claim is untouched");
        assert_eq!(erase_claims(&s, &l, &ids, "erase", "CG-4004", NOW).unwrap()["claims_erased"], 0, "a second erasure finds nothing and is harmless");
        let e = l.read().unwrap();
        assert_eq!((e[0]["claim_ids"].as_array().unwrap().len(), e[0]["kind"].as_str()), (2, Some("erase")), "the erasure was written down before it was done");
    }

    #[test]
    fn a_claim_somebody_is_working_on_is_not_erased_from_under_them() {
        let s = store();
        assert!(s.transition("A1", 1, "triaged", "explanation_skipped", "system:t", NOW + 1.0, None, None, None).unwrap().is_some());
        assert!(s.transition("A1", 1, "explanation_skipped", "ready", "system:t", NOW + 2.0, None, None, None).unwrap().is_some());
        assert!(s.lease("A1", 1, "CG-2002", NOW + 3.0, NOW + 100.0).unwrap().is_some());
        let (_d, l) = ledger();
        let e = erase_claims(&s, &l, &["A1".to_string(), "A2".to_string()], "erase", "CG-4004", NOW).unwrap_err().to_string();
        assert!(e.contains("being worked on") && e.contains("A1"), "{e}");
        assert!(l.read().unwrap().is_empty(), "a refused erasure leaves no ledger line");
        assert!(s.get("A2", None).unwrap().is_some(), "nothing at all is erased when one claim is refused");
    }

    #[test]
    fn retention_erases_only_what_was_decided_long_ago_and_is_off_by_default() {
        let s = store();
        let decide = |id: &str, at: f64| {
            s.transition(id, 1, "triaged", "explanation_skipped", "system:t", at - 2.0, None, None, None).unwrap().unwrap();
            s.transition(id, 1, "explanation_skipped", "ready", "system:t", at - 1.0, None, None, None).unwrap().unwrap();
            s.lease(id, 1, "CG-2002", at - 0.5, at + 100.0).unwrap().unwrap();
            s.transition(id, 1, "leased", "decided", "CG-2002", at, None, None, Some("CG-2002")).unwrap().unwrap();
        };
        let (_d, l) = ledger();
        let day = 86_400.0;
        decide("A1", NOW);
        decide("B1", NOW + 400.0 * day);
        let now = NOW + 500.0 * day;
        assert_eq!(run_retention(&s, &l, "system:worker", now, 0, 100, false).unwrap()["retention"], "off");
        assert!(s.get("A1", None).unwrap().is_some(), "with retention off nothing is touched");
        let dry = run_retention(&s, &l, "system:worker", now, 365, 100, true).unwrap();
        assert_eq!((dry["claims"].as_i64(), dry["dry_run"].as_bool()), (Some(1), Some(true)));
        assert!(s.get("A1", None).unwrap().is_some(), "a dry run deletes nothing");
        let real = run_retention(&s, &l, "system:worker", now, 365, 100, false).unwrap();
        assert_eq!((real["claims"].as_i64(), real["claim_ids"][0].as_str()), (Some(1), Some("A1")));
        assert!(s.get("A1", None).unwrap().is_none(), "A1 was decided 500 days ago");
        assert!(s.get("B1", None).unwrap().is_some(), "B1 was decided 100 days ago");
        assert!(s.get("A2", None).unwrap().is_some(), "A2 was never decided");
        assert_eq!(l.read().unwrap().len(), 1, "the retention run was written down, the dry run was not");
    }

    #[test]
    fn replaying_the_ledger_deletes_what_a_restored_backup_brings_back() {
        let (_d, l) = ledger();
        let live = store();
        erase_claims(&live, &l, &claim_ids_of(&live, "PAT-A").unwrap(), "erase", "CG-4004", NOW).unwrap();
        // a backup taken before the erasure is restored: A1 and A2 are back, and so is B1
        let restored = store();
        assert_eq!(export_subject(&restored, "PAT-A", NOW).unwrap()["totals"]["claims"], 2);
        let r = replay_ledger(&restored, &l).unwrap();
        assert_eq!((r["claims_erased_again"].as_i64(), r["ledger_entries"].as_i64()), (Some(2), Some(1)));
        assert_eq!(export_subject(&restored, "PAT-A", NOW).unwrap()["totals"]["claims"], 0, "the erased patient is gone again");
        assert!(restored.get("B1", None).unwrap().is_some(), "nobody else is touched");
        assert_eq!(replay_ledger(&restored, &l).unwrap()["claims_erased_again"], 0, "replaying twice changes nothing");
        std::fs::write(l.path(), "garbage\n").unwrap();
        assert!(replay_ledger(&restored, &l).is_err(), "a damaged ledger stops the replay instead of being skipped");
    }
}
