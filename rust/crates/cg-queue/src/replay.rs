//! Replay and rerun: prove what a stored receipt says, and apply a new rule pack without rewriting history (`src/workqueue/replay.py`).
//!
//! `replay_claim` re-runs the stored claim through the engine and compares result hashes, and also checks that the stored claim and
//! results still match the hashes written on the receipt (a claim edited in the database after intake is caught here). `rerun` finds the
//! claims evaluated by an old rule pack; only those whose results change under the new one become a new version with a new receipt, and
//! the old version, its decisions and its audit trail stay exactly as they were.

use crate::flow::{EngineFn, Intake};
use crate::service::QueueError;
use crate::states;
use crate::store::QueueStore;
use crate::triage;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

const BIG: usize = 100_000;

type R<T> = Result<T, QueueError>;

fn store_err(e: cg_access::store::StoreError) -> QueueError {
    QueueError::Invalid(e.to_string())
}

fn status_row(row: &Value) -> Value {
    json!([row["status"], row["severity"], row["affected_line_ids"]])
}

pub fn replay_claim(store: &dyn QueueStore, engine: &EngineFn, claim_id: &str, version: Option<i64>) -> R<Value> {
    let doc = store.get(claim_id, version).map_err(store_err)?.ok_or_else(|| QueueError::NotFound(claim_id.to_string()))?;
    let results = engine(&doc["claim"]).map_err(QueueError::Invalid)?;
    let receipt = &doc["receipt"];
    let new_hash = triage::result_hash(&results);
    let rows = |v: &Value| -> BTreeMap<String, Value> { v.as_array().into_iter().flatten().filter(|r| r.is_object()).map(|r| (r["rule_id"].as_str().unwrap_or("").to_string(), status_row(r))).collect() };
    let (old, new) = (rows(&doc["results"]), rows(&results));
    let ids: BTreeSet<&String> = old.keys().chain(new.keys()).collect();
    let diff: Vec<Value> = ids.into_iter().filter(|id| old.get(*id) != new.get(*id)).map(|id| json!({"rule_id": id, "was": old.get(id), "now": new.get(id)})).collect();
    Ok(json!({
        "claim_id": claim_id, "version": doc["version"], "same": receipt["result_hash"] == new_hash.as_str(), "old_hash": receipt["result_hash"], "new_hash": new_hash, "diff": diff,
        "claim_intact": triage::input_hash(&doc["claim"]) == receipt["input_hash"].as_str().unwrap_or(""),
        "results_intact": triage::result_hash(&doc["results"]) == receipt["result_hash"].as_str().unwrap_or(""),
    }))
}

/// The newest version of every claim.
pub fn latest_documents(store: &dyn QueueStore) -> R<Vec<Value>> {
    let mut newest: BTreeMap<String, Value> = BTreeMap::new();
    for state in states::STATES {
        for d in store.by_state(state, BIG).map_err(store_err)? {
            let id = d["claim_id"].as_str().unwrap_or("").to_string();
            if newest.get(&id).is_none_or(|n| d["version"].as_i64() > n["version"].as_i64()) {
                newest.insert(id, d);
            }
        }
    }
    Ok(newest.into_values().collect())
}

pub fn rerun(store: &dyn QueueStore, engine: &EngineFn, old_rule_pack_hash: &str, intake: &Intake, dry_run: bool) -> R<Value> {
    if intake.rule_pack_hash == old_rule_pack_hash {
        return Err(QueueError::Invalid("the running rule pack is the one to be replaced; there is nothing to rerun".into()));
    }
    let (mut checked, mut changed, mut errors) = (0, vec![], vec![]);
    for doc in latest_documents(store)? {
        if doc["receipt"]["rule_pack_hash"] != old_rule_pack_hash {
            continue;
        }
        checked += 1;
        let id = doc["claim_id"].as_str().unwrap_or("").to_string();
        match engine(&doc["claim"]) {
            Ok(r) if triage::result_hash(&r) != doc["receipt"]["result_hash"].as_str().unwrap_or("") => changed.push(id),
            Ok(_) => {}
            Err(_) => errors.push(id), // one bad claim must not stop the rerun; it is reported
        }
    }
    let mut created = vec![];
    if !dry_run {
        for id in &changed {
            let claim = store.get(id, None).map_err(store_err)?.map(|d| d["claim"].clone()).unwrap_or(Value::Null);
            intake.submit(&claim).map_err(|e| QueueError::Invalid(e.to_string()))?;
            let version = store.get(id, None).map_err(store_err)?.map(|d| d["version"].clone()).unwrap_or(Value::Null);
            created.push(json!({"claim_id": id, "version": version}));
        }
    }
    Ok(json!({"dry_run": dry_run, "checked": checked, "changed": changed, "unchanged": checked - changed.len() - errors.len(), "errors": errors, "created": created}))
}

/// Move a dead-lettered claim back to `triaged` (and re-arm its publish marker), once only.
pub fn replay_dead_letter(store: &dyn QueueStore, dead_id: &str, now: f64, actor: &str) -> R<Value> {
    let letters = store.dead_letters(BIG).map_err(store_err)?;
    let letter = letters.iter().find(|d| d["dead_id"] == dead_id).ok_or_else(|| QueueError::NotFound(dead_id.to_string()))?;
    let mut set = serde_json::Map::new();
    set.insert("enqueue_pending".into(), json!(true));
    let moved = store
        .transition(letter["claim_id"].as_str().unwrap_or(""), letter["version"].as_i64().unwrap_or(0), "dead_lettered", "triaged", actor, now, Some(json!({"event": "dead_letter_replay", "dead_id": dead_id})), Some(set), None)
        .map_err(store_err)?
        .ok_or_else(|| QueueError::Conflict("the claim is no longer dead-lettered".into()))?;
    store.pop_dead_letter(dead_id).map_err(store_err)?;
    Ok(moved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemoryQueueStore;
    use std::sync::Arc;

    #[test]
    fn a_dead_letter_returns_once_and_only_once() {
        let store = MemoryQueueStore::new();
        let claim = json!({"claim_id": "C1"});
        let doc = json!({"claim_id": "C1", "version": 1, "input_hash": "h", "claim": claim, "results": [], "receipt": {"created_at": 1.0, "lane": "A", "score": 0, "config_version": 1, "eligibility": "any"}, "advisory": []});
        assert!(store.put_triaged(&doc).unwrap());
        store.transition("C1", 1, "triaged", "dead_lettered", "system", 2.0, None, None, None).unwrap().unwrap();
        store.add_dead_letter(&json!({"dead_id": "D1", "claim_id": "C1", "version": 1, "reason": "x", "attempts": 3, "at": 2.0})).unwrap();
        let moved = replay_dead_letter(&store, "D1", 3.0, "CG-4004").unwrap();
        assert_eq!(moved["state"], "triaged");
        assert!(matches!(replay_dead_letter(&store, "D1", 4.0, "CG-4004"), Err(QueueError::NotFound(_))));
        let _unused: Option<Arc<dyn QueueStore>> = None;
    }
}
