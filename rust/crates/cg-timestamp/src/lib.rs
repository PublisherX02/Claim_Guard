//! RFC 3161 trusted timestamp for the audit log's anchor (`src/timestamp_anchor.py`).
//!
//! The audit log's anchor file (`<log>.head.json`) holds the chain head and the event count. An HMAC protects it only while the key stays
//! secret. A timestamp token from an independent Time-Stamp Authority (TSA) proves the head hash existed at a given time and is signed by a
//! party the log writer does not control, so a later rewrite of the chain cannot also rewrite the token.
//!
//! * What is stamped: SHA-256 of the ASCII string `"<head>|<count>"` (the same string the HMAC covers), not every event.
//! * Where it is stored: a sidecar `<log>.head.tsr` (JSON: the base64 DER response, the stamped head and count, the time it was
//!   requested). The anchor file and the log format do not change.
//! * It never blocks anything: stamping is a separate step; appends never call the network. A TSA outage leaves the previous token.
//! * Verification checks: PKIStatus granted; the imprint equals the digest of the stamped head and count; the nonce matches the request;
//!   the CMS signature over the TSTInfo is valid for the signer certificate; that certificate carries the critical timeStamping extended
//!   key usage, was valid at the stamped time, and was issued directly by a CA the verifier trusts (a PEM file the verifier supplies; the
//!   certificates inside the token are never trusted on their own).
//!
//! The Python build writes and reads the same files; `tests/fixtures` holds tokens signed by the Python test authority and the verdict
//! the Python verifier gives each, which this crate must reproduce.

use base64::Engine as _;
use cms::cert::CertificateChoices;
use cms::signed_data::{SignedData, SignerIdentifier};
use const_oid::ObjectIdentifier;
use der::{asn1::OctetString, Decode, Encode};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use x509_cert::ext::pkix::ExtendedKeyUsage;
use x509_cert::Certificate;
use x509_tsp::{TimeStampReq, TimeStampResp, TstInfo};

pub const MAX_RESPONSE_BYTES: usize = 1_000_000;
pub const SIDECAR_SUFFIX: &str = ".head.tsr";
const OID_SHA256: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.16.840.1.101.3.4.2.1");
const OID_MESSAGE_DIGEST: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.9.4");
const OID_TST_INFO: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.9.16.1.4");
const OID_SIGNED_DATA: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.113549.1.7.2");
const OID_EKU: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.29.37");
const OID_KP_TIMESTAMPING: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.6.1.5.5.7.3.8");

/// The token is missing, malformed, or does not prove what it claims.
#[derive(Debug, thiserror::Error, PartialEq)]
#[error("{0}")]
pub struct TimestampError(pub String);

fn fail<T>(m: impl Into<String>) -> Result<T, TimestampError> {
    Err(TimestampError(m.into()))
}

pub fn stamped_message(head: &str, count: u64) -> Vec<u8> {
    format!("{head}|{count}").into_bytes()
}

pub fn imprint(head: &str, count: u64) -> [u8; 32] {
    Sha256::digest(stamped_message(head, count)).into()
}

/// The shortest two's-complement bytes of a non-negative integer (what DER requires).
fn minimal_positive(n: u64) -> Vec<u8> {
    let mut b: Vec<u8> = n.to_be_bytes().iter().copied().skip_while(|x| *x == 0).collect();
    if b.is_empty() {
        b.push(0);
    }
    if b[0] & 0x80 != 0 {
        b.insert(0, 0);
    }
    b
}

/// DER TimeStampReq for a SHA-256 digest, asking for the TSA certificate to be included.
pub fn build_request(digest: &[u8; 32], nonce: u64) -> Result<Vec<u8>, TimestampError> {
    use x509_tsp::MessageImprint;
    let req = TimeStampReq {
        version: x509_tsp::TspVersion::V1,
        message_imprint: MessageImprint {
            hash_algorithm: spki::AlgorithmIdentifierOwned { oid: OID_SHA256, parameters: Some(der::asn1::Any::null()) },
            hashed_message: OctetString::new(digest.to_vec()).map_err(|e| TimestampError(e.to_string()))?,
        },
        req_policy: None,
        nonce: Some(der::asn1::Int::new(&minimal_positive(nonce)).map_err(|e| TimestampError(e.to_string()))?),
        cert_req: true,
        extensions: None,
    };
    req.to_der().map_err(|e| TimestampError(e.to_string()))
}

/// POST the request to the TSA and return the DER response. Any failure means "untimestamped", never a reason to stop writing the log.
pub fn request_token(tsa_url: &str, digest: &[u8; 32], nonce: u64, timeout_seconds: f64) -> Result<Vec<u8>, TimestampError> {
    use std::io::Read;
    let body = build_request(digest, nonce)?;
    let agent = ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs_f64(timeout_seconds.clamp(0.5, 120.0))).build();
    let resp = agent.post(tsa_url).set("Content-Type", "application/timestamp-query").send_bytes(&body).map_err(|e| TimestampError(format!("TSA request failed: {e}")))?;
    let mut data = Vec::new();
    resp.into_reader().take(MAX_RESPONSE_BYTES as u64 + 1).read_to_end(&mut data).map_err(|e| TimestampError(format!("TSA request failed: {e}")))?;
    if data.len() > MAX_RESPONSE_BYTES {
        return fail("TSA response is larger than 1 MB");
    }
    Ok(data)
}

#[derive(Debug, Clone, PartialEq)]
pub struct TokenInfo {
    pub gen_time: String,
    pub tsa_subject: String,
    pub serial: String,
}

fn der_err<T>(r: Result<T, der::Error>, what: &str) -> Result<T, TimestampError> {
    r.map_err(|e| TimestampError(format!("{what}: {e}")))
}

fn load_cas(pem: &str) -> Result<Vec<Certificate>, TimestampError> {
    let certs = Certificate::load_pem_chain(pem.as_bytes()).unwrap_or_default();
    if certs.is_empty() {
        return fail("no trusted CA certificate supplied");
    }
    Ok(certs)
}

/// Verifies a signature `sig` over `data` made by the key in `cert` (RSA PKCS#1 v1.5 or ECDSA P-256/P-384) with the given SHA-2 hash.
fn check_signature(cert: &Certificate, sig: &[u8], data: &[u8], hash: &str) -> Result<(), TimestampError> {
    use signature::hazmat::PrehashVerifier;
    let spki_der = der_err(cert.tbs_certificate.subject_public_key_info.to_der(), "public key")?;
    let bad = || TimestampError("the token signature does not verify".into());
    let digest: Vec<u8> = match hash {
        "sha256" => Sha256::digest(data).to_vec(),
        "sha384" => sha2::Sha384::digest(data).to_vec(),
        "sha512" => sha2::Sha512::digest(data).to_vec(),
        _ => return fail("unsupported digest algorithm"),
    };
    let alg = cert.tbs_certificate.subject_public_key_info.algorithm.oid.to_string();
    match alg.as_str() {
        "1.2.840.113549.1.1.1" => {
            use rsa::pkcs8::DecodePublicKey;
            let key = rsa::RsaPublicKey::from_public_key_der(&spki_der).map_err(|_| TimestampError("unsupported signer key type".into()))?;
            let scheme = match hash {
                "sha256" => rsa::Pkcs1v15Sign::new::<Sha256>(),
                "sha384" => rsa::Pkcs1v15Sign::new::<sha2::Sha384>(),
                _ => rsa::Pkcs1v15Sign::new::<sha2::Sha512>(),
            };
            key.verify(scheme, &digest, sig).map_err(|_| bad())
        }
        "1.2.840.10045.2.1" => {
            use p256::pkcs8::DecodePublicKey as _;
            if let Ok(key) = p256::ecdsa::VerifyingKey::from_public_key_der(&spki_der) {
                let s = p256::ecdsa::Signature::from_der(sig).map_err(|_| bad())?;
                return key.verify_prehash(&digest, &s).map_err(|_| bad());
            }
            let key = p384::ecdsa::VerifyingKey::from_public_key_der(&spki_der).map_err(|_| TimestampError("unsupported signer key type".into()))?;
            let s = p384::ecdsa::Signature::from_der(sig).map_err(|_| bad())?;
            key.verify_prehash(&digest, &s).map_err(|_| bad())
        }
        _ => fail("unsupported signer key type"),
    }
}

/// True when `cert` names `ca` as its issuer and carries a valid signature by the CA's key (`verify_directly_issued_by`).
fn issued_by(cert: &Certificate, ca: &Certificate) -> bool {
    if cert.tbs_certificate.issuer != ca.tbs_certificate.subject {
        return false;
    }
    let (Ok(tbs), Some(sig)) = (cert.tbs_certificate.to_der(), cert.signature.as_bytes()) else { return false };
    let hash = match cert.signature_algorithm.oid.to_string().as_str() {
        "1.2.840.113549.1.1.11" | "1.2.840.10045.4.3.2" => "sha256",
        "1.2.840.113549.1.1.12" | "1.2.840.10045.4.3.3" => "sha384",
        "1.2.840.113549.1.1.13" | "1.2.840.10045.4.3.4" => "sha512",
        _ => return false,
    };
    check_signature(ca, sig, &tbs, hash).is_ok()
}

fn unix_to_iso(secs: u64) -> String {
    // civil-from-days (Howard Hinnant), UTC
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}+00:00", rem / 3600, (rem % 3600) / 60, rem % 60)
}

/// Verifies a DER TimeStampResp for the given head and count. `trusted_ca_pem` is the PEM text of the CAs allowed to issue the TSA
/// certificate. Returns the stamped time, the signer and the serial, or the first problem found.
pub fn verify_token(response_der: &[u8], head: &str, count: u64, trusted_ca_pem: &str, expected_nonce: Option<u64>) -> Result<TokenInfo, TimestampError> {
    let resp = match TimeStampResp::from_der(response_der) {
        Ok(r) => r,
        Err(e) => {
            // from_der also refuses trailing bytes; tell the two apart like the Python verifier does
            return match TimeStampResp::from_der(&response_der[..response_der.len().saturating_sub(1)]) {
                Ok(_) => fail("trailing bytes after the response"),
                Err(_) => fail(format!("not a TimeStampResp: {e}")),
            };
        }
    };
    let status = resp.status.status as u8;
    if status != 0 && status != 1 {
        return fail(format!("TSA refused the request (PKIStatus {status})"));
    }
    let Some(token) = resp.time_stamp_token else { return fail("response carries no signed-data token") };
    if token.content_type != OID_SIGNED_DATA {
        return fail("response carries no signed-data token");
    }
    let signed: SignedData = match token.content.decode_as() {
        Ok(s) => s,
        Err(e) => return fail(format!("token is not CMS SignedData: {e}")),
    };
    let encap = &signed.encap_content_info;
    let Some(econtent) = encap.econtent.as_ref().filter(|_| encap.econtent_type == OID_TST_INFO) else { return fail("token does not contain TSTInfo") };
    let tst_der: Vec<u8> = match econtent.decode_as::<OctetString>() {
        Ok(o) => o.as_bytes().to_vec(),
        Err(_) => return fail("token does not contain TSTInfo"),
    };
    let tst = der_err(TstInfo::from_der(&tst_der), "TSTInfo")?;

    if tst.message_imprint.hash_algorithm.oid != OID_SHA256 || tst.message_imprint.hashed_message.as_bytes() != imprint(head, count) {
        return fail("the token stamps a different head or count than the one claimed");
    }
    if let Some(want) = expected_nonce {
        let got = tst.nonce.as_ref().map(|n| n.as_bytes().iter().fold(0u128, |a, b| (a << 8) | *b as u128));
        if got != Some(want as u128) {
            return fail("the token nonce does not match the request");
        }
    }
    let gen_secs = tst.gen_time.to_unix_duration().as_secs();

    let infos: Vec<_> = signed.signer_infos.0.iter().collect();
    if infos.len() != 1 {
        return fail("expected exactly one signer");
    }
    let si = infos[0];
    let Some(attrs) = si.signed_attrs.as_ref() else { return fail("signer has no signed attributes") };
    if si.digest_alg.oid != OID_SHA256 {
        return fail(format!("unsupported digest algorithm {}", si.digest_alg.oid));
    }
    let tst_digest = Sha256::digest(&tst_der).to_vec();
    let mut attr_digest: Option<Vec<u8>> = None;
    for a in attrs.iter() {
        if a.oid == OID_MESSAGE_DIGEST {
            if let Some(v) = a.values.iter().next() {
                attr_digest = v.decode_as::<OctetString>().ok().map(|o| o.as_bytes().to_vec());
            }
        }
    }
    if attr_digest.as_deref() != Some(tst_digest.as_slice()) {
        return fail("messageDigest attribute does not match the TSTInfo");
    }

    let cas = load_cas(trusted_ca_pem)?;
    let SignerIdentifier::IssuerAndSerialNumber(sid) = &si.sid else { return fail("the signer certificate is not in the token") };
    let want_serial = sid.serial_number.as_bytes();
    let mut signer: Option<&Certificate> = None;
    if let Some(set) = signed.certificates.as_ref() {
        for c in set.0.iter() {
            if let CertificateChoices::Certificate(cert) = c {
                if cert.tbs_certificate.serial_number.as_bytes() == want_serial {
                    signer = Some(cert);
                }
            }
        }
    }
    let Some(signer) = signer else { return fail("the signer certificate is not in the token") };

    // the signature covers the DER of signedAttrs with the universal SET OF tag, which is how the set encodes by itself
    let signed_bytes = der_err(attrs.to_der(), "signed attributes")?;
    check_signature(signer, si.signature.as_bytes(), &signed_bytes, "sha256")?;

    if !cas.iter().any(|ca| issued_by(signer, ca)) {
        return fail("the signer certificate was not issued by a trusted CA");
    }
    let v = &signer.tbs_certificate.validity;
    if !(v.not_before.to_unix_duration().as_secs() <= gen_secs && gen_secs <= v.not_after.to_unix_duration().as_secs()) {
        return fail("the signer certificate was not valid at the stamped time");
    }
    let eku = signer.tbs_certificate.extensions.as_ref().and_then(|exts| exts.iter().find(|e| e.extn_id == OID_EKU));
    let Some(eku) = eku else { return fail("the signer certificate has no extended key usage") };
    let parsed = ExtendedKeyUsage::from_der(eku.extn_value.as_bytes()).ok();
    if !parsed.is_some_and(|u| u.0.contains(&OID_KP_TIMESTAMPING)) || !eku.critical {
        return fail("the signer certificate is not a critical timeStamping certificate");
    }
    let serial = tst.serial_number.as_bytes().iter().fold(0u128, |a, b| (a << 8) | *b as u128);
    Ok(TokenInfo { gen_time: unix_to_iso(gen_secs), tsa_subject: signer.tbs_certificate.subject.to_string(), serial: serial.to_string() })
}

pub fn sidecar_path(log: &Path) -> PathBuf {
    let name = log.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    log.with_file_name(format!("{name}{SIDECAR_SUFFIX}"))
}

/// Requests a token for the log's current anchor and stores it. Leaves the previous sidecar untouched on any failure.
pub fn stamp_log(log: &Path, anchor: &Path, tsa_url: &str, timeout_seconds: f64) -> Result<Value, TimestampError> {
    let a: Value = serde_json::from_str(&std::fs::read_to_string(anchor).map_err(|e| TimestampError(e.to_string()))?).map_err(|e| TimestampError(e.to_string()))?;
    let (head, count) = (a["head"].as_str().unwrap_or("").to_string(), a["count"].as_u64().unwrap_or(0));
    let nonce: u64 = rand::random::<u64>() >> 1;
    let der = request_token(tsa_url, &imprint(&head, count), nonce, timeout_seconds)?;
    let side = json!({"head": head, "count": count, "nonce": nonce, "tsa_url": tsa_url,
        "requested_at": unix_to_iso(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)),
        "token": base64::engine::general_purpose::STANDARD.encode(&der)});
    let path = sidecar_path(log);
    let tmp = path.with_extension(format!("tsr.{}.tmp", std::process::id()));
    std::fs::write(&tmp, serde_json::to_string_pretty(&side).unwrap_or_default()).map_err(|e| TimestampError(e.to_string()))?;
    std::fs::rename(&tmp, &path).map_err(|e| TimestampError(e.to_string()))?;
    Ok(side)
}

/// The timestamp state, separate from the chain state: none, current, stale, unverified or invalid (see the Python module).
pub fn timestamp_status(log: &Path, anchor: &Path, trusted_ca_pem: Option<&str>) -> Value {
    let sp = sidecar_path(log);
    if !sp.exists() {
        return json!({"state": "none"});
    }
    let side: Value = match std::fs::read_to_string(&sp).ok().and_then(|t| serde_json::from_str(&t).ok()) {
        Some(v) => v,
        None => return json!({"state": "invalid", "reason": "the sidecar cannot be read"}),
    };
    let Some(pem) = trusted_ca_pem.filter(|p| !p.is_empty()) else { return json!({"state": "unverified", "stamped_count": side["count"]}) };
    let (head, count) = (side["head"].as_str().unwrap_or(""), side["count"].as_u64().unwrap_or(0));
    let der = match base64::engine::general_purpose::STANDARD.decode(side["token"].as_str().unwrap_or("")) {
        Ok(d) => d,
        Err(e) => return json!({"state": "invalid", "reason": e.to_string()}),
    };
    let info = match verify_token(&der, head, count, pem, side["nonce"].as_u64()) {
        Ok(i) => i,
        Err(e) => return json!({"state": "invalid", "reason": e.0}),
    };
    let a: Value = std::fs::read_to_string(anchor).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
    let mut out = json!({"gen_time": info.gen_time, "tsa_subject": info.tsa_subject, "serial": info.serial, "stamped_count": count});
    let (ah, ac) = (a["head"].as_str().unwrap_or(""), a["count"].as_u64().unwrap_or(0));
    if head == ah && count == ac {
        out["state"] = json!("current");
        return out;
    }
    if count < ac && head_in_log(log, head, count) {
        out["state"] = json!("stale");
        out["unstamped_events"] = json!(ac - count);
        return out;
    }
    json!({"state": "invalid", "reason": "the stamped head is not a point in this log (the log was rewritten or replaced)"})
}

/// True when the row at position `count` has hash `head`: the stamped state is a prefix of the present chain.
fn head_in_log(log: &Path, head: &str, count: u64) -> bool {
    let Ok(rows) = cg_audit::read_rows(log) else { return false };
    rows.get(count.saturating_sub(1) as usize).filter(|_| count > 0).and_then(|l| serde_json::from_str::<Value>(l).ok()).is_some_and(|v| v["hash"] == head)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fx(name: &str) -> Vec<u8> {
        std::fs::read(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)).unwrap()
    }

    #[test]
    fn the_request_is_byte_identical_to_the_python_one() {
        assert_eq!(build_request(&imprint(&"ab".repeat(32), 7), 12345).unwrap(), fx("request.der"));
    }

    #[test]
    fn every_fixture_gets_the_verdict_the_python_verifier_gave_it() {
        let manifest: Vec<Value> = serde_json::from_slice(&fx("manifest.json")).unwrap();
        assert!(manifest.len() >= 19);
        for c in manifest {
            let name = c["name"].as_str().unwrap();
            let pem = String::from_utf8(fx(c["ca"].as_str().unwrap())).unwrap();
            let got = verify_token(&fx(c["file"].as_str().unwrap()), c["head"].as_str().unwrap(), c["count"].as_u64().unwrap(), &pem, c["nonce"].as_u64());
            let want = c["python_says"].as_str().unwrap();
            match (&got, want) {
                (Ok(info), "ok") => {
                    assert_eq!(info.gen_time, c["info"]["gen_time"].as_str().unwrap(), "{name}");
                    assert_eq!(info.tsa_subject, c["info"]["tsa_subject"].as_str().unwrap(), "{name}");
                    assert_eq!(info.serial, c["info"]["serial"].as_str().unwrap(), "{name}");
                }
                (Err(e), w) if w != "ok" => {
                    // the bit-flip cases may fail at a different (equally fatal) step than Python's; every other case must give the same reason
                    if !name.starts_with("bitflip") {
                        let head: String = w.chars().take(30).collect();
                        assert!(e.0.starts_with(&head), "{name}: rust says {:?}, python says {:?}", e.0, w);
                    }
                }
                _ => panic!("{name}: rust says {got:?}, python says {want:?}"),
            }
        }
    }

    #[test]
    fn a_log_is_none_then_current_then_stale_then_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("audit.jsonl");
        let anchor = cg_audit::anchor_path_of(&log);
        let rows: Vec<Value> = (1..=7).map(|n| json!({"sequence": n, "hash": if n == 7 { "ab".repeat(32) } else { format!("{n:064}") }})).collect();
        std::fs::write(&log, rows.iter().map(|r| r.to_string()).collect::<Vec<_>>().join("\n") + "\n").unwrap();
        std::fs::write(&anchor, json!({"head": "ab".repeat(32), "count": 7}).to_string()).unwrap();
        let pem = String::from_utf8(fx("ca.pem")).unwrap();
        assert_eq!(timestamp_status(&log, &anchor, Some(&pem))["state"], "none");
        let side = json!({"head": "ab".repeat(32), "count": 7, "nonce": 12345, "tsa_url": "x", "requested_at": "x", "token": base64::engine::general_purpose::STANDARD.encode(fx("good.tsr"))});
        std::fs::write(sidecar_path(&log), side.to_string()).unwrap();
        assert_eq!(timestamp_status(&log, &anchor, None)["state"], "unverified");
        let s = timestamp_status(&log, &anchor, Some(&pem));
        assert_eq!(s["state"], "current", "{s}");
        // the log grows: the stamped state is still a prefix
        let mut longer = rows.clone();
        longer.push(json!({"sequence": 8, "hash": "cd".repeat(32)}));
        std::fs::write(&log, longer.iter().map(|r| r.to_string()).collect::<Vec<_>>().join("\n") + "\n").unwrap();
        std::fs::write(&anchor, json!({"head": "cd".repeat(32), "count": 8}).to_string()).unwrap();
        let s = timestamp_status(&log, &anchor, Some(&pem));
        assert_eq!((s["state"].as_str(), s["unstamped_events"].as_u64()), (Some("stale"), Some(1)));
        // the log is rewritten: the stamped head is no longer in it
        let mut rewritten = longer.clone();
        rewritten[6]["hash"] = json!("ee".repeat(32));
        std::fs::write(&log, rewritten.iter().map(|r| r.to_string()).collect::<Vec<_>>().join("\n") + "\n").unwrap();
        assert_eq!(timestamp_status(&log, &anchor, Some(&pem))["state"], "invalid");
        // a tampered sidecar
        std::fs::write(sidecar_path(&log), json!({"head": "ab".repeat(32), "count": 7, "nonce": 1, "token": "AAAA"}).to_string()).unwrap();
        assert_eq!(timestamp_status(&log, &anchor, Some(&pem))["state"], "invalid");
    }

    #[test]
    fn an_unreachable_authority_is_an_error_and_keeps_the_old_token() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("audit.jsonl");
        let anchor = cg_audit::anchor_path_of(&log);
        std::fs::write(&anchor, json!({"head": "ab".repeat(32), "count": 7}).to_string()).unwrap();
        std::fs::write(sidecar_path(&log), "previous").unwrap();
        assert!(stamp_log(&log, &anchor, "http://127.0.0.1:1/tsr", 1.0).is_err());
        assert_eq!(std::fs::read_to_string(sidecar_path(&log)).unwrap(), "previous");
    }

    #[test]
    fn time_formatting_matches_python_isoformat() {
        assert_eq!(unix_to_iso(0), "1970-01-01T00:00:00+00:00");
        assert_eq!(unix_to_iso(1_791_564_724), "2026-10-09T16:52:04+00:00");
        assert_eq!(unix_to_iso(951_782_400), "2000-02-29T00:00:00+00:00");
    }
}
