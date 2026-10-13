//! Signed checkpoints and inclusion proofs for the audit logs: the log as a ledger anyone can verify with a public key.
//!
//! The logs are already hash chains (every row names the hash of the row before it) with an HMAC anchor, but an HMAC can only be checked by someone who holds
//! the secret, and a chain alone cannot show that rows were not cut off the end. A **checkpoint** closes both gaps: at intervals a separate process signs, with
//! an Ed25519 key, the log's name, how many rows it holds, the hash of the last one, the Merkle root over every row hash and the time. Anyone who has the
//! public key (and no secret at all) can then verify that
//!   * the log still holds the exact history each checkpoint vouches for (a rewritten row changes the Merkle root, a cut log has fewer rows than a checkpoint
//!     said, a forged checkpoint fails its signature, checkpoints that go backwards in count or time are refused), and
//!   * one particular row was in the log at a checkpoint, from a short proof (a Merkle audit path) and the signed root alone, without seeing any other row.
//! The signing seed lives only in the worker; the API needs the public key. A compromised API can read and verify but cannot sign.
//!
//! Merkle tree: RFC 6962 (certificate transparency): leaf = SHA-256(0x00 || row hash), node = SHA-256(0x01 || left || right), a tree of n leaves splits at the
//! largest power of two below n.
//! Not covered, and said so: rows removed after the NEWEST surviving checkpoint (the HMAC anchor and the mirror cover the live tail), and someone who holds
//! both the log writer's files and the signing seed.

use crate::{read_rows, verify, AuditError};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
pub use ed25519_dalek::{SigningKey, VerifyingKey};
use ed25519_dalek::{Signature, Signer, Verifier};
use rand_core::OsRng;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const CHECKPOINT_VERSION: &str = "claimguard-checkpoint-v1";

fn sha(parts: &[&[u8]]) -> [u8; 32] {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

fn leaf_hash(row_hash_hex: &str) -> [u8; 32] {
    sha(&[&[0u8], &hex::decode(row_hash_hex).unwrap_or_default()])
}

fn node_hash(l: &[u8; 32], r: &[u8; 32]) -> [u8; 32] {
    sha(&[&[1u8], l, r])
}

fn split(n: usize) -> usize {
    let mut k = 1;
    while k * 2 < n {
        k *= 2;
    }
    k
}

/// The Merkle tree hash of the leaves (already leaf-hashed).
fn mth(leaves: &[[u8; 32]]) -> [u8; 32] {
    match leaves.len() {
        0 => sha(&[]),
        1 => leaves[0],
        n => {
            let k = split(n);
            node_hash(&mth(&leaves[..k]), &mth(&leaves[k..]))
        }
    }
}

/// The audit path (RFC 6962 2.1.1) for leaf `m`, oldest sibling last; each entry says whether the sibling sits to the right.
fn audit_path(m: usize, leaves: &[[u8; 32]]) -> Vec<(bool, [u8; 32])> {
    let n = leaves.len();
    if n <= 1 {
        return vec![];
    }
    let k = split(n);
    if m < k {
        let mut p = audit_path(m, &leaves[..k]);
        p.push((true, mth(&leaves[k..])));
        p
    } else {
        let mut p = audit_path(m - k, &leaves[k..]);
        p.push((false, mth(&leaves[..k])));
        p
    }
}

fn root_from_path(leaf: [u8; 32], m: usize, n: usize, path: &[(bool, [u8; 32])]) -> Option<[u8; 32]> {
    // recompute from the top: the path is ordered from the leaf upwards, so walk the tree shape from the root down to find each side
    fn go(m: usize, n: usize, path: &[(bool, [u8; 32])], leaf: [u8; 32]) -> Option<[u8; 32]> {
        if n <= 1 {
            return if path.is_empty() { Some(leaf) } else { None };
        }
        let (last, rest) = path.split_last()?;
        let k = split(n);
        if m < k {
            if !last.0 {
                return None;
            }
            Some(node_hash(&go(m, k, rest, leaf)?, &last.1))
        } else {
            if last.0 {
                return None;
            }
            Some(node_hash(&last.1, &go(m - k, n - k, rest, leaf)?))
        }
    }
    if m >= n { None } else { go(m, n, path, leaf) }
}

pub fn merkle_root_of_rows(row_hashes: &[String]) -> String {
    let leaves: Vec<[u8; 32]> = row_hashes.iter().map(|h| leaf_hash(h)).collect();
    hex::encode(mth(&leaves))
}

#[derive(Debug, Clone, PartialEq)]
pub struct Checkpoint {
    pub log: String,
    pub count: u64,
    pub head: String,
    pub merkle_root: String,
    pub time: String,
    pub key_id: String,
    pub signature: String,
}

impl Checkpoint {
    fn message(&self) -> Vec<u8> {
        format!("{CHECKPOINT_VERSION}\n{}\n{}\n{}\n{}\n{}\n{}", self.log, self.count, self.head, self.merkle_root, self.time, self.key_id).into_bytes()
    }

    pub fn to_json(&self) -> Value {
        json!({"version": CHECKPOINT_VERSION, "log": self.log, "count": self.count, "head": self.head, "merkle_root": self.merkle_root, "time": self.time, "key_id": self.key_id, "signature": self.signature})
    }

    pub fn from_json(v: &Value) -> Option<Checkpoint> {
        if v["version"].as_str()? != CHECKPOINT_VERSION {
            return None;
        }
        Some(Checkpoint { log: v["log"].as_str()?.into(), count: v["count"].as_u64()?, head: v["head"].as_str()?.into(), merkle_root: v["merkle_root"].as_str()?.into(),
                          time: v["time"].as_str()?.into(), key_id: v["key_id"].as_str()?.into(), signature: v["signature"].as_str()?.into() })
    }

    pub fn signature_ok(&self, key: &VerifyingKey) -> bool {
        let Some(sig) = B64.decode(&self.signature).ok().and_then(|b| <[u8; 64]>::try_from(b.as_slice()).ok()) else { return false };
        key.verify(&self.message(), &Signature::from_bytes(&sig)).is_ok()
    }
}

// ---- keys -------------------------------------------------------------------------------------------------------------------------

pub fn key_id(public: &VerifyingKey) -> String {
    hex::encode(&sha(&[public.as_bytes()])[..8])
}

/// A fresh key: (seed, public key), both URL-safe base64 without padding. The seed is a secret: it goes to the worker only.
pub fn keygen() -> (String, String) {
    let sk = SigningKey::generate(&mut OsRng);
    (B64.encode(sk.to_bytes()), B64.encode(sk.verifying_key().to_bytes()))
}

pub fn signing_key_from(seed_b64: &str) -> Result<SigningKey, String> {
    let bytes = B64.decode(seed_b64.trim()).map_err(|_| "the signing key is not URL-safe base64".to_string())?;
    let arr: [u8; 32] = bytes.as_slice().try_into().map_err(|_| "the signing key must be 32 bytes".to_string())?;
    Ok(SigningKey::from_bytes(&arr))
}

pub fn public_key_from(public_b64: &str) -> Result<VerifyingKey, String> {
    let bytes = B64.decode(public_b64.trim()).map_err(|_| "the public key is not URL-safe base64".to_string())?;
    let arr: [u8; 32] = bytes.as_slice().try_into().map_err(|_| "the public key must be 32 bytes".to_string())?;
    VerifyingKey::from_bytes(&arr).map_err(|_| "the public key is not a valid Ed25519 key".to_string())
}

pub fn public_key_b64(key: &VerifyingKey) -> String {
    B64.encode(key.as_bytes())
}

pub fn public_of_seed(seed_b64: &str) -> Result<String, String> {
    Ok(B64.encode(signing_key_from(seed_b64)?.verifying_key().to_bytes()))
}

// ---- checkpoints ------------------------------------------------------------------------------------------------------------------

pub fn checkpoints_path_of(log: &Path) -> PathBuf {
    let mut name = log.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".checkpoints.jsonl");
    log.with_file_name(name)
}

fn row_hashes(log: &Path) -> Result<Vec<String>, AuditError> {
    verify(log)?; // the chain must be intact before anything is signed over it
    read_rows(log)?
        .iter()
        .map(|r| serde_json::from_str::<Value>(r).ok().and_then(|v| v["hash"].as_str().map(str::to_string)).ok_or_else(|| AuditError::Invalid("a row has no hash".into())))
        .collect()
}

/// Signs the log as it is now and appends the checkpoint to `<log>.checkpoints.jsonl`. Refuses to sign a log whose chain does not verify.
pub fn sign_checkpoint(log: &Path, label: &str, key: &SigningKey) -> Result<Checkpoint, AuditError> {
    let hashes = row_hashes(log)?;
    let public = key.verifying_key();
    let mut c = Checkpoint { log: label.to_string(), count: hashes.len() as u64, head: hashes.last().cloned().unwrap_or_else(|| crate::GENESIS.to_string()), merkle_root: merkle_root_of_rows(&hashes),
                             time: crate::iso_now(), key_id: key_id(&public), signature: String::new() };
    c.signature = B64.encode(key.sign(&c.message()).to_bytes());
    let path = checkpoints_path_of(log);
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
    f.write_all(format!("{}\n", c.to_json()).as_bytes())?;
    f.sync_all()?;
    Ok(c)
}

/// Like `sign_checkpoint`, but does nothing when the newest checkpoint already covers every row (no new events since): the file does not grow while the
/// system is idle. None means "nothing new to sign".
pub fn sign_if_new(log: &Path, label: &str, key: &SigningKey) -> Result<Option<Checkpoint>, AuditError> {
    let (head, count) = verify(log)?;
    if let Some(Ok(last)) = read_checkpoints(log).last() {
        if last.count == count && last.head == head && last.log == label {
            return Ok(None);
        }
    }
    sign_checkpoint(log, label, key).map(Some)
}

/// The newest checkpoint whose signature verifies with `public` (and whose log name matches).
pub fn latest_valid_checkpoint(log: &Path, label: &str, public: &VerifyingKey) -> Option<Checkpoint> {
    read_checkpoints(log).into_iter().rev().filter_map(Result::ok).find(|c| c.log == label && c.key_id == key_id(public) && c.signature_ok(public))
}

pub fn read_checkpoints(log: &Path) -> Vec<Result<Checkpoint, String>> {
    let Ok(text) = std::fs::read_to_string(checkpoints_path_of(log)) else { return vec![] };
    text.split('\n').filter(|l| !l.trim().is_empty()).map(|l| serde_json::from_str::<Value>(l).ok().as_ref().and_then(Checkpoint::from_json).ok_or_else(|| "a checkpoint line is malformed".to_string())).collect()
}

#[derive(Debug, Clone)]
pub struct LedgerReport {
    pub ok: bool,
    pub checkpoints: usize,
    pub log_rows: u64,
    pub latest: Option<Checkpoint>,
    pub covered_rows: u64,
    pub problems: Vec<String>,
}

impl LedgerReport {
    pub fn to_json(&self) -> Value {
        json!({"ok": self.ok, "checkpoints": self.checkpoints, "log_rows": self.log_rows, "covered_rows": self.covered_rows, "uncovered_rows": self.log_rows.saturating_sub(self.covered_rows),
               "latest": self.latest.as_ref().map(Checkpoint::to_json), "problems": self.problems})
    }
}

/// Checks the log against every checkpoint with the public key alone. How many of the newest checkpoints also have their Merkle root recomputed
/// (every checkpoint has its head and signature checked) is `full_roots`.
pub fn verify_ledger(log: &Path, label: &str, public: &VerifyingKey, full_roots: usize) -> LedgerReport {
    let mut problems = vec![];
    let hashes = match row_hashes(log) {
        Ok(h) => h,
        Err(e) => return LedgerReport { ok: false, checkpoints: 0, log_rows: 0, latest: None, covered_rows: 0, problems: vec![format!("the chain itself fails: {e}")] },
    };
    let cps = read_checkpoints(log);
    let kid = key_id(public);
    let (mut prev_count, mut prev_time, mut latest) = (0u64, String::new(), None);
    let total = cps.len();
    for (i, cp) in cps.into_iter().enumerate() {
        let n = i + 1;
        let cp = match cp {
            Ok(c) => c,
            Err(e) => {
                problems.push(format!("checkpoint {n}: {e}"));
                continue;
            }
        };
        if cp.key_id != kid {
            problems.push(format!("checkpoint {n} was signed by a different key ({} instead of {kid})", cp.key_id));
            continue;
        }
        if !cp.signature_ok(public) {
            problems.push(format!("checkpoint {n}: the signature does not verify (forged or altered)"));
            continue;
        }
        if cp.log != label {
            problems.push(format!("checkpoint {n} is for another log ('{}')", cp.log));
            continue;
        }
        if cp.count > hashes.len() as u64 {
            problems.push(format!("checkpoint {n} vouched for {} rows but the log has only {}: the log was cut", cp.count, hashes.len()));
            continue;
        }
        if cp.count < prev_count || cp.time < prev_time {
            problems.push(format!("checkpoint {n} goes backwards (count {} after {prev_count}, or time before the previous one)", cp.count));
        }
        let at = if cp.count == 0 { crate::GENESIS.to_string() } else { hashes[(cp.count - 1) as usize].clone() };
        if at != cp.head {
            problems.push(format!("checkpoint {n}: the row at position {} is not the one the checkpoint vouched for: history was rewritten", cp.count));
        } else if total - i <= full_roots && merkle_root_of_rows(&hashes[..cp.count as usize]) != cp.merkle_root {
            problems.push(format!("checkpoint {n}: the Merkle root over the first {} rows does not match: an earlier row was changed", cp.count));
        }
        prev_count = cp.count;
        prev_time = cp.time.clone();
        latest = Some(cp);
    }
    let covered = latest.as_ref().map(|c| c.count).unwrap_or(0);
    LedgerReport { ok: problems.is_empty(), checkpoints: total, log_rows: hashes.len() as u64, latest, covered_rows: covered, problems }
}

// ---- inclusion proofs -------------------------------------------------------------------------------------------------------------

/// A proof that row `sequence` (1-based) is in the log as of `checkpoint`: the row's hash, the Merkle audit path, and the signed checkpoint. It contains no
/// other row and needs no access to the log to check.
pub fn prove(log: &Path, sequence: u64, checkpoint: &Checkpoint) -> Result<Value, String> {
    let hashes = row_hashes(log).map_err(|e| e.to_string())?;
    if sequence == 0 || sequence > checkpoint.count || checkpoint.count > hashes.len() as u64 {
        return Err(format!("row {sequence} is not covered by a checkpoint of {} rows", checkpoint.count));
    }
    let leaves: Vec<[u8; 32]> = hashes[..checkpoint.count as usize].iter().map(|h| leaf_hash(h)).collect();
    if hex::encode(mth(&leaves)) != checkpoint.merkle_root {
        return Err("the log no longer matches that checkpoint".into());
    }
    let path = audit_path((sequence - 1) as usize, &leaves);
    Ok(json!({"sequence": sequence, "row_hash": hashes[(sequence - 1) as usize], "tree_size": checkpoint.count,
              "path": path.iter().map(|(right, h)| json!({"side": if *right { "right" } else { "left" }, "hash": hex::encode(h)})).collect::<Vec<_>>(),
              "checkpoint": checkpoint.to_json()}))
}

/// Verifies a proof from its own contents and the public key: the checkpoint's signature, and that the row's hash leads up the audit path to the signed root.
pub fn verify_proof(proof: &Value, public: &VerifyingKey) -> Result<(), String> {
    let cp = Checkpoint::from_json(&proof["checkpoint"]).ok_or("the proof carries no readable checkpoint")?;
    if cp.key_id != key_id(public) {
        return Err("the checkpoint was signed by a different key".into());
    }
    if !cp.signature_ok(public) {
        return Err("the checkpoint's signature does not verify".into());
    }
    let seq = proof["sequence"].as_u64().ok_or("no sequence")?;
    let size = proof["tree_size"].as_u64().ok_or("no tree size")?;
    if size != cp.count || seq == 0 || seq > size {
        return Err("the proof's size does not match its checkpoint".into());
    }
    let row_hash = proof["row_hash"].as_str().ok_or("no row hash")?;
    if row_hash.len() != 64 || hex::decode(row_hash).is_err() {
        return Err("the row hash is not a SHA-256 hash".into());
    }
    let mut path = vec![];
    for step in proof["path"].as_array().ok_or("no path")? {
        let h: [u8; 32] = hex::decode(step["hash"].as_str().ok_or("bad path step")?).ok().and_then(|b| b.try_into().ok()).ok_or("bad path hash")?;
        path.push((step["side"].as_str() == Some("right"), h));
    }
    let root = root_from_path(leaf_hash(row_hash), (seq - 1) as usize, size as usize, &path).ok_or("the path does not fit a tree of that size")?;
    if hex::encode(root) != cp.merkle_root {
        return Err("the path does not lead to the signed root: this row was not in the log at that checkpoint".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AuditLog;

    fn log_with(dir: &Path, n: usize) -> PathBuf {
        let p = dir.join("security.jsonl");
        let log = AuditLog::open(&p).unwrap();
        for i in 0..n {
            log.append_system_events(&[json!({"event_type": "login_success", "badge_id": format!("CG-{i}")})]).unwrap();
        }
        p
    }

    #[test]
    fn the_merkle_tree_matches_rfc_6962_shapes() {
        let leaves: Vec<[u8; 32]> = (0..7u8).map(|i| sha(&[&[0u8], &[i]])).collect();
        // 7 leaves: split at 4: (4 leaves) + (3 leaves: split at 2: (2) + (1))
        let left = node_hash(&node_hash(&leaves[0], &leaves[1]), &node_hash(&leaves[2], &leaves[3]));
        let right = node_hash(&node_hash(&leaves[4], &leaves[5]), &leaves[6]);
        assert_eq!(mth(&leaves), node_hash(&left, &right));
        assert_eq!(mth(&leaves[..1]), leaves[0]);
        assert_eq!(mth(&[]), sha(&[]));
    }

    #[test]
    fn every_leaf_of_every_size_has_a_path_that_verifies_and_no_other_leaf_does() {
        for n in 1..=40usize {
            let leaves: Vec<[u8; 32]> = (0..n as u32).map(|i| sha(&[&[0u8], &i.to_be_bytes()])).collect();
            let root = mth(&leaves);
            for m in 0..n {
                let path = audit_path(m, &leaves);
                assert_eq!(root_from_path(leaves[m], m, n, &path), Some(root), "n={n} m={m}");
                if n > 1 {
                    let other = leaves[(m + 1) % n];
                    assert_ne!(root_from_path(other, m, n, &path), Some(root), "a different leaf must not verify");
                }
            }
        }
        assert_eq!(root_from_path([0; 32], 5, 5, &[]), None);
    }

    #[test]
    fn a_checkpoint_verifies_with_the_public_key_alone_and_detects_tampering() {
        let dir = tempfile::tempdir().unwrap();
        let log = log_with(dir.path(), 12);
        let (seed, public) = keygen();
        let sk = signing_key_from(&seed).unwrap();
        let pk = public_key_from(&public).unwrap();
        assert_eq!(public_of_seed(&seed).unwrap(), public);
        let c1 = sign_checkpoint(&log, "security", &sk).unwrap();
        assert_eq!((c1.count, c1.log.as_str()), (12, "security"));
        let r = verify_ledger(&log, "security", &pk, 3);
        assert!(r.ok, "{:?}", r.problems);
        assert_eq!((r.checkpoints, r.covered_rows, r.log_rows), (1, 12, 12));
        // more rows, another checkpoint
        let l = AuditLog::open(&log).unwrap();
        l.append_system_events(&[json!({"event_type": "login_success", "badge_id": "CG-99"})]).unwrap();
        sign_checkpoint(&log, "security", &sk).unwrap();
        let r = verify_ledger(&log, "security", &pk, 3);
        assert!(r.ok && r.covered_rows == 13 && r.checkpoints == 2, "{:?}", r.problems);
        // a different key does not verify
        let (_, other_public) = keygen();
        assert!(!verify_ledger(&log, "security", &public_key_from(&other_public).unwrap(), 3).ok);
        // the wrong log name is refused
        assert!(!verify_ledger(&log, "review", &pk, 3).ok);
    }

    #[test]
    fn a_cut_log_a_rewritten_history_and_a_forged_checkpoint_are_each_detected() {
        let dir = tempfile::tempdir().unwrap();
        let log = log_with(dir.path(), 10);
        let (seed, public) = keygen();
        let (sk, pk) = (signing_key_from(&seed).unwrap(), public_key_from(&public).unwrap());
        sign_checkpoint(&log, "security", &sk).unwrap();
        let original = std::fs::read_to_string(&log).unwrap();
        let anchor = crate::anchor_path_of(&log);
        let _ = std::fs::remove_file(&anchor);
        // 1. the last two rows cut off
        let cut: String = original.split_inclusive('\n').take(8).collect();
        std::fs::write(&log, &cut).unwrap();
        let r = verify_ledger(&log, "security", &pk, 3);
        assert!(!r.ok && r.problems.iter().any(|p| p.contains("the log was cut")), "{:?}", r.problems);
        // 2. a row in the middle rewritten AND the chain repaired by an attacker who recomputes every hash after it
        std::fs::write(&log, &original).unwrap();
        let mut rows: Vec<Value> = original.split('\n').filter(|l| !l.is_empty()).map(|l| serde_json::from_str(l).unwrap()).collect();
        rows[3]["event"]["badge_id"] = json!("CG-EVIL");
        let mut prev = rows[2]["hash"].as_str().unwrap().to_string();
        for row in rows.iter_mut().skip(3) {
            row.as_object_mut().unwrap().remove("hash");
            row["previous_hash"] = json!(prev);
            prev = crate::canonical::digest(row);
            row["hash"] = json!(prev);
        }
        let repaired: String = rows.iter().map(|r| format!("{}\n", crate::canonical::line(r))).collect();
        std::fs::write(&log, repaired).unwrap();
        assert!(crate::verify(&log).is_ok(), "the attacker's chain is internally consistent");
        let r = verify_ledger(&log, "security", &pk, 3);
        assert!(!r.ok && r.problems.iter().any(|p| p.contains("history was rewritten")), "the checkpoint still catches it: {:?}", r.problems);
        // 3. a forged checkpoint (a different key signs a fresh one)
        std::fs::write(&log, &original).unwrap();
        let (evil_seed, _) = keygen();
        let mut forged = read_checkpoints(&log)[0].clone().unwrap();
        forged.count = 9;
        forged.signature = B64.encode(signing_key_from(&evil_seed).unwrap().sign(&forged.message()).to_bytes());
        std::fs::write(checkpoints_path_of(&log), format!("{}\n", forged.to_json())).unwrap();
        let r = verify_ledger(&log, "security", &pk, 3);
        assert!(!r.ok && r.problems.iter().any(|p| p.contains("signature does not verify")), "{:?}", r.problems);
        // 4. an altered field inside a genuine checkpoint
        let mut genuine = sign_checkpoint(&log, "security", &sk).unwrap();
        std::fs::write(checkpoints_path_of(&log), "").unwrap();
        genuine.count -= 1;
        std::fs::write(checkpoints_path_of(&log), format!("{}\n", genuine.to_json())).unwrap();
        assert!(!verify_ledger(&log, "security", &pk, 3).ok);
    }

    #[test]
    fn checkpoints_that_go_backwards_and_a_chain_that_fails_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        let log = log_with(dir.path(), 6);
        let (seed, public) = keygen();
        let (sk, pk) = (signing_key_from(&seed).unwrap(), public_key_from(&public).unwrap());
        let a = sign_checkpoint(&log, "security", &sk).unwrap();
        let l = AuditLog::open(&log).unwrap();
        l.append_system_events(&[json!({"event_type": "login_success", "badge_id": "CG-7"})]).unwrap();
        let b = sign_checkpoint(&log, "security", &sk).unwrap();
        // swap the order of the two lines: the newer checkpoint first
        std::fs::write(checkpoints_path_of(&log), format!("{}\n{}\n", b.to_json(), a.to_json())).unwrap();
        let r = verify_ledger(&log, "security", &pk, 3);
        assert!(!r.ok && r.problems.iter().any(|p| p.contains("goes backwards")), "{:?}", r.problems);
        // a log that does not verify cannot be signed over
        let text = std::fs::read_to_string(&log).unwrap().replacen("CG-7", "CG-8", 1);
        std::fs::write(&log, text).unwrap();
        assert!(sign_checkpoint(&log, "security", &sk).is_err());
        assert!(!verify_ledger(&log, "security", &pk, 3).ok);
    }

    #[test]
    fn an_inclusion_proof_stands_alone_and_a_changed_proof_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let log = log_with(dir.path(), 21);
        let (seed, public) = keygen();
        let (sk, pk) = (signing_key_from(&seed).unwrap(), public_key_from(&public).unwrap());
        let cp = sign_checkpoint(&log, "security", &sk).unwrap();
        for seq in [1u64, 2, 7, 16, 20, 21] {
            let proof = prove(&log, seq, &cp).unwrap();
            assert_eq!(verify_proof(&proof, &pk), Ok(()), "row {seq}");
        }
        let proof = prove(&log, 7, &cp).unwrap();
        // another row's hash does not fit this path
        let mut other = proof.clone();
        other["row_hash"] = json!(prove(&log, 8, &cp).unwrap()["row_hash"]);
        assert!(verify_proof(&other, &pk).is_err());
        // a different sequence number does not fit it either
        let mut moved = proof.clone();
        moved["sequence"] = json!(8);
        assert!(verify_proof(&moved, &pk).is_err());
        // a tampered path step
        let mut bent = proof.clone();
        bent["path"][0]["hash"] = json!("00".repeat(32));
        assert!(verify_proof(&bent, &pk).is_err());
        // another key
        assert!(verify_proof(&proof, &public_key_from(&keygen().1).unwrap()).is_err());
        // rows outside the checkpoint
        assert!(prove(&log, 22, &cp).is_err());
        assert!(prove(&log, 0, &cp).is_err());
        // the proof holds no other row's content
        assert!(!proof.to_string().contains("CG-3"));
    }

    #[test]
    fn the_proof_of_a_log_that_has_since_changed_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let log = log_with(dir.path(), 5);
        let (seed, _) = keygen();
        let sk = signing_key_from(&seed).unwrap();
        let cp = sign_checkpoint(&log, "security", &sk).unwrap();
        let text = std::fs::read_to_string(&log).unwrap().replacen("CG-2", "CG-9", 1);
        std::fs::write(&log, text).unwrap();
        assert!(prove(&log, 3, &cp).is_err());
    }

    #[test]
    fn key_handling_rejects_bad_input() {
        assert!(signing_key_from("not base64!!").is_err());
        assert!(signing_key_from("AAAA").is_err());
        assert!(public_key_from("AAAA").is_err());
        let (seed, public) = keygen();
        assert_ne!(seed, public);
        assert_eq!(seed.len(), 43);
    }
}
