//! The MongoDB user store (`src/access/store_mongo.py`).

use cg_access::store::{validate_update, validate_user, StoreError, User, UserStore, UserUpdate};
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

fn to_user(d: &Document) -> Result<User, StoreError> {
    let text = |k: &str| d.get_str(k).map(str::to_string).map_err(|_| StoreError::Unavailable);
    Ok(User {
        badge_id: text("badge_id")?,
        name: text("name")?,
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

fn opt_f64(v: Option<f64>) -> Bson {
    v.map(Bson::Double).unwrap_or(Bson::Null)
}

pub struct MongoUserStore {
    client: Client,
    db_name: String,
    users: Collection<Document>,
    totp: Collection<Document>,
    revoked: Collection<Document>,
}

impl MongoUserStore {
    pub fn new(client: Client, db_name: &str) -> MongoUserStore {
        let db = client.database(db_name);
        MongoUserStore { users: db.collection("users"), totp: db.collection("used_totp"), revoked: db.collection("revoked_tokens"), client, db_name: db_name.to_string() }
    }

    pub fn ensure_indexes(&self) -> Result<(), StoreError> {
        let unique = || IndexOptions::builder().unique(true).build();
        let ttl = || IndexOptions::builder().expire_after(Duration::from_secs(0)).build();
        self.users.create_index(IndexModel::builder().keys(doc! {"badge_id": 1}).options(unique()).build()).run().map_err(unavailable)?;
        self.totp.create_index(IndexModel::builder().keys(doc! {"badge_id": 1, "step": 1}).options(unique()).build()).run().map_err(unavailable)?;
        self.totp.create_index(IndexModel::builder().keys(doc! {"expires_at": 1}).options(ttl()).build()).run().map_err(unavailable)?;
        self.revoked.create_index(IndexModel::builder().keys(doc! {"jti": 1}).options(unique()).build()).run().map_err(unavailable)?;
        self.revoked.create_index(IndexModel::builder().keys(doc! {"expires_at": 1}).options(ttl()).build()).run().map_err(unavailable)?;
        Ok(())
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
        let d = doc! {"badge_id": &u.badge_id, "name": &u.name, "password_hash": &u.password_hash, "totp_secret_enc": &u.totp_secret_enc, "level": u.level,
            "grants": u.grants.clone(), "revokes": u.revokes.clone(), "active": u.active, "failed_attempts": u.failed_attempts, "locked_until": opt_f64(u.locked_until),
            "must_change_password": u.must_change_password, "created_by": &u.created_by, "created_at": u.created_at, "last_login": opt_f64(u.last_login)};
        match self.users.insert_one(d).run() {
            Ok(_) => Ok(()),
            Err(e) if duplicate(&e) => Err(StoreError::Duplicate),
            Err(e) => Err(unavailable(e)),
        }
    }

    fn get_user(&self, badge_id: &str) -> Result<Option<User>, StoreError> {
        self.users.find_one(doc! {"badge_id": badge_id}).run().map_err(unavailable)?.as_ref().map(to_user).transpose()
    }

    fn list_users(&self) -> Result<Vec<User>, StoreError> {
        let cursor = self.users.find(doc! {}).sort(doc! {"badge_id": 1}).run().map_err(unavailable)?;
        cursor.map(|r| r.map_err(unavailable).and_then(|d| to_user(&d))).collect()
    }

    fn update_user(&self, badge_id: &str, f: &UserUpdate) -> Result<User, StoreError> {
        validate_update(f)?;
        let mut set = Document::new();
        if let Some(v) = &f.name {
            set.insert("name", v);
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
        found.as_ref().map(to_user).transpose()?.ok_or(StoreError::NotFound)
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
        found.as_ref().map(to_user).transpose()?.ok_or(StoreError::NotFound)
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

    #[test]
    fn an_unreachable_database_is_unavailable_not_a_hang() {
        let store = MongoUserStore::new(crate::connect("mongodb://127.0.0.1:1/?serverSelectionTimeoutMS=300", 300).unwrap(), "x");
        assert!(!store.ping());
        assert_eq!(store.get_user("CG-1"), Err(StoreError::Unavailable));
        assert_eq!(store.create_user(&contract::sample("CG-1", 1)), Err(StoreError::Unavailable));
    }
}
