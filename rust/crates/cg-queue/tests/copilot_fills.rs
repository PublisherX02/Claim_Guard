//! The copilot's fills for missing authorization references (R008) and wrong ones (R009), which the 600 public claims never exercise (their failures have no
//! matching authorization on the claim). Method: take a public claim that is clean for the rule, damage one line the way a bad submission would, and check with
//! the real engine that (1) the rule flags it, (2) the copilot proposes the authorization the claim itself carries, (3) applying it clears the rule and flags
//! nothing new. A perturbation test, not evidence about real submissions. Writes `outputs/defense/copilot_fills.json`.
//!
//!     cargo test -p cg-queue --test copilot_fills -- --nocapture

use cg_engine::Engine;
use cg_queue::copilot::{self, Change};
use serde_json::{json, Value};
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

fn status(results: &Value, rule: &str) -> String {
    results.as_array().unwrap().iter().find(|r| r["rule_id"] == rule).map(|r| r["status"].as_str().unwrap_or("").to_string()).unwrap_or_default()
}

#[test]
fn authorization_fills_clear_the_rule_they_are_proposed_for_and_create_nothing_new() {
    let engine = Engine::load(&root()).expect("the rule engine loads");
    let cfg = engine.cfg();
    let mut pool: Vec<Value> = vec![];
    for dir in ["development", "validation", "stress"] {
        pool.extend(lines(root().join("data").join(dir).join("claims.jsonl")));
    }
    // (tried, flagged by the engine, proposed the original authorization, cleared the rule, created a new finding, left to a person although only one fits)
    let mut stats = [("R008", 0, 0, 0, 0, 0, 0), ("R009", 0, 0, 0, 0, 0, 0)];
    for claim in &pool {
        let base = run(&engine, claim);
        for (k, rule) in ["R008", "R009"].iter().enumerate() {
            // only claims whose authorization was valid to begin with: a line that already exceeded its authorization's maximum is a different problem
            if status(&base, "R008") != "PASS" || status(&base, "R009") != "PASS" {
                continue;
            }
            for (i, line) in claim["lines"].as_array().unwrap().iter().enumerate() {
                let Some(auth) = line["authorization_id"].as_str() else { continue };
                let mut broken = claim.clone();
                broken["lines"][i]["authorization_id"] = if *rule == "R008" { Value::Null } else { json!("AUTH-NOT-ON-THIS-CLAIM") };
                let before = run(&engine, &broken);
                stats[k].1 += 1;
                if status(&before, rule) != "FAIL" {
                    continue;
                }
                stats[k].2 += 1;
                let s = copilot::suggest(&broken, &before, &cfg);
                let mine = s.iter().find(|x| x["rule_id"] == *rule).unwrap();
                let edits: Vec<Change> = mine["proposed_edits"].as_array().unwrap().iter().map(|e| Change { path: e["path"].as_str().unwrap().into(), value: e["to"].clone() }).collect();
                if edits.len() == 1 && edits[0].path == format!("/lines/{i}/authorization_id") && edits[0].value == json!(auth) {
                    stats[k].3 += 1;
                }
                if edits.is_empty() {
                    // allowed only when the claim carries two or more authorizations that fit: choosing between them is a person's decision
                    let fitting = claim["authorizations"].as_array().unwrap().iter().filter(|a| a["status"] == "approved" && a["service_code"] == line["service_code"] && a["patient_id"] == claim["patient_id"] && a["valid_from"].as_str() <= line["service_date"].as_str() && line["service_date"].as_str() <= a["valid_to"].as_str()).count();
                    if fitting == 1 {
                        stats[k].6 += 1;
                    }
                    continue;
                }
                let (fixed, _) = copilot::apply(&broken, &edits).expect("a proposed edit is always an allowed edit");
                let after = run(&engine, &fixed);
                if !flagged(after.as_array().unwrap().iter().find(|r| r["rule_id"] == *rule).unwrap()) {
                    stats[k].4 += 1;
                }
                if before.as_array().unwrap().iter().zip(after.as_array().unwrap()).any(|(b, a)| !flagged(b) && flagged(a)) {
                    stats[k].5 += 1;
                }
            }
        }
    }
    let rows: Vec<Value> = stats.iter().map(|s| json!({"rule": s.0, "damaged_lines": s.1, "engine_flagged_it": s.2, "copilot_proposed_the_original_authorization": s.3, "cleared_the_rule": s.4, "created_a_new_finding": s.5, "left_to_a_person_because_several_authorizations_fit": s.2 - s.3, "left_to_a_person_although_only_one_fits": s.6})).collect();
    let report = json!({"method": "a clean public claim, one line damaged (authorization removed for R008, replaced by an id that is not on the claim for R009), then the copilot's proposal applied and the engine run again",
        "rules": rows, "note": "Perturbation test: it shows the fill is right when the claim carries exactly one fitting authorization. The public claims' own R008/R009 failures have none, so there the copilot only asks."});
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
    let out = root().join("outputs").join("defense");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("copilot_fills.json"), serde_json::to_string_pretty(&report).unwrap()).unwrap();
    for s in &stats {
        assert!(s.2 >= 10, "{}: the damage must be one the engine flags often enough to measure ({} of {})", s.0, s.2, s.1);
        assert!(s.3 * 10 >= s.2 * 9, "{}: a proposal is made for nearly every flagged line ({} of {})", s.0, s.3, s.2);
        assert_eq!(s.6, 0, "{}: the copilot only holds back when several authorizations fit", s.0);
        assert_eq!(s.4, s.3, "{}: every proposal is the original authorization and clears the rule", s.0);
        assert_eq!(s.5, 0, "{}: and flags nothing new", s.0);
    }
}

#[test]
fn with_two_authorizations_that_fit_the_copilot_proposes_nothing_and_asks() {
    let engine = Engine::load(&root()).expect("the rule engine loads");
    let cfg = engine.cfg();
    let mut pool: Vec<Value> = vec![];
    for dir in ["development", "validation", "stress"] {
        pool.extend(lines(root().join("data").join(dir).join("claims.jsonl")));
    }
    let mut checked = 0;
    for claim in &pool {
        let base = run(&engine, claim);
        if status(&base, "R008") != "PASS" || status(&base, "R009") != "PASS" {
            continue;
        }
        let Some((i, auth)) = claim["lines"].as_array().unwrap().iter().enumerate().find_map(|(i, l)| {
            let id = l["authorization_id"].as_str()?;
            claim["authorizations"].as_array()?.iter().find(|a| a["authorization_id"] == id).map(|a| (i, a.clone()))
        }) else { continue };
        let mut twice = claim.clone();
        let mut twin = auth.clone();
        twin["authorization_id"] = json!("AUTH-TWIN");
        twice["authorizations"].as_array_mut().unwrap().push(twin);
        twice["lines"][i]["authorization_id"] = Value::Null;
        let before = run(&engine, &twice);
        if status(&before, "R008") != "FAIL" {
            continue;
        }
        let s = copilot::suggest(&twice, &before, &cfg);
        let mine = s.iter().find(|x| x["rule_id"] == "R008").unwrap();
        assert!(mine["proposed_edits"].as_array().unwrap().is_empty(), "two authorizations fit: choosing is a person's decision");
        assert_eq!(mine["kind"], "request_information");
        checked += 1;
    }
    assert!(checked >= 10, "enough claims were tried ({checked})");
}
