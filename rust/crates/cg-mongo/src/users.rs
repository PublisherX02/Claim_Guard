//! The MongoDB user store (`src/access/store_mongo.py`).

use crate::crypt::FieldCrypt;
use cg_access::store::{validate_update, validate_user, Approval, StoreError, User, UserStore, UserUpdate};
use mongodb::bson::{doc, Bson, DateTime, Document};
use mongodb::options::{IndexOptions, ReturnDocument, UpdateModifications};
use mongodb::sync::{Client, Collection};
use mongodb::IndexModel;
use std::time::Duration;

fn dt(ts: f64) -> DateTime {
    DateTime::from_millis((ts * 1000.0) as i64)
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

fn unavailable(_: mongodb::error::Error) -> StoreError {
    StoreError::Unavailable
}

fn f64_of(d: &Document, key: &str) -> Option<f64> {
    match d.get(key) {
        Some(Bson::Double(f)) => Some(*f),
        Some(Bson::Int32(i)) => Some(*i as f64),
        Some(Bson::Int64(i)) => Some(*i as f64),
        _ => None,
    }
}

fn i64_of(d: &Document, key: &str) -> Result<i64, StoreError> {
    match d.get(key) {
        Some(Bson::Int64(i)) => Ok(*i),
        Some(Bson::Int32(i)) => Ok(*i as i64),
        Some(Bson::Double(f)) if f.fract() == 0.0 => Ok(*f as i64),
        _ => Err(StoreError::Unavailable),
    }
}

fn strings(d: &Document, key: &str) -> Vec<String> {
    d.get_array(key).map(|a| a.iter().filter_map(|b| b.as_str().map(str::to_string)).collect()).unwrap_or_default()
}

fn to_user(d: &Document, crypt: Option<&FieldCrypt>) -> Result<User, StoreError> {
    let text = |k: &str| d.get_str(k).map(str::to_string).map_err(|_| StoreError::Unavailable);
    let badge_id = text("badge_id")?;
    let name = match crypt {
        Some(c) => c.open_text(&badge_id, "name", &text("name")?).map_err(|_| StoreError::Unavailable)?,
        None => text("name")?,
    };
    Ok(User {
        badge_id,
        name,
        password_hash: text("password_hash")?,
        totp_secret_enc: text("totp_secret_enc")?,
        level: i64_of(d, "level")?,
        grants: strings(d, "grants"),
        revokes: strings(d, "revokes"),
        active: d.get_bool("active").map_err(|_| StoreError::Unavailable)?,
        failed_attempts: i64_of(d, "failed_attempts")?,
        locked_until: f64_of(d, "locked_until"),
        must_change_password: d.get_bool("must_change_password").map_err(|_| StoreError::Unavailable)?,
        created_by: text("created_by")?,
        created_at: f64_of(d, "created_at").unwrap_or(0.0),
        last_login: f64_of(d, "last_login"),
    })
}

fn to_approval(d: &Document) -> Result<Approval, StoreError> {
    let text = |k: &str| d.get_str(k).map(str::to_string).map_err(|_| StoreError::Unavailable);
    Ok(Approval {
        id: text("id")?,
        action: text("action")?,
        target: text("target")?,
        requested_by: text("requested_by")?,
        requested_at: f64_of(d, "requested_at").unwrap_or(0.0),
        expires_at: f64_of(d, "expires_at").unwrap_or(0.0),
        status: text("status")?,
        summary: text("summary")?,
        payload: text("payload")?,
        decided_by: d.get_str("decided_by").ok().map(str::to_string),
        decided_at: f64_of(d, "decided_at"),
        note: d.get_str("note").ok().map(str::to_string),
    })
}

fn opt_f64(v: Option<f64>) -> Bson {
    v.map(Bson::Double).unwrap_or(Bson::Null)
}

pub struct MongoUserStore {
    client: Client,
    db_name: String,
    users: Collection<Document>,
    totp: Collection<Document>,
    revoked: Collection<Document>,
    counters: Collection<Document>,
    clients: Collection<Document>,
    approvals: Collection<Document>,
    crypt: Option<FieldCrypt>,
}

impl MongoUserStore {
    /// Stores people's names encrypted (readable only with the key); names stored before this was switched on are still read.
    pub fn with_encryption(mut self, crypt: FieldCrypt) -> MongoUserStore {
        self.crypt = Some(crypt);
        self
    }

    fn user_of(&self, d: &Document) -> Result<User, StoreError> {
        to_user(d, self.crypt.as_ref())
    }

    fn stored_name(&self, badge_id: &str, name: &str) -> String {
        self.crypt.as_ref().map_or_else(|| name.to_string(), |c| c.seal_text(badge_id, "name", name))
    }

    pub fn new(client: Client, db_name: &str) -> MongoUserStore {
        let db = client.database(db_name);
        MongoUserStore { users: db.collection("users"), totp: db.collection("used_totp"), revoked: db.collection("revoked_tokens"), counters: db.collection("counters"), clients: db.collection("known_clients"), approvals: db.collection("approvals"), crypt: None, client, db_name: db_name.to_string() }
    }

    pub fn ensure_indexes(&self) -> Result<(), StoreError> {
        let unique = || IndexOptions::builder().unique(true).build();
        let ttl = || IndexOptions::builder().expire_after(Duration::from_secs(0)).build();
        self.users.create_index(IndexModel::builder().keys(doc! {"badge_id": 1}).options(unique()).build()).run().map_err(unavailable)?;
        self.totp.create_index(IndexModel::builder().keys(doc! {"badge_id": 1, "step": 1}).options(unique()).build()).run().map_err(unavailable)?;
        self.totp.create_index(IndexModel::builder().keys(doc! {"expires_at": 1}).options(ttl()).build()).run().map_err(unavailable)?;
        self.revoked.create_index(IndexModel::builder().keys(doc! {"jti": 1}).options(unique()).build()).run().map_err(unavailable)?;
        self.revoked.create_index(IndexModel::builder().keys(doc! {"expires_at": 1}).options(ttl()).build()).run().map_err(unavailable)?;
        self.counters.create_index(IndexModel::builder().keys(doc! {"expires_at": 1}).options(ttl()).build()).run().map_err(unavailable)?;
        self.clients.create_index(IndexModel::builder().keys(doc! {"badge_id": 1, "client": 1}).options(unique()).build()).run().map_err(unavailable)?;
        self.clients.create_index(IndexModel::builder().keys(doc! {"expires_at": 1}).options(ttl()).build()).run().map_err(unavailable)?;
        self.approvals.create_index(IndexModel::builder().keys(doc! {"id": 1}).options(unique()).build()).run().map_err(unavailable)?;
        // at most one pending request per (action, target), enforced by the database itself
        let one_pending = IndexOptions::builder().unique(true).partial_filter_expression(doc! {"status": "pending"}).build();
        self.approvals.create_index(IndexModel::builder().keys(doc! {"action": 1, "target": 1}).options(one_pending).build()).run().map_err(unavailable)?;
        Ok(())
    }

    /// Re-encrypts every person's name from this store's key to `new` (see `MongoQueueStore::reencrypt_all`). Returns (rewritten, already on
    /// the new key, skipped because it changed meanwhile).
    pub fn reencrypt_names(&self, new: &FieldCrypt, dry_run: bool) -> Result<(usize, usize, usize), StoreError> {
        let old = self.crypt.as_ref().ok_or_else(|| StoreError::Invalid("this store has no encryption key to rotate from".into()))?;
        let (mut rewritten, mut current, mut skipped) = (0, 0, 0);
        let all: Vec<Document> = self.users.find(doc! {}).run().map_err(unavailable)?.collect::<Result<_, _>>().map_err(unavailable)?;
        for raw in all {
            let (Ok(badge), Ok(stored)) = (raw.get_str("badge_id"), raw.get_str("name")) else { continue };
            let plain = match old.open_text(badge, "name", stored) {
                Ok(p) => p, // sealed with the old key, or a plain name from before encryption was on
                Err(_) => match new.open_text(badge, "name", stored) {
                    Ok(_) => {
                        current += 1;
                        continue;
                    }
                    Err(e) => return Err(StoreError::Invalid(format!("the name of {badge} opens with neither key: {}", e.0))),
                },
            };
            if dry_run {
                rewritten += 1;
                continue;
            }
            let r = self.users.update_one(doc! {"badge_id": badge, "name": stored}, doc! {"$set": {"name": new.seal_text(badge, "name", &plain)}}).run().map_err(unavailable)?;
            if r.modified_count == 1 {
                rewritten += 1;
            } else {
                skipped += 1;
            }
        }
        Ok((rewritten, current, skipped))
    }

    /// Re-encrypts every authenticator seed from the Fernet key `old` to `new` (rotating `FERNET_KEY`). A seed that opens with neither key
    /// stops the run with an error and nothing is half-changed for that person. Returns (rewritten, already on the new key, skipped).
    pub fn reencrypt_seeds(&self, old: &str, new: &str, dry_run: bool) -> Result<(usize, usize, usize), StoreError> {
        let (o, n) = match (fernet::Fernet::new(old), fernet::Fernet::new(new)) {
            (Some(o), Some(n)) => (o, n),
            _ => return Err(StoreError::Invalid("a Fernet key is not valid".into())),
        };
        let (mut rewritten, mut current, mut skipped) = (0, 0, 0);
        let all: Vec<Document> = self.users.find(doc! {}).run().map_err(unavailable)?.collect::<Result<_, _>>().map_err(unavailable)?;
        for raw in all {
            let (Ok(badge), Ok(stored)) = (raw.get_str("badge_id"), raw.get_str("totp_secret_enc")) else { continue };
            let Ok(plain) = o.decrypt(stored) else {
                if n.decrypt(stored).is_ok() {
                    current += 1;
                    continue;
                }
                return Err(StoreError::Invalid(format!("the authenticator seed of {badge} opens with neither key")));
            };
            if dry_run {
                rewritten += 1;
                continue;
            }
            let r = self.users.update_one(doc! {"badge_id": badge, "totp_secret_enc": stored}, doc! {"$set": {"totp_secret_enc": n.encrypt(&plain)}}).run().map_err(unavailable)?;
            if r.modified_count == 1 {
                rewritten += 1;
            } else {
                skipped += 1;
            }
        }
        Ok((rewritten, current, skipped))
    }

    pub fn drop_database(&self) {
        let _ = self.client.database(&self.db_name).drop().run();
    }
}

impl UserStore for MongoUserStore {
    fn ping(&self) -> bool {
        self.client
            .database("admin")
            .run_command(doc! {"ping": 1})
            .run()
            .map(|d| matches!(d.get("ok"), Some(Bson::Double(f)) if *f == 1.0) || matches!(d.get("ok"), Some(Bson::Int32(1))))
            .unwrap_or(false)
    }

    fn create_user(&self, u: &User) -> Result<(), StoreError> {
        validate_user(u)?;
        let d = doc! {"badge_id": &u.badge_id, "name": self.stored_name(&u.badge_id, &u.name), "password_hash": &u.password_hash, "totp_secret_enc": &u.totp_secret_enc, "level": u.level,
            "grants": u.grants.clone(), "revokes": u.revokes.clone(), "active": u.active, "failed_attempts": u.failed_attempts, "locked_until": opt_f64(u.locked_until),
            "must_change_password": u.must_change_password, "created_by": &u.created_by, "created_at": u.created_at, "last_login": opt_f64(u.last_login)};
        match self.users.insert_one(d).run() {
            Ok(_) => Ok(()),
            Err(e) if duplicate(&e) => Err(StoreError::Duplicate),
            Err(e) => Err(unavailable(e)),
        }
    }

    fn get_user(&self, badge_id: &str) -> Result<Option<User>, StoreError> {
        self.users.find_one(doc! {"badge_id": badge_id}).run().map_err(unavailable)?.as_ref().map(|d| self.user_of(d)).transpose()
    }

    fn list_users(&self) -> Result<Vec<User>, StoreError> {
        let cursor = self.users.find(doc! {}).sort(doc! {"badge_id": 1}).run().map_err(unavailable)?;
        cursor.map(|r| r.map_err(unavailable).and_then(|d| self.user_of(&d))).collect()
    }

    fn update_user(&self, badge_id: &str, f: &UserUpdate) -> Result<User, StoreError> {
        validate_update(f)?;
        let mut set = Document::new();
        if let Some(v) = &f.name {
            set.insert("name", self.stored_name(badge_id, v));
        }
        if let Some(v) = &f.password_hash {
            set.insert("password_hash", v);
        }
        if let Some(v) = &f.totp_secret_enc {
            set.insert("totp_secret_enc", v);
        }
        if let Some(v) = f.level {
            set.insert("level", v);
        }
        if let Some(v) = &f.grants {
            set.insert("grants", v.clone());
        }
        if let Some(v) = &f.revokes {
            set.insert("revokes", v.clone());
        }
        if let Some(v) = f.active {
            set.insert("active", v);
        }
        if let Some(v) = f.must_change_password {
            set.insert("must_change_password", v);
        }
        if let Some(v) = f.locked_until {
            set.insert("locked_until", opt_f64(v));
        }
        if let Some(v) = f.failed_attempts {
            set.insert("failed_attempts", v);
        }
        let found = if set.is_empty() {
            self.users.find_one(doc! {"badge_id": badge_id}).run()
        } else {
            self.users.find_one_and_update(doc! {"badge_id": badge_id}, doc! {"$set": set}).return_document(ReturnDocument::After).run()
        }
        .map_err(unavailable)?;
        found.as_ref().map(|d| self.user_of(d)).transpose()?.ok_or(StoreError::NotFound)
    }

    fn record_failed_login(&self, badge_id: &str, now: f64, max_failed: i64, lockout_seconds: f64) -> Result<User, StoreError> {
        // One atomic pipeline update: a lock that has run out starts a new count; otherwise the count goes up by one; the lock is set
        // when the count reaches the limit and is never extended by further failures.
        let expired = doc! {"$and": [{"$ne": ["$locked_until", Bson::Null]}, {"$lte": ["$locked_until", now]}]};
        let pipeline = vec![
            doc! {"$set": {"failed_attempts": {"$cond": [expired.clone(), 1_i64, {"$add": ["$failed_attempts", 1_i64]}]}, "locked_until": {"$cond": [expired, Bson::Null, "$locked_until"]}}},
            doc! {"$set": {"locked_until": {"$cond": [{"$and": [{"$gte": ["$failed_attempts", max_failed]}, {"$eq": ["$locked_until", Bson::Null]}]}, now + lockout_seconds, "$locked_until"]}}},
        ];
        let found = self
            .users
            .find_one_and_update(doc! {"badge_id": badge_id}, UpdateModifications::Pipeline(pipeline))
            .return_document(ReturnDocument::After)
            .run()
            .map_err(unavailable)?;
        found.as_ref().map(|d| self.user_of(d)).transpose()?.ok_or(StoreError::NotFound)
    }

    fn record_successful_login(&self, badge_id: &str, now: f64) -> Result<(), StoreError> {
        let r = self
            .users
            .update_one(doc! {"badge_id": badge_id}, doc! {"$set": {"failed_attempts": 0_i64, "locked_until": Bson::Null, "last_login": now}})
            .run()
            .map_err(unavailable)?;
        if r.matched_count == 0 {
            Err(StoreError::NotFound)
        } else {
            Ok(())
        }
    }

    fn mark_totp_used(&self, badge_id: &str, step: i64, expires_at: f64) -> Result<bool, StoreError> {
        match self.totp.insert_one(doc! {"badge_id": badge_id, "step": step, "expires_at": dt(expires_at)}).run() {
            Ok(_) => Ok(true),
            Err(e) if duplicate(&e) => Ok(false),
            Err(e) => Err(unavailable(e)),
        }
    }

    fn revoke_token(&self, jti: &str, expires_at: f64) -> Result<(), StoreError> {
        let update = doc! {"$max": {"expires_ts": expires_at, "expires_at": dt(expires_at)}};
        for attempt in 0..2 {
            // two simultaneous first revocations race on the unique index; retry once
            match self.revoked.update_one(doc! {"jti": jti}, update.clone()).upsert(true).run() {
                Ok(_) => return Ok(()),
                Err(e) if duplicate(&e) && attempt == 0 => continue,
                Err(e) => return Err(unavailable(e)),
            }
        }
        Ok(())
    }

    fn is_revoked(&self, jti: &str, now: f64) -> Result<bool, StoreError> {
        Ok(self.revoked.find_one(doc! {"jti": jti, "expires_ts": {"$gt": now}}).run().map_err(unavailable)?.is_some())
    }

    fn count_active_admins(&self) -> Result<i64, StoreError> {
        Ok(self.users.count_documents(doc! {"level": 4, "active": true}).run().map_err(unavailable)? as i64)
    }

    fn counter(&self, key: &str, window_seconds: u64, now: f64) -> Result<i64, StoreError> {
        let id = counter_id(key, window_seconds, now);
        Ok(self.counters.find_one(doc! {"_id": id}).run().map_err(unavailable)?.map_or(0, |d| i64_of(&d, "n").unwrap_or(0)))
    }

    fn bump_counter(&self, key: &str, window_seconds: u64, now: f64) -> Result<i64, StoreError> {
        let id = counter_id(key, window_seconds, now);
        let w = window_seconds.max(1) as f64;
        let expires = dt(((now / w).floor() + 2.0) * w);
        for attempt in 0..2 {
            // two simultaneous first bumps race on the unique _id; the loser retries once and then increments
            let r = self
                .counters
                .find_one_and_update(doc! {"_id": &id}, doc! {"$inc": {"n": 1_i64}, "$setOnInsert": {"expires_at": expires}})
                .upsert(true)
                .return_document(ReturnDocument::After)
                .run();
            match r {
                Ok(Some(d)) => return i64_of(&d, "n"),
                Ok(None) => return Err(StoreError::Unavailable),
                Err(e) if duplicate(&e) && attempt == 0 => continue,
                Err(e) => return Err(unavailable(e)),
            }
        }
        Err(StoreError::Unavailable)
    }

    fn client_known(&self, badge_id: &str, client: &str, now: f64) -> Result<bool, StoreError> {
        let cutoff = now - cg_access::store::KNOWN_CLIENT_SECONDS;
        Ok(self.clients.find_one(doc! {"badge_id": badge_id, "client": client, "seen_ts": {"$gt": cutoff}}).run().map_err(unavailable)?.is_some())
    }

    fn create_approval(&self, a: &Approval, now: f64) -> Result<(), StoreError> {
        self.approvals.update_many(doc! {"status": "pending", "expires_at": {"$lte": now}}, doc! {"$set": {"status": "expired"}}).run().map_err(unavailable)?;
        let d = doc! {"id": &a.id, "action": &a.action, "target": &a.target, "requested_by": &a.requested_by, "requested_at": a.requested_at, "expires_at": a.expires_at,
            "status": &a.status, "summary": &a.summary, "payload": &a.payload, "decided_by": a.decided_by.clone(), "decided_at": opt_f64(a.decided_at), "note": a.note.clone()};
        match self.approvals.insert_one(d).run() {
            Ok(_) => Ok(()),
            Err(e) if duplicate(&e) => Err(StoreError::Duplicate),
            Err(e) => Err(unavailable(e)),
        }
    }

    fn get_approval(&self, id: &str) -> Result<Option<Approval>, StoreError> {
        self.approvals.find_one(doc! {"id": id}).run().map_err(unavailable)?.as_ref().map(to_approval).transpose()
    }

    fn list_approvals(&self, limit: usize) -> Result<Vec<Approval>, StoreError> {
        let cursor = self.approvals.find(doc! {}).sort(doc! {"_id": -1}).limit(limit as i64).run().map_err(unavailable)?;
        cursor.map(|r| r.map_err(unavailable).and_then(|d| to_approval(&d))).collect()
    }

    fn settle_approval(&self, id: &str, from: &str, to: &str, by: &str, now: f64, note: Option<&str>) -> Result<bool, StoreError> {
        let mut filter = doc! {"id": id, "status": from};
        if from == "pending" {
            filter.insert("expires_at", doc! {"$gt": now});
        }
        let set = doc! {"status": to, "decided_by": by, "decided_at": now, "note": note.map(str::to_string)};
        Ok(self.approvals.update_one(filter, doc! {"$set": set}).run().map_err(unavailable)?.modified_count == 1)
    }

    fn mark_client(&self, badge_id: &str, client: &str, now: f64) -> Result<(), StoreError> {
        let update = doc! {"$set": {"seen_ts": now, "expires_at": dt(now + cg_access::store::KNOWN_CLIENT_SECONDS)}};
        for attempt in 0..2 {
            match self.clients.update_one(doc! {"badge_id": badge_id, "client": client}, update.clone()).upsert(true).run() {
                Ok(_) => return Ok(()),
                Err(e) if duplicate(&e) && attempt == 0 => continue,
                Err(e) => return Err(unavailable(e)),
            }
        }
        Ok(())
    }
}

fn counter_id(key: &str, window_seconds: u64, now: f64) -> String {
    format!("{key}|{window_seconds}|{}", (now / window_seconds.max(1) as f64).floor() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cg_access::contract;

    fn uri() -> Option<String> {
        std::env::var("MONGO_URI").ok().filter(|u| !u.is_empty())
    }

    /// The same contract the in-memory twin passes, against a real database. Needs MONGO_URI (REQUIRE_MONGO=1 makes its absence an error).
    #[test]
    fn the_mongo_store_meets_the_contract() {
        let Some(uri) = uri() else {
            assert!(std::env::var("REQUIRE_MONGO").is_err(), "REQUIRE_MONGO is set but MONGO_URI is not");
            return;
        };
        let names = std::sync::Mutex::new(vec![]);
        let make = || -> Box<dyn UserStore> {
            let name = format!("claimguard_rs_test_{}", rand::random::<u64>());
            let store = MongoUserStore::new(crate::connect(&uri, 3000).expect("connect"), &name);
            store.ensure_indexes().expect("indexes");
            names.lock().unwrap().push(name);
            Box::new(store)
        };
        contract::run(&make);
        let client = crate::connect(&uri, 3000).unwrap();
        for n in names.lock().unwrap().iter() {
            let _ = client.database(n).drop().run();
        }
    }

    fn with_db<R>(f: impl FnOnce(&Client, &str) -> R) -> Option<R> {
        let uri = uri()?;
        let client = crate::connect(&uri, 3000).expect("connect");
        let name = format!("claimguard_rs_test_{}", rand::random::<u64>());
        let r = f(&client, &name);
        let _ = client.database(&name).drop().run();
        Some(r)
    }

    #[test]
    fn names_are_stored_encrypted_and_the_store_still_meets_the_contract() {
        let fk = fernet::Fernet::generate_key();
        with_db(|client, name| {
            let make = || -> Box<dyn UserStore> {
                let n = format!("{name}_{}", rand::random::<u32>());
                let s = MongoUserStore::new(client.clone(), &n).with_encryption(FieldCrypt::new(&fk, None).unwrap());
                s.ensure_indexes().unwrap();
                Box::new(s)
            };
            contract::run(&make);
            let store = MongoUserStore::new(client.clone(), name).with_encryption(FieldCrypt::new(&fk, None).unwrap());
            store.ensure_indexes().unwrap();
            let (mut a, mut b) = (contract::sample("CG-1001", 1), contract::sample("CG-1002", 2));
            a.name = "Amal Haddad".into();
            b.name = "Omar Rached".into();
            store.create_user(&a).unwrap();
            store.create_user(&b).unwrap();
            let raw = client.database(name).collection::<Document>("users").find_one(doc! {"badge_id": "CG-1001"}).run().unwrap().unwrap();
            assert!(!raw.get_str("name").unwrap().contains("Amal") && raw.get_str("name").unwrap().starts_with("enc1:"), "{raw:?}");
            assert_eq!(store.get_user("CG-1001").unwrap().unwrap().name, "Amal Haddad");
            let mut changed = UserUpdate::default();
            changed.name = Some("Amal H".into());
            assert_eq!(store.update_user("CG-1001", &changed).unwrap().name, "Amal H");
            // one person's sealed name copied onto another person is refused
            let other = client.database(name).collection::<Document>("users").find_one(doc! {"badge_id": "CG-1002"}).run().unwrap().unwrap();
            client.database(name).collection::<Document>("users").update_one(doc! {"badge_id": "CG-1001"}, doc! {"$set": {"name": other.get_str("name").unwrap()}}).run().unwrap();
            assert_eq!(store.get_user("CG-1001"), Err(StoreError::Unavailable), "the name of CG-1002 under CG-1001");
            // a store without the key cannot make sense of it either (it shows the token, not the name)
            let blind = MongoUserStore::new(client.clone(), name);
            assert!(blind.get_user("CG-1002").unwrap().unwrap().name.starts_with("enc1:"));
            // names written before encryption was on are still read
            client.database(name).collection::<Document>("users").update_one(doc! {"badge_id": "CG-1001"}, doc! {"$set": {"name": "Old Plain Name"}}).run().unwrap();
            assert_eq!(store.get_user("CG-1001").unwrap().unwrap().name, "Old Plain Name");
        })
        .unwrap_or_else(|| assert!(std::env::var("REQUIRE_MONGO").is_err(), "REQUIRE_MONGO is set but MONGO_URI is not"));
    }

    #[test]
    fn names_and_authenticator_seeds_can_be_rotated_to_new_keys() {
        let (fk1, fk2) = (fernet::Fernet::generate_key(), fernet::Fernet::generate_key());
        let (dk1, dk2) = (fernet::Fernet::generate_key(), fernet::Fernet::generate_key());
        with_db(|client, name| {
            let old = || FieldCrypt::new(&fk1, Some(&dk1)).unwrap();
            let store = MongoUserStore::new(client.clone(), name).with_encryption(old());
            store.ensure_indexes().unwrap();
            let mut a = contract::sample("CG-1001", 1);
            a.name = "Amal Haddad".into();
            a.totp_secret_enc = fernet::Fernet::new(&fk1).unwrap().encrypt(b"JBSWY3DPEHPK3PXP");
            let mut plain = contract::sample("CG-1002", 1);
            plain.name = "Written Before".into();
            plain.totp_secret_enc = fernet::Fernet::new(&fk1).unwrap().encrypt(b"JBSWY3DPEHPK3PXQ");
            store.create_user(&a).unwrap();
            client.database(name).collection::<Document>("users").insert_one(doc! {"badge_id": "CG-1002", "name": "Written Before", "password_hash": "x", "totp_secret_enc": &plain.totp_secret_enc, "level": 1_i64,
                "grants": Vec::<String>::new(), "revokes": Vec::<String>::new(), "active": true, "failed_attempts": 0_i64, "locked_until": Bson::Null, "must_change_password": false, "created_by": "t", "created_at": 0.0, "last_login": Bson::Null}).run().unwrap();

            // names: dry run changes nothing, the real run changes both (including the plain one), a repeat finds nothing to do
            let new = FieldCrypt::new(&fk1, Some(&dk2)).unwrap();
            assert_eq!(store.reencrypt_names(&new, true).unwrap(), (2, 0, 0));
            assert_eq!(store.get_user("CG-1001").unwrap().unwrap().name, "Amal Haddad");
            assert_eq!(store.reencrypt_names(&new, false).unwrap(), (2, 0, 0));
            assert_eq!(store.reencrypt_names(&new, false).unwrap(), (0, 2, 0), "repeating it is safe");
            let after = MongoUserStore::new(client.clone(), name).with_encryption(FieldCrypt::new(&fk1, Some(&dk2)).unwrap());
            assert_eq!(after.get_user("CG-1001").unwrap().unwrap().name, "Amal Haddad");
            assert_eq!(after.get_user("CG-1002").unwrap().unwrap().name, "Written Before");
            assert_eq!(store.get_user("CG-1001"), Err(StoreError::Unavailable), "the old key no longer opens the names");

            // seeds
            assert_eq!(store.reencrypt_seeds(&fk1, &fk2, true).unwrap(), (2, 0, 0));
            assert_eq!(store.reencrypt_seeds(&fk1, &fk2, false).unwrap(), (2, 0, 0));
            assert_eq!(store.reencrypt_seeds(&fk1, &fk2, false).unwrap(), (0, 2, 0));
            let seed = after.get_user("CG-1001").unwrap().unwrap().totp_secret_enc;
            assert_eq!(fernet::Fernet::new(&fk2).unwrap().decrypt(&seed).unwrap(), b"JBSWY3DPEHPK3PXP");
            assert!(fernet::Fernet::new(&fk1).unwrap().decrypt(&seed).is_err());
            // a seed that opens with neither key stops the run
            client.database(name).collection::<Document>("users").update_one(doc! {"badge_id": "CG-1001"}, doc! {"$set": {"totp_secret_enc": "garbage"}}).run().unwrap();
            assert!(store.reencrypt_seeds(&fk1, &fk2, false).unwrap_err().to_string().contains("neither key"));
            assert!(store.reencrypt_seeds("not a key", &fk2, false).is_err());
        })
        .unwrap_or_else(|| assert!(std::env::var("REQUIRE_MONGO").is_err(), "REQUIRE_MONGO is set but MONGO_URI is not"));
    }

    #[test]
    fn an_unreachable_database_is_unavailable_not_a_hang() {
        let store = MongoUserStore::new(crate::connect("mongodb://127.0.0.1:1/?serverSelectionTimeoutMS=300", 300).unwrap(), "x");
        assert!(!store.ping());
        assert_eq!(store.get_user("CG-1"), Err(StoreError::Unavailable));
        assert_eq!(store.create_user(&contract::sample("CG-1", 1)), Err(StoreError::Unavailable));
    }
}
