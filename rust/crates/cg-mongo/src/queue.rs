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
use cg_queue::store::{NOTE_RETAIN_SECONDS, check_notification, check_batch_event, check_request_event, check_config_doc, check_reliability_doc, check_reliability_move, reliability_event, check_decision, check_id_doc, check_job, check_number, check_set_fields, check_version, lane_key, prepare_new_doc, project_history, QueueStore, Result, DEALABLE};
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
    request_events: Collection<Document>,
    dead: Collection<Document>,
    cache: Collection<Document>,
    jobs: Collection<Document>,
    counters: Collection<Document>,
    notes: Collection<Document>,
    note_cursors: Collection<Document>,
    crypt: Option<FieldCrypt>,
}

impl MongoQueueStore {
    /// The ids of every claim of one patient. Documents written with encryption carry only a keyed hash of the patient id; older ones carry the id itself.
    fn patient_claim_ids(&self, patient_id: &str) -> Result<Vec<Bson>> {
        let filter = match &self.crypt {
            Some(c) => doc! {"$or": [{"patient_key": c.patient_key(patient_id)}, {"claim.patient_id": patient_id}]},
            None => doc! {"claim.patient_id": patient_id},
        };
        self.claims.distinct("claim_id", filter).run().map_err(unavailable)
    }

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
        let mut old_keys: Vec<String> = vec![];
        let all: Vec<Document> = self.claims.find(doc! {}).run().map_err(unavailable)?.collect::<std::result::Result<_, _>>().map_err(unavailable)?;
        for raw in all {
            let Some(id) = raw.get("_id").cloned() else { continue };
            let stored = from_doc(&raw);
            if FieldCrypt::is_patient_sealed(&stored) {
                // Sealed under the patient's own key: the data stays as it is. Only the patient index depends on the data key, so the patient's
                // key is carried to the new index and the document is pointed at it.
                let Ok(plain) = old.open(stored.clone()) else {
                    match new.open(stored.clone()) {
                        Ok(_) => {
                            current += 1; // already rotated
                            continue;
                        }
                        Err(e) => return Err(StoreError::Invalid(format!("claim {} opens with neither key: {}", stored["claim_id"].as_str().unwrap_or("?"), e.0))),
                    }
                };
                let (old_pk, new_pk) = (stored["patient_key"].as_str().unwrap_or("").to_string(), plain["claim"]["patient_id"].as_str().map(|p| new.patient_key(p)).unwrap_or_default());
                if new_pk.is_empty() || new_pk == old_pk {
                    current += 1;
                    continue;
                }
                if dry_run {
                    rewritten += 1;
                    continue;
                }
                old.carry_patient_key(&old_pk, new, &new_pk).map_err(|e| StoreError::Invalid(e.0))?;
                let r = self.claims.update_one(doc! {"_id": id, "state": raw.get("state").cloned().unwrap_or(Bson::Null), "state_at": raw.get("state_at").cloned().unwrap_or(Bson::Null)}, doc! {"$set": {"patient_key": &new_pk}}).run().map_err(unavailable)?;
                if r.modified_count == 1 {
                    rewritten += 1;
                    old_keys.push(old_pk);
                } else {
                    skipped += 1;
                }
                continue;
            }
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
            let replacement = to_doc(&new.seal_stored(&plain).map_err(|e| StoreError::Invalid(e.0))?);
            let r = self.claims.replace_one(doc! {"_id": id, "state": raw.get("state").cloned().unwrap_or(Bson::Null), "state_at": raw.get("state_at").cloned().unwrap_or(Bson::Null)}, replacement).run().map_err(unavailable)?;
            if r.modified_count == 1 {
                rewritten += 1;
            } else {
                skipped += 1;
            }
        }
        // an old index entry goes once no claim points at it; leaving it would leave a key wrapped by the old master that erasure never reaches
        old_keys.sort();
        old_keys.dedup();
        for k in old_keys {
            if self.claims.count_documents(doc! {"patient_key": &k}).run().map_err(unavailable)? == 0 {
                old.forget_patient_key(&k).map_err(|e| StoreError::Invalid(e.0))?;
            }
        }
        Ok((rewritten, current, skipped))
    }

    /// Moves every claim still sealed under the master key onto its patient's own key (so it can be shredded), and seals plain ones.
    /// Stop the API and the worker first. Returns (moved, already on a patient key or without a patient, skipped because it changed meanwhile).
    /// Backups made before this run still hold the old form: they cannot be shredded and must age out.
    pub fn adopt_patient_keys(&self, dry_run: bool) -> Result<(usize, usize, usize)> {
        let c = self.crypt.as_ref().filter(|c| c.has_patient_keys()).ok_or_else(|| StoreError::Invalid("per-patient keys are not switched on".into()))?;
        let (mut moved, mut already, mut skipped) = (0, 0, 0);
        let all: Vec<Document> = self.claims.find(doc! {}).run().map_err(unavailable)?.collect::<std::result::Result<_, _>>().map_err(unavailable)?;
        for raw in all {
            let Some(id) = raw.get("_id").cloned() else { continue };
            let stored = from_doc(&raw);
            if FieldCrypt::is_patient_sealed(&stored) {
                already += 1;
                continue;
            }
            let plain = c.open(stored).map_err(|e| StoreError::Invalid(e.0))?;
            if plain["claim"]["patient_id"].as_str().is_none() {
                already += 1; // nothing ties it to a patient, so there is no key to give it
                continue;
            }
            if dry_run {
                moved += 1;
                continue;
            }
            let replacement = to_doc(&c.seal_stored(&plain).map_err(|e| StoreError::Invalid(e.0))?);
            let r = self.claims.replace_one(doc! {"_id": id, "state": raw.get("state").cloned().unwrap_or(Bson::Null), "state_at": raw.get("state_at").cloned().unwrap_or(Bson::Null)}, replacement).run().map_err(unavailable)?;
            if r.modified_count == 1 {
                moved += 1;
            } else {
                skipped += 1;
            }
        }
        Ok((moved, already, skipped))
    }

    /// The patient index stored on a claim document (None when it has none, or the store is not encrypted).
    fn stored_patient_key(&self, claim_id: &str, version: Option<i64>) -> Result<Option<String>> {
        if self.crypt.is_none() {
            return Ok(None);
        }
        let mut filter = doc! {"claim_id": claim_id};
        if let Some(v) = version {
            filter.insert("version", v);
        }
        let d = self.claims.find_one(filter).projection(doc! {"patient_key": 1}).run().map_err(unavailable)?;
        Ok(d.and_then(|d| d.get_str("patient_key").ok().map(str::to_string)))
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

    /// When this badge last cleared its notifications (before every notification if it never has).
    fn cleared_at(&self, badge: &str) -> Result<f64> {
        let row = self.note_cursors.find_one(doc! {"badge": badge}).run().map_err(unavailable)?;
        Ok(row.and_then(|r| r.get_f64("cleared_at").ok()).unwrap_or(f64::NEG_INFINITY))
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
            request_events: db.collection("request_events"),
            dead: db.collection("dead_letters"),
            cache: db.collection("cache"),
            jobs: db.collection("jobs"),
            counters: db.collection("counters"),
            notes: db.collection("notifications"),
            note_cursors: db.collection("notification_cursors"),
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
        go(&self.request_events, plain(doc! {"claim_id": 1}))?;
        go(&self.dead, with(doc! {"dead_id": 1}, unique()))?;
        go(&self.cache, with(doc! {"key": 1}, unique()))?;
        go(&self.jobs, with(doc! {"name": 1}, unique()))?;
        go(&self.cache, with(doc! {"expires_at": 1}, ttl()))?;
        go(&self.notes, plain(doc! {"to": 1, "at": -1}))?;
        go(&self.notes, with(doc! {"id": 1}, unique()))?;
        go(&self.notes, plain(doc! {"claim_id": 1}))?;
        go(&self.notes, plain(doc! {"at": 1}))?;
        go(&self.note_cursors, with(doc! {"badge": 1}, unique()))?;
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
            c.seal_new_doc(&mut full).map_err(|e| StoreError::Invalid(e.0))?;
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
        if self.crypt.is_some() {
            let pk = self.stored_patient_key(claim_id, Some(version))?;
            if let Some(c) = &self.crypt {
                c.seal_set(pk.as_deref(), claim_id, version, &mut fields).map_err(|e| StoreError::Invalid(e.0))?;
            }
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
        let sealed = match &self.crypt {
            Some(c) => {
                let pk = self.stored_patient_key(claim_id, Some(version))?;
                c.seal(pk.as_deref(), claim_id, version, "decisions", decision).map_err(|e| StoreError::Invalid(e.0))?
            }
            None => decision.clone(),
        };
        let found = self
            .claims
            .find_one_and_update(
                doc! {"claim_id": claim_id, "version": version, "state": "leased", "lease.badge_id": badge_id, "lease.expires_at": {"$gt": now}},
                doc! {"$push": {"decisions": to_bson(&sealed)}},
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

    fn for_each_in_state(&self, state: &str, limit: usize, f: &mut dyn FnMut(Value)) -> Result<()> {
        if limit == 0 {
            return Ok(());
        }
        // the cursor is read one batch at a time: only the document in hand is held
        let cursor = self.claims.find(doc! {"state": state}).sort(doc! {"state_at": 1, "claim_id": 1}).limit(limit as i64).run().map_err(unavailable)?;
        for row in cursor {
            f(self.open(from_doc(&row.map_err(unavailable)?))?);
        }
        Ok(())
    }

    fn for_each_in_state_light(&self, state: &str, limit: usize, f: &mut dyn FnMut(Value)) -> Result<()> {
        if limit == 0 {
            return Ok(());
        }
        let cursor = self
            .claims
            .find(doc! {"state": state})
            .projection(doc! {"_id": 0, "claim_id": 1, "version": 1, "state": 1, "state_at": 1, "escalated": 1, "signoff": 1, "receipt.eligibility": 1})
            .sort(doc! {"state_at": 1, "claim_id": 1})
            .limit(limit as i64)
            .run()
            .map_err(unavailable)?;
        for row in cursor {
            f(self.open(from_doc(&row.map_err(unavailable)?))?);
        }
        Ok(())
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

    fn docs_for_patient(&self, patient_id: &str) -> Result<Vec<Value>> {
        let ids = self.patient_claim_ids(patient_id)?;
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let rows = self.many(self.claims.find(doc! {"claim_id": {"$in": ids}}).sort(doc! {"claim_id": 1, "version": 1}).run().map_err(unavailable)?)?;
        Ok(rows.into_iter().filter(|d| d["claim"]["patient_id"] == patient_id).collect())
    }

    fn erase_claim(&self, claim_id: &str) -> Result<i64> {
        let n = self.claims.delete_many(doc! {"claim_id": claim_id}).run().map_err(unavailable)?.deleted_count as i64;
        self.request_events.delete_many(doc! {"claim_id": claim_id}).run().map_err(unavailable)?;
        self.dead.delete_many(doc! {"claim_id": claim_id}).run().map_err(unavailable)?;
        self.notes.delete_many(doc! {"claim_id": claim_id}).run().map_err(unavailable)?;
        Ok(n)
    }

    fn patient_key_of_claim(&self, claim_id: &str) -> Result<Option<String>> {
        self.stored_patient_key(claim_id, None)
    }

    fn forget_patient_key_if_orphaned(&self, patient_key: &str) -> Result<bool> {
        let Some(c) = self.crypt.as_ref().filter(|c| c.has_patient_keys()) else { return Ok(false) };
        if self.claims.count_documents(doc! {"patient_key": patient_key}).run().map_err(unavailable)? > 0 {
            return Ok(false); // the patient still has claims in the store: the key stays
        }
        c.forget_patient_key(patient_key).map_err(|e| StoreError::Invalid(e.0))
    }

    fn closed_before(&self, cutoff: f64, limit: usize) -> Result<Vec<String>> {
        check_number(cutoff, "cutoff")?;
        if limit == 0 {
            return Ok(vec![]);
        }
        // candidates are the decided versions older than the cutoff; a claim counts only if that is also its newest version
        let rows = self.claims.find(doc! {"state": "decided", "state_at": {"$lt": cutoff}}).projection(doc! {"_id": 0, "claim_id": 1, "version": 1, "state_at": 1}).sort(doc! {"state_at": 1, "claim_id": 1}).run().map_err(unavailable)?;
        let mut out = vec![];
        for r in rows {
            let r = r.map_err(unavailable)?;
            let (id, ver) = (r.get_str("claim_id").unwrap_or("").to_string(), r.get_i64("version").unwrap_or_else(|_| r.get_i32("version").unwrap_or(0) as i64));
            if self.latest_versions(&[id.clone()])?.get(&id) == Some(&ver) && !out.contains(&id) {
                out.push(id);
                if out.len() >= limit {
                    break;
                }
            }
        }
        Ok(out)
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
        let rows = self.many(self.claims.find(doc! {"claim_id": {"$in": ids}}).projection(doc! {"_id": 0, "claim_id": 1, "version": 1, "claim": 1, "patient_key": 1}).run().map_err(unavailable)?)?;
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

    fn append_request_event(&self, e: &Value) -> Result<()> {
        check_request_event(e)?;
        self.request_events.insert_one(to_doc(e)).run().map(|_| ()).map_err(unavailable)
    }

    fn request_events(&self, claim_id: &str, limit: usize) -> Result<Vec<Value>> {
        if limit == 0 {
            return Ok(vec![]);
        }
        let mut rows = self.many(self.request_events.find(doc! {"claim_id": claim_id}).sort(doc! {"_id": -1}).limit(limit as i64).run().map_err(unavailable)?)?;
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

    fn append_notification(&self, n: &Value) -> Result<()> {
        check_notification(n)?;
        let cutoff = n["at"].as_f64().unwrap_or(0.0) - NOTE_RETAIN_SECONDS;
        self.notes.delete_many(doc! {"at": {"$lt": cutoff}}).run().map_err(unavailable)?;
        self.notes.insert_one(to_doc(n)).run().map(|_| ()).map_err(unavailable)
    }

    fn notifications(&self, badge: &str, limit: usize) -> Result<Vec<Value>> {
        if limit == 0 {
            return Ok(vec![]);
        }
        let cleared = self.cleared_at(badge)?;
        let filter = doc! {"$or": [{"to": badge}, {"to": "*", "at": {"$gt": cleared}}]};
        self.many(self.notes.find(filter).sort(doc! {"at": -1}).limit(limit as i64).run().map_err(unavailable)?)
    }

    fn clear_notifications(&self, badge: &str, now: f64) -> Result<i64> {
        check_number(now, "now")?;
        let cleared = self.cleared_at(badge)?;
        let hidden = self.notes.count_documents(doc! {"to": "*", "at": {"$gt": cleared}}).run().map_err(unavailable)? as i64;
        let own = self.notes.delete_many(doc! {"to": badge}).run().map_err(unavailable)?.deleted_count as i64;
        self.note_cursors.update_one(doc! {"badge": badge}, doc! {"$set": {"cleared_at": now}}).upsert(true).run().map_err(unavailable)?;
        Ok(own + hidden)
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
    fn and_so_does_the_store_with_a_key_per_patient() {
        let Some(uri) = uri() else { return };
        let names = std::sync::Mutex::new(vec![]);
        let key = fernet::Fernet::generate_key();
        let make = || -> Box<dyn QueueStore> {
            let name = format!("claimguard_rs_qkeys_{}", rand::random::<u64>());
            let client = crate::connect(&uri, 3000).expect("connect");
            let keys = std::sync::Arc::new(crate::keys::MongoKeyStore::new(client.clone(), &format!("{name}_keys")));
            let store = MongoQueueStore::new(client, &name).with_encryption(FieldCrypt::new(&key, None).unwrap().with_patient_keys(keys));
            store.ensure_indexes().expect("indexes");
            names.lock().unwrap().push(name);
            Box::new(store)
        };
        cg_queue::contract::run(&make);
        let client = crate::connect(&uri, 3000).unwrap();
        for n in names.lock().unwrap().iter() {
            let _ = client.database(n).drop().run();
            let _ = client.database(&format!("{n}_keys")).drop().run();
        }
    }

    /// The whole story of docs/59 against a real database: erase, shred, restore an old backup, replay the ledger.
    #[test]
    fn erasing_a_patient_shreds_their_key_and_a_restored_backup_is_unreadable_then_deleted() {
        let Some(uri) = uri() else { return };
        let name = format!("claimguard_rs_shred_{}", rand::random::<u64>());
        let client = crate::connect(&uri, 3000).unwrap();
        let key = fernet::Fernet::generate_key();
        let open_store = || {
            let keys = std::sync::Arc::new(crate::keys::MongoKeyStore::new(client.clone(), &format!("{name}_keys")));
            let s = MongoQueueStore::new(client.clone(), &name).with_encryption(FieldCrypt::new(&key, None).unwrap().with_patient_keys(keys));
            s.ensure_indexes().unwrap();
            s
        };
        let store = open_store();
        let new = |id: &str, patient: &str| {
            json!({"claim_id": id, "version": 1, "input_hash": format!("h-{id}"), "claim": {"claim_id": id, "patient_id": patient, "notes": "SHRED-ME"}, "results": [],
                "receipt": {"created_at": 1.0, "lane": "A", "eligibility": "any", "score": 3, "rule_pack_hash": "p"}, "advisory": []})
        };
        for (id, p) in [("C1", "PAT-A"), ("C2", "PAT-A"), ("C3", "PAT-B")] {
            assert!(store.put_triaged(&new(id, p)).unwrap());
        }
        let claims = client.database(&name).collection::<Document>("claims");
        let keys_coll = client.database(&format!("{name}_keys")).collection::<Document>("patient_keys");
        assert_eq!(keys_coll.count_documents(doc! {}).run().unwrap(), 2, "one key per patient");
        // a transition and a decision also seal under the patient's key
        store.transition("C1", 1, "triaged", "ready", "system", 0.5, None, None, None).unwrap().unwrap();
        // 1. a backup: copies of the claim documents (what mongodump takes) and, separately, of the key database
        let claims_backup: Vec<Document> = claims.find(doc! {}).run().unwrap().map(|d| d.unwrap()).collect();
        let keys_backup: Vec<Document> = keys_coll.find(doc! {}).run().unwrap().map(|d| d.unwrap()).collect();
        let tmp = tempfile::tempdir().unwrap();
        let ledger = cg_queue::erasure_ledger::Ledger::open(tmp.path().join("erasure_ledger.jsonl"));
        // 2. erase PAT-A
        let ids = cg_queue::privacy::claim_ids_of(&store, "PAT-A").unwrap();
        assert_eq!(ids, ["C1", "C2"]);
        let r = cg_queue::privacy::erase_claims(&store, &ledger, &ids, "erase", "CG-4004", 5.0).unwrap();
        assert_eq!((r["claims_erased"].as_i64(), r["patient_keys_destroyed"].as_i64()), (Some(2), Some(1)));
        assert_eq!(keys_coll.count_documents(doc! {}).run().unwrap(), 1, "PAT-A's key is gone, PAT-B's stays");
        assert!(store.get("C3", None).unwrap().is_some(), "another patient is untouched");
        // 3. the old claims backup alone (no key): the ciphertext is unreadable, even with the master key in hand
        claims.delete_many(doc! {}).run().unwrap();
        claims.insert_many(claims_backup.clone()).run().unwrap();
        let e = store.get("C1", None).unwrap_err().to_string();
        assert!(e.contains("no longer exists"), "a restored claim of an erased patient cannot be read: {e}");
        assert!(store.get("C3", None).unwrap().is_some(), "PAT-B still reads from the same restore");
        // 4. replaying the ledger deletes what the restore brought back
        let again = cg_queue::privacy::replay_ledger(&store, &ledger).unwrap();
        assert_eq!(again["claims_erased_again"], 2);
        assert!(store.get("C1", None).unwrap().is_none() && store.get("C2", None).unwrap().is_none());
        // 5. the worst case: the OLD KEY BACKUP is restored too, so PAT-A's key is back and the old claims read again
        keys_coll.delete_many(doc! {}).run().unwrap();
        keys_coll.insert_many(keys_backup).run().unwrap();
        claims.delete_many(doc! {}).run().unwrap();
        claims.insert_many(claims_backup).run().unwrap();
        let fresh = open_store(); // a new process: nothing cached
        assert_eq!(fresh.get("C1", None).unwrap().unwrap()["claim"]["notes"], "SHRED-ME", "with both backups the data reads: this is why the ledger is replayed");
        let again = cg_queue::privacy::replay_ledger(&fresh, &ledger).unwrap();
        assert_eq!((again["claims_erased_again"].as_i64(), again["patient_keys_destroyed"].as_i64()), (Some(2), Some(1)));
        assert!(fresh.get("C1", None).unwrap().is_none());
        assert_eq!(keys_coll.count_documents(doc! {}).run().unwrap(), 1, "the resurrected key was destroyed again");
        // 6. what is stored never held the identifier or the notes
        let raw = format!("{:?}", claims.find(doc! {}).run().unwrap().map(|d| d.unwrap()).collect::<Vec<_>>());
        assert!(!raw.contains("PAT-") && !raw.contains("SHRED-ME"));
        let _ = client.database(&name).drop().run();
        let _ = client.database(&format!("{name}_keys")).drop().run();
    }

    #[test]
    fn claims_sealed_under_the_master_key_move_onto_patient_keys_and_survive_a_key_rotation() {
        let Some(uri) = uri() else { return };
        let name = format!("claimguard_rs_adopt_{}", rand::random::<u64>());
        let client = crate::connect(&uri, 3000).unwrap();
        let (ka, kb) = (fernet::Fernet::generate_key(), fernet::Fernet::generate_key());
        let keys: std::sync::Arc<dyn crate::crypt::KeyStore> = std::sync::Arc::new(crate::keys::MongoKeyStore::new(client.clone(), &format!("{name}_keys")));
        // written before per-patient keys existed
        let legacy = MongoQueueStore::new(client.clone(), &name).with_encryption(FieldCrypt::new("x", Some(&ka)).unwrap());
        legacy.ensure_indexes().unwrap();
        for (id, p) in [("C1", "PAT-A"), ("C2", "PAT-B")] {
            let d = json!({"claim_id": id, "version": 1, "input_hash": format!("h-{id}"), "claim": {"claim_id": id, "patient_id": p, "notes": "ADOPT-ME"}, "results": [],
                "receipt": {"created_at": 1.0, "lane": "A", "eligibility": "any", "score": 3, "rule_pack_hash": "p"}, "advisory": []});
            assert!(legacy.put_triaged(&d).unwrap());
        }
        let store = MongoQueueStore::new(client.clone(), &name).with_encryption(FieldCrypt::new("x", Some(&ka)).unwrap().with_patient_keys(keys.clone()));
        assert_eq!(store.adopt_patient_keys(true).unwrap(), (2, 0, 0), "a dry run counts");
        assert_eq!(store.adopt_patient_keys(false).unwrap(), (2, 0, 0));
        assert_eq!(store.adopt_patient_keys(false).unwrap(), (0, 2, 0), "run again: already adopted");
        assert_eq!(store.get("C1", None).unwrap().unwrap()["claim"]["notes"], "ADOPT-ME");
        assert!(legacy.get("C1", None).is_err(), "without the key store the adopted claim is no longer readable");
        // now rotate the data key: the patient index changes, so each patient key moves to its new index
        let new = FieldCrypt::new("x", Some(&kb)).unwrap().with_patient_keys(keys.clone());
        assert_eq!(store.reencrypt_all(&new, true).unwrap(), (2, 0, 0));
        assert_eq!(store.reencrypt_all(&new, false).unwrap(), (2, 0, 0));
        let rotated = MongoQueueStore::new(client.clone(), &name).with_encryption(FieldCrypt::new("x", Some(&kb)).unwrap().with_patient_keys(keys.clone()));
        assert_eq!(rotated.get("C1", None).unwrap().unwrap()["claim"]["notes"], "ADOPT-ME");
        assert_eq!(rotated.claims_for_patient("PAT-A").unwrap().len(), 1, "the patient index follows the new key");
        assert_eq!(keys.list().unwrap().len(), 2, "the old index entries were removed, none left wrapped by the old master key");
        assert_eq!(store.reencrypt_all(&new, false).unwrap(), (0, 2, 0), "repeating after an interruption changes nothing");
        // and a rotated patient can still be shredded
        let ledger_dir = tempfile::tempdir().unwrap();
        let ledger = cg_queue::erasure_ledger::Ledger::open(ledger_dir.path().join("l.jsonl"));
        let r = cg_queue::privacy::erase_claims(&rotated, &ledger, &["C1".to_string()], "erase", "CG-4004", 1.0).unwrap();
        assert_eq!(r["patient_keys_destroyed"], 1);
        assert_eq!(keys.list().unwrap().len(), 1);
        let _ = client.database(&name).drop().run();
        let _ = client.database(&format!("{name}_keys")).drop().run();
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
