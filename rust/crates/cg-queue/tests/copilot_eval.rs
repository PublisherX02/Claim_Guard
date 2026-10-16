//! Does the reviewer copilot hold up on the 600 labelled public claims? For every flagged finding it must give a suggestion whose cited
//! paths exist on the claim; every proposed correction is applied to a copy and the real engine is run again, to measure how often it
//! really clears the finding it was proposed for and whether it ever creates a new one. Writes `outputs/defense/copilot_eval.json`.
//!
//!     cargo test -p cg-queue --test copilot_eval -- --nocapture

use cg_engine::Engine;
use cg_queue::copilot::{self, Change};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn lines(path: PathBuf) -> Vec<Value> {
    std::fs::read_to_string(path).unwrap().lines().filter(|l| !l.trim().is_empty()).map(|l| serde_json::from_str(l).unwrap()).collect()
}

fn run(engine: &Engine, claim: &Value) -> Value {
    let mut errors = vec![];
    serde_json::to_value(engine.review(claim, &mut errors).unwrap()).unwrap()
}

fn flagged(r: &Value) -> bool {
    ["FAIL", "UNABLE_TO_ASSESS"].contains(&r["status"].as_str().unwrap_or(""))
}

fn status_of(results: &Value, rule: &str) -> String {
    results.as_array().unwrap().iter().find(|r| r["rule_id"] == rule).map(|r| r["status"].as_str().unwrap_or("").to_string()).unwrap_or_default()
}

#[test]
fn the_copilot_is_grounded_and_its_corrections_are_measured_against_the_real_engine() {
    let engine = Engine::load(&root()).expect("the rule engine loads");
    let cfg = engine.cfg();
    let mut pool: Vec<Value> = vec![];
    for dir in ["development", "validation", "stress"] {
        pool.extend(lines(root().join("data").join(dir).join("claims.jsonl")));
    }
    assert_eq!(pool.len(), 600);

    let (mut findings, mut with_text, mut bad_cites, mut bad_words) = (0, 0, 0, 0);
    let mut kinds: BTreeMap<String, i64> = BTreeMap::new();
    // per rule: (suggestions with edits, resolved the target rule, created a new flagged rule)
    let mut edit_stats: BTreeMap<String, (i64, i64, i64)> = BTreeMap::new();
    let (mut claims_with_edits, mut claims_cleared_by_all, mut claims_with_new_flags) = (0, 0, 0);
    let mut identifier_edits_refused = 0;

    for claim in &pool {
        let results = run(&engine, claim);
        let sugg = copilot::suggest(claim, &results, &cfg);
        let n_flagged = results.as_array().unwrap().iter().filter(|r| flagged(r)).count();
        assert_eq!(sugg.len(), n_flagged, "one suggestion per flagged finding");
        let summary = copilot::summary(claim, &results, &sugg);
        assert_eq!(summary["cleared"], json!(n_flagged == 0));
        if n_flagged == 0 {
            assert!(summary["headline"].as_str().unwrap().contains("not payer approval"));
        }
        let mut all_edits: Vec<Change> = vec![];
        for s in &sugg {
            findings += 1;
            *kinds.entry(s["kind"].as_str().unwrap().to_string()).or_default() += 1;
            let text = s["text"].as_str().unwrap();
            if !text.trim().is_empty() {
                with_text += 1;
            }
            let lower = text.to_lowercase();
            if ["will be paid", "payable", "reimburs", "guarantee"].iter().any(|w| lower.contains(w)) {
                bad_words += 1;
            }
            for c in s["cites"].as_array().unwrap() {
                if claim.pointer(c.as_str().unwrap()).is_none() {
                    bad_cites += 1;
                }
            }
            let edits: Vec<Change> = s["proposed_edits"].as_array().unwrap().iter().map(|e| Change { path: e["path"].as_str().unwrap().into(), value: e["to"].clone() }).collect();
            if edits.is_empty() {
                continue;
            }
            let rule = s["rule_id"].as_str().unwrap().to_string();
            // a total proposed on top of corrected lines only works together with them: it is measured in the claim-level run below
            if rule == "R012" && status_of(&results, "R007") == "FAIL" {
                all_edits.extend(edits);
                continue;
            }
            let e = edit_stats.entry(rule.clone()).or_default();
            e.0 += 1;
            let (fixed, _) = copilot::apply(claim, &edits).expect("a proposed edit is always an allowed edit");
            let after = run(&engine, &fixed);
            if !flagged(after.as_array().unwrap().iter().find(|r| r["rule_id"] == rule).unwrap()) {
                e.1 += 1;
            }
            let new = results.as_array().unwrap().iter().zip(after.as_array().unwrap()).any(|(b, a)| !flagged(b) && flagged(a));
            if new {
                e.2 += 1;
            }
            all_edits.extend(edits);
        }
        if !all_edits.is_empty() {
            claims_with_edits += 1;
            // all proposed edits of the claim together (a path proposed twice is applied once)
            let mut seen = std::collections::BTreeSet::new();
            all_edits.retain(|c| seen.insert(c.path.clone()));
            let (fixed, _) = copilot::apply(claim, &all_edits).unwrap();
            let after = run(&engine, &fixed);
            let new = results.as_array().unwrap().iter().zip(after.as_array().unwrap()).any(|(b, a)| !flagged(b) && flagged(a));
            claims_with_new_flags += i32::from(new);
            if after.as_array().unwrap().iter().all(|r| !flagged(r)) {
                claims_cleared_by_all += 1;
            }
            let d = copilot::diff(&results, &after);
            assert_eq!(d["counts"]["newly_flagged"].as_i64().unwrap() > 0, new);
        }
        // identifiers are never editable
        for path in ["/patient_id", "/member_id", "/claim_id", "/coverage/beneficiary_patient_id", "/coverage/member_id", "/lines/0/line_id", "/notes"] {
            let r = copilot::apply(claim, &[Change { path: path.into(), value: json!("X") }]);
            assert!(r.is_err(), "{path} must not be editable");
            identifier_edits_refused += 1;
        }
    }
    assert_eq!(findings, with_text, "every flagged finding gets a suggestion with text");
    assert_eq!(bad_cites, 0, "every cited path exists on the claim");
    assert_eq!(bad_words, 0, "no approval or payment language");

    let per_rule: Value = edit_stats.iter().map(|(k, v)| (k.clone(), json!({"proposed": v.0, "resolved_the_finding": v.1, "created_a_new_finding": v.2}))).collect::<serde_json::Map<_, _>>().into();
    let report = json!({
        "claims": pool.len(), "flagged_findings": findings, "suggestions_by_kind": kinds, "suggestions_with_text": with_text,
        "cited_paths_missing_on_claim": bad_cites, "approval_or_payment_words": bad_words,
        "claims_with_a_proposed_correction": claims_with_edits,
        "claims_with_no_flagged_finding_after_applying_all_proposals": claims_cleared_by_all,
        "claims_where_applying_all_proposals_flagged_a_new_rule": claims_with_new_flags,
        "proposed_corrections_by_rule": per_rule,
        "identifier_edit_attempts_refused": identifier_edits_refused,
        "note": "Public synthetic claims only. A proposed correction is computed from the claim itself (amounts, totals) or the policy currency; it is a suggestion to try in a what-if, and a person checks it against the source bill.",
    });
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    let out = root().join("outputs").join("defense");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("copilot_eval.json"), serde_json::to_string_pretty(&report).unwrap()).unwrap();
    let r007 = edit_stats.get("R007").copied().unwrap_or_default();
    assert!(r007.0 > 0 && r007.1 == r007.0, "a quantity x price correction clears R007 every time it is proposed: {r007:?}");
    assert_eq!(status_of(&json!([]), "R001"), "");
}
