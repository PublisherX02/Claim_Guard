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
use crate::crypt::FieldCrypt;
use cg_access::store::StoreError;
use cg_queue::states;
use cg_queue::store::{check_batch_event, check_config_doc, check_reliability_doc, check_reliability_move, reliability_event, check_decision, check_id_doc, check_job, check_number, check_set_fields, check_version, lane_key, prepare_new_doc, project_history, QueueStore, Result, DEALABLE};
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


pub struct MongoQueueStore {
    client: Client,
    db_name: String,
    claims: Collection<Document>,
    configs: Collection<Document>,
    reliability: Collection<Document>,
    deals: Collection<Document>,
    batch_events: Collection<Document>,
    dead: Collection<Document>,
    cache: Collection<Document>,
    jobs: Collection<Document>,
    counters: Collection<Document>,
    crypt: Option<FieldCrypt>,
}

impl MongoQueueStore {
    /// Encrypts claim data (see `crypt`) on every write and decrypts it on every read. Documents already stored in plain form stay readable.
    pub fn with_encryption(mut self, crypt: FieldCrypt) -> MongoQueueStore {
        self.crypt = Some(crypt);
        self
    }

    /// Re-encrypts every claim document from this store's key to `new`. Stop the API and the worker first: a document changed by someone
    /// else in the middle is skipped and reported, not overwritten. Safe to run again after an interruption: a document that already opens
    /// with the new key is counted and left alone. Returns (rewritten, already on the new key, skipped because it changed meanwhile).
    pub fn reencrypt_all(&self, new: &FieldCrypt, dry_run: bool) -> Result<(usize, usize, usize)> {
        let old = self.crypt.as_ref().ok_or_else(|| StoreError::Invalid("this store has no encryption key to rotate from".into()))?;
        let (mut rewritten, mut current, mut skipped) = (0, 0, 0);
        let all: Vec<Document> = self.claims.find(doc! {}).run().map_err(unavailable)?.collect::<std::result::Result<_, _>>().map_err(unavailable)?;
        for raw in all {
            let Some(id) = raw.get("_id").cloned() else { continue };
            let stored = from_doc(&raw);
            let plain = match old.open(stored.clone()) {
                Ok(p) => p,
                Err(_) => match new.open(stored.clone()) {
                    Ok(_) => {
                        current += 1;
                        continue;
                    }
                    Err(e) => return Err(StoreError::Invalid(format!("claim {} opens with neither key: {}", stored["claim_id"].as_str().unwrap_or("?"), e.0))),
                },
            };
            if dry_run {
                rewritten += 1;
                continue;
            }
            let replacement = to_doc(&new.seal_stored(&plain));
            let r = self.claims.replace_one(doc! {"_id": id, "state": raw.get("state").cloned().unwrap_or(Bson::Null), "state_at": raw.get("state_at").cloned().unwrap_or(Bson::Null)}, replacement).run().map_err(unavailable)?;
            if r.modified_count == 1 {
                rewritten += 1;
            } else {
                skipped += 1;
            }
        }
        Ok((rewritten, current, skipped))
    }

    fn open(&self, v: Value) -> Result<Value> {
        match &self.crypt {
            None => Ok(v),
            Some(c) => c.open(v).map_err(|e| StoreError::Invalid(e.0)),
        }
    }

    fn opt(&self, d: Option<Document>) -> Result<Option<Value>> {
        d.map(|d| self.open(from_doc(&d))).transpose()
    }

    fn many(&self, c: mongodb::sync::Cursor<Document>) -> Result<Vec<Value>> {
        let docs = c.map(|r| r.map(|d| from_doc(&d)).map_err(unavailable)).collect::<Result<Vec<Value>>>()?;
        docs.into_iter().map(|d| self.open(d)).collect()
    }

    pub fn new(client: Client, db_name: &str) -> MongoQueueStore {
        let db = client.database(db_name);
        MongoQueueStore {
            claims: db.collection("claims"),
            configs: db.collection("configs"),
            reliability: db.collection("reliability"),
            deals: db.collection("deals"),
            batch_events: db.collection("batch_events"),
            dead: db.collection("dead_letters"),
            cache: db.collection("cache"),
            jobs: db.collection("jobs"),
            counters: db.collection("counters"),
            crypt: None,
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
        go(&self.claims, plain(doc! {"patient_key": 1}))?;
        go(&self.configs, with(doc! {"version": 1}, unique()))?;
        go(&self.reliability, with(doc! {"version": 1}, unique()))?;
        go(&self.batch_events, plain(doc! {"batch_id": 1}))?;
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
        let mut full = prepare_new_doc(doc)?;
        if let Some(c) = &self.crypt {
            c.seal_new_doc(&mut full);
        }
        match self.claims.insert_one(to_doc(&full)).run() {
            Ok(_) => Ok(true),
            Err(e) if duplicate(&e) => Ok(false),
            Err(e) => Err(unavailable(e)),
        }
    }

    fn get(&self, claim_id: &str, version: Option<i64>) -> Result<Option<Value>> {
        match version {
            None => self.opt(self.claims.find_one(doc! {"claim_id": claim_id}).sort(doc! {"version": -1}).run().map_err(unavailable)?),
            Some(v) => {
                check_version(v)?;
                self.opt(self.claims.find_one(doc! {"claim_id": claim_id, "version": v}).run().map_err(unavailable)?)
            }
        }
    }

    fn transition(&self, claim_id: &str, version: i64, from: &str, to: &str, actor: &str, now: f64, detail: Option<Value>, set_fields: Option<Map<String, Value>>, holder: Option<&str>)
        -> Result<Option<Value>> {
        check_version(version)?;
        check_number(now, "now")?;
        let event = states::make_event(from, to, actor, now, detail).map_err(StoreError::Invalid)?;
        let mut fields = check_set_fields(&set_fields)?;
        if let Some(c) = &self.crypt {
            c.seal_set(claim_id, version, &mut fields);
        }
        let mut filter = doc! {"claim_id": claim_id, "version": version, "state": from};
        if let Some(h) = holder {
            filter.insert("lease.badge_id", h);
        }
        let mut set = to_doc(&Value::Object(fields));
        set.insert("state", to);
        set.insert("state_at", now);
        let found = self.claims.find_one_and_update(filter, doc! {"$set": set, "$push": {"events": to_bson(&event)}}).return_document(ReturnDocument::After).run().map_err(unavailable)?;
        self.opt(found)
    }

    fn add_decision(&self, claim_id: &str, version: i64, badge_id: &str, now: f64, decision: &Value) -> Result<Option<Value>> {
        check_version(version)?;
        check_number(now, "now")?;
        check_decision(decision)?;
        let found = self
            .claims
            .find_one_and_update(
                doc! {"claim_id": claim_id, "version": version, "state": "leased", "lease.badge_id": badge_id, "lease.expires_at": {"$gt": now}},
                doc! {"$push": {"decisions": to_bson(&match &self.crypt {
                    Some(c) => c.seal(claim_id, version, "decisions", decision),
                    None => decision.clone(),
                })}},
            )
            .return_document(ReturnDocument::After)
            .run()
            .map_err(unavailable)?;
        self.opt(found)
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
        self.many(rows).map(|v| {
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
        self.many(self.claims.find(doc! {"enqueue_pending": true}).sort(doc! {"receipt.created_at": 1, "claim_id": 1}).limit(limit as i64).run().map_err(unavailable)?)
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
        self.many(self.claims.find(doc! {"state": state}).sort(doc! {"state_at": 1, "claim_id": 1}).limit(limit as i64).run().map_err(unavailable)?)
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
                return self.opt(found);
            }
        }
        Ok(None)
    }

    fn inbox(&self, badge_id: &str) -> Result<Vec<Value>> {
        self.many(self.claims.find(doc! {"state": "leased", "lease.badge_id": badge_id}).sort(doc! {"lease.leased_at": 1, "claim_id": 1}).run().map_err(unavailable)?)
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
        self.many(self.claims.find(doc! {"state": "leased", "lease.expires_at": {"$lte": now}}).sort(doc! {"lease.expires_at": 1, "claim_id": 1}).run().map_err(unavailable)?)
    }

    fn claims_for_patient(&self, patient_id: &str) -> Result<Vec<Value>> {
        // documents written with encryption carry only a keyed hash of the patient id; older ones carry the id itself
        let filter = match &self.crypt {
            Some(c) => doc! {"$or": [{"patient_key": c.patient_key(patient_id)}, {"claim.patient_id": patient_id}]},
            None => doc! {"claim.patient_id": patient_id},
        };
        let ids = self.claims.distinct("claim_id", filter).run().map_err(unavailable)?;
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let rows = self.many(self.claims.find(doc! {"claim_id": {"$in": ids}}).projection(doc! {"_id": 0, "claim_id": 1, "version": 1, "claim": 1}).run().map_err(unavailable)?)?;
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
        self.opt(self.configs.find_one(doc! {}).sort(doc! {"version": -1}).run().map_err(unavailable)?)
    }

    fn config_history(&self) -> Result<Vec<Value>> {
        self.many(self.configs.find(doc! {}).sort(doc! {"version": 1}).run().map_err(unavailable)?)
    }

    fn put_reliability(&self, doc: &Value, expected_version: i64) -> Result<bool> {
        check_reliability_doc(doc, expected_version)?;
        let top = self.reliability.find_one(doc! {}).sort(doc! {"version": -1}).run().map_err(unavailable)?;
        let current = top.and_then(|t| t.get("version").and_then(|v| v.as_i64().or_else(|| v.as_i32().map(i64::from)))).unwrap_or(0);
        if current != expected_version {
            return Ok(false);
        }
        let mut stored = doc.clone();
        stored["history"] = json!([]);
        match self.reliability.insert_one(to_doc(&stored)).run() {
            Ok(_) => Ok(true),
            Err(e) if duplicate(&e) => Ok(false),
            Err(e) => Err(unavailable(e)),
        }
    }

    fn reliability_history(&self) -> Result<Vec<Value>> {
        self.many(self.reliability.find(doc! {}).sort(doc! {"version": 1}).run().map_err(unavailable)?)
    }

    fn set_reliability_status(&self, version: i64, from: &str, to: &str, actor: &str, now: f64, detail: Option<Value>) -> Result<Option<Value>> {
        check_reliability_move(from, to, actor, now)?;
        let event = to_doc(&reliability_event(from, to, actor, now, detail));
        let updated = self
            .reliability
            .find_one_and_update(doc! {"version": version, "status": from}, doc! {"$set": {"status": to}, "$push": {"history": event}})
            .return_document(mongodb::options::ReturnDocument::After)
            .run()
            .map_err(unavailable)?;
        self.opt(updated)
    }

    fn append_deal(&self, d: &Value) -> Result<()> {
        check_id_doc(d, "deal_id")?;
        self.deals.insert_one(to_doc(d)).run().map(|_| ()).map_err(unavailable)
    }

    fn get_deal(&self, deal_id: &str) -> Result<Option<Value>> {
        self.opt(self.deals.find_one(doc! {"deal_id": deal_id}).run().map_err(unavailable)?)
    }

    fn deals(&self, limit: usize) -> Result<Vec<Value>> {
        if limit == 0 {
            return Ok(vec![]);
        }
        self.many(self.deals.find(doc! {}).sort(doc! {"_id": -1}).limit(limit as i64).run().map_err(unavailable)?)
    }

    fn append_batch_event(&self, e: &Value) -> Result<()> {
        check_batch_event(e)?;
        self.batch_events.insert_one(to_doc(e)).run().map(|_| ()).map_err(unavailable)
    }

    fn batch_events(&self, batch_id: Option<&str>, limit: usize) -> Result<Vec<Value>> {
        if limit == 0 {
            return Ok(vec![]);
        }
        let filter = match batch_id {
            Some(b) => doc! {"batch_id": b},
            None => doc! {},
        };
        let mut rows = self.many(self.batch_events.find(filter).sort(doc! {"_id": -1}).limit(limit as i64).run().map_err(unavailable)?)?;
        rows.reverse();
        Ok(rows)
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
        self.many(self.dead.find(doc! {}).sort(doc! {"_id": 1}).limit(limit as i64).run().map_err(unavailable)?)
    }

    fn pop_dead_letter(&self, dead_id: &str) -> Result<Option<Value>> {
        self.opt(self.dead.find_one_and_delete(doc! {"dead_id": dead_id}).run().map_err(unavailable)?)
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
        let mut rows = self.many(self.jobs.find(doc! {}).sort(doc! {"name": 1}).run().map_err(unavailable)?)?;
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

    fn uri() -> Option<String> {
        let uri = std::env::var("MONGO_URI").ok();
        if uri.is_none() {
            assert!(std::env::var("REQUIRE_MONGO").is_err(), "REQUIRE_MONGO is set but MONGO_URI is not");
        }
        uri
    }

    fn contract(encrypted: bool) {
        let Some(uri) = uri() else { return };
        let names = std::sync::Mutex::new(vec![]);
        let key = fernet::Fernet::generate_key();
        let make = || -> Box<dyn QueueStore> {
            let name = format!("claimguard_rs_qtest_{}", rand::random::<u64>());
            let mut store = MongoQueueStore::new(crate::connect(&uri, 3000).expect("connect"), &name);
            if encrypted {
                store = store.with_encryption(FieldCrypt::new(&key, None).unwrap());
            }
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

    #[test]
    fn the_mongo_queue_store_meets_the_same_contract_as_the_memory_twin() {
        contract(false);
    }

    #[test]
    fn and_so_does_the_encrypting_store_because_encryption_is_invisible_to_its_callers() {
        contract(true);
    }

    #[test]
    fn what_reaches_the_database_is_ciphertext_and_tampering_with_it_is_caught() {
        let Some(uri) = uri() else { return };
        let name = format!("claimguard_rs_enc_{}", rand::random::<u64>());
        let client = crate::connect(&uri, 3000).unwrap();
        let key = fernet::Fernet::generate_key();
        let store = MongoQueueStore::new(client.clone(), &name).with_encryption(FieldCrypt::new(&key, None).unwrap());
        store.ensure_indexes().unwrap();
        let new = |id: &str, patient: &str| {
            json!({"claim_id": id, "version": 1, "input_hash": format!("h-{id}"), "claim": {"claim_id": id, "patient_id": patient, "notes": "SECRET-NOTE knee surgery"},
                "results": [{"rule_id": "R001", "status": "FAIL", "evidence": [{"path": "/notes", "value": "SECRET-NOTE"}]}],
                "receipt": {"created_at": 1.0, "lane": "A", "eligibility": "any", "score": 3, "rule_pack_hash": "p"}, "advisory": []})
        };
        assert!(store.put_triaged(&new("C1", "PAT-777")).unwrap() && store.put_triaged(&new("C2", "PAT-888")).unwrap());
        // 1. through the store everything reads normally, including the lookup of a patient's claims
        assert_eq!(store.get("C1", None).unwrap().unwrap()["claim"]["patient_id"], "PAT-777");
        assert_eq!(store.claims_for_patient("PAT-777").unwrap().len(), 1);
        assert_eq!(store.claims_for_patient("PAT-000").unwrap().len(), 0);
        // 2. what is actually stored holds none of it
        let raw: Vec<Document> = client.database(&name).collection::<Document>("claims").find(doc! {}).run().unwrap().map(|d| d.unwrap()).collect();
        let text = format!("{raw:?}");
        for secret in ["PAT-777", "PAT-888", "SECRET-NOTE", "knee surgery", "R001"] {
            assert!(!text.contains(secret), "{secret} is readable in the database");
        }
        assert!(text.contains("$enc") && text.contains("patient_key"));
        // 3. a reader without the key cannot use the data, and swapping one claim's data under another is refused
        let other = MongoQueueStore::new(client.clone(), &name).with_encryption(FieldCrypt::new(&fernet::Fernet::generate_key(), None).unwrap());
        assert!(other.get("C1", None).is_err());
        let coll = client.database(&name).collection::<Document>("claims");
        let c1 = coll.find_one(doc! {"claim_id": "C1"}).run().unwrap().unwrap();
        coll.update_one(doc! {"claim_id": "C2"}, doc! {"$set": {"claim": c1.get("claim").unwrap().clone()}}).run().unwrap();
        assert!(store.get("C2", None).is_err(), "claim C1's data placed under C2 must be refused");
        assert!(store.get("C1", None).is_ok());
        // 4. a reviewer's decision and an AI explanation are sealed as well
        let mut set = Map::new();
        set.insert("explanation".into(), json!({"findings": {"R001": {"text": "Quantity SECRET-TEXT is too high"}}}));
        store.transition("C1", 1, "triaged", "ready", "system", 0.5, None, None, None).unwrap().unwrap();
        store.lease("C1", 1, "CG-2002", 1.0, 1e12).unwrap().unwrap();
        store.add_decision("C1", 1, "CG-2002", 2.0, &json!({"rule_id": "R001", "action": "confirm_issue", "reason": "SECRET-REASON", "badge_id": "CG-2002"})).unwrap().unwrap();
        store.transition("C1", 1, "leased", "decided", "CG-2002", 3.0, None, Some(set), Some("CG-2002")).unwrap().unwrap();
        let raw = format!("{:?}", coll.find_one(doc! {"claim_id": "C1"}).run().unwrap().unwrap());
        assert!(!raw.contains("SECRET-REASON") && !raw.contains("SECRET-TEXT"));
        let doc = store.get("C1", None).unwrap().unwrap();
        assert_eq!(doc["decisions"][0]["reason"], "SECRET-REASON");
        assert_eq!(doc["explanation"]["findings"]["R001"]["text"], "Quantity SECRET-TEXT is too high");
        // 5. data written before encryption was switched on is still read
        let plain = MongoQueueStore::new(client.clone(), &name);
        assert!(plain.put_triaged(&new("C3", "PAT-999")).unwrap());
        assert_eq!(store.get("C3", None).unwrap().unwrap()["claim"]["patient_id"], "PAT-999");
        assert_eq!(store.claims_for_patient("PAT-999").unwrap().len(), 1, "older documents are found by the plain id");
        let _ = client.database(&name).drop().run();
    }
}

#[cfg(test)]
mod rotation_tests {
    use super::*;

    #[test]
    fn rotating_the_key_re_encrypts_every_claim_and_can_be_repeated() {
        let Ok(uri) = std::env::var("MONGO_URI") else {
            assert!(std::env::var("REQUIRE_MONGO").is_err(), "REQUIRE_MONGO is set but MONGO_URI is not");
            return;
        };
        let name = format!("claimguard_rs_rot_{}", rand::random::<u64>());
        let client = crate::connect(&uri, 3000).unwrap();
        let (ka, kb) = (fernet::Fernet::generate_key(), fernet::Fernet::generate_key());
        let a = MongoQueueStore::new(client.clone(), &name).with_encryption(FieldCrypt::new(&ka, None).unwrap());
        a.ensure_indexes().unwrap();
        for (id, patient) in [("C1", "PAT-111"), ("C2", "PAT-111"), ("C3", "PAT-222")] {
            let doc = json!({"claim_id": id, "version": 1, "input_hash": format!("h{id}"), "claim": {"claim_id": id, "patient_id": patient, "notes": "ROTATE-ME"}, "results": [],
                "receipt": {"created_at": 1.0, "lane": "A", "eligibility": "any", "score": 1, "rule_pack_hash": "p"}, "advisory": []});
            assert!(a.put_triaged(&doc).unwrap());
        }
        a.transition("C1", 1, "triaged", "ready", "system", 0.5, None, None, None).unwrap().unwrap();
        a.lease("C1", 1, "CG-2002", 1.0, 1e12).unwrap().unwrap();
        a.add_decision("C1", 1, "CG-2002", 2.0, &json!({"rule_id": "R001", "action": "confirm_issue", "reason": "ROTATE-REASON", "badge_id": "CG-2002"})).unwrap().unwrap();
        let new = FieldCrypt::new(&kb, None).unwrap();
        assert_eq!(a.reencrypt_all(&new, true).unwrap(), (3, 0, 0), "a dry run counts and changes nothing");
        assert!(a.get("C1", None).is_ok());
        assert_eq!(a.reencrypt_all(&new, false).unwrap(), (3, 0, 0));
        let b = MongoQueueStore::new(client.clone(), &name).with_encryption(FieldCrypt::new(&kb, None).unwrap());
        let doc = b.get("C1", None).unwrap().unwrap();
        assert_eq!((doc["claim"]["notes"].as_str(), doc["decisions"][0]["reason"].as_str()), (Some("ROTATE-ME"), Some("ROTATE-REASON")));
        assert_eq!(b.claims_for_patient("PAT-111").unwrap().len(), 2, "the patient index was recomputed under the new key");
        assert!(a.get("C1", None).is_err(), "the old key no longer opens anything");
        let raw = format!("{:?}", client.database(&name).collection::<Document>("claims").find(doc! {}).run().unwrap().map(|d| d.unwrap()).collect::<Vec<_>>());
        assert!(!raw.contains("ROTATE-ME") && !raw.contains("ROTATE-REASON") && !raw.contains("PAT-111"));
        let again = MongoQueueStore::new(client.clone(), &name).with_encryption(FieldCrypt::new(&ka, None).unwrap());
        assert_eq!(again.reencrypt_all(&new, false).unwrap(), (0, 3, 0), "run again after an interruption: everything is already on the new key");
        let wrong = MongoQueueStore::new(client.clone(), &name).with_encryption(FieldCrypt::new(&fernet::Fernet::generate_key(), None).unwrap());
        let unrelated = FieldCrypt::new(&fernet::Fernet::generate_key(), None).unwrap();
        assert!(wrong.reencrypt_all(&unrelated, false).unwrap_err().to_string().contains("neither key"), "a document that opens with neither key stops the rotation instead of being damaged");
        let _ = client.database(&name).drop().run();
    }
}
