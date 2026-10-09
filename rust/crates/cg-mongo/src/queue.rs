//! The MongoDB queue store (`src/workqueue/store_mongo.py`).
//!
//! The guarantees that matter are enforced by the database, not by code that could race:
//!   * a claim version is stored once: unique indexes on (claim_id, version) and on (claim_id, input_hash, receipt.rule_pack_hash);
//!   * every state move and every lease is one find_one_and_update filtered on the expected state, so of 50 racing callers exactly one wins;
//!   * a configuration version exists once: a unique index on version;
//!   * a rate counter never passes its limit: one upsert with $inc that only matches while the count is below the limit;
//!   * cache entries and counters disappear on their own through TTL indexes.
//!
//! Any driver failure except a duplicate key becomes `StoreError::Unavailable`, which callers treat as "refuse".

use crate::bsonconv::{from_doc, to_bson, to_doc};
use cg_access::store::StoreError;
use cg_queue::states;
use cg_queue::store::{check_config_doc, check_decision, check_id_doc, check_job, check_number, check_set_fields, check_version, lane_key, prepare_new_doc, project_history, QueueStore, Result, DEALABLE};
use mongodb::bson::{doc, Bson, DateTime, Document};
use mongodb::options::{IndexOptions, ReturnDocument};
use mongodb::sync::{Client, Collection};
use mongodb::IndexModel;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

const CACHE_TTL_SECS: i64 = 7 * 86_400;
const COUNTER_TTL_SECS: i64 = 3 * 86_400;
const BUMP_RETRIES: usize = 5;

fn unavailable(_: mongodb::error::Error) -> StoreError {
    StoreError::Unavailable
}

fn duplicate(e: &mongodb::error::Error) -> bool {
    use mongodb::error::{ErrorKind, WriteFailure};
    match e.kind.as_ref() {
        ErrorKind::Write(WriteFailure::WriteError(w)) => w.code == 11000,
        ErrorKind::InsertMany(m) => m.write_errors.iter().flatten().any(|w| w.code == 11000),
        ErrorKind::Command(c) => c.code == 11000,
        _ => false,
    }
}

fn after_now(secs: i64) -> DateTime {
    DateTime::from_millis(DateTime::now().timestamp_millis() + secs * 1000)
}

fn opt(d: Option<Document>) -> Option<Value> {
    d.map(|d| from_doc(&d))
}

fn many(c: mongodb::sync::Cursor<Document>) -> Result<Vec<Value>> {
    c.map(|r| r.map(|d| from_doc(&d)).map_err(unavailable)).collect()
}

pub struct MongoQueueStore {
    client: Client,
    db_name: String,
    claims: Collection<Document>,
    configs: Collection<Document>,
    deals: Collection<Document>,
    dead: Collection<Document>,
    cache: Collection<Document>,
    jobs: Collection<Document>,
    counters: Collection<Document>,
}

impl MongoQueueStore {
    pub fn new(client: Client, db_name: &str) -> MongoQueueStore {
        let db = client.database(db_name);
        MongoQueueStore {
            claims: db.collection("claims"),
            configs: db.collection("configs"),
            deals: db.collection("deals"),
            dead: db.collection("dead_letters"),
            cache: db.collection("cache"),
            jobs: db.collection("jobs"),
            counters: db.collection("counters"),
            client,
            db_name: db_name.to_string(),
        }
    }

    pub fn ensure_indexes(&self) -> Result<()> {
        let unique = || IndexOptions::builder().unique(true).build();
        let ttl = || IndexOptions::builder().expire_after(Duration::from_secs(0)).build();
        let plain = |keys: Document| IndexModel::builder().keys(keys).build();
        let with = |keys: Document, o: IndexOptions| IndexModel::builder().keys(keys).options(o).build();
        let go = |c: &Collection<Document>, m: IndexModel| c.create_index(m).run().map(|_| ()).map_err(unavailable);
        go(&self.claims, with(doc! {"claim_id": 1, "version": 1}, unique()))?;
        go(&self.claims, with(doc! {"claim_id": 1, "input_hash": 1, "receipt.rule_pack_hash": 1}, unique()))?;
        go(&self.claims, plain(doc! {"state": 1, "state_at": 1}))?;
        go(&self.claims, plain(doc! {"lease.badge_id": 1}))?;
        go(&self.claims, plain(doc! {"enqueue_pending": 1}))?;
        go(&self.claims, plain(doc! {"claim.patient_id": 1}))?;
        go(&self.configs, with(doc! {"version": 1}, unique()))?;
        go(&self.dead, with(doc! {"dead_id": 1}, unique()))?;
        go(&self.cache, with(doc! {"key": 1}, unique()))?;
        go(&self.jobs, with(doc! {"name": 1}, unique()))?;
        go(&self.cache, with(doc! {"expires_at": 1}, ttl()))?;
        go(&self.counters, with(doc! {"counter": 1, "window": 1}, unique()))?;
        go(&self.counters, with(doc! {"expires_at": 1}, ttl()))?;
        Ok(())
    }

    pub fn drop_database(&self) {
        let _ = self.client.database(&self.db_name).drop().run();
    }
}

impl QueueStore for MongoQueueStore {
    fn put_triaged(&self, doc: &Value) -> Result<bool> {
        let full = prepare_new_doc(doc)?;
        match self.claims.insert_one(to_doc(&full)).run() {
            Ok(_) => Ok(true),
            Err(e) if duplicate(&e) => Ok(false),
            Err(e) => Err(unavailable(e)),
        }
    }

    fn get(&self, claim_id: &str, version: Option<i64>) -> Result<Option<Value>> {
        match version {
            None => Ok(opt(self.claims.find_one(doc! {"claim_id": claim_id}).sort(doc! {"version": -1}).run().map_err(unavailable)?)),
            Some(v) => {
                check_version(v)?;
                Ok(opt(self.claims.find_one(doc! {"claim_id": claim_id, "version": v}).run().map_err(unavailable)?))
            }
        }
    }

    fn transition(&self, claim_id: &str, version: i64, from: &str, to: &str, actor: &str, now: f64, detail: Option<Value>, set_fields: Option<Map<String, Value>>, holder: Option<&str>)
        -> Result<Option<Value>> {
        check_version(version)?;
        check_number(now, "now")?;
        let event = states::make_event(from, to, actor, now, detail).map_err(StoreError::Invalid)?;
        let fields = check_set_fields(&set_fields)?;
        let mut filter = doc! {"claim_id": claim_id, "version": version, "state": from};
        if let Some(h) = holder {
            filter.insert("lease.badge_id", h);
        }
        let mut set = to_doc(&Value::Object(fields));
        set.insert("state", to);
        set.insert("state_at", now);
        let found = self.claims.find_one_and_update(filter, doc! {"$set": set, "$push": {"events": to_bson(&event)}}).return_document(ReturnDocument::After).run().map_err(unavailable)?;
        Ok(opt(found))
    }

    fn add_decision(&self, claim_id: &str, version: i64, badge_id: &str, now: f64, decision: &Value) -> Result<Option<Value>> {
        check_version(version)?;
        check_number(now, "now")?;
        check_decision(decision)?;
        let found = self
            .claims
            .find_one_and_update(
                doc! {"claim_id": claim_id, "version": version, "state": "leased", "lease.badge_id": badge_id, "lease.expires_at": {"$gt": now}},
                doc! {"$push": {"decisions": to_bson(decision)}},
            )
            .return_document(ReturnDocument::After)
            .run()
            .map_err(unavailable)?;
        Ok(opt(found))
    }

    fn set_shadow(&self, claim_id: &str, version: i64, shadow: &Value) -> Result<bool> {
        check_version(version)?;
        check_decision(shadow)?;
        let r = self.claims.update_one(doc! {"claim_id": claim_id, "version": version, "shadow": Bson::Null}, doc! {"$set": {"shadow": to_bson(shadow)}}).run().map_err(unavailable)?;
        Ok(r.modified_count == 1)
    }

    fn latest_versions(&self, claim_ids: &[String]) -> Result<HashMap<String, i64>> {
        if claim_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let ids: Vec<Bson> = claim_ids.iter().map(|c| Bson::String(c.clone())).collect();
        let rows = self
            .claims
            .aggregate(vec![doc! {"$match": {"claim_id": {"$in": ids}}}, doc! {"$group": {"_id": "$claim_id", "v": {"$max": "$version"}}}])
            .run()
            .map_err(unavailable)?;
        let mut out = HashMap::new();
        for r in rows {
            let r = r.map_err(unavailable)?;
            if let (Ok(id), Some(v)) = (r.get_str("_id"), r.get("v").and_then(|v| v.as_i64().or_else(|| v.as_i32().map(i64::from)))) {
                out.insert(id.to_string(), v);
            }
        }
        Ok(out)
    }

    fn leased_summary(&self) -> Result<Vec<Value>> {
        let rows = self
            .claims
            .find(doc! {"state": "leased", "lease": {"$ne": Bson::Null}})
            .projection(doc! {"_id": 0, "claim_id": 1, "version": 1, "lease.badge_id": 1, "receipt.eligibility": 1, "escalated": 1, "signoff": 1})
            .run()
            .map_err(unavailable)?;
        many(rows).map(|v| {
            v.into_iter()
                .map(|r| json!({"claim_id": r["claim_id"], "version": r["version"], "badge_id": r["lease"]["badge_id"], "eligibility": r["receipt"]["eligibility"],
                                "escalated": r["escalated"].as_bool().unwrap_or(false), "signoff": !r["signoff"].is_null()}))
                .collect()
        })
    }

    fn inbox_load(&self, badge_id: &str) -> Result<(i64, i64)> {
        let mut rows = self
            .claims
            .aggregate(vec![
                doc! {"$match": {"state": "leased", "lease.badge_id": badge_id}},
                doc! {"$group": {"_id": Bson::Null, "n": {"$sum": 1}, "p": {"$sum": {"$ifNull": ["$receipt.score", 0]}}}},
            ])
            .run()
            .map_err(unavailable)?;
        match rows.next() {
            Some(r) => {
                let r = r.map_err(unavailable)?;
                let num = |k: &str| r.get(k).and_then(|v| v.as_i64().or_else(|| v.as_i32().map(i64::from)).or_else(|| v.as_f64().map(|f| f as i64))).unwrap_or(0);
                Ok((num("n"), num("p")))
            }
            None => Ok((0, 0)),
        }
    }

    fn pending_outbox(&self, limit: usize) -> Result<Vec<Value>> {
        if limit == 0 {
            return Ok(vec![]);
        }
        many(self.claims.find(doc! {"enqueue_pending": true}).sort(doc! {"receipt.created_at": 1, "claim_id": 1}).limit(limit as i64).run().map_err(unavailable)?)
    }

    fn clear_outbox(&self, claim_id: &str, version: i64) -> Result<bool> {
        check_version(version)?;
        let r = self.claims.update_one(doc! {"claim_id": claim_id, "version": version, "enqueue_pending": true}, doc! {"$set": {"enqueue_pending": false}}).run().map_err(unavailable)?;
        Ok(r.modified_count == 1)
    }

    fn by_state(&self, state: &str, limit: usize) -> Result<Vec<Value>> {
        if limit == 0 {
            return Ok(vec![]);
        }
        many(self.claims.find(doc! {"state": state}).sort(doc! {"state_at": 1, "claim_id": 1}).limit(limit as i64).run().map_err(unavailable)?)
    }

    fn counts(&self) -> Result<BTreeMap<String, i64>> {
        let rows = self
            .claims
            .aggregate(vec![doc! {"$group": {"_id": {"state": "$state", "lane": "$receipt.lane", "eligibility": "$receipt.eligibility"}, "n": {"$sum": 1}}}])
            .run()
            .map_err(unavailable)?;
        let mut out = BTreeMap::new();
        for r in rows {
            let r = r.map_err(unavailable)?;
            let id = r.get_document("_id").map_err(|_| StoreError::Unavailable)?;
            let key = lane_key(&json!({"state": id.get_str("state").unwrap_or(""), "receipt": {"lane": id.get_str("lane").unwrap_or(""), "eligibility": id.get_str("eligibility").unwrap_or("")}}));
            out.insert(key, r.get("n").and_then(|v| v.as_i64().or_else(|| v.as_i32().map(i64::from))).unwrap_or(0));
        }
        Ok(out)
    }

    fn lease(&self, claim_id: &str, version: i64, badge_id: &str, now: f64, expires_at: f64) -> Result<Option<Value>> {
        check_version(version)?;
        check_number(now, "now")?;
        check_number(expires_at, "expires_at")?;
        let lease = doc! {"badge_id": badge_id, "leased_at": now, "expires_at": expires_at, "heartbeat_at": now};
        for from in DEALABLE {
            let event = states::make_event(from, "leased", badge_id, now, Some(json!({"expires_at": expires_at}))).map_err(StoreError::Invalid)?;
            let found = self
                .claims
                .find_one_and_update(
                    doc! {"claim_id": claim_id, "version": version, "state": from},
                    doc! {"$set": {"state": "leased", "state_at": now, "lease": lease.clone()}, "$push": {"events": to_bson(&event)}},
                )
                .return_document(ReturnDocument::After)
                .run()
                .map_err(unavailable)?;
            if found.is_some() {
                return Ok(opt(found));
            }
        }
        Ok(None)
    }

    fn inbox(&self, badge_id: &str) -> Result<Vec<Value>> {
        many(self.claims.find(doc! {"state": "leased", "lease.badge_id": badge_id}).sort(doc! {"lease.leased_at": 1, "claim_id": 1}).run().map_err(unavailable)?)
    }

    fn heartbeat(&self, badge_id: &str, now: f64, expires_at: f64) -> Result<i64> {
        check_number(now, "now")?;
        check_number(expires_at, "expires_at")?;
        let r = self
            .claims
            .update_many(doc! {"state": "leased", "lease.badge_id": badge_id}, doc! {"$set": {"lease.expires_at": expires_at, "lease.heartbeat_at": now}})
            .run()
            .map_err(unavailable)?;
        Ok(r.matched_count as i64)
    }

    fn expired(&self, now: f64) -> Result<Vec<Value>> {
        check_number(now, "now")?;
        many(self.claims.find(doc! {"state": "leased", "lease.expires_at": {"$lte": now}}).sort(doc! {"lease.expires_at": 1, "claim_id": 1}).run().map_err(unavailable)?)
    }

    fn claims_for_patient(&self, patient_id: &str) -> Result<Vec<Value>> {
        let ids = self.claims.distinct("claim_id", doc! {"claim.patient_id": patient_id}).run().map_err(unavailable)?;
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let rows = many(self.claims.find(doc! {"claim_id": {"$in": ids}}).projection(doc! {"_id": 0, "claim_id": 1, "version": 1, "claim": 1}).run().map_err(unavailable)?)?;
        let mut latest: BTreeMap<String, Value> = BTreeMap::new();
        for d in rows {
            let id = d["claim_id"].as_str().unwrap_or("").to_string();
            if latest.get(&id).map_or(true, |cur| d["version"].as_i64() > cur["version"].as_i64()) {
                latest.insert(id, d);
            }
        }
        Ok(latest.values().filter(|d| d["claim"]["patient_id"] == patient_id).map(|d| project_history(&d["claim"])).collect())
    }

    fn put_config(&self, doc: &Value, expected_version: i64) -> Result<bool> {
        check_config_doc(doc, expected_version)?;
        let top = self.configs.find_one(doc! {}).sort(doc! {"version": -1}).run().map_err(unavailable)?;
        let current = top.and_then(|t| t.get("version").and_then(|v| v.as_i64().or_else(|| v.as_i32().map(i64::from)))).unwrap_or(0);
        if current != expected_version {
            return Ok(false);
        }
        match self.configs.insert_one(to_doc(doc)).run() {
            Ok(_) => Ok(true),
            Err(e) if duplicate(&e) => Ok(false),
            Err(e) => Err(unavailable(e)),
        }
    }

    fn latest_config(&self) -> Result<Option<Value>> {
        Ok(opt(self.configs.find_one(doc! {}).sort(doc! {"version": -1}).run().map_err(unavailable)?))
    }

    fn config_history(&self) -> Result<Vec<Value>> {
        many(self.configs.find(doc! {}).sort(doc! {"version": 1}).run().map_err(unavailable)?)
    }

    fn append_deal(&self, d: &Value) -> Result<()> {
        check_id_doc(d, "deal_id")?;
        self.deals.insert_one(to_doc(d)).run().map(|_| ()).map_err(unavailable)
    }

    fn get_deal(&self, deal_id: &str) -> Result<Option<Value>> {
        Ok(opt(self.deals.find_one(doc! {"deal_id": deal_id}).run().map_err(unavailable)?))
    }

    fn deals(&self, limit: usize) -> Result<Vec<Value>> {
        if limit == 0 {
            return Ok(vec![]);
        }
        many(self.deals.find(doc! {}).sort(doc! {"_id": -1}).limit(limit as i64).run().map_err(unavailable)?)
    }

    fn add_dead_letter(&self, d: &Value) -> Result<()> {
        check_id_doc(d, "dead_id")?;
        let id = d["dead_id"].as_str().unwrap_or("");
        self.dead.replace_one(doc! {"dead_id": id}, to_doc(d)).upsert(true).run().map(|_| ()).map_err(unavailable)
    }

    fn dead_letters(&self, limit: usize) -> Result<Vec<Value>> {
        if limit == 0 {
            return Ok(vec![]);
        }
        many(self.dead.find(doc! {}).sort(doc! {"_id": 1}).limit(limit as i64).run().map_err(unavailable)?)
    }

    fn pop_dead_letter(&self, dead_id: &str) -> Result<Option<Value>> {
        Ok(opt(self.dead.find_one_and_delete(doc! {"dead_id": dead_id}).run().map_err(unavailable)?))
    }

    fn cache_get(&self, key: &str) -> Result<Option<String>> {
        let row = self.cache.find_one(doc! {"key": key, "expires_at": {"$gt": DateTime::now()}}).run().map_err(unavailable)?;
        Ok(row.and_then(|r| r.get_str("text").ok().map(str::to_string)))
    }

    fn cache_put(&self, key: &str, text: &str) -> Result<()> {
        self.cache.replace_one(doc! {"key": key}, doc! {"key": key, "text": text, "expires_at": after_now(CACHE_TTL_SECS)}).upsert(true).run().map(|_| ()).map_err(unavailable)
    }

    fn bump(&self, counter: &str, window_key: &str, limit: i64) -> Result<bool> {
        if limit < 0 {
            return Err(StoreError::Invalid("limit must be a whole number of 0 or more".into()));
        }
        if limit == 0 {
            return Ok(false);
        }
        let key = doc! {"counter": counter, "window": window_key};
        for _ in 0..BUMP_RETRIES {
            let mut filter = key.clone();
            filter.insert("count", doc! {"$lt": limit});
            match self.counters.update_one(filter, doc! {"$inc": {"count": 1_i64}, "$setOnInsert": {"expires_at": after_now(COUNTER_TTL_SECS)}}).upsert(true).run() {
                Ok(_) => return Ok(true),
                Err(e) if duplicate(&e) => {
                    let row = self.counters.find_one(key.clone()).run().map_err(unavailable)?;
                    if row.and_then(|r| r.get("count").and_then(|v| v.as_i64().or_else(|| v.as_i32().map(i64::from)))).is_some_and(|c| c >= limit) {
                        return Ok(false);
                    }
                }
                Err(e) => return Err(unavailable(e)),
            }
        }
        Err(StoreError::Unavailable)
    }

    fn record_job(&self, name: &str, now: f64, ok: bool, detail: &str) -> Result<()> {
        check_job(name, now, detail)?;
        let mut set = doc! {"at": now, "ok": ok, "detail": detail};
        if ok {
            set.insert("last_ok_at", now);
        }
        self.jobs.update_one(doc! {"name": name}, doc! {"$set": set, "$inc": {"runs": 1_i64, "failures": if ok { 0_i64 } else { 1_i64 }}}).upsert(true).run().map(|_| ()).map_err(unavailable)
    }

    fn jobs(&self) -> Result<Vec<Value>> {
        let mut rows = many(self.jobs.find(doc! {}).sort(doc! {"name": 1}).run().map_err(unavailable)?)?;
        for r in rows.iter_mut() {
            if r.get("last_ok_at").is_none() {
                r["last_ok_at"] = Value::Null;
            }
        }
        Ok(rows)
    }

    fn ping(&self) -> bool {
        self.client.database("admin").run_command(doc! {"ping": 1}).run().is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mongo_queue_store_meets_the_same_contract_as_the_memory_twin() {
        let Ok(uri) = std::env::var("MONGO_URI") else {
            assert!(std::env::var("REQUIRE_MONGO").is_err(), "REQUIRE_MONGO is set but MONGO_URI is not");
            return;
        };
        let names = std::sync::Mutex::new(vec![]);
        let make = || -> Box<dyn QueueStore> {
            let name = format!("claimguard_rs_qtest_{}", rand::random::<u64>());
            let store = MongoQueueStore::new(crate::connect(&uri, 3000).expect("connect"), &name);
            store.ensure_indexes().expect("indexes");
            names.lock().unwrap().push(name);
            Box::new(store)
        };
        cg_queue::contract::run(&make);
        let client = crate::connect(&uri, 3000).unwrap();
        for n in names.lock().unwrap().iter() {
            let _ = client.database(n).drop().run();
        }
    }
}
