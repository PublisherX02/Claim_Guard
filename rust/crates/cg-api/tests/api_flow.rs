//! The API without a socket: login, CSRF, the permission matrix, masking, decisions and the audit endpoints, on the real engine.

use cg_access::service::testkit::{world, World};
use cg_api::claimstore::FileClaimStore;
use cg_api::{Api, ApiParts, ApiRequest, ApiResponse};
use cg_audit::AuditLog;
use cg_engine::Engine;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

const PW: &str = "Correct-Horse-9";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

struct T {
    api: Api,
    w: World,
}

fn t() -> T {
    let w = world();
    for (b, l) in [("CG-1001", 1), ("CG-2002", 2), ("CG-3003", 3), ("CG-4004", 4)] {
        w.provision(b, l, PW);
    }
    let engine = Engine::load(&root()).unwrap();
    let text = std::fs::read_to_string(root().join("data/stress/claims.jsonl")).unwrap();
    let rows: Vec<(Value, Value)> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(20)
        .map(|l| {
            let c: Value = serde_json::from_str(l).unwrap();
            let mut errs = vec![];
            let r = serde_json::to_value(engine.review(&c, &mut errs).unwrap()).unwrap();
            (c, r)
        })
        .collect();
    let review_log = Arc::new(AuditLog::open(w.dir.path().join("review.jsonl")).unwrap());
    let t0 = w.clock.clone();
    let clock: cg_access::Clock = Arc::new(move || t0.load(std::sync::atomic::Ordering::SeqCst) as f64 / 1000.0);
    let api = Api::new(ApiParts {
        access: w.svc.clone(),
        claims: Arc::new(FileClaimStore::new(rows)),
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

fn req(method: &str, path: &str, body: Option<Value>, cookie: Option<&str>, csrf: Option<&str>) -> ApiRequest {
    let mut headers = HashMap::new();
    if let Some(c) = cookie {
        headers.insert("cookie".to_string(), format!("cg_session={c}"));
    }
    if let Some(c) = csrf {
        headers.insert("x-csrf-token".to_string(), c.to_string());
    }
    let body = body.map(|b| b.to_string().into_bytes()).unwrap_or_default();
    if !body.is_empty() {
        headers.insert("content-type".into(), "application/json".into());
    }
    let (path, query) = path.split_once('?').unwrap_or((path, ""));
    ApiRequest { method: method.into(), path: path.into(), query: query.into(), headers, body, client: "10.0.0.1".into() }
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
        let r = self.api.handle(req("POST", "/api/v1/auth/login", Some(json!({"badge_id": badge, "password": PW, "totp": code})), None, None));
        assert_eq!(r.status, 200, "{:?}", j(&r));
        (cookie_of(&r, "cg_session"), j(&r)["csrf"].as_str().unwrap().to_string())
    }

    fn get(&self, path: &str, s: &(String, String)) -> ApiResponse {
        self.api.handle(req("GET", path, None, Some(&s.0), None))
    }

    fn post(&self, path: &str, body: Value, s: &(String, String)) -> ApiResponse {
        self.api.handle(req("POST", path, Some(body), Some(&s.0), Some(&s.1)))
    }
}

#[test]
fn unauthenticated_requests_are_refused_and_headers_are_set() {
    let t = t();
    let r = t.api.handle(req("GET", "/api/v1/claims", None, None, None));
    assert_eq!(r.status, 401);
    assert_eq!(j(&r)["error"], "unauthenticated");
    let h: HashMap<_, _> = r.headers.iter().cloned().collect();
    assert_eq!(h["cache-control"], "no-store");
    assert_eq!(h["x-content-type-options"], "nosniff");
    assert_eq!(h["content-security-policy"], "default-src 'none'");
    assert_eq!(t.api.handle(req("GET", "/healthz", None, None, None)).status, 200);
    assert_eq!(t.api.handle(req("GET", "/api/v1/nope", None, None, None)).status, 404);
}

#[test]
fn login_sets_httponly_session_and_csrf_is_required_on_unsafe_methods() {
    let t = t();
    let code = t.w.code("CG-3003");
    let r = t.api.handle(req("POST", "/api/v1/auth/login", Some(json!({"badge_id": "CG-3003", "password": PW, "totp": code})), None, None));
    let sess = r.headers.iter().find(|(n, v)| n == "set-cookie" && v.starts_with("cg_session=")).unwrap().1.clone();
    assert!(sess.contains("HttpOnly") && sess.contains("SameSite=Strict"));
    let csrf_cookie = r.headers.iter().find(|(n, v)| n == "set-cookie" && v.starts_with("cg_csrf=")).unwrap().1.clone();
    assert!(!csrf_cookie.contains("HttpOnly"));
    let s = (cookie_of(&r, "cg_session"), j(&r)["csrf"].as_str().unwrap().to_string());
    let no_csrf = t.api.handle(req("POST", "/api/v1/auth/logout", None, Some(&s.0), None));
    assert_eq!(no_csrf.status, 403);
    assert_eq!(j(&no_csrf)["error"], "csrf");
    let wrong = t.api.handle(req("POST", "/api/v1/auth/logout", None, Some(&s.0), Some("nope")));
    assert_eq!(wrong.status, 403);
    assert_eq!(t.post("/api/v1/auth/logout", json!({}), &s).status, 200);
    assert_eq!(t.get("/api/v1/auth/me", &s).status, 401, "a logged-out session is dead");
}

#[test]
fn bad_credentials_look_the_same_and_the_throttle_blocks_a_client() {
    let t = t();
    let mut statuses = vec![];
    for i in 0..105 {
        // a different badge each time: the address as a whole is counted, whoever it tries to sign in as
        let r = t.api.handle(req("POST", "/api/v1/auth/login", Some(json!({"badge_id": format!("CG-9{i:03}"), "password": "x", "totp": "000000"})), None, None));
        statuses.push((r.status, j(&r)["error"].as_str().unwrap_or("").to_string()));
    }
    assert_eq!(statuses[0], (401, "invalid_credentials".to_string()));
    assert_eq!(statuses[99].0, 401, "the hundredth failure is still an ordinary refusal");
    assert_eq!(statuses[104].0, 429, "after a hundred failures from one address it is told to stop");
    let mut other = req("POST", "/api/v1/auth/login", Some(json!({"badge_id": "CG-9999", "password": "x", "totp": "000000"})), None, None);
    other.client = "10.0.0.2".into();
    assert_eq!(t.api.handle(other).status, 401, "a different client is unaffected");
}

#[test]
fn bodies_are_strict() {
    let t = t();
    let s = t.login("CG-3003");
    let r = t.post("/api/v1/auth/change-password", json!({"old_password": PW, "new_password": "x", "extra": 1}), &s);
    assert_eq!(r.status, 422);
    let mut big = req("POST", "/api/v1/auth/logout", None, Some(&s.0), Some(&s.1));
    big.body = vec![b' '; 70_000];
    big.headers.insert("content-type".into(), "application/json".into());
    assert_eq!(t.api.handle(big).status, 413);
    let mut wrong_type = req("POST", "/api/v1/auth/logout", Some(json!({"a": 1})), Some(&s.0), Some(&s.1));
    wrong_type.headers.insert("content-type".into(), "text/plain".into());
    assert_eq!(t.api.handle(wrong_type).status, 415);
}

#[test]
fn permission_matrix() {
    let t = t();
    let sessions: Vec<(i64, (String, String))> = [(1, "CG-1001"), (2, "CG-2002"), (3, "CG-3003"), (4, "CG-4004")].iter().map(|(l, b)| (*l, t.login(b))).collect();
    // (path, the levels that may call it): level 4 is the administrator and sees no claims; 1 to 3 are reviewers
    let cases: [(&str, &[i64]); 6] = [
        ("/api/v1/claims", &[1, 2, 3]),
        ("/api/v1/audit/events", &[4]),
        ("/api/v1/audit/verify", &[4]),
        ("/api/v1/users", &[4]),
        ("/api/v1/ops/health", &[4]),
        ("/api/v1/ops/audit/summary", &[4]),
    ];
    for (path, allowed) in cases {
        for (level, s) in &sessions {
            let r = t.api.handle(req("GET", path, None, Some(&s.0), None));
            if allowed.contains(level) {
                assert_eq!(r.status, 200, "{path} level {level}: {:?}", j(&r));
            } else {
                assert_eq!(r.status, 403, "{path} level {level}");
            }
        }
    }
}

#[test]
fn claim_views_are_masked_by_level_and_unmask_is_audited() {
    let t = t();
    let (low, senior, admin) = (t.login("CG-1001"), t.login("CG-3003"), t.login("CG-4004"));
    let list = j(&t.get("/api/v1/claims?limit=5", &low));
    let id = list["claims"][0]["claim_id"].as_str().unwrap().to_string();
    let view = t.get(&format!("/api/v1/claims/{id}"), &low);
    assert_eq!(view.status, 200);
    let text = String::from_utf8(view.body.clone()).unwrap();
    assert!(!text.contains("PAT-657E845CD1") && !text.contains("MEM-657E845CD1"), "a level-1 view carries no raw patient id");
    let raw = j(&t.post(&format!("/api/v1/claims/{id}/unmask"), json!({"reason": "audit check"}), &senior)).to_string();
    assert!(raw.contains("PAT-"));
    assert!(j(&view).get("allowed_actions").is_none(), "hide, not disable");
    assert_eq!(t.post(&format!("/api/v1/claims/{id}/unmask"), json!({"reason": "audit"}), &low).status, 403);
    let un = t.post(&format!("/api/v1/claims/{id}/unmask"), json!({"reason": "audit check"}), &senior);
    assert_eq!(un.status, 200, "{:?}", j(&un));
    let events = j(&t.get("/api/v1/audit/events?event_type=unmask", &admin));
    assert_eq!(events["events"].as_array().unwrap().len(), 2, "every unmask is audited");
    assert_eq!(t.get("/api/v1/claims/not%20an%20id", &low).status, 404);
}

#[test]
fn decisions_follow_severity_rules_and_are_recorded_under_the_session_badge() {
    let t = t();
    let (mid, high) = (t.login("CG-2002"), t.login("CG-3003"));
    let mut found = None;
    let list = j(&t.get("/api/v1/claims?limit=200&status=FAIL", &high));
    for c in list["claims"].as_array().unwrap() {
        let id = c["claim_id"].as_str().unwrap();
        let v = j(&t.get(&format!("/api/v1/claims/{id}"), &high));
        if let Some(f) = v["findings"].as_array().unwrap().iter().find(|f| f["status"] == "FAIL" && f.get("allowed_actions").is_some()) {
            found = Some((id.to_string(), f["rule_id"].as_str().unwrap().to_string(), f["severity"].clone()));
            break;
        }
    }
    let (id, rule, sev) = found.expect("a failing finding exists in the sample");
    let path = format!("/api/v1/claims/{id}/findings/{rule}/decision");
    let body = json!({"action": "confirm_issue", "reason": "checked the invoice"});
    if sev == "high" {
        assert_eq!(t.post(&path, body.clone(), &mid).status, 403, "level 2 may not decide high severity");
    }
    let ok = t.post(&path, body.clone(), &high);
    assert_eq!(ok.status, 200, "{:?}", j(&ok));
    assert_eq!(j(&ok)["review_state"], "resolved");
    assert_eq!(t.post(&path, json!({"action": "confirm_issue", "reason": "   "}), &high).status, 422);
    assert_eq!(t.post(&path, json!({"action": "erase", "reason": "x"}), &high).status, 422);
    assert_eq!(t.post(&path, json!({"action": "confirm_issue", "reason": "x", "actor": "CG-1001"}), &high).status, 422, "the client cannot name the reviewer");
    let log = std::fs::read_to_string(t.api.review_log.path()).unwrap();
    assert!(log.contains("CG-3003") && !log.contains("CG-1001"));
}

#[test]
fn audit_endpoints_verify_and_filter() {
    let t = t();
    let s = t.login("CG-4004");
    let v = j(&t.get("/api/v1/audit/verify", &s));
    assert_eq!(v["security"]["ok"], true);
    assert_eq!(v["review"]["ok"], true);
    let f = j(&t.get("/api/v1/audit/events?badge=CG-4004&newest_first=true&limit=3", &s));
    assert!(f["events"].as_array().unwrap().len() <= 3 && f["matched"].as_u64().unwrap() >= 1);
    assert_eq!(t.get("/api/v1/audit/events?event_type=bogus", &s).status, 422);
    assert_eq!(t.get("/api/v1/audit/events?limit=0", &s).status, 422);
    assert!(j(&t.get("/api/v1/ops/audit/summary?hours=24", &s)).is_object());
    assert!(j(&t.get("/api/v1/ops/health", &s))["components"].is_array());
}

#[test]
fn user_management_and_console_files() {
    let t = t();
    let admin = t.login("CG-4004");
    let made = t.post("/api/v1/users", json!({"badge_id": "CG-5005", "name": "New Person", "password": PW, "level": 2}), &admin);
    assert_eq!(made.status, 201, "{:?}", j(&made));
    assert!(j(&made)["provisioning_uri"].as_str().unwrap().starts_with("otpauth://"));
    assert_eq!(t.post("/api/v1/users", json!({"badge_id": "CG-5005", "name": "Dup", "password": PW, "level": 2}), &admin).status, 409);
    let users = j(&t.get("/api/v1/users", &admin));
    assert!(users["users"].as_array().unwrap().iter().any(|u| u["badge_id"] == "CG-5005"));
    let patch = t.api.handle(req("PATCH", "/api/v1/users/CG-5005", Some(json!({"active": false})), Some(&admin.0), Some(&admin.1)));
    assert_eq!(patch.status, 200, "{:?}", j(&patch));
    let bad = t.api.handle(req("PATCH", "/api/v1/users/CG-5005", Some(json!({"password": "x"})), Some(&admin.0), Some(&admin.1)));
    assert_eq!(bad.status, 422);
    let page = t.api.handle(req("GET", "/ui/", None, None, None));
    assert_eq!(page.status, 200);
    let h: HashMap<_, _> = page.headers.iter().cloned().collect();
    assert!(h["content-security-policy"].contains("script-src 'self'"));
    assert_eq!(t.api.handle(req("GET", "/ui/../secrets", None, None, None)).status, 404);
}

fn find_failing(t: &T, s: &(String, String), severity: &str) -> (String, String) {
    let list = j(&t.get("/api/v1/claims?limit=200&status=FAIL", s));
    for c in list["claims"].as_array().unwrap() {
        let id = c["claim_id"].as_str().unwrap();
        let v = j(&t.get(&format!("/api/v1/claims/{id}"), s));
        if let Some(f) = v["findings"].as_array().unwrap().iter().find(|f| f["status"] == "FAIL" && f["severity"] == severity && f.get("allowed_actions").is_some()) {
            return (id.to_string(), f["rule_id"].as_str().unwrap().to_string());
        }
    }
    panic!("no failing {severity} finding in the sample");
}

#[test]
fn a_reviewer_may_not_decide_a_high_severity_finding_but_may_decide_a_medium_one() {
    let t = t();
    let (mid, high) = (t.login("CG-2002"), t.login("CG-3003"));
    let (id, rule) = find_failing(&t, &high, "high");
    let path = format!("/api/v1/claims/{id}/findings/{rule}/decision");
    let body = json!({"action": "confirm_issue", "reason": "checked"});
    assert_eq!(t.post(&path, body.clone(), &mid).status, 403, "level 2 on high severity");
    assert_eq!(t.post(&path, body, &high).status, 200, "level 3 on high severity");
    let (id, rule) = find_failing(&t, &mid, "medium");
    let ok = t.post(&format!("/api/v1/claims/{id}/findings/{rule}/decision"), json!({"action": "dismiss_with_reason", "reason": "not an issue"}), &mid);
    assert_eq!(ok.status, 200, "{:?}", j(&ok));
}

#[test]
fn findings_offer_actions_only_to_people_who_may_decide() {
    let t = t();
    let (viewer, reviewer) = (t.login("CG-1001"), t.login("CG-2002"));
    let id = j(&t.get("/api/v1/claims?limit=1&status=FAIL", &viewer))["claims"][0]["claim_id"].as_str().unwrap().to_string();
    let seen_by = |s: &(String, String)| j(&t.get(&format!("/api/v1/claims/{id}"), s))["findings"].as_array().unwrap().iter().filter(|f| f.get("allowed_actions").is_some()).count();
    assert_eq!(seen_by(&viewer), 0, "hide, not disable: no finding of a viewer carries actions");
    assert!(seen_by(&reviewer) > 0);
}

#[test]
fn a_user_who_must_change_the_password_can_do_nothing_else() {
    let t = t();
    t.w.provision_with("CG-6006", 2, PW, true);
    let s = t.login("CG-6006");
    assert_eq!(t.get("/api/v1/auth/me", &s).status, 200);
    let r = t.get("/api/v1/claims", &s);
    assert_eq!((r.status, j(&r)["error"].as_str().unwrap().to_string()), (403, "password_change_required".to_string()));
    assert_eq!(t.post("/api/v1/auth/change-password", json!({"old_password": PW, "new_password": "Another-Good-9x"}), &s).status, 200);
}

#[test]
fn bodies_with_unknown_fields_are_refused_on_login_and_on_decisions() {
    let t = t();
    let code = t.w.code("CG-3003");
    let r = t.api.handle(req("POST", "/api/v1/auth/login", Some(json!({"badge_id": "CG-3003", "password": PW, "totp": code, "level": 4})), None, None));
    assert_eq!(r.status, 422, "a login with an extra field is refused even though the credentials are right");
    let s = t.login("CG-3003");
    let (id, rule) = find_failing(&t, &s, "high");
    let bad = t.post(&format!("/api/v1/claims/{id}/findings/{rule}/decision"), json!({"action": "confirm_issue", "reason": "x", "original_status": "PASS"}), &s);
    assert_eq!(bad.status, 422);
}

#[test]
fn every_response_carries_the_framing_and_sniffing_headers_and_a_refusal_is_audited() {
    let t = t();
    let r = t.api.handle(req("GET", "/healthz", None, None, None));
    let h: HashMap<_, _> = r.headers.iter().cloned().collect();
    assert_eq!(h.get("x-frame-options").map(String::as_str), Some("DENY"));
    assert_eq!(h.get("referrer-policy").map(String::as_str), Some("no-referrer"));
    let (viewer, admin) = (t.login("CG-1001"), t.login("CG-4004"));
    assert_eq!(t.get("/api/v1/users", &viewer).status, 403);
    let ev = j(&t.get("/api/v1/audit/events?event_type=forbidden", &admin));
    assert_eq!(ev["events"].as_array().unwrap().len(), 1, "the refused attempt is on the security log");
    assert!(ev["events"][0].to_string().contains("CG-1001"), "{}", ev["events"][0]);
}

fn with_headers(mut r: ApiRequest, pairs: &[(&str, &str)]) -> ApiRequest {
    for (k, v) in pairs {
        r.headers.insert(k.to_string(), v.to_string());
    }
    r
}

#[test]
fn a_state_changing_request_that_the_browser_says_is_cross_site_is_refused_even_before_signing_in() {
    let t = t();
    let code = t.w.code("CG-3003");
    let login = |extra: &[(&str, &str)]| t.api.handle(with_headers(req("POST", "/api/v1/auth/login", Some(json!({"badge_id": "CG-3003", "password": PW, "totp": code})), None, None), extra));
    for bad in [&[("sec-fetch-site", "cross-site")][..], &[("origin", "https://evil.example"), ("host", "127.0.0.1:8443")], &[("origin", "null"), ("host", "127.0.0.1:8443")], &[("origin", "http://127.0.0.1:8443.evil.example"), ("host", "127.0.0.1:8443")]] {
        let r = login(bad);
        assert_eq!((r.status, j(&r)["error"].as_str().unwrap_or("").to_string()), (403, "cross_site".to_string()), "{bad:?}");
    }
    // the same request from the console itself, from a program with no browser headers, or from the same site is fine
    assert_eq!(login(&[("origin", "http://127.0.0.1:8443"), ("host", "127.0.0.1:8443"), ("sec-fetch-site", "same-origin")]).status, 200);
    let s = t.login("CG-3003");
    assert_eq!(t.post("/api/v1/auth/logout", json!({}), &s).status, 200, "no browser headers at all");
    // reads are not state-changing: a cross-site GET is only subject to the session
    let r = t.api.handle(with_headers(req("GET", "/healthz", None, None, None), &[("sec-fetch-site", "cross-site"), ("origin", "https://evil.example")]));
    assert_eq!(r.status, 200);
}

#[test]
fn responses_carry_the_extra_browser_hardening_headers() {
    let t = t();
    for path in ["/healthz", "/ui/", "/api/v1/claims"] {
        let h: HashMap<_, _> = t.api.handle(req("GET", path, None, None, None)).headers.into_iter().collect();
        assert_eq!(h.get("cross-origin-opener-policy").map(String::as_str), Some("same-origin"), "{path}");
        assert_eq!(h.get("cross-origin-resource-policy").map(String::as_str), Some("same-origin"), "{path}");
        assert!(h.get("permissions-policy").is_some_and(|v| v.contains("camera=()") && v.contains("geolocation=()")), "{path}");
    }
}

#[test]
fn a_reason_that_contains_the_patients_identifier_is_refused_and_unmasking_is_rationed() {
    let mut t = t();
    let senior = t.login("CG-3003");
    let id = j(&t.get("/api/v1/claims?limit=1", &senior))["claims"][0]["claim_id"].as_str().unwrap().to_string();
    let patient = t.api.claims.get(&id).unwrap().0["patient_id"].as_str().unwrap().to_string();
    let r = t.post(&format!("/api/v1/claims/{id}/unmask"), json!({"reason": format!("checking {} against the card", patient.to_lowercase())}), &senior);
    assert_eq!((r.status, j(&r)["error"].as_str().unwrap_or("").to_string()), (422, "invalid_request".to_string()));
    assert_eq!(t.post(&format!("/api/v1/claims/{id}/findings/R001/decision"), json!({"action": "dismiss_with_reason", "reason": format!("{patient} is fine")}), &senior).status, 422);
    assert!(t.get("/api/v1/audit/events?event_type=unmask", &t.login("CG-4004")).body.len() > 0);
    t.api.unmask_per_hour = 3;
    let statuses: Vec<u16> = (0..5).map(|_| t.post(&format!("/api/v1/claims/{id}/unmask"), json!({"reason": "routine check"}), &senior).status).collect();
    assert_eq!(statuses, vec![200, 200, 200, 429, 429], "the fourth unmask in the hour is refused");
    let admin = t.login("CG-4004");
    let ev = j(&t.get("/api/v1/audit/events?event_type=forbidden", &admin));
    assert!(ev["events"].to_string().contains("hourly limit"), "{ev}");
}
