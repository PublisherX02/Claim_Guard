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
//! * It does not hide the *shape* of the data (how many claims, which states, which lanes) or protect against anyone who has the key.

use hmac::{Hmac, Mac};
use serde_json::{json, Map, Value};
use sha2::Sha256;

/// Top-level fields of a claim document that are encrypted. `decisions` is a list whose elements are encrypted one by one.
pub const SEALED_FIELDS: [&str; 4] = ["claim", "results", "advisory", "explanation"];
const MARKER: &str = "$enc";
const TEXT_PREFIX: &str = "enc1:";

#[derive(Debug, PartialEq)]
pub struct CryptError(pub String);

pub struct FieldCrypt {
    fernet: fernet::Fernet,
    mac_key: Vec<u8>,
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
        Ok(FieldCrypt { fernet, mac_key: derive(data_key.filter(|k| !k.trim().is_empty()).unwrap_or(fernet_key), "claimguard data at rest v1: patient index") })
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

    /// `value` as an opaque marker object, bound to (claim id, version, field).
    pub fn seal(&self, claim_id: &str, version: i64, field: &str, value: &Value) -> Value {
        let envelope = json!({"c": claim_id, "v": version, "f": field, "d": value});
        json!({ MARKER: self.fernet.encrypt(envelope.to_string().as_bytes()) })
    }

    /// Seals the sensitive fields of a whole new document in place and adds the patient index.
    pub fn seal_new_doc(&self, doc: &mut Value) {
        let (id, version) = (doc["claim_id"].as_str().unwrap_or("").to_string(), doc["version"].as_i64().unwrap_or(0));
        if let Some(p) = doc["claim"]["patient_id"].as_str() {
            let key = self.patient_key(p);
            doc["patient_key"] = json!(key);
        }
        for field in SEALED_FIELDS {
            if let Some(v) = doc.get(field).filter(|v| !v.is_null()).cloned() {
                doc[field] = self.seal(&id, version, field, &v);
            }
        }
    }

    /// The stored form of a plain document: every sensitive field sealed, each decision sealed on its own, the patient index added.
    pub fn seal_stored(&self, plain: &Value) -> Value {
        let mut doc = plain.clone();
        let (id, version) = (doc["claim_id"].as_str().unwrap_or("").to_string(), doc["version"].as_i64().unwrap_or(0));
        self.seal_new_doc(&mut doc);
        if let Some(list) = doc.get("decisions").and_then(Value::as_array).cloned() {
            doc["decisions"] = Value::Array(list.iter().map(|d| self.seal(&id, version, "decisions", d)).collect());
        }
        doc
    }

    /// Seals the sensitive entries of a `set` map (only `explanation` can occur) for the given claim.
    pub fn seal_set(&self, claim_id: &str, version: i64, set: &mut Map<String, Value>) {
        for field in SEALED_FIELDS {
            if let Some(v) = set.get(field).filter(|v| !v.is_null()).cloned() {
                set.insert(field.to_string(), self.seal(claim_id, version, field, &v));
            }
        }
    }

    /// Decrypts every marker object found anywhere in a stored document, checking each against the document it sits in.
    pub fn open(&self, doc: Value) -> Result<Value, CryptError> {
        let ids = (doc.get("claim_id").and_then(Value::as_str).map(str::to_string), doc.get("version").and_then(Value::as_i64));
        self.walk(doc, &ids, "")
    }

    fn walk(&self, v: Value, ids: &(Option<String>, Option<i64>), field: &str) -> Result<Value, CryptError> {
        match v {
            Value::Object(m) if m.len() == 1 && m.contains_key(MARKER) => {
                let token = m[MARKER].as_str().ok_or_else(|| CryptError("an encrypted field is not text".into()))?;
                let plain = self.fernet.decrypt(token).map_err(|_| CryptError("stored data cannot be decrypted (wrong key, or the data was changed)".into()))?;
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
        c.seal_new_doc(&mut doc);
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
        c.seal_new_doc(&mut one);
        c.seal_new_doc(&mut two);
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
        a.seal_new_doc(&mut doc);
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
        separate.seal_new_doc(&mut doc);
        assert!(derived.open(doc.clone()).is_err() && separate.open(doc).is_ok());
        assert!(FieldCrypt::new(&fk, Some("not a key")).is_err());
        let _ = KEY;
    }
}
