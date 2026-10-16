//! Where the reviewer API reads claims and rule results from, and the console files it serves (`src/access/claimstore.py`, `ui.py`).
//!
//! The claim store is a read-only view over the claims and the engine's results held in memory (loaded from the existing JSONL files).
//! Every accessor validates its inputs and returns copies, so a caller can neither reach a path through a claim id nor change the stored data.

use cg_engine::Engine;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::Path;

pub const STATUSES: [&str; 4] = ["PASS", "FAIL", "UNABLE_TO_ASSESS", "NOT_APPLICABLE"];
const FLAGGED: [&str; 2] = ["FAIL", "UNABLE_TO_ASSESS"];
pub const MAX_PAGE: usize = 200;

/// What a listing needs about one claim, worked out once when the store is built (the store never changes afterwards).
struct Digest {
    row: Value,
    statuses: Vec<String>,
    /// (rule id, status) of every result, to answer rule filters without touching the results again.
    rules: Vec<(String, String)>,
}

pub struct FileClaimStore {
    order: Vec<String>,
    claims: BTreeMap<String, Value>,
    results: BTreeMap<String, Value>,
    digests: BTreeMap<String, Digest>,
}

fn digest(id: &str, results: &Value) -> Digest {
    let list = results.as_array().map(Vec::as_slice).unwrap_or(&[]);
    let flagged: Vec<&Value> = list.iter().filter(|r| r["status"].as_str().is_some_and(|s| FLAGGED.contains(&s))).collect();
    let top = flagged.iter().filter_map(|r| r["severity"].as_str()).max_by_key(|s| if *s == "high" { 2 } else { 1 });
    let mut counts = Map::new();
    for r in list {
        let c = counts.entry(r["status"].as_str().unwrap_or("").to_string()).or_insert(json!(0));
        *c = json!(c.as_i64().unwrap_or(0) + 1);
    }
    Digest {
        row: json!({"claim_id": id, "counts": counts, "max_severity": top, "flagged": !flagged.is_empty()}),
        statuses: list.iter().map(|r| r["status"].as_str().unwrap_or("").to_string()).collect(),
        rules: list.iter().map(|r| (r["rule_id"].as_str().unwrap_or("").to_string(), r["status"].as_str().unwrap_or("").to_string())).collect(),
    }
}

fn claim_id_ok(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn rule_id_ok(id: &str) -> bool {
    id.len() == 4 && id.starts_with('R') && id[1..].bytes().all(|b| b.is_ascii_digit())
}

impl FileClaimStore {
    pub fn new(claims: Vec<(Value, Value)>) -> FileClaimStore {
        let mut s = FileClaimStore { order: vec![], claims: BTreeMap::new(), results: BTreeMap::new(), digests: BTreeMap::new() };
        for (c, r) in claims {
            let id = c["claim_id"].as_str().unwrap_or("").to_string();
            if s.claims.insert(id.clone(), c).is_none() {
                s.order.push(id.clone());
            }
            s.digests.insert(id.clone(), digest(&id, &r));
            s.results.insert(id, r);
        }
        s
    }

    pub fn from_jsonl(path: &Path, engine: &Engine) -> Result<FileClaimStore, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let mut rows = vec![];
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let claim: Value = serde_json::from_str(line).map_err(|e| e.to_string())?;
            let mut errors = vec![];
            let results = serde_json::to_value(engine.review(&claim, &mut errors).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
            rows.push((claim, results));
        }
        Ok(FileClaimStore::new(rows))
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    pub fn summaries(&self, limit: usize, offset: usize, status: Option<&str>, rule_id: Option<&str>) -> Result<Vec<Value>, String> {
        if !(1..=MAX_PAGE).contains(&limit) {
            return Err(format!("limit must be 1 to {MAX_PAGE} and offset 0 or more"));
        }
        if status.is_some_and(|s| !STATUSES.contains(&s)) {
            return Err("unknown status".into());
        }
        if rule_id.is_some_and(|r| !rule_id_ok(r)) {
            return Err("rule_id must look like R009".into());
        }
        let mut rows = vec![];
        for id in &self.order {
            let d = &self.digests[id];
            if let Some(rid) = rule_id {
                let hit = d.rules.iter().filter(|(r, _)| r == rid).any(|(_, st)| match status { Some(s) => st == s, None => FLAGGED.contains(&st.as_str()) });
                if !hit {
                    continue;
                }
            } else if let Some(s) = status {
                if !d.statuses.iter().any(|st| st == s) {
                    continue;
                }
            }
            rows.push(&d.row);
        }
        Ok(rows.into_iter().skip(offset).take(limit).cloned().collect())
    }

    pub fn get(&self, claim_id: &str) -> Option<(Value, Value)> {
        if !claim_id_ok(claim_id) {
            return None;
        }
        Some((self.claims.get(claim_id)?.clone(), self.results.get(claim_id)?.clone()))
    }
}

/// Per-finding review state derived from the review audit log (the log is the source of truth; results are never edited). A decision
/// counts only if it was recorded AFTER that claim's latest `run_started` event; within a run the latest decision wins.
pub fn review_state(results: &Value, log_path: &Path) -> BTreeMap<(String, String), &'static str> {
    let rows: Vec<Value> = cg_audit::read_rows(log_path).unwrap_or_default().iter().filter_map(|l| serde_json::from_str(l).ok()).collect();
    let mut run_start: BTreeMap<String, i64> = BTreeMap::new();
    for row in &rows {
        let e = &row["event"];
        if e["event_type"] == "run_started" {
            run_start.insert(e["claim_id"].as_str().unwrap_or("").into(), row["sequence"].as_i64().unwrap_or(0));
        }
    }
    let mut latest: BTreeMap<(String, String), String> = BTreeMap::new();
    for row in &rows {
        let e = &row["event"];
        if e.get("action").is_some() && row["sequence"].as_i64().unwrap_or(0) > run_start.get(e["claim_id"].as_str().unwrap_or("")).copied().unwrap_or(0) {
            latest.insert((e["claim_id"].as_str().unwrap_or("").into(), e["rule_id"].as_str().unwrap_or("").into()), e["action"].as_str().unwrap_or("").into());
        }
    }
    let mut state = BTreeMap::new();
    for r in results.as_array().into_iter().flatten().filter(|r| r["status"].as_str().is_some_and(|s| FLAGGED.contains(&s))) {
        let key = (r["claim_id"].as_str().unwrap_or("").to_string(), r["rule_id"].as_str().unwrap_or("").to_string());
        let st = match latest.get(&key).map(String::as_str) {
            None => "unreviewed",
            Some("confirm_issue" | "dismiss_with_reason") => "resolved",
            Some(_) => "awaiting_follow_up",
        };
        state.insert(key, st);
    }
    state
}

// ---- the console's static files ---------------------------------------------------------------------------------------------------

pub const UI_PREFIX: &str = "/ui/";
pub const UI_CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data: blob:; connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// {name: (bytes, content type)}: a fixed table, so a request can only ever name a file in it (there is no path to join).
pub fn ui_files() -> BTreeMap<&'static str, (&'static [u8], &'static str)> {
    let js = "text/javascript; charset=utf-8";
    BTreeMap::from([
        ("index.html", (&include_bytes!("../../../../src/access/ui/index.html")[..], "text/html; charset=utf-8")),
        ("app.js", (&include_bytes!("../../../../src/access/ui/app.js")[..], js)),
        ("lib.js", (&include_bytes!("../../../../src/access/ui/lib.js")[..], js)),
        ("views-work.js", (&include_bytes!("../../../../src/access/ui/views-work.js")[..], js)),
        ("views-fix.js", (&include_bytes!("../../../../src/access/ui/views-fix.js")[..], js)),
        ("views-admin.js", (&include_bytes!("../../../../src/access/ui/views-admin.js")[..], js)),
        ("views-ops.js", (&include_bytes!("../../../../src/access/ui/views-ops.js")[..], js)),
        ("views-import.js", (&include_bytes!("../../../../src/access/ui/views-import.js")[..], js)),
        ("views-rules.js", (&include_bytes!("../../../../src/access/ui/views-rules.js")[..], js)),
        ("views-ask.js", (&include_bytes!("../../../../src/access/ui/views-ask.js")[..], js)),
        ("style.css", (&include_bytes!("../../../../src/access/ui/style.css")[..], "text/css; charset=utf-8")),
        ("icon.svg", (&include_bytes!("../../../../src/access/ui/icon.svg")[..], "image/svg+xml")),
        ("vendor/qrcode.js", (&include_bytes!("../../../../src/access/ui/vendor/qrcode.js")[..], js)),
    ])
}
