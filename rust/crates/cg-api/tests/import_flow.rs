//! Importing claims from files: the preview routes, with a stub document reader on a real socket so the whole path (gate, reader client, extraction,
//! response, security log) runs. Nothing here queues a claim: the preview changes no state.

use cg_access::service::testkit::{world, World};
use cg_api::claimstore::FileClaimStore;
use cg_api::import::ImportParts;
use cg_api::{Api, ApiParts, ApiRequest, ApiResponse};
use cg_audit::AuditLog;
use cg_ingest::document::ReaderConfig;
use cg_ingest::extract::Catalog;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const PW: &str = "Correct-Horse-9";
const TOKEN: &str = "stub-reader-token";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

/// What the stub reader answers with.
#[derive(Clone)]
enum Stub {
    Pages(Value),
    Status(u16, &'static str),
}

struct Reader {
    url: String,
    mode: Arc<Mutex<Stub>>,
    seen_tokens: Arc<Mutex<Vec<String>>>,
    seen_types: Arc<Mutex<Vec<String>>>,
}

fn start_reader() -> Reader {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let mode = Arc::new(Mutex::new(Stub::Status(500, "not set")));
    let (tokens, types) = (Arc::new(Mutex::new(vec![])), Arc::new(Mutex::new(vec![])));
    let (m, t, ty) = (mode.clone(), tokens.clone(), types.clone());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            s.set_read_timeout(Some(Duration::from_secs(5))).ok();
            let mut buf = vec![];
            let mut chunk = [0u8; 8192];
            let (mut head_end, mut want) = (None, 0usize);
            loop {
                let Ok(n) = s.read(&mut chunk) else { break };
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                if head_end.is_none() {
                    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        head_end = Some(i + 4);
                        let head = String::from_utf8_lossy(&buf[..i]).to_lowercase();
                        want = head.lines().find_map(|l| l.strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0))).unwrap_or(0);
                        if let Some(a) = head.lines().find_map(|l| l.strip_prefix("authorization:")) {
                            t.lock().unwrap().push(a.trim().to_string());
                        }
                        if let Some(c) = head.lines().find_map(|l| l.strip_prefix("content-type:")) {
                            ty.lock().unwrap().push(c.trim().to_string());
                        }
                    }
                }
                if let Some(h) = head_end {
                    if buf.len() >= h + want {
                        break;
                    }
                }
            }
            let (status, body) = match m.lock().unwrap().clone() {
                Stub::Pages(p) => (200, p.to_string()),
                Stub::Status(c, msg) => (c, json!({"error": msg}).to_string()),
            };
            let reply = format!("HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
            let _ = s.write_all(reply.as_bytes());
        }
    });
    Reader { url, mode, seen_tokens: tokens, seen_types: types }
}

struct T {
    api: Api,
    w: World,
}

fn t(reader: Option<&Reader>) -> T {
    let w = world();
    for (b, l) in [("CG-2002", 2), ("CG-4004", 4)] {
        w.provision(b, l, PW);
    }
    let review_log = Arc::new(AuditLog::open(w.dir.path().join("review.jsonl")).unwrap());
    let t0 = w.clock.clone();
    let clock: cg_access::Clock = Arc::new(move || t0.load(std::sync::atomic::Ordering::SeqCst) as f64 / 1000.0);
    let reader_cfg = reader.map(|r| ReaderConfig { url: r.url.clone(), token: TOKEN.into(), engine: "auto".into(), langs: "eng".into(), timeout: Duration::from_secs(10) });
    let api = Api::new(ApiParts {
        access: w.svc.clone(),
        claims: Arc::new(FileClaimStore::new(vec![])),
        review_log,
        securitylog: w.svc.log.clone(),
        settings: cg_access::settings::test_settings(),
        clock,
        queue: None,
        ui: true,
        extra_probes: vec![],
        data_dir: w.dir.path().to_path_buf(),
        import: Some(ImportParts { reader: reader_cfg, catalog: Catalog::load(&root()) }),
        payer: None,
    });
    T { api, w }
}

fn j(r: &ApiResponse) -> Value {
    serde_json::from_slice(&r.body).unwrap_or(Value::Null)
}

fn cookie_of(r: &ApiResponse, name: &str) -> String {
    r.headers.iter().filter(|(n, _)| n == "set-cookie").find_map(|(_, v)| v.strip_prefix(&format!("{name}=")).map(|s| s.split(';').next().unwrap().to_string())).unwrap()
}

impl T {
    fn login(&self, badge: &str) -> (String, String) {
        let code = self.w.code(badge);
        let body = json!({"badge_id": badge, "password": PW, "totp": code}).to_string().into_bytes();
        let r = self.api.handle(ApiRequest { method: "POST".into(), path: "/api/v1/auth/login".into(), query: String::new(), headers: HashMap::from([("content-type".to_string(), "application/json".to_string())]), body, client: "10.0.0.1".into() });
        assert_eq!(r.status, 200, "{:?}", j(&r));
        (cookie_of(&r, "cg_session"), j(&r)["csrf"].as_str().unwrap().to_string())
    }

    fn upload(&self, path: &str, bytes: Vec<u8>, ctype: &str, s: &(String, String)) -> ApiResponse {
        let headers = HashMap::from([("cookie".to_string(), format!("cg_session={}", s.0)), ("x-csrf-token".to_string(), s.1.clone()), ("content-type".to_string(), ctype.to_string()), ("x-file-name".to_string(), "scan (1).png".to_string())]);
        self.api.handle(ApiRequest { method: "POST".into(), path: path.into(), query: String::new(), headers, body: bytes, client: "10.0.0.1".into() })
    }
}

fn png(w: u32, h: u32) -> Vec<u8> {
    let mut v = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, b'I', b'H', b'D', b'R'];
    v.extend_from_slice(&w.to_be_bytes());
    v.extend_from_slice(&h.to_be_bytes());
    v.extend_from_slice(&[8, 2, 0, 0, 0, 0, 0, 0, 0, 1, 2, 3, 4]);
    v.extend_from_slice(&[0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82]);
    v
}

const INVOICE: &str = "INVOICE INV-CG-11D0FB9D7A3F\nInvoice date: 2026-05-29\nReference (claim): CG-11D0FB9D7A3F\nPatient: PAT-F1B1050177\nMember No.: MEM-F1B1050177\nTreating provider: EDU-PROV-01\nInsurer: EDU-PAYER / Plan: EDU-PLUS\nICD / Dx code: DX-EDU-01\nCoverage period: 2026-01-01 - 2026-05-24 (active)\nL1 SVC-IMAGE 2026-05-25 - 1 1520.00 1520.00 AUTH-CG-11D0FB9D7A3F-1\nAmount payable: SAR 1520.00\n";

fn pages(text: &str) -> Value {
    json!({"engine": "stub", "pages": [{"page": 1, "text": text, "source": "ocr", "words": []}], "warnings": []})
}

fn lines_of(file: &str, n: usize) -> Vec<String> {
    std::fs::read_to_string(root().join(file)).unwrap().lines().take(n).map(str::to_string).collect()
}

#[test]
fn only_those_who_may_submit_can_import() {
    let t = t(None);
    let anon = t.api.handle(ApiRequest { method: "POST".into(), path: "/api/v1/queue/import/file".into(), query: String::new(), headers: HashMap::new(), body: b"{}".to_vec(), client: "10.0.0.1".into() });
    assert_eq!(anon.status, 401);
    let reviewer = t.login("CG-2002");
    assert_eq!(t.upload("/api/v1/queue/import/file", b"{}".to_vec(), "application/json", &reviewer).status, 403);
    let admin = t.login("CG-4004");
    assert_eq!(t.upload("/api/v1/queue/import/file", vec![], "application/octet-stream", &admin).status, 422, "an empty file");
    let no_csrf = {
        let headers = HashMap::from([("cookie".to_string(), format!("cg_session={}", admin.0))]);
        t.api.handle(ApiRequest { method: "POST".into(), path: "/api/v1/queue/import/file".into(), query: String::new(), headers, body: b"{}".to_vec(), client: "10.0.0.1".into() })
    };
    assert_eq!(no_csrf.status, 403);
}

#[test]
fn a_jsonl_file_is_previewed_record_by_record_and_a_bad_line_costs_only_itself() {
    let t = t(None);
    let admin = t.login("CG-4004");
    let mut lines = lines_of("data/validation/claims.jsonl", 3);
    lines.insert(1, "this is not json".into());
    lines.push("[1, 2]".into());
    let r = t.upload("/api/v1/queue/import/file", lines.join("\n").into_bytes(), "application/x-ndjson", &admin);
    assert_eq!(r.status, 200, "{:?}", j(&r));
    let v = j(&r);
    assert_eq!(v["kind"], "records");
    let items = v["items"].as_array().unwrap();
    assert_eq!(items.len(), 5);
    assert_eq!(items.iter().filter(|i| i["accepted"] == true).count(), 3);
    assert!(items[1]["error"].as_str().unwrap().starts_with("Invalid JSON"));
    assert!(items[4]["error"].as_str().unwrap().contains("got list"));
    assert_eq!((v["summary"]["accepted"].clone(), v["summary"]["quarantined"].clone()), (json!(3), json!(2)));
    assert_eq!(v["source"]["sha256"].as_str().unwrap().len(), 64);
}

#[test]
fn fhir_bundles_are_converted_and_a_damaged_one_is_quarantined_with_a_reason() {
    let t = t(None);
    let admin = t.login("CG-4004");
    let mut lines = lines_of("data/validation/fhir_bundles.jsonl", 2);
    let mut broken: Value = serde_json::from_str(&lines[0]).unwrap();
    broken["entry"] = json!("not a list");
    lines.push(broken.to_string());
    let r = t.upload("/api/v1/queue/import/file", lines.join("\n").into_bytes(), "application/json", &admin);
    let v = j(&r);
    assert_eq!(r.status, 200);
    assert_eq!(v["source"]["format"], "fhir_bundle");
    assert_eq!(v["items"][0]["accepted"], true);
    assert_eq!(v["items"][0]["claim"]["schema_version"], "1.0.0");
    assert_eq!(v["items"][2]["accepted"], false);
    assert!(v["items"][2]["error"].as_str().unwrap().starts_with("FHIR mapping failed"));
}

#[test]
fn the_csv_tables_are_rebuilt_into_claims() {
    let t = t(None);
    let admin = t.login("CG-4004");
    let dir = root().join("data/validation/csv");
    let mut body = serde_json::Map::new();
    for name in cg_ingest::csvfmt::TABLES {
        let text = std::fs::read_to_string(dir.join(format!("{name}.csv"))).unwrap();
        body.insert(name.to_string(), json!(text));
    }
    let r = t.upload("/api/v1/queue/import/csv", Value::Object(body.clone()).to_string().into_bytes(), "application/json", &admin);
    let v = j(&r);
    assert_eq!(r.status, 200, "{v:?}");
    assert_eq!(v["summary"]["accepted"], 150);
    body.remove("lines");
    assert_eq!(t.upload("/api/v1/queue/import/csv", Value::Object(body).to_string().into_bytes(), "application/json", &admin).status, 422, "a table is missing");
}

#[test]
fn files_the_gate_refuses_never_reach_the_reader() {
    let reader = start_reader();
    *reader.mode.lock().unwrap() = Stub::Pages(pages(INVOICE));
    let t = t(Some(&reader));
    let admin = t.login("CG-4004");
    let hostile = b"%PDF-1.4\n1 0 obj\n<< /OpenAction << /S /JavaScript /JS (x) >> >>\nendobj\n".to_vec();
    let r = t.upload("/api/v1/queue/import/file", hostile, "application/pdf", &admin);
    assert_eq!(r.status, 422);
    assert_eq!(j(&r)["code"], "pdf_active_content");
    let tiny = t.upload("/api/v1/queue/import/file", png(20, 20), "image/png", &admin);
    assert_eq!((tiny.status, j(&tiny)["code"].clone()), (422, json!("bad_image")));
    let bomb = t.upload("/api/v1/queue/import/file", png(30000, 30000), "image/png", &admin);
    assert_eq!((bomb.status, j(&bomb)["code"].clone()), (422, json!("image_too_large")));
    assert!(reader.seen_tokens.lock().unwrap().is_empty(), "nothing was sent to the reader");
}

#[test]
fn a_document_becomes_a_draft_with_evidence_and_the_reader_is_asked_properly() {
    let reader = start_reader();
    *reader.mode.lock().unwrap() = Stub::Pages(pages(INVOICE));
    let t = t(Some(&reader));
    let admin = t.login("CG-4004");
    let r = t.upload("/api/v1/queue/import/file", png(800, 600), "image/png", &admin);
    let v = j(&r);
    assert_eq!(r.status, 200, "{v:?}");
    assert_eq!(v["kind"], "document");
    assert_eq!(v["source"]["mime"], "image/png");
    assert_eq!(v["source"]["name"], "scan (1).png");
    let d = &v["draft"];
    assert_eq!(d["kind"], "claim");
    assert_eq!(d["claim"]["claim_id"], "CG-11D0FB9D7A3F");
    assert_eq!(d["claim"]["lines"][0]["service_code"], "SVC-IMAGE");
    assert_eq!(d["valid"], true, "{d:?}");
    assert!(d["fields"].as_array().unwrap().iter().any(|f| f["path"] == "patient_id" && f["method"] == "label"));
    assert_eq!(reader.seen_tokens.lock().unwrap()[0], format!("bearer {TOKEN}"));
    assert_eq!(reader.seen_types.lock().unwrap()[0], "image/png");
    // the security log names the file by its hash, not by its content
    let log = serde_json::to_string(&t.w.svc.log.scan(100).unwrap()).unwrap();
    assert!(log.contains("import_preview") && log.contains(v["source"]["sha256"].as_str().unwrap()));
    assert!(!log.contains("PAT-F1B1050177"), "no claim value is written to the security log");
}

#[test]
fn reader_failures_are_reported_as_what_they_are() {
    let reader = start_reader();
    let t = t(Some(&reader));
    let admin = t.login("CG-4004");
    *reader.mode.lock().unwrap() = Stub::Status(422, "pdf_active_content: JavaScript");
    let r = t.upload("/api/v1/queue/import/file", png(800, 600), "image/png", &admin);
    assert_eq!((r.status, j(&r)["code"].clone()), (422, json!("reader_refused")));
    *reader.mode.lock().unwrap() = Stub::Status(500, "boom");
    assert_eq!(t.upload("/api/v1/queue/import/file", png(800, 600), "image/png", &admin).status, 503);
    let none = crate::t(None);
    let a2 = none.login("CG-4004");
    let r = none.upload("/api/v1/queue/import/file", png(800, 600), "image/png", &a2);
    assert_eq!((r.status, j(&r)["error"].clone()), (503, json!("document_reader_not_configured")));
}

#[test]
fn a_supporting_document_is_offered_as_an_attachment_not_as_a_claim() {
    let reader = start_reader();
    *reader.mode.lock().unwrap() = Stub::Pages(pages("RADIOLOGY - IMAGING REPORT\nPatient ID: PAT-F1B1050177\nSVC-IMAGE performed 2026-05-25\nStatus: FINAL"));
    let t = t(Some(&reader));
    let admin = t.login("CG-4004");
    let v = j(&t.upload("/api/v1/queue/import/file", png(800, 600), "image/png", &admin));
    assert_eq!(v["draft"]["kind"], "attachment");
    assert_eq!(v["draft"]["attachment"]["type"], "imaging-report");
    assert_eq!(v["draft"]["attachment"]["document_status"], "final");
}

#[test]
fn the_body_limit_is_larger_only_on_the_import_and_submit_routes() {
    let t = t(None);
    let admin = t.login("CG-4004");
    let big = vec![b'x'; 70_000];
    assert_eq!(t.upload("/api/v1/queue/import/file", big.clone(), "application/octet-stream", &admin).status, 200, "70 KB is fine on an import route (quarantined as not JSON)");
    assert_ne!(t.upload("/api/v1/queue/submit", big.clone(), "application/json", &admin).status, 413, "70 KB is fine on the submit route (a batch of up to 1,000 claims)");
    let other = t.upload("/api/v1/queue/config", big, "application/json", &admin);
    assert_eq!(other.status, 413, "but not on any other route");
    let huge = vec![b'x'; 17 * 1024 * 1024];
    assert_eq!(t.upload("/api/v1/queue/import/file", huge, "application/octet-stream", &admin).status, 413);
}
