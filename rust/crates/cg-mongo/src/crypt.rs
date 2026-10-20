//! Encryption of claim data at rest, inside the application, before it reaches the database.
//!
//! A claim document carries health-insurance data about a person: the claim itself, the rule results (whose evidence quotes it), the
//! advisory results, the AI explanations (which quote values from it) and the reviewers' free-text reasons. Those fields are stored as
//! `{"$enc": "<token>"}`: a Fernet token (AES-128-CBC with an HMAC-SHA256 tag) of a small envelope that also names the claim, version
//! and field the data belongs to. What stays readable is what the database needs to route work and what holds no personal data: ids,
//! state, lane, score, lease holder, timestamps, hashes, and a keyed hash of the patient id used to find a patient's earlier claims.
//!
//! * A copy of the database, a backup, or an administrator with database access sees ciphertext. The key is the application's, from the
//!   environment: `DATA_KEY` (a Fernet key) if set, otherwise a key derived from `FERNET_KEY`, so existing deployments are covered with no
//!   new setting. Rotating it needs a re-encryption pass (not built).
//! * The envelope is bound to its place. Moving an encrypted field from one claim to another, or one field name to another, is detected on
//!   read and refused, so someone with write access to the database cannot swap one claim's evidence under another claim.
//! * Documents written before encryption was switched on are still readable (a field without the marker is plain data).
//! * **A key per patient (crypto-shredding, docs/59).** When a key store is attached, a claim's sealed fields are encrypted under a random
//!   key that belongs to that patient (found by the keyed `patient_key`), not under the master key; the key itself is stored wrapped by the
//!   master key, in a separate database that is backed up on its own, short schedule. Erasing a patient deletes that one key: every copy of
//!   their sealed data that survives anywhere, in a backup included, can then never be read again. Tokens made this way start with `p1:`;
//!   older ones (no prefix) still open with the master key.
//! * It does not hide the *shape* of the data (how many claims, which states, which lanes) or protect against anyone who has the key.

use hmac::{Hmac, Mac};
use serde_json::{json, Map, Value};
use sha2::Sha256;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Top-level fields of a claim document that are encrypted. `decisions` is a list whose elements are encrypted one by one.
pub const SEALED_FIELDS: [&str; 5] = ["claim", "results", "advisory", "explanation", "corrected_from"];
const MARKER: &str = "$enc";
const TEXT_PREFIX: &str = "enc1:";
/// Marks a token sealed under a patient's own key.
const PATIENT_PREFIX: &str = "p1:";
/// How long a process keeps an unwrapped patient key. An erasure clears it at once in the process that does it; another process may still
/// open that patient's data for up to this long.
const KEY_CACHE: Duration = Duration::from_secs(30);

#[derive(Debug, PartialEq)]
pub struct CryptError(pub String);

/// Where the wrapped per-patient keys live. It holds only wrapped keys, so it never sees one in the clear.
pub trait KeyStore: Send + Sync {
    fn get(&self, patient_key: &str) -> Result<Option<String>, CryptError>;
    /// Stores the key unless one exists; true if this call stored it.
    fn put_if_absent(&self, patient_key: &str, wrapped: &str) -> Result<bool, CryptError>;
    fn replace(&self, patient_key: &str, wrapped: &str) -> Result<(), CryptError>;
    /// True if a key was deleted.
    fn delete(&self, patient_key: &str) -> Result<bool, CryptError>;
    fn list(&self) -> Result<Vec<(String, String)>, CryptError>;
}

/// A key store in memory, for tests and for the development server.
#[derive(Default)]
pub struct MemoryKeyStore(Mutex<BTreeMap<String, String>>);

impl MemoryKeyStore {
    pub fn new() -> MemoryKeyStore {
        MemoryKeyStore::default()
    }
}

impl KeyStore for MemoryKeyStore {
    fn get(&self, k: &str) -> Result<Option<String>, CryptError> {
        Ok(self.0.lock().unwrap().get(k).cloned())
    }
    fn put_if_absent(&self, k: &str, w: &str) -> Result<bool, CryptError> {
        let mut m = self.0.lock().unwrap();
        if m.contains_key(k) {
            return Ok(false);
        }
        m.insert(k.to_string(), w.to_string());
        Ok(true)
    }
    fn replace(&self, k: &str, w: &str) -> Result<(), CryptError> {
        self.0.lock().unwrap().insert(k.to_string(), w.to_string());
        Ok(())
    }
    fn delete(&self, k: &str) -> Result<bool, CryptError> {
        Ok(self.0.lock().unwrap().remove(k).is_some())
    }
    fn list(&self) -> Result<Vec<(String, String)>, CryptError> {
        Ok(self.0.lock().unwrap().iter().map(|(k, v)| (k.clone(), v.clone())).collect())
    }
}

pub struct FieldCrypt {
    fernet: fernet::Fernet,
    mac_key: Vec<u8>,
    keys: Option<Arc<dyn KeyStore>>,
    cache: Mutex<HashMap<String, (Instant, Arc<fernet::Fernet>)>>,
}

fn derive(secret: &str, label: &str) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("a key of any length is accepted");
    mac.update(label.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

impl FieldCrypt {
    /// `data_key`: a Fernet key (`DATA_KEY`) used as is; otherwise the keys are derived from `fernet_key` (`FERNET_KEY`).
    pub fn new(fernet_key: &str, data_key: Option<&str>) -> Result<FieldCrypt, CryptError> {
        let fernet = match data_key.filter(|k| !k.trim().is_empty()) {
            Some(k) => fernet::Fernet::new(k.trim()).ok_or_else(|| CryptError("DATA_KEY is not a valid Fernet key".into()))?,
            None => {
                use base64::Engine as _;
                let key = base64::engine::general_purpose::URL_SAFE.encode(derive(fernet_key, "claimguard data at rest v1: encryption"));
                fernet::Fernet::new(&key).ok_or_else(|| CryptError("cannot derive a data key".into()))?
            }
        };
        Ok(FieldCrypt { fernet, mac_key: derive(data_key.filter(|k| !k.trim().is_empty()).unwrap_or(fernet_key), "claimguard data at rest v1: patient index"), keys: None, cache: Mutex::new(HashMap::new()) })
    }

    /// Seals new claim data under a key per patient, kept in `store` (see the module notes).
    pub fn with_patient_keys(mut self, store: Arc<dyn KeyStore>) -> FieldCrypt {
        self.keys = Some(store);
        self
    }

    pub fn has_patient_keys(&self) -> bool {
        self.keys.is_some()
    }

    fn wrap(&self, patient_key: &str, key: &str) -> String {
        self.fernet.encrypt(json!({"p": patient_key, "k": key}).to_string().as_bytes())
    }

    fn unwrap_key(&self, patient_key: &str, wrapped: &str) -> Result<String, CryptError> {
        let plain = self.fernet.decrypt(wrapped).map_err(|_| CryptError("a patient key cannot be unwrapped (wrong master key, or it was changed)".into()))?;
        let env: Value = serde_json::from_slice(&plain).map_err(|_| CryptError("a patient key is damaged".into()))?;
        if env["p"] != patient_key {
            return Err(CryptError("a patient key belongs to another patient: it was moved".into()));
        }
        Ok(env["k"].as_str().unwrap_or("").to_string())
    }

    /// The patient's own cipher: from the cache, else from the key store; made if `create`. None when there is no key (erased, or never made).
    fn patient_fernet(&self, patient_key: &str, create: bool) -> Result<Option<Arc<fernet::Fernet>>, CryptError> {
        let Some(store) = &self.keys else { return Ok(None) };
        if let Some((at, f)) = self.cache.lock().unwrap().get(patient_key) {
            if at.elapsed() < KEY_CACHE {
                return Ok(Some(f.clone()));
            }
        }
        let wrapped = match store.get(patient_key)? {
            Some(w) => Some(w),
            None if create => {
                let wrapped = self.wrap(patient_key, &fernet::Fernet::generate_key());
                if store.put_if_absent(patient_key, &wrapped)? {
                    Some(wrapped)
                } else {
                    store.get(patient_key)?
                }
            }
            None => None,
        };
        let Some(wrapped) = wrapped else {
            self.cache.lock().unwrap().remove(patient_key);
            return Ok(None);
        };
        let f = Arc::new(fernet::Fernet::new(&self.unwrap_key(patient_key, &wrapped)?).ok_or_else(|| CryptError("a patient key is not a valid key".into()))?);
        self.cache.lock().unwrap().insert(patient_key.to_string(), (Instant::now(), f.clone()));
        Ok(Some(f))
    }

    /// Deletes a patient's key for good. True if there was one. Their sealed data can no longer be read, wherever a copy of it is.
    pub fn forget_patient_key(&self, patient_key: &str) -> Result<bool, CryptError> {
        self.cache.lock().unwrap().remove(patient_key);
        match &self.keys {
            Some(s) => s.delete(patient_key),
            None => Ok(false),
        }
    }

    /// Rotation: the patient index depends on the data key, so a rotated patient gets a new index. This stores the same patient key under
    /// `new_pk`, wrapped by `new`'s master key, so documents that carry `new_pk` open with `new`. The old entry is removed afterwards by the
    /// caller, once no document carries `old_pk`. Safe to repeat.
    pub fn carry_patient_key(&self, old_pk: &str, new: &FieldCrypt, new_pk: &str) -> Result<(), CryptError> {
        let store = self.keys.as_ref().ok_or_else(|| CryptError("per-patient keys are not switched on".into()))?;
        let wrapped = store.get(old_pk)?.ok_or_else(|| CryptError("this patient's key no longer exists: their data was erased and cannot be read".into()))?;
        let key = self.unwrap_key(old_pk, &wrapped)?;
        store.put_if_absent(new_pk, &new.wrap(new_pk, &key))?;
        Ok(())
    }

    /// True if the document's data is sealed under a patient's own key.
    pub fn is_patient_sealed(doc: &Value) -> bool {
        SEALED_FIELDS.iter().any(|f| doc[*f][MARKER].as_str().is_some_and(|t| t.starts_with(PATIENT_PREFIX)))
    }

    /// A short text (a person's name) as `enc1:<token>`, bound to its owner (`scope`, e.g. the badge) and field name.
    pub fn seal_text(&self, scope: &str, field: &str, text: &str) -> String {
        format!("{TEXT_PREFIX}{}", self.fernet.encrypt(json!({"s": scope, "f": field, "d": text}).to_string().as_bytes()))
    }

    /// The text behind `seal_text`; text written before encryption was on (no prefix) is returned as it is.
    pub fn open_text(&self, scope: &str, field: &str, stored: &str) -> Result<String, CryptError> {
        let Some(token) = stored.strip_prefix(TEXT_PREFIX) else { return Ok(stored.to_string()) };
        let plain = self.fernet.decrypt(token).map_err(|_| CryptError("a stored text cannot be decrypted (wrong key, or the data was changed)".into()))?;
        let env: Value = serde_json::from_slice(&plain).map_err(|_| CryptError("a stored text is damaged".into()))?;
        if env["s"] != scope || env["f"] != field {
            return Err(CryptError("an encrypted text belongs to another record or field: the data was moved".into()));
        }
        Ok(env["d"].as_str().unwrap_or("").to_string())
    }

    pub fn is_sealed_text(stored: &str) -> bool {
        stored.starts_with(TEXT_PREFIX)
    }

    /// A keyed hash of a patient id, stored in the clear so a patient's earlier claims can be found without the id being readable.
    pub fn patient_key(&self, patient_id: &str) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.mac_key).expect("a key of any length is accepted");
        mac.update(format!("patient|{patient_id}").as_bytes());
        mac.finalize().into_bytes().iter().take(16).map(|b| format!("{b:02x}")).collect()
    }

    /// `value` as an opaque marker object, bound to (claim id, version, field). With a `patient_key` and a key store it is sealed under
    /// that patient's own key, otherwise under the master key.
    pub fn seal(&self, patient_key: Option<&str>, claim_id: &str, version: i64, field: &str, value: &Value) -> Result<Value, CryptError> {
        let envelope = json!({"c": claim_id, "v": version, "f": field, "d": value}).to_string();
        let token = match patient_key.filter(|_| self.keys.is_some()) {
            Some(pk) => {
                let f = self.patient_fernet(pk, true)?.ok_or_else(|| CryptError("no key could be made for this patient".into()))?;
                format!("{PATIENT_PREFIX}{}", f.encrypt(envelope.as_bytes()))
            }
            None => self.fernet.encrypt(envelope.as_bytes()),
        };
        Ok(json!({ MARKER: token }))
    }

    /// Seals the sensitive fields of a whole new document in place and adds the patient index.
    pub fn seal_new_doc(&self, doc: &mut Value) -> Result<(), CryptError> {
        let (id, version) = (doc["claim_id"].as_str().unwrap_or("").to_string(), doc["version"].as_i64().unwrap_or(0));
        if let Some(p) = doc["claim"]["patient_id"].as_str() {
            let key = self.patient_key(p);
            doc["patient_key"] = json!(key);
        }
        let pk = doc["patient_key"].as_str().map(str::to_string);
        for field in SEALED_FIELDS {
            if let Some(v) = doc.get(field).filter(|v| !v.is_null()).cloned() {
                doc[field] = self.seal(pk.as_deref(), &id, version, field, &v)?;
            }
        }
        Ok(())
    }

    /// The stored form of a plain document: every sensitive field sealed, each decision sealed on its own, the patient index added.
    pub fn seal_stored(&self, plain: &Value) -> Result<Value, CryptError> {
        let mut doc = plain.clone();
        let (id, version) = (doc["claim_id"].as_str().unwrap_or("").to_string(), doc["version"].as_i64().unwrap_or(0));
        self.seal_new_doc(&mut doc)?;
        let pk = doc["patient_key"].as_str().map(str::to_string);
        if let Some(list) = doc.get("decisions").and_then(Value::as_array).cloned() {
            doc["decisions"] = Value::Array(list.iter().map(|d| self.seal(pk.as_deref(), &id, version, "decisions", d)).collect::<Result<_, _>>()?);
        }
        Ok(doc)
    }

    /// Seals the sensitive entries of a `set` map (only `explanation` can occur) for the given claim.
    pub fn seal_set(&self, patient_key: Option<&str>, claim_id: &str, version: i64, set: &mut Map<String, Value>) -> Result<(), CryptError> {
        for field in SEALED_FIELDS {
            if let Some(v) = set.get(field).filter(|v| !v.is_null()).cloned() {
                set.insert(field.to_string(), self.seal(patient_key, claim_id, version, field, &v)?);
            }
        }
        Ok(())
    }

    /// Decrypts every marker object found anywhere in a stored document, checking each against the document it sits in.
    pub fn open(&self, doc: Value) -> Result<Value, CryptError> {
        let ids = (doc.get("claim_id").and_then(Value::as_str).map(str::to_string), doc.get("version").and_then(Value::as_i64), doc.get("patient_key").and_then(Value::as_str).map(str::to_string));
        self.walk(doc, &ids, "")
    }

    fn walk(&self, v: Value, ids: &(Option<String>, Option<i64>, Option<String>), field: &str) -> Result<Value, CryptError> {
        match v {
            Value::Object(m) if m.len() == 1 && m.contains_key(MARKER) => {
                let token = m[MARKER].as_str().ok_or_else(|| CryptError("an encrypted field is not text".into()))?;
                let plain = match token.strip_prefix(PATIENT_PREFIX) {
                    Some(t) => {
                        let pk = ids.2.as_deref().ok_or_else(|| CryptError("stored data is sealed under a patient key but names no patient".into()))?;
                        let f = self.patient_fernet(pk, false)?.ok_or_else(|| CryptError("this patient's key no longer exists: their data was erased and cannot be read".into()))?;
                        f.decrypt(t).map_err(|_| CryptError("stored data cannot be decrypted (wrong key, or the data was changed)".into()))?
                    }
                    None => self.fernet.decrypt(token).map_err(|_| CryptError("stored data cannot be decrypted (wrong key, or the data was changed)".into()))?,
                };
                let env: Value = serde_json::from_slice(&plain).map_err(|_| CryptError("stored data is damaged".into()))?;
                let here_ok = ids.0.as_deref().map_or(true, |id| env["c"] == id) && ids.1.map_or(true, |ver| env["v"] == ver) && (field.is_empty() || env["f"] == field);
                if !here_ok {
                    return Err(CryptError("an encrypted field belongs to another claim, version or field: the data was moved".into()));
                }
                Ok(env["d"].clone())
            }
            Value::Object(m) => {
                let mut out = Map::new();
                for (k, x) in m {
                    let key = if field.is_empty() { k.clone() } else { field.to_string() };
                    out.insert(k, self.walk(x, ids, &key)?);
                }
                Ok(Value::Object(out))
            }
            Value::Array(a) => Ok(Value::Array(a.into_iter().map(|x| self.walk(x, ids, field)).collect::<Result<_, _>>()?)),
            other => Ok(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "ZmFrZS1mZXJuZXQta2V5LWZvci10ZXN0cy0wMDAwMDA=";

    fn crypt() -> FieldCrypt {
        let k = fernet::Fernet::generate_key();
        FieldCrypt::new(&k, None).unwrap()
    }

    #[test]
    fn sealed_fields_are_opaque_and_come_back_exactly() {
        let c = crypt();
        let mut doc = json!({"claim_id": "C1", "version": 2, "claim": {"patient_id": "PAT-123", "notes": "knee surgery"}, "results": [{"rule_id": "R001"}], "advisory": [], "receipt": {"lane": "A"}, "state": "triaged"});
        let plain = doc.clone();
        c.seal_new_doc(&mut doc).unwrap();
        let text = doc.to_string();
        assert!(!text.contains("PAT-123") && !text.contains("knee surgery") && !text.contains("R001"), "{text}");
        assert!(text.contains("\"lane\":\"A\"") && text.contains("\"state\":\"triaged\""), "routing data stays readable");
        assert_eq!(doc["patient_key"].as_str().unwrap().len(), 32);
        let mut back = c.open(doc).unwrap();
        back.as_object_mut().unwrap().remove("patient_key");
        assert_eq!(back, plain);
    }

    #[test]
    fn the_patient_key_is_stable_keyed_and_not_the_id() {
        let (a, b) = (crypt(), crypt());
        assert_eq!(a.patient_key("PAT-1"), a.patient_key("PAT-1"));
        assert_ne!(a.patient_key("PAT-1"), a.patient_key("PAT-2"));
        assert_ne!(a.patient_key("PAT-1"), b.patient_key("PAT-1"), "another key gives another index");
        assert!(!a.patient_key("PAT-1").contains("PAT"));
    }

    #[test]
    fn a_field_moved_to_another_claim_version_or_name_is_refused() {
        let c = crypt();
        let mut one = json!({"claim_id": "C1", "version": 1, "claim": {"x": 1}, "results": []});
        let mut two = json!({"claim_id": "C2", "version": 1, "claim": {"x": 2}, "results": []});
        c.seal_new_doc(&mut one).unwrap();
        c.seal_new_doc(&mut two).unwrap();
        let mut swapped = two.clone();
        swapped["claim"] = one["claim"].clone();
        assert!(c.open(swapped).unwrap_err().0.contains("moved"), "claim C1's data under C2");
        let mut renamed = one.clone();
        renamed["results"] = one["claim"].clone();
        assert!(c.open(renamed).unwrap_err().0.contains("moved"), "the claim field read as the results field");
        let mut newer = one.clone();
        newer["version"] = json!(2);
        assert!(c.open(newer).is_err(), "version 1's data under version 2");
        assert!(c.open(one).is_ok());
    }

    #[test]
    fn the_wrong_key_garbage_and_old_plain_documents() {
        let (a, b) = (crypt(), crypt());
        let mut doc = json!({"claim_id": "C1", "version": 1, "claim": {"x": 1}});
        a.seal_new_doc(&mut doc).unwrap();
        assert!(b.open(doc.clone()).unwrap_err().0.contains("cannot be decrypted"));
        let mut broken = doc.clone();
        broken["claim"] = json!({"$enc": "not a token"});
        assert!(a.open(broken).is_err());
        let plain = json!({"claim_id": "C1", "version": 1, "claim": {"x": 1}, "results": []});
        assert_eq!(a.open(plain.clone()).unwrap(), plain, "a document written before encryption was on reads as it is");
    }

    #[test]
    fn a_separate_data_key_is_used_when_given_and_checked() {
        let fk = fernet::Fernet::generate_key();
        let dk = fernet::Fernet::generate_key();
        let (derived, separate) = (FieldCrypt::new(&fk, None).unwrap(), FieldCrypt::new(&fk, Some(&dk)).unwrap());
        let mut doc = json!({"claim_id": "C1", "version": 1, "claim": {"x": 1}});
        separate.seal_new_doc(&mut doc).unwrap();
        assert!(derived.open(doc.clone()).is_err() && separate.open(doc).is_ok());
        assert!(FieldCrypt::new(&fk, Some("not a key")).is_err());
        let _ = KEY;
    }

    fn shredding_crypt() -> (FieldCrypt, Arc<MemoryKeyStore>) {
        let keys = Arc::new(MemoryKeyStore::new());
        (FieldCrypt::new(&fernet::Fernet::generate_key(), None).unwrap().with_patient_keys(keys.clone()), keys)
    }

    fn claim(id: &str, patient: &str) -> Value {
        json!({"claim_id": id, "version": 1, "claim": {"patient_id": patient, "notes": "knee surgery"}, "results": [{"rule_id": "R001"}], "state": "triaged"})
    }

    #[test]
    fn data_sealed_under_a_patient_key_dies_with_that_key() {
        let (c, keys) = shredding_crypt();
        let (mut a, mut b) = (claim("C1", "PAT-A"), claim("C2", "PAT-B"));
        c.seal_new_doc(&mut a).unwrap();
        c.seal_new_doc(&mut b).unwrap();
        assert!(a["claim"]["$enc"].as_str().unwrap().starts_with("p1:"), "a patient's data carries the patient-key marker");
        assert_eq!(keys.list().unwrap().len(), 2, "one key per patient");
        let copy_of_a = a.clone(); // what a backup would hold
        assert!(c.open(a.clone()).is_ok() && c.open(b.clone()).is_ok());
        let pk = c.patient_key("PAT-A");
        assert!(c.forget_patient_key(&pk).unwrap());
        let e = c.open(copy_of_a.clone()).unwrap_err().0;
        assert!(e.contains("no longer exists"), "{e}");
        assert!(c.open(b).is_ok(), "another patient is untouched");
        assert!(!c.forget_patient_key(&pk).unwrap(), "forgetting twice is harmless");
        // a second process with a cold cache and the same stores cannot read it either
        let other = FieldCrypt::new("irrelevant", None).unwrap().with_patient_keys(keys);
        assert!(other.open(copy_of_a).is_err());
    }

    #[test]
    fn a_patient_who_returns_gets_a_new_key_that_cannot_open_the_old_data() {
        let (c, _keys) = shredding_crypt();
        let mut old = claim("C1", "PAT-A");
        c.seal_new_doc(&mut old).unwrap();
        c.forget_patient_key(&c.patient_key("PAT-A")).unwrap();
        let mut fresh = claim("C9", "PAT-A");
        c.seal_new_doc(&mut fresh).unwrap();
        assert!(c.open(fresh).is_ok());
        assert!(c.open(old).is_err(), "the old ciphertext stays unreadable even though the patient has a key again");
    }

    #[test]
    fn old_master_key_documents_still_open_and_a_store_without_keys_cannot_open_patient_data() {
        let master = FieldCrypt::new("k", None).unwrap();
        let mut legacy = claim("C1", "PAT-A");
        master.seal_new_doc(&mut legacy).unwrap();
        assert!(!legacy["claim"]["$enc"].as_str().unwrap().starts_with("p1:"));
        let with_keys = FieldCrypt::new("k", None).unwrap().with_patient_keys(Arc::new(MemoryKeyStore::new()));
        assert!(with_keys.open(legacy).is_ok(), "a document from before per-patient keys opens with the master key");
        let mut sealed = claim("C2", "PAT-A");
        with_keys.seal_new_doc(&mut sealed).unwrap();
        assert!(master.open(sealed).unwrap_err().0.contains("no longer exists"));
    }

    #[test]
    fn a_patient_key_moved_to_another_patient_or_the_wrong_master_is_refused() {
        let (c, keys) = shredding_crypt();
        let (mut a, mut b) = (claim("C1", "PAT-A"), claim("C2", "PAT-B"));
        c.seal_new_doc(&mut a).unwrap();
        c.seal_new_doc(&mut b).unwrap();
        let (pa, pb) = (c.patient_key("PAT-A"), c.patient_key("PAT-B"));
        let wrapped_a = keys.get(&pa).unwrap().unwrap();
        keys.replace(&pb, &wrapped_a).unwrap(); // someone with database access swaps one patient's key under another
        // a different master key cannot unwrap anything
        let cold = FieldCrypt::new(&fernet::Fernet::generate_key(), None).unwrap().with_patient_keys(keys.clone());
        assert!(cold.open(a.clone()).is_err());
        c.cache.lock().unwrap().clear();
        assert!(c.open(b).unwrap_err().0.contains("belongs to another patient"), "the wrapped key names its patient");
        assert!(c.open(a).is_ok());
    }

    #[test]
    fn rotating_the_data_key_carries_each_patient_key_to_its_new_index() {
        let keys = Arc::new(MemoryKeyStore::new());
        let (fk1, fk2) = (fernet::Fernet::generate_key(), fernet::Fernet::generate_key());
        let old = FieldCrypt::new("x", Some(&fk1)).unwrap().with_patient_keys(keys.clone());
        let new = FieldCrypt::new("x", Some(&fk2)).unwrap().with_patient_keys(keys.clone());
        let mut doc = claim("C1", "PAT-A");
        old.seal_new_doc(&mut doc).unwrap();
        let (old_pk, new_pk) = (old.patient_key("PAT-A"), new.patient_key("PAT-A"));
        assert_ne!(old_pk, new_pk, "the index depends on the data key");
        assert!(new.open(doc.clone()).is_err(), "before the carry the new key cannot open it");
        old.carry_patient_key(&old_pk, &new, &new_pk).unwrap();
        old.carry_patient_key(&old_pk, &new, &new_pk).unwrap(); // repeating is harmless
        let mut moved = doc.clone();
        moved["patient_key"] = json!(new_pk);
        assert!(new.open(moved).is_ok(), "the same data opens once the document points at the new index");
        assert!(old.forget_patient_key(&old_pk).unwrap());
        assert!(FieldCrypt::is_patient_sealed(&doc));
    }
}
