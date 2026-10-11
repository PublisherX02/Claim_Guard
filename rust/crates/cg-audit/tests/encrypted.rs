//! The audit log encrypted at rest. This file is its own test program because the key is process-wide.

use cg_audit::{atrest, mirror, verify, verify_with_anchor, AuditLog};
use serde_json::json;

fn login(badge: &str) -> serde_json::Value {
    json!({"event_type": "login_success", "badge_id": badge})
}

#[test]
fn an_encrypted_log_hides_its_content_and_still_verifies_mirrors_and_mixes_with_plain_rows() {
    let key = atrest::derive_key("a deployment secret");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.jsonl");

    // rows written before the key existed stay readable afterwards
    atrest::set_key(None).unwrap();
    let log = AuditLog::open(&path).unwrap();
    log.append_system_events(&[login("CG-1001")]).unwrap();
    assert!(std::fs::read_to_string(&path).unwrap().contains("CG-1001"));

    atrest::set_key(Some(&key)).unwrap();
    let log = AuditLog::open(&path).unwrap();
    let (head, count) = log.append_system_events(&[login("CG-2002"), login("CG-3003")]).unwrap();
    assert_eq!(count, 3);
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(raw.contains("CG-1001"), "the first row is still plain");
    assert!(!raw.contains("CG-2002") && !raw.contains("CG-3003"), "later rows are ciphertext: {raw}");
    assert_eq!(raw.matches("enc1:").count(), 2);
    assert_eq!(verify(&path).unwrap(), (head.clone(), 3), "the chain verifies over the plain rows");
    assert_eq!(verify_with_anchor(&path, None, true).unwrap().1, 3);
    assert_eq!(cg_audit::read_rows(&path).unwrap().len(), 3);
    assert_eq!(AuditLog::open(&path).unwrap().append_system_events(&[login("CG-4004")]).unwrap().1, 4, "appending to a mixed log continues the chain");

    // the mirror copies stored bytes, so the second copy is encrypted too
    let m = dir.path().join("mirror").join("a.jsonl");
    mirror::sync_mirror(&path, &m).unwrap();
    assert_eq!(std::fs::read(&m).unwrap(), std::fs::read(&path).unwrap());
    assert!(!std::fs::read_to_string(&m).unwrap().contains("CG-4004"));

    // altering one byte of a ciphertext row is caught (by the cipher's own tag, then by the chain)
    let text = std::fs::read_to_string(&path).unwrap();
    let mut bytes = text.clone().into_bytes();
    let at = text.find("enc1:").unwrap() + 40;
    bytes[at] = if bytes[at] == b'A' { b'B' } else { b'A' };
    std::fs::write(&path, &bytes).unwrap();
    assert!(verify(&path).is_err());
    assert!(AuditLog::open(&path).is_err());
    std::fs::write(&path, text).unwrap();
    assert!(verify(&path).is_ok());

    // the wrong key, and no key, are errors and never a silent skip
    atrest::set_key(Some(&atrest::derive_key("another secret"))).unwrap();
    assert!(verify(&path).unwrap_err().to_string().contains("cannot be decrypted"));
    assert!(cg_audit::read_rows(&path).is_err());
    atrest::set_key(None).unwrap();
    assert!(verify(&path).unwrap_err().to_string().contains("no audit log key"));
    atrest::set_key(Some(&key)).unwrap();
    assert!(verify(&path).is_ok());
    assert!(atrest::set_key(Some("not a key")).is_err());

    // rotation: new rows use the new key, the old key stays as a read-only previous key, and nothing is rewritten
    let newer = atrest::derive_key("the next secret");
    atrest::set_keys(Some(&newer), &[key.clone()]).unwrap();
    let before = std::fs::read(&path).unwrap();
    assert!(verify(&path).is_ok(), "rows under the previous key are still read");
    let log = AuditLog::open(&path).unwrap();
    assert_eq!(log.append_system_events(&[login("CG-5005")]).unwrap().1, 5);
    let after = std::fs::read(&path).unwrap();
    assert_eq!(&after[..before.len()], &before[..], "the earlier rows are byte for byte unchanged");
    assert!(verify(&path).is_ok());
    atrest::set_key(Some(&newer)).unwrap();
    assert!(verify(&path).is_err(), "without the previous key the older rows cannot be read");
    atrest::set_key(None).unwrap();
}
