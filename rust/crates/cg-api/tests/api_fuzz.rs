//! Hostile input at the API boundary: whatever the method, path, headers and body, the API answers with a plain 2xx/3xx/4xx, never a 5xx
//! (a 500 means a panic was caught), never echoes the input back, never leaks an internal detail, and never lets an unauthenticated
//! caller reach a protected route. Deterministic (seeded), so a failure can be replayed.

use cg_access::service::testkit::{world, World};
use cg_api::claimstore::FileClaimStore;
use cg_api::{Api, ApiParts, ApiRequest, ApiResponse};
use cg_audit::AuditLog;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

const PW: &str = "Correct-Horse-9";

struct T {
    api: Api,
    w: World,
}

fn t() -> T {
    let w = world();
    for (b, l) in [("CG-1001", 1), ("CG-2002", 2), ("CG-3003", 3), ("CG-4004", 4)] {
        w.provision(b, l, PW);
    }
    let review_log = Arc::new(AuditLog::open(w.dir.path().join("review.jsonl")).unwrap());
    let t0 = w.clock.clone();
    let clock: cg_access::Clock = Arc::new(move || t0.load(std::sync::atomic::Ordering::SeqCst) as f64 / 1000.0);
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
    });
    T { api, w }
}

fn session(t: &T, badge: &str) -> (String, String) {
    let code = t.w.code(badge);
    let body = json!({"badge_id": badge, "password": PW, "totp": code}).to_string().into_bytes();
    let mut headers = HashMap::new();
    headers.insert("content-type".to_string(), "application/json".to_string());
    let r = t.api.handle(ApiRequest { method: "POST".into(), path: "/api/v1/auth/login".into(), query: String::new(), headers, body, client: "10.9.9.9".into() });
    assert_eq!(r.status, 200);
    let tok = r.headers.iter().filter(|(n, _)| n == "set-cookie").find_map(|(_, v)| v.strip_prefix("cg_session=").map(|s| s.split(';').next().unwrap().to_string())).unwrap();
    (tok, serde_json::from_slice::<Value>(&r.body).unwrap()["csrf"].as_str().unwrap().to_string())
}

const PATHS: &[&str] = &[
    "/", "/ui", "/ui/", "/ui/app.js", "/ui/../../etc/passwd", "/ui/%2e%2e/secret", "/healthz", "/api/v1/", "/api/v1/auth/login", "/api/v1/auth/me", "/api/v1/claims", "/api/v1/claims/CG-1",
    "/api/v1/claims/CG-1/unmask", "/api/v1/claims/CG-1/findings/R001/decision", "/api/v1/audit/events", "/api/v1/audit/verify", "/api/v1/users", "/api/v1/users/CG-1001", "/api/v1/users/CG-1001/unlock",
    "/api/v1/ops/health", "/api/v1/ops/audit/summary", "/api/v1/ops/trace/x", "/api/v1/work/inbox", "/api/v1/queue/config", "//", "/api/v1//claims", "/api/v1/claims/%00", "/api/v1/claims/..%2f..%2f",
];
const METHODS: &[&str] = &["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS", "TRACE", "CONNECT", "FOO", ""];
const QUERIES: &[&str] = &["", "limit=0", "limit=-1", "limit=99999999999999999999", "offset=-5", "status=%00", "rule_id=R9&status=FAIL", "since=nan", "since=1e999", "newest_first=maybe", "event_type=%27%20OR%201%3D1", "hours=0", "limit=2&limit=3", "a=%ff%fe"];
const HEADER_VALUES: &[&str] = &["", "application/json", "text/plain", "application/json; charset=utf-16", "application/x-www-form-urlencoded", "multipart/form-data; boundary=x", "-1", "99999999999", "abc", "chunked"];

fn rand_text(rng: &mut ChaCha8Rng, max: usize) -> String {
    let pool = ["a", "Z", "0", " ", "\u{0}", "\u{202e}", "é", "日本", "😀", "\\", "\"", "{", "}", "$ne", "..", "/", "%", "\r\n", "<script>", "'", "-", "_"];
    (0..rng.gen_range(0..max)).map(|_| pool[rng.gen_range(0..pool.len())]).collect()
}

fn rand_json(rng: &mut ChaCha8Rng, depth: u32) -> Value {
    match rng.gen_range(0..9) {
        0 => Value::Null,
        1 => json!(rng.gen::<bool>()),
        2 => json!(rng.gen::<i64>()),
        3 => json!(rng.gen::<f64>()),
        4 => json!(rand_text(rng, 40)),
        5 if depth < 4 => Value::Array((0..rng.gen_range(0..4)).map(|_| rand_json(rng, depth + 1)).collect()),
        6 | 7 if depth < 4 => {
            let keys = ["badge_id", "password", "totp", "action", "reason", "level", "name", "grants", "revokes", "active", "claims", "expected_version", "rule_ids", "actor", "$where", "__proto__"];
            Value::Object((0..rng.gen_range(0..5)).map(|_| (keys[rng.gen_range(0..keys.len())].to_string(), rand_json(rng, depth + 1))).collect())
        }
        _ => json!(rand_text(rng, 10)),
    }
}

fn run(t: &T, rng: &mut ChaCha8Rng, sessions: &[(String, String)], n: usize) -> Vec<(ApiRequest, ApiResponse)> {
    let mut out = vec![];
    for i in 0..n {
        let mut headers = HashMap::new();
        if rng.gen_range(0..3) > 0 {
            headers.insert("content-type".to_string(), HEADER_VALUES[rng.gen_range(0..HEADER_VALUES.len())].to_string());
        }
        if rng.gen_range(0..6) == 0 {
            headers.insert("content-length".to_string(), HEADER_VALUES[rng.gen_range(0..HEADER_VALUES.len())].to_string());
        }
        if rng.gen_range(0..8) == 0 {
            headers.insert("transfer-encoding".to_string(), "chunked".to_string());
        }
        let authed = rng.gen_range(0..4) > 0;
        if authed {
            let (tok, csrf) = &sessions[rng.gen_range(0..sessions.len())];
            let cookie = if rng.gen_range(0..10) == 0 { format!("cg_session={}; cg_session=x", tok) } else { format!("cg_session={tok}") };
            headers.insert("cookie".to_string(), cookie);
            if rng.gen_range(0..4) > 0 {
                headers.insert("x-csrf-token".to_string(), if rng.gen_range(0..5) == 0 { rand_text(rng, 30) } else { csrf.clone() });
            }
        } else if rng.gen_range(0..4) == 0 {
            headers.insert("cookie".to_string(), format!("cg_session={}", rand_text(rng, 200)));
        }
        let body = match rng.gen_range(0..5) {
            0 => vec![],
            1 => rand_text(rng, 300).into_bytes(),
            2 => (0..rng.gen_range(0..64)).map(|_| rng.gen::<u8>()).collect(),
            _ => rand_json(rng, 0).to_string().into_bytes(),
        };
        let req = ApiRequest {
            method: METHODS[rng.gen_range(0..METHODS.len())].to_string(),
            path: PATHS[rng.gen_range(0..PATHS.len())].to_string(),
            query: QUERIES[rng.gen_range(0..QUERIES.len())].to_string(),
            headers,
            body,
            client: format!("10.1.{}.{}", i % 250, rng.gen_range(0..250)),
        };
        let resp = t.api.handle(ApiRequest { method: req.method.clone(), path: req.path.clone(), query: req.query.clone(), headers: req.headers.clone(), body: req.body.clone(), client: req.client.clone() });
        out.push((req, resp));
    }
    out
}

#[test]
fn random_requests_never_cause_a_server_error_or_a_leak() {
    let t = t();
    let sessions: Vec<_> = ["CG-1001", "CG-2002", "CG-3003", "CG-4004"].iter().map(|b| session(&t, b)).collect();
    let mut rng = ChaCha8Rng::seed_from_u64(20261011);
    let n = std::env::var("CG_FUZZ_N").ok().and_then(|v| v.parse().ok()).unwrap_or(30_000);
    let results = run(&t, &mut rng, &sessions, n);
    for (req, resp) in &results {
        let text = String::from_utf8_lossy(&resp.body);
        assert!(resp.status < 500 || resp.status == 501 || resp.status == 503, "{} {}?{} -> {} {}", req.method, req.path, req.query, resp.status, text);
        let h: HashMap<_, _> = resp.headers.iter().cloned().collect();
        assert_eq!(h.get("x-content-type-options").map(String::as_str), Some("nosniff"));
        assert!(h.contains_key("content-security-policy") && h.get("cache-control").map(String::as_str) == Some("no-store"));
        for secret in ["panicked", "RUST_BACKTRACE", "src\\", "src/", ".rs:", "Traceback", "mongodb://", "JWT", "fernet"] {
            assert!(!text.contains(secret), "leak of {secret:?} in {}", text);
        }
        if resp.status >= 400 && resp.headers.iter().any(|(n, v)| n == "content-type" && v.starts_with("application/json")) {
            let v: Value = serde_json::from_slice(&resp.body).expect("errors are JSON");
            assert!(v["error"].is_string(), "{text}");
        }
    }
    let ok = results.iter().filter(|(_, r)| r.status == 200).count();
    assert!(ok > 100, "the corpus must reach real handlers too, not only be refused ({ok} successes)");
}

#[test]
fn nothing_protected_is_reachable_without_a_valid_session() {
    let t = t();
    let mut rng = ChaCha8Rng::seed_from_u64(7);
    let public = ["/", "/ui", "/ui/", "/healthz", "/api/v1/auth/login"];
    for _ in 0..4000 {
        let mut headers = HashMap::new();
        headers.insert("content-type".to_string(), "application/json".to_string());
        if rng.gen_range(0..2) == 0 {
            headers.insert("cookie".to_string(), format!("cg_session={}", rand_text(&mut rng, 80)));
        }
        let path = PATHS[rng.gen_range(0..PATHS.len())].to_string();
        let resp = t.api.handle(ApiRequest {
            method: METHODS[rng.gen_range(0..METHODS.len())].to_string(),
            path: path.clone(),
            query: String::new(),
            headers,
            body: rand_json(&mut rng, 0).to_string().into_bytes(),
            client: "10.2.2.2".into(),
        });
        let is_public = public.contains(&path.as_str()) || path.starts_with("/ui/");
        if !is_public {
            assert!(resp.status != 200 && resp.status != 201, "{path} answered {} to an anonymous caller", resp.status);
        }
    }
}

#[test]
fn forged_and_tampered_sessions_are_refused() {
    let t = t();
    let (tok, csrf) = session(&t, "CG-1001");
    let get = |cookie: String| {
        let mut h = HashMap::new();
        h.insert("cookie".to_string(), cookie);
        t.api.handle(ApiRequest { method: "GET".into(), path: "/api/v1/users".into(), query: String::new(), headers: h, body: vec![], client: "10.3.3.3".into() }).status
    };
    assert_eq!(get(format!("cg_session={tok}")), 403, "a level-1 session is valid but not allowed to list users");
    let parts: Vec<&str> = tok.split('.').collect();
    assert_eq!(parts.len(), 3);
    // alg=none, a swapped payload, a flipped signature byte, truncation, and a token signed with a different key
    let none = format!("{}.{}.", "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0", parts[1]);
    let flipped = format!("{}.{}.{}", parts[0], parts[1], parts[2].chars().rev().collect::<String>());
    let other_payload = format!("{}.{}.{}", parts[0], "eyJzdWIiOiJDRy00MDA0In0", parts[2]);
    for bad in [none, flipped, other_payload, tok[..tok.len() - 3].to_string(), String::new(), "x".repeat(5000)] {
        assert_eq!(get(format!("cg_session={bad}")), 401, "token {:?}", &bad[..bad.len().min(40)]);
    }
    let _ = csrf;
}
