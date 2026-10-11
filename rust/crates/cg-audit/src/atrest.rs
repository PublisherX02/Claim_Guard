//! Encryption of the audit log files at rest.
//!
//! The logs hold badge numbers, claim ids, client-independent reasons and what each person did. A copy of the disk, a backup, or an
//! administrator who can read the volume would see all of it. With a key set (`set_key`), every row is stored as `enc1:` followed by a
//! Fernet token of the row's JSON line; the hash chain, the signed anchor and the checks work on the plain row exactly as before (the
//! row is opened first), so verification and the Python build's format are unchanged for a plain log. Rows written before the key was
//! switched on stay plain and are still read; after that the log is a mix, which is fine because every row is self-describing.
//!
//! The mirror copies stored bytes, so the second copy is encrypted too. The anchor (`<log>.head.json`) holds only the head hash, the
//! count and a signature, nothing personal, and stays readable so it can be checked and timestamped without the key.
//!
//! The key is process-wide (one deployment has one key), set once at start-up. Rotating it never rewrites the log (that would break
//! the mirror and every earlier stamp): new rows use the new key, and the older keys are kept as read-only `previous` keys. Opening a row that is encrypted when no key is set, or
//! with the wrong key, is an error, never a silent skip: an unreadable row would otherwise look like a missing one.

use crate::AuditError;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::sync::RwLock;

pub const PREFIX: &str = "enc1:";
struct Keys {
    current: Option<fernet::Fernet>,
    previous: Vec<fernet::Fernet>,
}

static KEYS: RwLock<Keys> = RwLock::new(Keys { current: None, previous: Vec::new() });

/// A Fernet key derived from a secret the deployment already has (`DATA_KEY` or `FERNET_KEY`), separate from the claim-data key.
pub fn derive_key(secret: &str) -> String {
    use base64::Engine as _;
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("a key of any length is accepted");
    mac.update(b"claimguard audit log at rest v1");
    base64::engine::general_purpose::URL_SAFE.encode(mac.finalize().into_bytes())
}

/// Switches encryption of newly written rows on (a Fernet key) or off (None). `previous` are older keys, used only for reading.
pub fn set_keys(current: Option<&str>, previous: &[String]) -> Result<(), AuditError> {
    let parse = |k: &str| fernet::Fernet::new(k.trim()).ok_or_else(|| AuditError::Invalid("an audit log key is not a valid Fernet key".into()));
    let keys = Keys { current: current.map(parse).transpose()?, previous: previous.iter().map(|k| parse(k)).collect::<Result<_, _>>()? };
    *KEYS.write().unwrap_or_else(|e| e.into_inner()) = keys;
    Ok(())
}

pub fn set_key(key: Option<&str>) -> Result<(), AuditError> {
    set_keys(key, &[])
}

pub fn enabled() -> bool {
    KEYS.read().unwrap_or_else(|e| e.into_inner()).current.is_some()
}

/// The stored form of a row (one line, no newline).
pub fn seal_line(plain: &str) -> String {
    match KEYS.read().unwrap_or_else(|e| e.into_inner()).current.as_ref() {
        Some(f) => format!("{PREFIX}{}", f.encrypt(plain.as_bytes())),
        None => plain.to_string(),
    }
}

/// The plain row for a stored line: encrypted lines are opened, plain lines pass through.
pub fn open_line(line: &str) -> Result<String, AuditError> {
    let Some(token) = line.trim_end_matches('\r').strip_prefix(PREFIX) else { return Ok(line.to_string()) };
    let guard = KEYS.read().unwrap_or_else(|e| e.into_inner());
    if guard.current.is_none() && guard.previous.is_empty() {
        return Err(AuditError::Invalid("the audit log is encrypted and no audit log key is set".into()));
    }
    let bytes = guard
        .current
        .iter()
        .chain(guard.previous.iter())
        .find_map(|f| f.decrypt(token).ok())
        .ok_or_else(|| AuditError::Invalid("an audit row cannot be decrypted (wrong key, or the row was altered)".into()))?;
    String::from_utf8(bytes).map_err(|_| AuditError::Invalid("an audit row is not valid UTF-8".into()))
}
