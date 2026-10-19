//! The per-patient key store in MongoDB: one document per patient in its own database (`<queue db>_keys`), holding only the key wrapped by
//! the master key. It is a separate database so the backup script can dump it into a file of its own with a short retention (docs/59):
//! once the last key backup that held a patient's key has aged out, that patient's data in every older data backup is unreadable.

use crate::crypt::{CryptError, KeyStore};
use mongodb::bson::{doc, Document};
use mongodb::sync::{Client, Collection};

pub struct MongoKeyStore {
    keys: Collection<Document>,
}

fn down<E>(_: E) -> CryptError {
    CryptError("the patient key store cannot be reached".into())
}

impl MongoKeyStore {
    pub fn new(client: Client, db_name: &str) -> MongoKeyStore {
        MongoKeyStore { keys: client.database(db_name).collection("patient_keys") }
    }
}

impl KeyStore for MongoKeyStore {
    fn get(&self, patient_key: &str) -> Result<Option<String>, CryptError> {
        let d = self.keys.find_one(doc! {"_id": patient_key}).run().map_err(down)?;
        Ok(d.and_then(|d| d.get_str("wrapped").ok().map(str::to_string)))
    }

    fn put_if_absent(&self, patient_key: &str, wrapped: &str) -> Result<bool, CryptError> {
        match self.keys.insert_one(doc! {"_id": patient_key, "wrapped": wrapped}).run() {
            Ok(_) => Ok(true),
            Err(e) if e.to_string().contains("E11000") => Ok(false),
            Err(e) => Err(down(e)),
        }
    }

    fn replace(&self, patient_key: &str, wrapped: &str) -> Result<(), CryptError> {
        self.keys.replace_one(doc! {"_id": patient_key}, doc! {"_id": patient_key, "wrapped": wrapped}).upsert(true).run().map(|_| ()).map_err(down)
    }

    fn delete(&self, patient_key: &str) -> Result<bool, CryptError> {
        Ok(self.keys.delete_one(doc! {"_id": patient_key}).run().map_err(down)?.deleted_count > 0)
    }

    fn list(&self) -> Result<Vec<(String, String)>, CryptError> {
        let mut out = vec![];
        for d in self.keys.find(doc! {}).run().map_err(down)? {
            let d = d.map_err(down)?;
            out.push((d.get_str("_id").unwrap_or("").to_string(), d.get_str("wrapped").unwrap_or("").to_string()));
        }
        Ok(out)
    }
}
