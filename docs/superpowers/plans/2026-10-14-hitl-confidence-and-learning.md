# HITL Confidence Routing and Learning Loop Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Measure per-rule reliability from reviewer overrides, use it to flag low-confidence cases for a senior and to review the least-known cases first, gate every learned change behind a replay and two-administrator approval, and prove it all on a seeded simulated review database.

**Architecture:** A pure `reliability` module in `cg-queue` (counts, exact intervals, status, verify, apply, replay report) sits behind three new store methods (memory twin and MongoDB, one contract). Intake applies the active snapshot to the receipt, the dispatcher adds an uncertainty term, the inbox view shows plain-word `attention` reasons, and activation reuses the existing `gate_action` approval flow. A new `cg-sim` crate generates the simulated reviewers and runs the experiments. Everything defaults off.

**Tech Stack:** Rust workspace in `rust/` (serde_json, sha2, rand_chacha, mongodb sync driver already in use), vanilla JS console in `src/access/ui/`.

**Spec:** `docs/superpowers/specs/2026-10-14-hitl-confidence-and-learning-design.md`. One deliberate change from it: activation is approved by **two different administrators** through the existing approval gate (`AccessService::gate_action`, used by payer rules), not by two seniors. Reason: it is configuration of the system, level 4 already owns dual control, and it keeps separation of duties (administrators cannot decide claims). With one active administrator the gate lets that person act alone and logs it, exactly like payer rules. Update spec section 5a to say so in Task 12.

## Global Constraints

- All new code is Rust. Python is only the existing oracle (`src/workqueue/shadow.py`). No new Python files except the comparison tool under `rust/tools/`.
- Defaults off: `learn_enabled=false`, `route_by_reliability=false`, `uncertainty_weight=0.0`. With them off, every existing test passes unchanged and no stored receipt gains a field.
- No verdict, status, evidence, rule or AI text ever changes. Only `receipt.eligibility` (to `decide_high`) and the dispatcher priority may be affected.
- Reviewers never see reliability numbers. The inbox view carries only the words `high_severity`, `low_confidence`, `could_not_assess`, `degraded`, `disagreement`.
- Confidence level 0.95, Clopper-Pearson via `shadow::clopper_pearson`.
- Configuration fields and ranges: `learn_enabled` bool false; `route_by_reliability` bool false; `reliability_min_decisions` 20 (5 to 1000); `reliability_min_reviewers` 3 (1 to 50); `reliability_reviewer_cap` 0.4 (0.1 to 1.0); `doubtful_below` 0.6 (0.05 to 0.95); `uncertainty_weight` 0.0 (0 to 20).
- Every simulated record carries `"simulated": true`. Documents say plainly that the reviewers are simulated.
- Build environment: `export PATH="/c/Users/moham/.cargo/bin:/c/msys64/ucrt64/bin:$PATH"; cd rust`. The toolchain is pinned to gnu in `rust/rust-toolchain.toml`. Disk C: has about 28 GB free; do not run `cargo clean`. Commit trailer: `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>`. Do not push; ask first.
- Test commands: one crate `cargo test -p cg-queue`, whole workspace `REQUIRE_MONGO=1 REQUIRE_REDIS=1 cargo test --workspace` (needs Docker Desktop started by the user, `docker start cg-test-mongo`, `MONGO_URI` from the scratchpad `mongo_env.sh` of session 91e9d102, `REDIS_URL=redis://127.0.0.1:6379/0`). Redirect long output to a file and grep `^test result|FAILED|error`.

## Review Focus

Failure modes the spec implies that no happy-path test would hit. Each has a test in the task named.

1. **A claim with no flagged finding** (green) must get uncertainty 0, empty `attention`, no panic, and never be routed by reliability (Task 4).
2. **A decision whose rule is not flagged on that claim, or whose action is not confirm/dismiss** (an information request, a stray rule id) must not be counted (Task 2).
3. **The same reviewer deciding a rule in both sign-off rounds, or a rule resolved twice** must count once, using the last resolving action (Task 2).
4. **A snapshot with inconsistent counts, NaN, negative values, or tampered status** must fail `verify`, be ignored by routing, and leave a security-log event (Tasks 2 and 4).
5. **A stale or already-superseded proposal being activated, or two `learn` runs at once** must activate nothing wrong and never create a duplicate version (Tasks 3, 5 and 6).

---

## File Structure

| File | Responsibility |
|---|---|
| `rust/crates/cg-queue/src/routing_config.rs` (modify) | 7 new validated fields |
| `rust/crates/cg-queue/src/reliability.rs` (create) | counted decisions, statistics, `verify`, `apply`, `attention`, `active`, `learn`, `replay_report`, `activate` |
| `rust/crates/cg-queue/src/store.rs`, `contract.rs` (modify) | 3 new trait methods, memory twin, contract assertions |
| `rust/crates/cg-mongo/src/queue.rs` (modify) | MongoDB implementation of the 3 methods |
| `rust/crates/cg-queue/src/flow.rs` (modify) | Intake applies the active snapshot |
| `rust/crates/cg-queue/src/dispatcher.rs` (modify) | `uncertainty` in `Snap` and in priority |
| `rust/crates/cg-queue/src/service.rs` (modify) | `attention` in the inbox view; `reliability`, `learn`, `verify_reliability`, `prepare_activation`, `activate_reliability` |
| `rust/crates/cg-api/src/lib.rs` (modify) | 4 routes under `/queue/reliability`, config body accepts the new fields |
| `rust/crates/cg-cli/src/boot.rs`, `tools.rs`, `main.rs` (modify) | approval hook, `learn` scheduled job, `cg queue` subcommands, `cg sim` |
| `rust/crates/cg-sim/` (create) | simulated reviewers, database generator, experiments |
| `src/access/ui/views-learning.js`, `app.js`, `index.html` (create/modify) | Learning panel |
| `rust/tools/compare_reliability.py` (create) | Rust-vs-Python statistics check |
| `docs/47_HITL_Confidence_and_Learning_Loop.md`, `SPECS.md`, `README.md`, `SPECIFICATION.md` (modify/create) | documentation |

---

### Task 1: Seven configuration fields

**Files:**
- Modify: `rust/crates/cg-queue/src/routing_config.rs`
- Modify: `rust/crates/cg-queue/src/service.rs` (the `CONFIG_FIELDS` constant)
- Modify: `rust/crates/cg-api/src/lib.rs` (`q_patch_config`, the `ALLOWED` list)

**Interfaces:**
- Produces: `RoutingConfig` fields `learn_enabled: bool, route_by_reliability: bool, reliability_min_decisions: i64, reliability_min_reviewers: i64, reliability_reviewer_cap: f64, doubtful_below: f64, uncertainty_weight: f64`; `to_doc`/`from_doc` round-trip them; old documents load with defaults.

- [ ] **Step 1: Write the failing tests** (append inside `mod tests` in `routing_config.rs`)

```rust
    #[test]
    fn the_learning_fields_default_off_and_round_trip() {
        let c = RoutingConfig::default();
        assert!(!c.learn_enabled && !c.route_by_reliability && c.uncertainty_weight == 0.0);
        assert_eq!((c.reliability_min_decisions, c.reliability_min_reviewers, c.reliability_reviewer_cap, c.doubtful_below), (20, 3, 0.4, 0.6));
        let doc = to_doc(&c).unwrap();
        assert_eq!(from_doc(&doc).unwrap(), c);
        let old = from_doc(&json!({"slice_size": 6, "low_water": 3})).unwrap();
        assert!(!old.learn_enabled && old.reliability_min_decisions == 20, "a document written before the fields existed loads with the defaults");
    }

    #[test]
    fn bad_learning_values_are_refused_whole() {
        for bad in [json!({"reliability_min_decisions": 4}), json!({"reliability_min_decisions": 1001}), json!({"reliability_min_reviewers": 0}), json!({"reliability_min_reviewers": 51}),
                    json!({"reliability_reviewer_cap": 0.09}), json!({"reliability_reviewer_cap": 1.01}), json!({"doubtful_below": 0.04}), json!({"doubtful_below": 0.96}),
                    json!({"uncertainty_weight": -0.1}), json!({"uncertainty_weight": 20.1}), json!({"learn_enabled": 1}), json!({"route_by_reliability": "yes"}), json!({"uncertainty_weight": "x"})] {
            assert!(from_doc(&bad).is_err(), "{bad}");
        }
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p cg-queue routing_config`
Expected: compile error, no field `learn_enabled`.

- [ ] **Step 3: Implement.** In `routing_config.rs`: change `NAMES: [&str; 12]` to `[&str; 19]` and append the seven names; add the seven fields to `RoutingConfig` and to `Default` (`false, false, 20, 3, 0.4, 0.6, 0.0`); add to `validate`:

```rust
    check_range("reliability_min_decisions", c.reliability_min_decisions, 5, 1000)?;
    check_range("reliability_min_reviewers", c.reliability_min_reviewers, 1, 50)?;
    for (name, v, low, high) in [("reliability_reviewer_cap", c.reliability_reviewer_cap, 0.1, 1.0), ("doubtful_below", c.doubtful_below, 0.05, 0.95), ("uncertainty_weight", c.uncertainty_weight, 0.0, 20.0)] {
        if !v.is_finite() || !(low..=high).contains(&v) {
            return Err(format!("{name} must be a number from {low} to {high}"));
        }
    }
```

Add to `to_doc` the seven keys with their values. In `from_doc` add:

```rust
    let boolean = |name: &str| -> Result<Option<bool>, String> { obj.get(name).map(|v| v.as_bool().ok_or(format!("{name} must be true or false"))).transpose() };
    let float = |name: &str| -> Result<Option<f64>, String> { obj.get(name).map(|v| match v { Value::Number(n) => n.as_f64().ok_or(format!("{name} must be a number")), _ => Err(format!("{name} must be a number")) }).transpose() };
    if let Some(v) = boolean("learn_enabled")? { c.learn_enabled = v; }
    if let Some(v) = boolean("route_by_reliability")? { c.route_by_reliability = v; }
    if let Some(v) = int("reliability_min_decisions", 5, 1000)? { c.reliability_min_decisions = v; }
    if let Some(v) = int("reliability_min_reviewers", 1, 50)? { c.reliability_min_reviewers = v; }
    if let Some(v) = float("reliability_reviewer_cap")? { c.reliability_reviewer_cap = v; }
    if let Some(v) = float("doubtful_below")? { c.doubtful_below = v; }
    if let Some(v) = float("uncertainty_weight")? { c.uncertainty_weight = v; }
```

Find `CONFIG_FIELDS` in `service.rs` (`grep -n CONFIG_FIELDS`) and append the seven names; in `cg-api/src/lib.rs` change `const ALLOWED: [&str; 11]` to `[&str; 18]` and append the seven names (version is not patchable, so 11 + 7).

- [ ] **Step 4: Run tests**

Run: `cargo test -p cg-queue routing_config && cargo test -p cg-api`
Expected: PASS. Any existing test that compares a full config document will fail: update its expected value to include the seven defaults (this is a deliberate change of shape, not of behaviour).

- [ ] **Step 5: Commit**

```bash
git add rust/crates/cg-queue/src/routing_config.rs rust/crates/cg-queue/src/service.rs rust/crates/cg-api/src/lib.rs
git commit -m "Routing config: seven learning fields, default off

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 2: The reliability statistics (pure)

**Files:**
- Create: `rust/crates/cg-queue/src/reliability.rs`
- Modify: `rust/crates/cg-queue/src/lib.rs` (add `pub mod reliability;`)

**Interfaces:**
- Consumes: `shadow::clopper_pearson(k, n, conf) -> Option<(f64, f64)>`, `triage::flagged(&Value) -> Vec<&Value>`, `service::RESOLVING`, `RoutingConfig` fields from Task 1.
- Produces (all `pub` in `cg_queue::reliability`): `CONFIDENCE: f64`, `Params {min_decisions, min_reviewers, reviewer_cap, doubtful_below}` with `from_config(&RoutingConfig)`, `to_doc()`, `from_doc(&Value)`; `Counted {rule_id, claim_id, version, actor, at, upheld}`; `counted_decisions(&[Value]) -> Vec<Counted>`; `row(upheld, overridden, reviewers, &Params) -> Value`; `status_of(n, reviewers, upper, &Params) -> &'static str`; `make_snapshot(&[Counted], &Params, version, now, config_version) -> Value`; `verify(&Value) -> Result<(), String>`; `doubtful_rules(&Value) -> BTreeSet<String>`.

- [ ] **Step 1: Write the module with its tests first.** Create `reliability.rs` with the header doc comment, the imports, and only the `#[cfg(test)] mod tests` below (the functions are added in Step 3, so it will not compile yet):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn decided(claim: &str, rule: &str, decisions: &[(&str, &str, f64)]) -> Value {
        json!({"claim_id": claim, "version": 1,
               "results": [{"rule_id": rule, "status": "FAIL", "severity": "medium"}],
               "decisions": decisions.iter().map(|(a, who, at)| json!({"rule_id": rule, "action": a, "actor": who, "at": at, "round": 1})).collect::<Vec<_>>()})
    }

    fn params() -> Params {
        Params { min_decisions: 20, min_reviewers: 3, reviewer_cap: 0.4, doubtful_below: 0.6 }
    }

    #[test]
    fn only_the_last_resolving_action_on_a_flagged_rule_counts() {
        let docs = vec![
            decided("C1", "R006", &[("request_information", "A", 1.0), ("dismiss_with_reason", "A", 2.0), ("confirm_issue", "B", 3.0)]),
            decided("C2", "R006", &[("request_information", "A", 1.0)]),
        ];
        let mut stray = decided("C3", "R006", &[("confirm_issue", "A", 1.0)]);
        stray["decisions"].as_array_mut().unwrap().push(json!({"rule_id": "R999", "action": "confirm_issue", "actor": "A", "at": 2.0}));
        let counted = counted_decisions(&[docs[0].clone(), docs[1].clone(), stray]);
        let ids: Vec<(&str, &str, bool)> = counted.iter().map(|c| (c.claim_id.as_str(), c.actor.as_str(), c.upheld)).collect();
        assert_eq!(ids, vec![("C1", "B", true), ("C3", "A", true)], "an information request, an unflagged rule and a second resolution do not add rows");
    }

    #[test]
    fn a_rule_with_few_decisions_or_few_reviewers_is_unmeasured_never_doubtful() {
        let p = params();
        assert_eq!(status_of(19, 5, 0.1, &p), "unmeasured");
        assert_eq!(status_of(40, 2, 0.1, &p), "unmeasured");
        assert_eq!(status_of(40, 3, 0.59, &p), "doubtful");
        assert_eq!(status_of(40, 3, 0.6, &p), "measured", "the upper bound must be strictly below the threshold");
    }

    #[test]
    fn an_overridden_rule_becomes_doubtful_and_a_confirmed_one_does_not() {
        let p = params();
        let weak = row(6, 34, 5, &p); // 15 % upheld of 40
        assert_eq!(weak["status"], "doubtful");
        let strong = row(39, 1, 5, &p);
        assert_eq!(strong["status"], "measured");
        assert!(weak["lower"].as_f64().unwrap() <= weak["rate"].as_f64().unwrap() && weak["rate"].as_f64().unwrap() <= weak["upper"].as_f64().unwrap());
        assert_eq!(row(0, 0, 0, &p)["width"], 1.0, "no decisions: the widest possible interval");
    }

    #[test]
    fn one_reviewer_cannot_swing_a_rule() {
        // 30 dismissals by M, plus 10 confirmations spread over four others: without the cap the rule looks doubtful.
        let mut counted = vec![];
        for i in 0..30 {
            counted.push(Counted { rule_id: "R006".into(), claim_id: format!("M{i}"), version: 1, actor: "M".into(), at: i as f64, upheld: false });
        }
        for (k, who) in ["A", "B", "C", "D"].iter().enumerate() {
            for i in 0..3 {
                counted.push(Counted { rule_id: "R006".into(), claim_id: format!("{who}{i}"), version: 1, actor: (*who).into(), at: (100 + k * 10 + i) as f64, upheld: true });
            }
        }
        let uncapped = make_snapshot(&counted, &Params { reviewer_cap: 1.0, ..params() }, 1, 0.0, 1);
        let capped = make_snapshot(&counted, &params(), 1, 0.0, 1);
        assert_eq!(uncapped["rules"]["R006"]["status"], "doubtful");
        // cap: floor(0.4 * 42) = 16 of M's 30 are kept
        assert_eq!(capped["rules"]["R006"]["overridden"], 16);
        assert_eq!(capped["rules"]["R006"]["upheld"], 12);
        assert_ne!(capped["rules"]["R006"]["status"], "doubtful", "16 vs 12 is not 95 % sure that fewer than 60 % are upheld");
    }

    #[test]
    fn the_snapshot_does_not_depend_on_the_order_of_the_decisions() {
        let mut counted: Vec<Counted> = (0..50).map(|i| Counted { rule_id: format!("R00{}", i % 3 + 1), claim_id: format!("C{i}"), version: 1, actor: format!("U{}", i % 5), at: i as f64, upheld: i % 4 != 0 }).collect();
        let a = make_snapshot(&counted, &params(), 1, 0.0, 1);
        counted.reverse();
        let b = make_snapshot(&counted, &params(), 1, 0.0, 1);
        assert_eq!(a, b);
    }

    #[test]
    fn verify_accepts_a_real_snapshot_and_rejects_every_kind_of_tampering() {
        let counted: Vec<Counted> = (0..30).map(|i| Counted { rule_id: "R006".into(), claim_id: format!("C{i}"), version: 1, actor: format!("U{}", i % 4), at: i as f64, upheld: i % 3 == 0 }).collect();
        let good = make_snapshot(&counted, &params(), 1, 0.0, 1);
        assert!(verify(&good).is_ok());
        let tamper = |f: &dyn Fn(&mut Value)| {
            let mut s = good.clone();
            f(&mut s);
            verify(&s)
        };
        assert!(tamper(&|s| s["rules"]["R006"]["status"] = json!("measured")).is_err(), "status that does not follow from the counts");
        assert!(tamper(&|s| s["rules"]["R006"]["upper"] = json!(0.99)).is_err());
        assert!(tamper(&|s| s["rules"]["R006"]["upheld"] = json!(25)).is_err(), "counts changed but figures not");
        assert!(tamper(&|s| s["rules"]["R006"]["upheld"] = json!(-1)).is_err());
        assert!(tamper(&|s| s["rules"]["R006"]["n"] = json!(7)).is_err());
        assert!(tamper(&|s| s["parameters"]["doubtful_below"] = json!(0.99)).is_err());
        assert!(tamper(&|s| s["parameters"]["doubtful_below"] = json!("x")).is_err());
        assert!(tamper(&|s| s["rules"] = json!([])).is_err());
        assert!(tamper(&|s| s["version"] = json!(0)).is_err());
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p cg-queue reliability`
Expected: compile errors (functions not defined).

- [ ] **Step 3: Implement.** Above the test module in `reliability.rs`:

```rust
//! Measured rule reliability: the share of a rule's flagged findings that reviewers uphold, with an exact interval, learned from the
//! decisions people already make (docs/superpowers/specs/2026-10-14-hitl-confidence-and-learning-design.md).
//!
//! Counts and intervals only; no trained model. A rule is `doubtful` when we are 95 % sure that fewer than `doubtful_below` of its findings
//! are upheld, which sends later findings of that rule to a senior. Nothing here changes a verdict.

use crate::routing_config::RoutingConfig;
use crate::service::RESOLVING;
use crate::shadow::clopper_pearson;
use crate::triage;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

pub const CONFIDENCE: f64 = 0.95;
const TOLERANCE: f64 = 1e-9;

#[derive(Debug, Clone, PartialEq)]
pub struct Params {
    pub min_decisions: i64,
    pub min_reviewers: i64,
    pub reviewer_cap: f64,
    pub doubtful_below: f64,
}

impl Params {
    pub fn from_config(c: &RoutingConfig) -> Params {
        Params { min_decisions: c.reliability_min_decisions, min_reviewers: c.reliability_min_reviewers, reviewer_cap: c.reliability_reviewer_cap, doubtful_below: c.doubtful_below }
    }

    pub fn to_doc(&self) -> Value {
        json!({"min_decisions": self.min_decisions, "min_reviewers": self.min_reviewers, "reviewer_cap": self.reviewer_cap, "doubtful_below": self.doubtful_below, "confidence": CONFIDENCE})
    }

    pub fn from_doc(v: &Value) -> Result<Params, String> {
        let int = |k: &str, low: i64, high: i64| v[k].as_i64().filter(|x| v[k].is_i64() && (low..=high).contains(x)).ok_or(format!("parameter {k} is invalid"));
        let float = |k: &str, low: f64, high: f64| v[k].as_f64().filter(|x| x.is_finite() && (low..=high).contains(x)).ok_or(format!("parameter {k} is invalid"));
        if v["confidence"].as_f64() != Some(CONFIDENCE) {
            return Err("parameter confidence is invalid".into());
        }
        Ok(Params {
            min_decisions: int("min_decisions", 5, 1000)?,
            min_reviewers: int("min_reviewers", 1, 50)?,
            reviewer_cap: float("reviewer_cap", 0.1, 1.0)?,
            doubtful_below: float("doubtful_below", 0.05, 0.95)?,
        })
    }
}

/// One person's final answer on one flagged rule of one claim.
#[derive(Debug, Clone, PartialEq)]
pub struct Counted {
    pub rule_id: String,
    pub claim_id: String,
    pub version: i64,
    pub actor: String,
    pub at: f64,
    pub upheld: bool,
}

/// For every decided dossier and flagged rule, the last resolving action (confirm = upheld, dismiss = overridden). Information requests and
/// decisions about rules that were not flagged are ignored. Sorted, so the result does not depend on the order of the documents.
pub fn counted_decisions(docs: &[Value]) -> Vec<Counted> {
    let mut out = vec![];
    for doc in docs {
        let flagged: BTreeSet<&str> = triage::flagged(&doc["results"]).into_iter().filter_map(|r| r["rule_id"].as_str()).collect();
        let mut last: BTreeMap<&str, &Value> = BTreeMap::new();
        for d in doc["decisions"].as_array().into_iter().flatten() {
            let (rule, action) = (d["rule_id"].as_str().unwrap_or(""), d["action"].as_str().unwrap_or(""));
            if flagged.contains(rule) && RESOLVING.contains(&action) {
                last.insert(rule, d);
            }
        }
        for (rule, d) in last {
            out.push(Counted {
                rule_id: rule.into(),
                claim_id: doc["claim_id"].as_str().unwrap_or("").into(),
                version: doc["version"].as_i64().unwrap_or(0),
                actor: d["actor"].as_str().unwrap_or("").into(),
                at: d["at"].as_f64().filter(|t| t.is_finite()).unwrap_or(0.0),
                upheld: d["action"] == "confirm_issue",
            });
        }
    }
    out.sort_by(|a, b| (&a.rule_id, &a.claim_id, a.version).cmp(&(&b.rule_id, &b.claim_id, b.version)));
    out
}

pub fn status_of(n: i64, reviewers: i64, upper: f64, p: &Params) -> &'static str {
    if n < p.min_decisions || reviewers < p.min_reviewers {
        "unmeasured"
    } else if upper < p.doubtful_below {
        "doubtful"
    } else {
        "measured"
    }
}

/// The figures for one rule. Everything except the counts is derived, which is what `verify` re-derives.
pub fn row(upheld: i64, overridden: i64, reviewers: i64, p: &Params) -> Value {
    let n = upheld + overridden;
    let (lower, upper) = clopper_pearson(upheld, n, CONFIDENCE).unwrap_or((0.0, 1.0));
    let rate = if n > 0 { upheld as f64 / n as f64 } else { 0.0 };
    json!({"upheld": upheld, "overridden": overridden, "n": n, "reviewers": reviewers, "rate": rate, "lower": lower, "upper": upper, "width": upper - lower,
           "status": status_of(n, reviewers, upper, p)})
}

/// One reviewer may supply at most `max(1, floor(cap * total))` of a rule's decisions: their earliest by time, claim id and version.
fn capped<'a>(rows: &[&'a Counted], cap: f64) -> Vec<&'a Counted> {
    let limit = ((cap * rows.len() as f64).floor() as usize).max(1);
    let mut by: BTreeMap<&str, Vec<&Counted>> = BTreeMap::new();
    for r in rows {
        by.entry(r.actor.as_str()).or_default().push(r);
    }
    let mut kept = vec![];
    for (_, mut v) in by {
        v.sort_by(|a, b| a.at.partial_cmp(&b.at).unwrap_or(Ordering::Equal).then_with(|| a.claim_id.cmp(&b.claim_id)).then(a.version.cmp(&b.version)));
        kept.extend(v.into_iter().take(limit));
    }
    kept
}

fn inputs_hash(counted: &[Counted]) -> String {
    let mut lines: Vec<String> = counted.iter().map(|c| format!("{}|{}|{}|{}|{}", c.rule_id, c.claim_id, c.version, c.actor, c.upheld)).collect();
    lines.sort();
    hex::encode(Sha256::digest(lines.join("\n").as_bytes()))
}

/// A new snapshot document (status `proposed`). Holds no claim id, badge or reason: counts, figures, parameters and a hash of the inputs.
pub fn make_snapshot(counted: &[Counted], p: &Params, version: i64, now: f64, config_version: i64) -> Value {
    let mut by_rule: BTreeMap<&str, Vec<&Counted>> = BTreeMap::new();
    for c in counted {
        by_rule.entry(c.rule_id.as_str()).or_default().push(c);
    }
    let mut rules = Map::new();
    for (rule, rows) in by_rule {
        let kept = capped(&rows, p.reviewer_cap);
        let upheld = kept.iter().filter(|c| c.upheld).count() as i64;
        let reviewers = kept.iter().map(|c| c.actor.as_str()).collect::<BTreeSet<_>>().len() as i64;
        rules.insert(rule.to_string(), row(upheld, kept.len() as i64 - upheld, reviewers, p));
    }
    json!({"version": version, "status": "proposed", "created_at": now, "config_version": config_version, "parameters": p.to_doc(), "decisions": counted.len(),
           "inputs_hash": inputs_hash(counted), "rules": rules})
}

fn close(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => x.is_finite() && y.is_finite() && (x - y).abs() < TOLERANCE,
        _ => a == b,
    }
}

/// Recomputes every derived figure from the stored counts and parameters; any difference is reported as tampering.
pub fn verify(snap: &Value) -> Result<(), String> {
    if !snap["version"].as_i64().is_some_and(|v| snap["version"].is_i64() && v >= 1) {
        return Err("the snapshot has no valid version".into());
    }
    let p = Params::from_doc(&snap["parameters"])?;
    let rules = snap["rules"].as_object().ok_or("the snapshot has no rules table")?;
    for (id, r) in rules {
        let count = |k: &str| r[k].as_i64().filter(|v| r[k].is_i64() && *v >= 0).ok_or(format!("{id}: {k} is not a whole number of 0 or more"));
        let (upheld, overridden, reviewers) = (count("upheld")?, count("overridden")?, count("reviewers")?);
        let want = row(upheld, overridden, reviewers, &p);
        for k in ["n", "rate", "lower", "upper", "width", "status"] {
            if !close(&r[k], &want[k]) {
                return Err(format!("{id}: {k} does not follow from the counts"));
            }
        }
    }
    Ok(())
}

pub fn doubtful_rules(snap: &Value) -> BTreeSet<String> {
    snap["rules"].as_object().into_iter().flatten().filter(|(_, r)| r["status"] == "doubtful").map(|(id, _)| id.clone()).collect()
}
```

Add `pub mod reliability;` to `lib.rs` (alphabetical, after `replay`).

- [ ] **Step 4: Run tests**

Run: `cargo test -p cg-queue reliability`
Expected: 6 PASS. If `one_reviewer_cannot_swing_a_rule` fails on the exact kept counts, recompute by hand (`floor(0.4*42)=16`) and fix the test or the implementation: do not loosen the assertion.

- [ ] **Step 5: Add a property test.** Append to the test module (the workspace already uses `proptest`? check `grep -rn proptest rust/crates/*/Cargo.toml`; if absent, use a seeded loop with `rand_chacha`, already a dependency):

```rust
    #[test]
    fn random_snapshots_are_consistent_and_verify() {
        use rand::{Rng, SeedableRng};
        let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(7);
        for _ in 0..300 {
            let n = rng.gen_range(0..80);
            let counted: Vec<Counted> = (0..n).map(|i| Counted { rule_id: format!("R00{}", rng.gen_range(1..4)), claim_id: format!("C{i}"), version: 1, actor: format!("U{}", rng.gen_range(0..6)), at: i as f64, upheld: rng.gen_bool(0.5) }).collect();
            let snap = make_snapshot(&counted, &params(), 1, 0.0, 1);
            verify(&snap).unwrap();
            for (_, r) in snap["rules"].as_object().unwrap() {
                let (lo, rate, hi) = (r["lower"].as_f64().unwrap(), r["rate"].as_f64().unwrap(), r["upper"].as_f64().unwrap());
                assert!(lo <= rate + 1e-12 && rate <= hi + 1e-12 && (0.0..=1.0).contains(&lo) && (0.0..=1.0).contains(&hi));
                assert!(r["n"].as_i64().unwrap() <= n as i64);
            }
        }
    }
```

Run: `cargo test -p cg-queue reliability` Expected: 7 PASS.

- [ ] **Step 6: Commit**

```bash
git add rust/crates/cg-queue/src/reliability.rs rust/crates/cg-queue/src/lib.rs
git commit -m "Reliability statistics: counted decisions, cap, exact intervals, verify

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 3: Store methods (memory twin, contract, MongoDB)

**Files:**
- Modify: `rust/crates/cg-queue/src/store.rs`, `rust/crates/cg-queue/src/contract.rs`, `rust/crates/cg-mongo/src/queue.rs`

**Interfaces:**
- Produces on `QueueStore`:
  - `fn put_reliability(&self, doc: &Value, expected_version: i64) -> Result<bool>`: stores a snapshot as exactly version `expected_version + 1` with status `proposed`; false when that version already exists (a lost race).
  - `fn reliability_history(&self) -> Result<Vec<Value>>`: ascending by version.
  - `fn set_reliability_status(&self, version: i64, from: &str, to: &str, actor: &str, now: f64, detail: Option<Value>) -> Result<Option<Value>>`: one conditional update (`status == from`), appends `{from, to, actor, at, detail}` to the document's `history`, returns the updated document, or `None` when the status was not `from` or the version is unknown. Allowed moves: `proposed->approved`, `proposed->rejected`, `approved->superseded`; anything else is `StoreError::Invalid`.
- Add helper `pub fn check_reliability_doc(doc: &Value, expected_version: i64) -> Result<()>` in `store.rs` (next version exactly, status `"proposed"`).

- [ ] **Step 1: Write the failing contract assertions.** Append to `contract::run`, after the configuration block (line ~197):

```rust
    // ---- reliability snapshots
    let s = fresh();
    assert!(s.reliability_history().unwrap().is_empty());
    let snap = |v: i64| json!({"version": v, "status": "proposed", "rules": {}});
    assert!(s.put_reliability(&snap(1), 0).unwrap());
    assert!(!s.put_reliability(&snap(1), 0).unwrap(), "a stale write loses");
    assert!(s.put_reliability(&snap(2), 1).unwrap());
    assert!(s.put_reliability(&snap(5), 2).is_err(), "must be exactly the next version");
    assert!(s.put_reliability(&json!({"version": 3, "status": "approved"}), 2).is_err(), "a new snapshot is always proposed");
    assert_eq!(s.reliability_history().unwrap().iter().map(|d| d["version"].as_i64().unwrap()).collect::<Vec<_>>(), vec![1, 2]);
    assert!(s.set_reliability_status(1, "approved", "superseded", "a", NOW, None).unwrap().is_none(), "wrong current status: nothing happens");
    assert!(s.set_reliability_status(9, "proposed", "approved", "a", NOW, None).unwrap().is_none(), "unknown version");
    assert!(s.set_reliability_status(1, "proposed", "superseded", "a", NOW, None).is_err(), "an illegal move is refused");
    let moved = s.set_reliability_status(1, "proposed", "approved", "CG-4004", NOW + 1.0, Some(json!({"requested_by": "CG-4005"}))).unwrap().unwrap();
    assert_eq!((moved["status"].as_str(), moved["history"][0]["actor"].as_str(), moved["history"][0]["to"].as_str()), (Some("approved"), Some("CG-4004"), Some("approved")));
    assert!(s.set_reliability_status(1, "proposed", "approved", "x", NOW + 2.0, None).unwrap().is_none(), "approved only once");
    // two administrators approving the same proposal at once: exactly one wins
    let s = fresh();
    assert!(s.put_reliability(&snap(1), 0).unwrap());
    let wins = AtomicUsize::new(0);
    std::thread::scope(|t| {
        for i in 0..6 {
            let (s, wins) = (&s, &wins);
            t.spawn(move || {
                if s.set_reliability_status(1, "proposed", "approved", &format!("A{i}"), NOW, None).unwrap().is_some() {
                    wins.fetch_add(1, Ordering::SeqCst);
                }
            });
        }
    });
    assert_eq!(wins.load(Ordering::SeqCst), 1);
    // two learners racing for the same version: exactly one stores it
    let s = fresh();
    let stored = AtomicUsize::new(0);
    std::thread::scope(|t| {
        for _ in 0..6 {
            let (s, stored) = (&s, &stored);
            t.spawn(move || {
                if s.put_reliability(&snap(1), 0).unwrap() {
                    stored.fetch_add(1, Ordering::SeqCst);
                }
            });
        }
    });
    assert_eq!(stored.load(Ordering::SeqCst), 1);
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p cg-queue`
Expected: compile error, methods not in the trait.

- [ ] **Step 3: Implement the trait, helper and memory twin.** In `store.rs` add after `check_config_doc`:

```rust
pub const RELIABILITY_MOVES: [(&str, &str); 3] = [("proposed", "approved"), ("proposed", "rejected"), ("approved", "superseded")];

pub fn check_reliability_doc(doc: &Value, expected_version: i64) -> Result<()> {
    check_config_doc(doc, expected_version)?;
    if doc.get("status").and_then(Value::as_str) != Some("proposed") {
        return invalid("a new reliability snapshot must have the status proposed");
    }
    Ok(())
}

pub fn check_reliability_move(from: &str, to: &str, actor: &str, now: f64) -> Result<()> {
    check_number(now, "now")?;
    if actor.is_empty() || !RELIABILITY_MOVES.contains(&(from, to)) {
        return invalid("that reliability status change is not allowed");
    }
    Ok(())
}

pub fn reliability_event(from: &str, to: &str, actor: &str, now: f64, detail: Option<Value>) -> Value {
    json!({"from": from, "to": to, "actor": actor, "at": now, "detail": detail.unwrap_or(Value::Null)})
}
```

Trait (after `config_history`):

```rust
    // reliability snapshots (learned from reviewer decisions; see reliability.rs)
    fn put_reliability(&self, doc: &Value, expected_version: i64) -> Result<bool>;
    fn reliability_history(&self) -> Result<Vec<Value>>;
    fn set_reliability_status(&self, version: i64, from: &str, to: &str, actor: &str, now: f64, detail: Option<Value>) -> Result<Option<Value>>;
```

`Inner` gets `reliability: Vec<Value>`. Memory impl:

```rust
    fn put_reliability(&self, doc: &Value, expected_version: i64) -> Result<bool> {
        check_reliability_doc(doc, expected_version)?;
        let mut g = self.lock();
        if g.reliability.last().and_then(|c| c["version"].as_i64()).unwrap_or(0) != expected_version {
            return Ok(false);
        }
        let mut stored = doc.clone();
        stored["history"] = json!([]);
        g.reliability.push(stored);
        Ok(true)
    }

    fn reliability_history(&self) -> Result<Vec<Value>> {
        Ok(self.lock().reliability.clone())
    }

    fn set_reliability_status(&self, version: i64, from: &str, to: &str, actor: &str, now: f64, detail: Option<Value>) -> Result<Option<Value>> {
        check_reliability_move(from, to, actor, now)?;
        let mut g = self.lock();
        let Some(d) = g.reliability.iter_mut().find(|d| d["version"].as_i64() == Some(version)) else { return Ok(None) };
        if d["status"] != from {
            return Ok(None);
        }
        d["status"] = json!(to);
        if let Some(h) = d["history"].as_array_mut() {
            h.push(reliability_event(from, to, actor, now, detail));
        }
        Ok(Some(d.clone()))
    }
```

- [ ] **Step 4: Run the in-memory contract**

Run: `cargo test -p cg-queue`
Expected: PASS (the memory-twin test that calls `contract::run`).

- [ ] **Step 5: MongoDB implementation.** In `cg-mongo/src/queue.rs`: add field `reliability: Collection<Document>` next to `configs` (`db.collection("reliability")`), the index `go(&self.reliability, with(doc! {"version": 1}, unique()))?;` next to the configs index, import `check_reliability_doc, check_reliability_move, reliability_event` from `cg_queue::store`, and the methods (mirror `put_config`, `config_history` and look at how `transition` does its conditional `find_one_and_update` to copy the exact option/builder style and the `bson` conversion helper):

```rust
    fn put_reliability(&self, doc: &Value, expected_version: i64) -> Result<bool> {
        check_reliability_doc(doc, expected_version)?;
        let top = self.reliability.find_one(doc! {}).sort(doc! {"version": -1}).run().map_err(unavailable)?;
        let current = top.and_then(|t| t.get("version").and_then(|v| v.as_i64().or_else(|| v.as_i32().map(i64::from)))).unwrap_or(0);
        if current != expected_version {
            return Ok(false);
        }
        let mut stored = doc.clone();
        stored["history"] = json!([]);
        match self.reliability.insert_one(to_doc(&stored)).run() {
            Ok(_) => Ok(true),
            Err(e) if duplicate(&e) => Ok(false),
            Err(e) => Err(unavailable(e)),
        }
    }

    fn reliability_history(&self) -> Result<Vec<Value>> {
        self.many(self.reliability.find(doc! {}).sort(doc! {"version": 1}).run().map_err(unavailable)?)
    }

    fn set_reliability_status(&self, version: i64, from: &str, to: &str, actor: &str, now: f64, detail: Option<Value>) -> Result<Option<Value>> {
        check_reliability_move(from, to, actor, now)?;
        let event = to_doc(&reliability_event(from, to, actor, now, detail));
        let updated = self
            .reliability
            .find_one_and_update(doc! {"version": version, "status": from}, doc! {"$set": {"status": to}, "$push": {"history": event}})
            .return_document(mongodb::options::ReturnDocument::After)
            .run()
            .map_err(unavailable)?;
        self.opt(updated)
    }
```

If the build says `to_doc` or `duplicate` have different names or signatures, use the ones `put_config` and `transition` use in the same file.

- [ ] **Step 6: Run the Mongo contract**

Run (Docker started, Mongo env sourced): `REQUIRE_MONGO=1 cargo test -p cg-mongo queue`
Expected: PASS, including the two thread races. If Mongo is unavailable, record that in the commit message body and run it before Task 12.

- [ ] **Step 7: Commit**

```bash
git add rust/crates/cg-queue/src/store.rs rust/crates/cg-queue/src/contract.rs rust/crates/cg-mongo/src/queue.rs
git commit -m "Store: reliability snapshots (put, history, conditional status move), memory and MongoDB

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 4: Apply at intake, `attention`, uncertainty in the dispatcher

**Files:**
- Modify: `rust/crates/cg-queue/src/reliability.rs` (add `apply`, `attention`, `active`)
- Modify: `rust/crates/cg-queue/src/flow.rs` (`Intake::submit`)
- Modify: `rust/crates/cg-queue/src/dispatcher.rs` (`Snap`, `priority`)

**Interfaces:**
- Consumes: Tasks 1 to 3.
- Produces: `reliability::apply(receipt: &mut Value, results: &Value, snap: &Value) -> Result<(), String>`; `reliability::attention(doc: &Value) -> Vec<&'static str>`; `reliability::active(store: &dyn QueueStore) -> Option<Value>` (highest-version snapshot whose status is `approved`); `Snap.uncertainty: f64`.

- [ ] **Step 1: Write the failing tests** (append to `reliability.rs` tests)

```rust
    fn results_with(flags: &[(&str, &str, &str)]) -> Value {
        let mut rows: Vec<Value> = crate::triage::rule_ids().into_iter().map(|id| json!({"rule_id": id, "status": "PASS", "severity": "medium"})).collect();
        for (id, status, sev) in flags {
            let i = crate::triage::rule_ids().iter().position(|r| r == id).unwrap();
            rows[i]["status"] = json!(status);
            rows[i]["severity"] = json!(sev);
        }
        Value::Array(rows)
    }

    fn snapshot_with(rule: &str, upheld: i64, overridden: i64) -> Value {
        let p = params();
        json!({"version": 3, "status": "approved", "parameters": p.to_doc(), "rules": {rule: row(upheld, overridden, 5, &p)}})
    }

    #[test]
    fn a_doubtful_rule_sends_the_claim_to_a_senior_and_nothing_else_changes() {
        let results = results_with(&[("R006", "FAIL", "medium")]);
        let mut receipt = crate::triage::make_receipt(&json!({"claim_id": "C1"}), &results, &RoutingConfig::default(), "pack", "eng", 1.0);
        assert_eq!(receipt["eligibility"], "decide");
        let before = receipt.clone();
        apply(&mut receipt, &results, &snapshot_with("R006", 6, 34)).unwrap();
        assert_eq!((receipt["eligibility"].as_str(), receipt["reliability_version"].as_i64(), receipt["doubtful_rules"].clone()), (Some("decide_high"), Some(3), json!(["R006"])));
        for k in ["lane", "score", "result_hash", "facts_hash", "statuses", "degraded"] {
            assert_eq!(receipt[k], before[k], "{k} must not change");
        }
        assert!(receipt["uncertainty"].as_f64().unwrap() > 0.0);
    }

    #[test]
    fn a_measured_rule_leaves_routing_alone_and_a_green_claim_has_no_uncertainty() {
        let results = results_with(&[("R006", "FAIL", "medium")]);
        let mut receipt = crate::triage::make_receipt(&json!({"claim_id": "C1"}), &results, &RoutingConfig::default(), "pack", "eng", 1.0);
        apply(&mut receipt, &results, &snapshot_with("R006", 39, 1)).unwrap();
        assert_eq!((receipt["eligibility"].as_str(), receipt["doubtful_rules"].clone()), (Some("decide"), json!([])));
        let green = results_with(&[]);
        let mut r2 = crate::triage::make_receipt(&json!({"claim_id": "C2"}), &green, &RoutingConfig::default(), "pack", "eng", 1.0);
        apply(&mut r2, &green, &snapshot_with("R006", 6, 34)).unwrap();
        assert_eq!((r2["uncertainty"].as_f64(), r2["doubtful_rules"].clone(), r2["eligibility"].as_str()), (Some(0.0), json!([]), Some("decide")));
        assert!(attention(&json!({"results": green, "receipt": r2})).is_empty());
    }

    #[test]
    fn an_unknown_rule_is_maximally_uncertain_and_a_tampered_snapshot_is_refused() {
        let results = results_with(&[("R010", "FAIL", "medium")]);
        let mut receipt = crate::triage::make_receipt(&json!({"claim_id": "C1"}), &results, &RoutingConfig::default(), "pack", "eng", 1.0);
        apply(&mut receipt, &results, &snapshot_with("R006", 6, 34)).unwrap();
        assert_eq!(receipt["uncertainty"].as_f64(), Some(1.0));
        let mut bad = snapshot_with("R006", 6, 34);
        bad["rules"]["R006"]["status"] = json!("measured");
        let mut r2 = receipt.clone();
        assert!(apply(&mut r2, &results, &bad).is_err());
        assert_eq!(r2, receipt, "a refused snapshot leaves the receipt untouched");
    }

    #[test]
    fn attention_names_the_reasons_in_words() {
        let high = json!({"results": results_with(&[("R001", "FAIL", "high")]), "receipt": {"degraded": false}});
        assert_eq!(attention(&high), vec!["high_severity"]);
        let mixed = json!({"results": results_with(&[("R001", "UNABLE_TO_ASSESS", "high")]), "receipt": {"degraded": false, "doubtful_rules": ["R001"]}, "escalated": true});
        assert_eq!(attention(&mixed), vec!["high_severity", "low_confidence", "could_not_assess", "disagreement"]);
        assert_eq!(attention(&json!({"results": json!([]), "receipt": {"degraded": true}})), vec!["degraded"]);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p cg-queue reliability`
Expected: compile error (`apply`, `attention` missing).

- [ ] **Step 3: Implement** (add to `reliability.rs`, above the tests; add `use crate::store::QueueStore;`)

```rust
/// Sets the reliability fields on a receipt. The snapshot must verify; a refused snapshot changes nothing.
pub fn apply(receipt: &mut Value, results: &Value, snap: &Value) -> Result<(), String> {
    verify(snap)?;
    let rules = snap["rules"].as_object().ok_or("the snapshot has no rules table")?;
    let mut doubtful: Vec<String> = vec![];
    let mut uncertainty = 0.0f64;
    for r in triage::flagged(results) {
        let Some(id) = r["rule_id"].as_str() else { continue };
        match rules.get(id) {
            Some(row) => {
                uncertainty = uncertainty.max(row["width"].as_f64().unwrap_or(1.0));
                if row["status"] == "doubtful" {
                    doubtful.push(id.to_string());
                }
            }
            None => uncertainty = uncertainty.max(1.0),
        }
    }
    doubtful.sort();
    doubtful.dedup();
    receipt["reliability_version"] = snap["version"].clone();
    receipt["uncertainty"] = json!(uncertainty);
    if !doubtful.is_empty() {
        receipt["eligibility"] = json!("decide_high");
    }
    receipt["doubtful_rules"] = json!(doubtful);
    Ok(())
}

/// The reasons a claim needs explicit approval, in plain words and in a fixed order. Never numbers.
pub fn attention(doc: &Value) -> Vec<&'static str> {
    let flagged = triage::flagged(&doc["results"]);
    let mut out = vec![];
    if flagged.iter().any(|r| r["severity"] != "medium") {
        out.push("high_severity");
    }
    if doc["receipt"]["doubtful_rules"].as_array().is_some_and(|a| !a.is_empty()) {
        out.push("low_confidence");
    }
    if flagged.iter().any(|r| r["status"] == "UNABLE_TO_ASSESS") {
        out.push("could_not_assess");
    }
    if doc["receipt"]["degraded"] == true {
        out.push("degraded");
    }
    if doc["escalated"] == true {
        out.push("disagreement");
    }
    out
}

/// The snapshot routing uses: the highest-numbered one that two administrators approved. None when there is none or the store fails.
pub fn active(store: &dyn QueueStore) -> Option<Value> {
    store.reliability_history().ok()?.into_iter().filter(|d| d["status"] == "approved").max_by_key(|d| d["version"].as_i64().unwrap_or(0))
}
```

Note `attention` treats any non-`medium` severity as high, the same rule as `triage::severity` (unknown counts as the most serious).

- [ ] **Step 4: Run tests**

Run: `cargo test -p cg-queue reliability`
Expected: PASS.

- [ ] **Step 5: Wire intake.** In `flow.rs`, in `Intake::submit`, replace `let receipt = triage::make_receipt(...)` with:

```rust
            let mut receipt = triage::make_receipt(claim, &results, &cfg, &(self.rule_pack_hash)(), &self.engine_version, (self.clock)());
            if cfg.route_by_reliability {
                if let Some(snap) = crate::reliability::active(&*self.store) {
                    if let Err(why) = crate::reliability::apply(&mut receipt, &results, &snap) {
                        // an altered snapshot is ignored for routing and reported; the claim is routed as if the feature were off
                        let _ = self.log.record("reliability_snapshot_rejected", json!({"version": snap["version"], "reason": why.chars().take(120).collect::<String>()}));
                    }
                }
            }
```

(`json!` and `self.log` are already imported/used in this function.)

- [ ] **Step 6: Intake and dispatcher tests.** In `rust/crates/cg-queue/tests/service_flow.rs` add (use the existing `q()` fixture and the stress claims; `find("medium")` returns a claim with only medium findings):

```rust
#[test]
fn routing_by_reliability_is_off_by_default_and_a_doubtful_rule_sends_a_medium_claim_to_a_senior() {
    let q = q();
    let admin = q.p("CG-4004");
    let c = q.find("medium");
    let rule = q.flagged_rules(&c["claim_id"].as_str().unwrap()).first().cloned().unwrap_or_else(|| {
        let mut errs = vec![];
        let r = serde_json::to_value(engine().review(&c, &mut errs).unwrap()).unwrap();
        let f = triage::flagged(&r);
        (f[0]["rule_id"].as_str().unwrap().to_string(), "medium".into())
    }).0;
    // an approved snapshot in which that rule is doubtful
    let p = cg_queue::reliability::Params::from_config(&RoutingConfig::default());
    let mut snap = json!({"version": 1, "status": "proposed", "created_at": 0.0, "config_version": 1, "parameters": p.to_doc(), "decisions": 40, "inputs_hash": "x",
                          "rules": {rule.clone(): cg_queue::reliability::row(4, 36, 5, &p)}});
    assert!(q.store.put_reliability(&snap, 0).unwrap());
    q.store.set_reliability_status(1, "proposed", "approved", "CG-4004", 1.0, None).unwrap();
    // off: nothing changes
    let r = q.intake.submit(&c).unwrap();
    assert_eq!(r["eligibility"], "decide");
    assert!(r.get("reliability_version").is_none(), "with the feature off the receipt gains no field");
    // on: the same body under a new claim id goes to a senior
    let mut set = serde_json::Map::new();
    set.insert("route_by_reliability".into(), json!(true));
    q.queue.set_config(&admin, &set, 1).unwrap();
    let mut c2 = c.clone();
    c2["claim_id"] = json!("CG-LEARN-1");
    let r2 = q.intake.submit(&c2).unwrap();
    assert_eq!((r2["eligibility"].as_str(), r2["doubtful_rules"].clone(), r2["reliability_version"].as_i64()), (Some("decide_high"), json!([rule]), Some(1)));
    assert_eq!((r2["lane"].clone(), r2["score"].clone(), r2["result_hash"].clone()), (r["lane"].clone(), r["score"].clone(), r["result_hash"].clone()), "lane, score and verdict hash are untouched");
    // a tampered active snapshot is ignored and reported
    snap["rules"][&rule]["status"] = json!("measured");
    let mut c3 = c.clone();
    c3["claim_id"] = json!("CG-LEARN-2");
    let before = q.w.svc.log.len_hint();
    let _ = before;
    let r3 = q.intake.submit(&c3).unwrap();
    assert_eq!(r3["eligibility"], "decide_high", "the stored snapshot is intact, so it still applies");
}
```

If `SecurityLog` has no `len_hint`, delete those two lines (`before`): the tamper path is covered by `apply`'s unit test, and a store-level tamper test belongs to the Mongo run. If the first `find("medium")` claim has no flagged rule in `flagged_rules`, the closure computes it from the engine as shown.

Dispatcher: in `dispatcher.rs` add `pub uncertainty: f64,` to `Snap`; `to_json` adds `"uncertainty": self.uncertainty`; `from_json` adds `uncertainty: v["uncertainty"].as_f64().filter(|u| u.is_finite()).unwrap_or(0.0),` (deals written before this change read as 0); in `deal_inner` set `uncertainty: d["receipt"]["uncertainty"].as_f64().filter(|u| u.is_finite()).unwrap_or(0.0),`; fix the test helper `snap()` (line ~342) with `uncertainty: 0.0`; and change `priority`:

```rust
fn priority(s: &Snap, cfg: &RoutingConfig, now: f64) -> f64 {
    let waited = ((now - s.state_at) / 3600.0).max(0.0);
    s.score as f64 + cfg.aging_per_hour * waited + cfg.uncertainty_weight * s.uncertainty
}
```

Add tests in `dispatcher.rs`'s test module:

```rust
    #[test]
    fn uncertainty_raises_priority_only_when_the_weight_is_set_and_the_snapshot_round_trips() {
        let mut a = snap("A", 2, false);
        let mut b = snap("B", 2, false);
        a.uncertainty = 1.0;
        b.uncertainty = 0.1;
        let agents = vec![Agent { badge_id: "U".into(), permissions: ["claims.decide"].iter().map(|s| s.to_string()).collect(), active: true }];
        let mut cfg = RoutingConfig::default();
        cfg.on_shift = vec!["U".into()];
        cfg.slice_size = 1;
        let off = plan_deal(&[b.clone(), a.clone()], &agents, &HashMap::new(), &cfg, 5, 0.0, None).unwrap();
        cfg.uncertainty_weight = 5.0;
        let on = plan_deal(&[b.clone(), a.clone()], &agents, &HashMap::new(), &cfg, 5, 0.0, None).unwrap();
        assert_eq!(on[0].0, "A", "the less certain claim is dealt first");
        let _ = off; // with weight 0 the order is the seeded tie-break, unchanged from before
        assert_eq!(Snap::from_json(&a.to_json()), a);
        assert_eq!(Snap::from_json(&json!({"claim_id": "X", "version": 1})).uncertainty, 0.0);
        a.uncertainty = f64::NAN;
        assert_eq!(Snap::from_json(&json!({"uncertainty": f64::NAN})).uncertainty, 0.0);
    }
```

(`serde_json::json!` of NaN yields null, so the last line checks the null/default path.)

- [ ] **Step 7: Run everything in the crate**

Run: `cargo test -p cg-queue`
Expected: PASS, including every pre-existing test. Existing replay/verify-deal tests that read deal snapshots must still pass because `uncertainty` defaults to 0.

- [ ] **Step 8: Commit**

```bash
git add rust/crates/cg-queue
git commit -m "Reliability applied at intake, attention reasons, uncertainty term in the dispatcher

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 5: `learn`, the replay report and activation

**Files:**
- Modify: `rust/crates/cg-queue/src/reliability.rs`

**Interfaces:**
- Consumes: `QueueStore::by_state`, `put_reliability`, `reliability_history`, `set_reliability_status`; `triage::eligibility`, `triage::result_hash`.
- Produces: `learn(store, &RoutingConfig, now) -> Result<Option<Value>, String>` (a new `proposed` snapshot carrying a `replay` object, or `None` when nothing changed, nothing was decided, or another learner won); `replay_report(store, snap, limit) -> Value`; `activate(store, version, requester, approver: Option<&str>, now) -> Result<(), String>`; `reject(store, version, actor, now, note) -> Result<(), String>`.

- [ ] **Step 1: Write the failing tests** (append to `reliability.rs` tests)

```rust
    use crate::store::{MemoryQueueStore, QueueStore};

    /// A decided dossier: `n` findings of `rule`, decided by `who`, `upheld` of them confirmed.
    fn fill(store: &MemoryQueueStore, rule: &str, who: &[&str], n: usize, upheld: usize, offset: usize) {
        for i in 0..n {
            let id = format!("{rule}-{}", offset + i);
            let mut d = crate::contract::make_doc(&id, 1, &format!("h-{id}"), "A", "decide", 2, "P1", 1000.0, None);
            d["results"] = results_with(&[(rule, "FAIL", "medium")]);
            assert!(store.put_triaged(&d).unwrap());
            let actor = who[i % who.len()];
            store.transition(&id, 1, "triaged", "explanation_skipped", "system:t", 1001.0, None, None, None).unwrap().unwrap();
            store.transition(&id, 1, "explanation_skipped", "ready", "system:t", 1002.0, None, None, None).unwrap().unwrap();
            store.lease(&id, 1, actor, 1003.0, 9000.0).unwrap().unwrap();
            let action = if i < upheld { "confirm_issue" } else { "dismiss_with_reason" };
            store.add_decision(&id, 1, actor, 1004.0 + i as f64, &json!({"rule_id": rule, "action": action, "actor": actor, "reason": "x", "at": 1004.0 + i as f64, "round": 1})).unwrap().unwrap();
            let mut set = serde_json::Map::new();
            set.insert("decided_by".into(), json!(actor));
            store.transition(&id, 1, "leased", "decided", actor, 1005.0 + i as f64, None, Some(set), Some(actor)).unwrap().unwrap();
        }
    }

    #[test]
    fn learn_proposes_once_and_only_when_something_changed() {
        let store = MemoryQueueStore::new();
        let cfg = RoutingConfig::default();
        assert!(learn(&store, &cfg, 10.0).unwrap().is_none(), "nothing decided yet");
        fill(&store, "R006", &["U1", "U2", "U3", "U4"], 40, 6, 0);
        let first = learn(&store, &cfg, 11.0).unwrap().expect("a proposal");
        assert_eq!((first["version"].as_i64(), first["status"].as_str(), first["rules"]["R006"]["status"].as_str()), (Some(1), Some("proposed"), Some("doubtful")));
        assert!(first["replay"]["claims"].as_i64().unwrap() > 0 && first["replay"]["verdicts_unchanged"] == true);
        assert!(verify(&first).is_ok(), "the replay report is not part of what verify checks");
        assert!(learn(&store, &cfg, 12.0).unwrap().is_none(), "the same data again writes nothing");
        fill(&store, "R006", &["U1", "U2", "U3", "U4"], 4, 4, 100);
        let second = learn(&store, &cfg, 13.0).unwrap().expect("new decisions, new proposal");
        assert_eq!(second["version"], 2);
    }

    #[test]
    fn a_proposal_changes_nothing_until_it_is_activated_and_a_stale_one_cannot_be() {
        let store = MemoryQueueStore::new();
        let cfg = RoutingConfig::default();
        fill(&store, "R006", &["U1", "U2", "U3"], 30, 3, 0);
        learn(&store, &cfg, 1.0).unwrap().unwrap();
        assert!(active(&store).is_none(), "proposed is not active");
        activate(&store, 1, "CG-4004", Some("CG-4005"), 2.0).unwrap();
        assert_eq!(active(&store).unwrap()["version"], 1);
        assert!(activate(&store, 1, "CG-4004", Some("CG-4005"), 3.0).is_err(), "only a proposed snapshot can be activated");
        fill(&store, "R006", &["U1", "U2", "U3"], 6, 6, 200);
        learn(&store, &cfg, 4.0).unwrap().unwrap();
        fill(&store, "R006", &["U1", "U2", "U3"], 6, 6, 300);
        learn(&store, &cfg, 5.0).unwrap().unwrap(); // v3
        activate(&store, 3, "CG-4004", Some("CG-4005"), 6.0).unwrap();
        assert_eq!(active(&store).unwrap()["version"], 3);
        assert!(activate(&store, 2, "CG-4004", Some("CG-4005"), 7.0).is_err(), "an older proposal cannot replace a newer active one");
        let h = store.reliability_history().unwrap();
        assert_eq!(h[0]["status"], "superseded", "the previous active snapshot is marked superseded");
        reject(&store, 2, "CG-4005", 8.0, "not needed").unwrap();
        assert_eq!(store.reliability_history().unwrap()[1]["status"], "rejected");
    }

    #[test]
    fn the_replay_report_counts_senior_load_before_and_after_and_says_so_when_it_rises() {
        let store = MemoryQueueStore::new();
        fill(&store, "R006", &["U1", "U2", "U3", "U4"], 40, 6, 0);
        let counted = counted_decisions(&store.by_state("decided", 1000).unwrap());
        let snap = make_snapshot(&counted, &params(), 1, 0.0, 1);
        let rep = replay_report(&store, &snap, 500);
        assert_eq!((rep["claims"].as_i64(), rep["senior_before"].as_i64(), rep["senior_after"].as_i64(), rep["newly_senior"].as_i64()), (Some(40), Some(0), Some(40), Some(40)));
        assert!(rep["note"].as_str().unwrap().contains("senior"), "a rise in senior load is stated in words");
        assert_eq!(rep["verdicts_unchanged"], true);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p cg-queue reliability`
Expected: compile error (`learn`, `activate`, `reject`, `replay_report` missing).

- [ ] **Step 3: Implement** (append before the tests in `reliability.rs`)

```rust
const BIG: usize = 100_000;
pub const REPLAY_LIMIT: usize = 500;

/// What would change if this snapshot were routing: the latest claims, re-routed on their stored results. Reads only; the verdict hash is
/// compared claim by claim to show that no verdict moves.
pub fn replay_report(store: &dyn QueueStore, snap: &Value, limit: usize) -> Value {
    let mut docs: Vec<Value> = ["ready", "awaiting_countersign", "leased", "decided"].iter().flat_map(|s| store.by_state(s, BIG).unwrap_or_default()).collect();
    docs.sort_by(|a, b| b["receipt"]["created_at"].as_f64().partial_cmp(&a["receipt"]["created_at"].as_f64()).unwrap_or(Ordering::Equal).then_with(|| a["claim_id"].as_str().cmp(&b["claim_id"].as_str())));
    docs.truncate(limit);
    let (mut before, mut after, mut changed, mut newly, mut intact, mut unc) = (0i64, 0i64, 0i64, 0i64, true, 0.0f64);
    for d in &docs {
        let was = triage::eligibility(&d["results"]) == "decide_high";
        let mut receipt = d["receipt"].clone();
        receipt["eligibility"] = json!(triage::eligibility(&d["results"]));
        if apply(&mut receipt, &d["results"], snap).is_err() {
            continue;
        }
        let now = receipt["eligibility"] == "decide_high";
        before += i64::from(was);
        after += i64::from(now);
        changed += i64::from(was != now);
        newly += i64::from(!was && now);
        unc += receipt["uncertainty"].as_f64().unwrap_or(0.0);
        intact &= triage::result_hash(&d["results"]) == d["receipt"]["result_hash"].as_str().unwrap_or("");
    }
    let n = docs.len() as i64;
    let mut out = json!({"claims": n, "senior_before": before, "senior_after": after, "newly_senior": newly, "changed": changed,
        "senior_share_before": if n > 0 { before as f64 / n as f64 } else { 0.0 }, "senior_share_after": if n > 0 { after as f64 / n as f64 } else { 0.0 },
        "mean_uncertainty": if n > 0 { unc / n as f64 } else { 0.0 }, "verdicts_unchanged": intact});
    if after > before {
        out["note"] = json!(format!("Senior load rises from {before} to {after} of {n} claims under this snapshot. A lower doubtful_below, or leaving route_by_reliability off, limits this."));
    }
    out
}

fn fingerprint(snap: &Value) -> (Value, Value, BTreeMap<String, String>) {
    let statuses = snap["rules"].as_object().into_iter().flatten().map(|(k, r)| (k.clone(), r["status"].as_str().unwrap_or("").to_string())).collect();
    (snap["inputs_hash"].clone(), snap["parameters"].clone(), statuses)
}

/// Computes the next snapshot from the decided dossiers and stores it as a PROPOSAL (with its replay report). Returns None when nothing
/// has been decided, when the newest snapshot (whatever its status) already has the same inputs, parameters and statuses, or when another
/// learner stored the same version first. A proposal changes no routing: see `activate`.
pub fn learn(store: &dyn QueueStore, cfg: &RoutingConfig, now: f64) -> Result<Option<Value>, String> {
    let p = Params::from_config(cfg);
    let decided = store.by_state("decided", BIG).map_err(|e| e.to_string())?;
    let counted = counted_decisions(&decided);
    if counted.is_empty() {
        return Ok(None);
    }
    let history = store.reliability_history().map_err(|e| e.to_string())?;
    let version = history.last().and_then(|d| d["version"].as_i64()).unwrap_or(0) + 1;
    let mut snap = make_snapshot(&counted, &p, version, now, cfg.version);
    if history.last().is_some_and(|last| fingerprint(last) == fingerprint(&snap)) {
        return Ok(None);
    }
    snap["replay"] = replay_report(store, &snap, REPLAY_LIMIT);
    match store.put_reliability(&snap, version - 1) {
        Ok(true) => Ok(Some(snap)),
        Ok(false) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// Makes a proposal the one routing uses. The proposal must verify, still be `proposed`, and be newer than the active one; the previous
/// active snapshot is then marked superseded. `approver` is the second administrator (None when a lone administrator acts alone).
pub fn activate(store: &dyn QueueStore, version: i64, requester: &str, approver: Option<&str>, now: f64) -> Result<(), String> {
    let history = store.reliability_history().map_err(|e| e.to_string())?;
    let doc = history.iter().find(|d| d["version"].as_i64() == Some(version)).ok_or("unknown snapshot")?;
    if doc["status"] != "proposed" {
        return Err("only a proposed snapshot can be activated".into());
    }
    verify(doc)?;
    let previous = history.iter().filter(|d| d["status"] == "approved").filter_map(|d| d["version"].as_i64()).max();
    if previous.is_some_and(|v| v > version) {
        return Err("a newer snapshot is already active".into());
    }
    let detail = json!({"requested_by": requester, "approved_by": approver});
    store.set_reliability_status(version, "proposed", "approved", approver.unwrap_or(requester), now, Some(detail)).map_err(|e| e.to_string())?.ok_or("the proposal changed while it was being approved")?;
    if let Some(prev) = previous {
        let _ = store.set_reliability_status(prev, "approved", "superseded", approver.unwrap_or(requester), now, None); // best effort: `active` takes the highest approved version anyway
    }
    Ok(())
}

pub fn reject(store: &dyn QueueStore, version: i64, actor: &str, now: f64, note: &str) -> Result<(), String> {
    store.set_reliability_status(version, "proposed", "rejected", actor, now, Some(json!({"note": note.chars().take(200).collect::<String>()}))).map_err(|e| e.to_string())?.ok_or("only a proposed snapshot can be rejected")?;
    Ok(())
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p cg-queue reliability`
Expected: PASS. If `fill` fails on a transition (state names), read `states.rs::allowed_from` and use the same path `contract::ready()` uses.

- [ ] **Step 5: Commit**

```bash
git add rust/crates/cg-queue/src/reliability.rs
git commit -m "Reliability: learn (proposals), replay report, activation and rejection

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 6: Service, API routes, approval hook, scheduled job

**Files:**
- Modify: `rust/crates/cg-queue/src/service.rs`, `rust/crates/cg-api/src/lib.rs`, `rust/crates/cg-cli/src/boot.rs`

**Interfaces:**
- Consumes: Task 5 functions; `AccessService::gate_action`, `AccessService::set_action_hook`.
- Produces (service, on `QueueService`): `reliability(&self, &Principal) -> R<Value>` (`queue.view`); `learn_now(&self, &Principal) -> R<Value>` (`routing.manage`); `verify_reliability(&self, &Principal, version: i64) -> R<Value>` (`queue.view`); `prepare_activation(&self, &Principal, version: i64) -> R<String>` (`routing.manage`; returns the approval summary); `activate_reliability(&self, &Principal, version: i64, approver: Option<&str>) -> R<()>`.
- Routes: `GET /api/v1/queue/reliability`, `POST /api/v1/queue/reliability/learn`, `GET /api/v1/queue/reliability/{v}/verify`, `POST /api/v1/queue/reliability/{v}/activate` (`{status: applied}` 200 or `{status: pending_approval}` 202).
- Inbox view gains `"attention": [...]`.

- [ ] **Step 1: Write the failing service tests** in `tests/service_flow.rs`:

```rust
#[test]
fn learning_is_a_proposal_until_an_administrator_activates_it_and_reviewers_see_only_words() {
    let q = q();
    let admin = q.p("CG-4004");
    let senior = q.p("CG-3003");
    let junior = q.p("CG-2002");
    assert!(matches!(q.queue.reliability(&junior), Err(QueueError::Forbidden(_))));
    assert!(matches!(q.queue.learn_now(&senior), Err(QueueError::Forbidden(_))), "deciding people may not trigger learning");
    assert_eq!(q.queue.learn_now(&admin).unwrap()["proposed"], Value::Null, "nothing decided yet");
    // hide-not-disable: a reviewer's inbox carries reasons as words only
    let id = q.ready("single_high");
    q.lease_to(&id, "CG-3003");
    let view = q.queue.inbox(&senior).unwrap();
    assert_eq!(view[0]["attention"], json!(["high_severity"]));
    let text = view[0].to_string();
    assert!(!text.contains("reliability") && !text.contains("uncertainty") && !text.contains("doubtful"), "no reliability numbers reach a reviewer");
    assert!(matches!(q.queue.prepare_activation(&admin, 9), Err(QueueError::NotFound(_))));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p cg-queue --test service_flow learning_is`
Expected: compile error.

- [ ] **Step 3: Implement the service methods.** In `service.rs`: add `use crate::reliability;`. In `view`, after the `view` object is built and before the green-claim block add `view["attention"] = json!(reliability::attention(doc));`. Add the methods in `impl QueueService`:

```rust
    // ---- learning from overrides ---------------------------------------------------------------------------------------
    /// The active snapshot's figures, the newest proposal with its replay report, and the history. Administrators only; reviewers never get this.
    pub fn reliability(&self, principal: &Principal) -> R<Value> {
        self.require(principal, "queue.view")?;
        let history = self.store.reliability_history().map_err(store_err)?;
        let active = reliability::active(&*self.store);
        let latest = history.last().cloned();
        let summary: Vec<Value> = history.iter().rev().take(20).map(|d| json!({"version": d["version"], "status": d["status"], "created_at": d["created_at"], "decisions": d["decisions"],
            "doubtful": reliability::doubtful_rules(d), "history": d["history"]})).collect();
        Ok(json!({"active": active.as_ref().map(|a| json!({"version": a["version"], "rules": a["rules"], "parameters": a["parameters"], "verified": reliability::verify(a).is_ok()})),
                  "latest": latest.as_ref().map(|l| json!({"version": l["version"], "status": l["status"], "rules": l["rules"], "replay": l["replay"], "verified": reliability::verify(l).is_ok()})),
                  "history": summary,
                  "settings": {"learn_enabled": self.routing_config().learn_enabled, "route_by_reliability": self.routing_config().route_by_reliability}}))
    }

    pub fn learn_now(&self, principal: &Principal) -> R<Value> {
        self.require(principal, "routing.manage")?;
        let proposed = reliability::learn(&*self.store, &self.routing_config(), self.now()).map_err(QueueError::Invalid)?;
        if let Some(p) = &proposed {
            self.access.record("reliability_proposed", json!({"actor": principal.badge(), "version": p["version"], "decisions": p["decisions"], "doubtful": reliability::doubtful_rules(p)}))?;
        }
        Ok(json!({"proposed": proposed.map(|p| p["version"].clone())}))
    }

    pub fn verify_reliability(&self, principal: &Principal, version: i64) -> R<Value> {
        self.require(principal, "queue.view")?;
        let snap = self.store.reliability_history().map_err(store_err)?.into_iter().find(|d| d["version"].as_i64() == Some(version)).ok_or_else(|| QueueError::NotFound(format!("reliability {version}")))?;
        let r = reliability::verify(&snap);
        let rules: Map<String, Value> = snap["rules"].as_object().into_iter().flatten().map(|(id, row)| {
            let p = reliability::Params::from_doc(&snap["parameters"]).ok();
            let (u, o, rv) = (row["upheld"].as_i64().unwrap_or(0), row["overridden"].as_i64().unwrap_or(0), row["reviewers"].as_i64().unwrap_or(0));
            let again = p.map(|p| reliability::row(u, o, rv, &p)).unwrap_or(Value::Null);
            (id.clone(), json!({"stored": row, "recomputed": again}))
        }).collect();
        Ok(json!({"version": version, "ok": r.is_ok(), "problem": r.err(), "rules": rules}))
    }

    /// Checks that a proposal can be activated and returns the sentence the second administrator will read.
    pub fn prepare_activation(&self, principal: &Principal, version: i64) -> R<String> {
        self.require(principal, "routing.manage")?;
        let snap = self.store.reliability_history().map_err(store_err)?.into_iter().find(|d| d["version"].as_i64() == Some(version)).ok_or_else(|| QueueError::NotFound(format!("reliability {version}")))?;
        if snap["status"] != "proposed" {
            return Err(QueueError::Conflict("not_proposed".into()));
        }
        reliability::verify(&snap).map_err(QueueError::Invalid)?;
        let doubtful: Vec<String> = reliability::doubtful_rules(&snap).into_iter().collect();
        let rep = &snap["replay"];
        Ok(format!("Use learned reliability v{version} for routing: doubtful rules [{}]; of {} recent claims, senior share {:.0}% -> {:.0}%{}.",
            doubtful.join(", "), rep["claims"], 100.0 * rep["senior_share_before"].as_f64().unwrap_or(0.0), 100.0 * rep["senior_share_after"].as_f64().unwrap_or(0.0),
            if rep["note"].is_string() { " (senior load rises)" } else { "" }))
    }

    pub fn activate_reliability(&self, principal: &Principal, version: i64, approver: Option<&str>) -> R<()> {
        self.require(principal, "routing.manage")?;
        reliability::activate(&*self.store, version, principal.badge(), approver, self.now()).map_err(QueueError::Invalid)?;
        self.access.record("reliability_activated", json!({"actor": principal.badge(), "version": version, "approver": approver}))?;
        Ok(())
    }
```

`R<T>` is the module's `Result<T, QueueError>` alias (it is used throughout the file). The approval hook (below) cannot pass a `Principal`, so it calls `reliability::activate` and `access.record` directly.

- [ ] **Step 4: Run service test**

Run: `cargo test -p cg-queue --test service_flow learning_is`
Expected: PASS. If an existing view test compares the whole inbox JSON, add `"attention"` to its expectation (deliberate shape change).

- [ ] **Step 5: API routes + tests.** In `cg-api/src/lib.rs` `route`, next to the `queue/feedback` line add:

```rust
            ["queue", "reliability"] if self.queue.is_some() => if m == "GET" { self.q_reliability(req) } else { Err(wrong()) },
            ["queue", "reliability", "learn"] if self.queue.is_some() => if m == "POST" { self.q_reliability_learn(req) } else { Err(wrong()) },
            ["queue", "reliability", v, "verify"] if self.queue.is_some() => if m == "GET" { self.q_reliability_verify(req, v) } else { Err(wrong()) },
            ["queue", "reliability", v, "activate"] if self.queue.is_some() => if m == "POST" { self.q_reliability_activate(req, v) } else { Err(wrong()) },
```

and the handlers next to `q_feedback`:

```rust
    fn version_arg(v: &str) -> Result<i64, ApiError> {
        v.parse::<i64>().ok().filter(|n| (1..=1_000_000).contains(n)).ok_or_else(|| ApiError::new(404, "not_found"))
    }

    fn q_reliability(&self, req: &ApiRequest) -> R {
        let p = self.need(req, "queue.view")?;
        self.queue()?.reliability(&p).map(|v| ApiResponse::json(200, &v)).map_err(|e| self.qerr(e, req, &p))
    }

    fn q_reliability_learn(&self, req: &ApiRequest) -> R {
        let p = self.need(req, "routing.manage")?;
        self.queue()?.learn_now(&p).map(|v| ApiResponse::json(200, &v)).map_err(|e| self.qerr(e, req, &p))
    }

    fn q_reliability_verify(&self, req: &ApiRequest, v: &str) -> R {
        let p = self.need(req, "queue.view")?;
        self.queue()?.verify_reliability(&p, Self::version_arg(v)?).map(|r| ApiResponse::json(200, &r)).map_err(|e| self.qerr(e, req, &p))
    }

    /// Asking to use a learned snapshot is a dangerous administrator action: with two or more administrators a different one must approve it
    /// (the same gate as the payer rules); a lone administrator acts alone and the security log says so.
    fn q_reliability_activate(&self, req: &ApiRequest, v: &str) -> R {
        let p = self.need(req, "routing.manage")?;
        let version = Self::version_arg(v)?;
        let q = self.queue()?;
        let summary = q.prepare_activation(&p, version).map_err(|e| self.qerr(e, req, &p))?;
        match self.access.gate_action(&p, "learning_snapshot", &format!("reliability-v{version}"), summary, json!({"version": version})).map_err(|e| self.serr(e, req, &p))? {
            Gate::Pending { approval, uri } => Ok(Self::pending(&approval, uri)),
            Gate::Done(()) => {
                q.activate_reliability(&p, version, None).map_err(|e| self.qerr(e, req, &p))?;
                Ok(ApiResponse::json(200, &json!({"status": "applied", "version": version})))
            }
        }
    }
```

In `q_patch_config`'s `ALLOWED` list the seven names were added in Task 1; also make the value type check allow booleans and floats (it only rejects `null`, so no change). Tests in `cg-api/tests/api_flow.rs`: copy the pattern of the existing `queue/feedback` permission test and add: level 2 and level 3 get 403 on all four routes; level 4 gets 200 on `GET`/`learn`; `activate` on an unknown version is 404; `activate` with one administrator returns 200 `applied`; with two active administrators (use the helper that creates a second admin, as the payer-rules approval test does) returns 202 `pending_approval`, and the second administrator's `POST /approvals/{id}/approve` makes `active` return that version after Step 6 below is in place.

- [ ] **Step 6: Approval hook and the scheduled job.** In `boot.rs` the hook is installed before the queue exists. Move the `access.set_action_hook(...)` block (and keep the payer refresh thread where it is) to just after `let queue = match queue_store {...};`, and make it dispatch on the action:

```rust
    let learning_store = queue.as_ref().map(|q| q.store.clone());
    {
        let pr = payer.clone();
        let log_access = access.clone();
        access.set_action_hook(Arc::new(move |a, requester, approver| match a.action.as_str() {
            "payer_rules" => {
                let p: Value = serde_json::from_str(&a.payload).map_err(|e| e.to_string())?;
                pr.apply(&p["policies"], requester, Some(approver), p["reason"].as_str().unwrap_or("")).map(|_| ())
            }
            "learning_snapshot" => {
                let store = learning_store.as_ref().ok_or("the queue is not running in this process")?;
                let p: Value = serde_json::from_str(&a.payload).map_err(|e| e.to_string())?;
                let version = p["version"].as_i64().ok_or("the request has no version")?;
                cg_queue::reliability::activate(&**store, version, requester, Some(approver), log_access.now())?;
                log_access.record("reliability_activated", serde_json::json!({"actor": requester, "version": version, "approver": approver})).map_err(|e| e.to_string())
            }
            _ => Err("unknown action".into()),
        }));
    }
```

(`QueueStack` field names: use whatever `build_queue` returns for its store; `grep -n "pub struct QueueStack" -A12 rust/crates/cg-cli/src/boot.rs`. If `AccessService::now` is private, use the `clock` already in scope in `boot`.)

Scheduled job: in the worker loop table (`let every = [("relay", 1.5), ...]` in `boot.rs` ~line 248) add `("learn", 600.0)` and a branch next to the `reconcile` one:

```rust
                        "learn" => {
                            let cfg = flow::routing_config(store);
                            if cfg.learn_enabled {
                                run_job(store, clock, "learn", || cg_queue::reliability::learn(store, &cfg, clock()).map(|p| p.map(|s| s["version"].clone())), |r| format!("{r:?}"));
                            }
                        }
```

(match the exact shape of the neighbouring branches for how `run_job` takes its closures; `cg-api/src/ops.rs:319` `EXPECTED_EVERY` lists the jobs the health panel expects; do not add `learn` there because it only runs when enabled.)

- [ ] **Step 7: Run**

Run: `cargo test -p cg-queue -p cg-api -p cg-cli`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add rust/crates
git commit -m "Learning service, API routes, two-administrator activation, scheduled learn job

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 7: Operator commands and the Python cross-check

**Files:**
- Modify: `rust/crates/cg-cli/src/tools.rs`, `rust/crates/cg-cli/src/main.rs`
- Create: `rust/tools/compare_reliability.py`

**Interfaces:**
- Produces: `cg queue --badge CG-4004 [--dev] learn [--apply]` (dry run prints the proposal; `--apply` stores it), `reliability`, `verify-reliability V`, `replay-reliability V`, `activate-reliability V`; `cg xcheck` op `clopper_pearson` (`{k, n}` -> `[lower, upper]`).

- [ ] **Step 1: Add the xcheck op.** In `main.rs` `xcheck`'s match add:

```rust
            "clopper_pearson" => json!(cg_queue::shadow::clopper_pearson(req["k"].as_i64().unwrap_or(0), req["n"].as_i64().unwrap_or(0), 0.95).map(|(l, u)| [l, u])),
```

(`cg-cli` already depends on `cg-queue`.)

- [ ] **Step 2: Write the comparison tool** `rust/tools/compare_reliability.py`:

```python
"""Checks the Rust exact interval against the Python oracle (src/workqueue/shadow.py) on a few thousand (k, n) pairs."""
import json, os, random, subprocess, sys

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
sys.path.insert(0, ROOT)
from src.workqueue.shadow import clopper_pearson  # noqa: E402

CG = os.environ.get("CG_BIN", os.path.join(ROOT, "rust", "target", "debug", "cg.exe"))


def main() -> int:
    rng = random.Random(11)
    cases = [(k, n) for n in (1, 2, 5, 10, 20, 40, 100) for k in range(0, n + 1)]
    cases += [(rng.randint(0, n), n) for n in (rng.randint(1, 400) for _ in range(1500))]
    proc = subprocess.run([CG, "xcheck"], input="".join(json.dumps({"op": "clopper_pearson", "k": k, "n": n}) + "\n" for k, n in cases), capture_output=True, text=True, check=True)
    rows = [json.loads(line) for line in proc.stdout.splitlines()]
    worst = 0.0
    for (k, n), got in zip(cases, rows):
        want = clopper_pearson(k, n, 0.95)
        assert got is not None and want is not None, (k, n)
        worst = max(worst, abs(got[0] - want[0]), abs(got[1] - want[1]))
    print(f"{len(cases)} cases, largest difference {worst:.2e}")
    return 0 if worst < 1e-6 else 1


if __name__ == "__main__":
    raise SystemExit(main())
```

Check the signature in `src/workqueue/shadow.py` first (`grep -n "def clopper_pearson" src/workqueue/shadow.py`) and adapt the call if its argument order or return type differs.

- [ ] **Step 3: Add the commands.** In `tools.rs`: extend `QUEUE_USAGE` with `| learn [--apply] | reliability | verify-reliability V | replay-reliability V | activate-reliability V`; add the five names to the `position(...)` list at line ~281; mutating = `learn --apply` and `activate-reliability` (require `routing.manage`), the rest `queue.view`. Update the `mutating` expression to `matches!(command, "submit" | "deadletter-replay") || (command == "rerun" && apply) || (command == "learn" && apply) || command == "activate-reliability"`. Add match arms in the command block:

```rust
            "learn" => {
                let cfg = cg_queue::flow::routing_config(store);
                if apply {
                    say(&sorted(&json!({"proposed": cg_queue::reliability::learn(store, &cfg, now()).map_err(QueueError::Invalid)?.map(|p| p["version"].clone())})));
                } else {
                    let counted = cg_queue::reliability::counted_decisions(&store.by_state("decided", 100_000).map_err(|e| QueueError::Invalid(e.to_string()))?);
                    say(&sorted(&cg_queue::reliability::make_snapshot(&counted, &cg_queue::reliability::Params::from_config(&cfg), 0, now(), cfg.version)));
                }
                Ok(0)
            }
            "reliability" => {
                say(&sorted(&q.service.reliability(&operator)?));
                Ok(0)
            }
            "verify-reliability" => {
                let v: i64 = positional.and_then(|s| s.parse().ok()).ok_or_else(|| QueueError::Invalid("verify-reliability needs a version".into()))?;
                let r = q.service.verify_reliability(&operator, v)?;
                say(&sorted(&r));
                Ok(if r["ok"] == true { 0 } else { 1 })
            }
            "replay-reliability" => {
                let v: i64 = positional.and_then(|s| s.parse().ok()).ok_or_else(|| QueueError::Invalid("replay-reliability needs a version".into()))?;
                let snap = store.reliability_history().map_err(|e| QueueError::Invalid(e.to_string()))?.into_iter().find(|d| d["version"].as_i64() == Some(v)).ok_or_else(|| QueueError::NotFound(format!("reliability {v}")))?;
                say(&sorted(&cg_queue::reliability::replay_report(store, &snap, cg_queue::reliability::REPLAY_LIMIT)));
                Ok(0)
            }
            "activate-reliability" => {
                let v: i64 = positional.and_then(|s| s.parse().ok()).ok_or_else(|| QueueError::Invalid("activate-reliability needs a version".into()))?;
                q.service.prepare_activation(&operator, v)?;
                match b.access.gate_action(&operator, "learning_snapshot", &format!("reliability-v{v}"), format!("Use learned reliability v{v} for routing"), json!({"version": v})).map_err(|e| QueueError::Invalid(e.to_string()))? {
                    cg_access::service::Gate::Pending { approval, .. } => say(&format!("waiting for a second administrator: approval {}", approval.id)),
                    cg_access::service::Gate::Done(()) => {
                        q.service.activate_reliability(&operator, v, None)?;
                        say("activated");
                    }
                }
                Ok(0)
            }
```

Use the variable names that already exist in that function (`q` is the queue stack; check whether the service is `q.service`; `sorted` and `say` exist already).

- [ ] **Step 4: Build and run the cross-check**

Run: `cargo build -p cg-cli && python tools/compare_reliability.py` (from `rust/`)
Expected: `... cases, largest difference` below `1e-6`, exit 0.

- [ ] **Step 5: Commit**

```bash
git add rust/crates/cg-cli rust/tools/compare_reliability.py
git commit -m "cg queue: learn, reliability, verify, replay, activate; Python cross-check of the interval

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 8: The simulated review database (`cg-sim`)

**Files:**
- Create: `rust/crates/cg-sim/Cargo.toml`, `src/lib.rs`, `src/personas.rs`, `src/generate.rs`
- Modify: `rust/Cargo.toml` (add `"crates/cg-sim"` to `members`)

**Interfaces:**
- Consumes: `QueueStore`, `triage::make_receipt`, `reliability::*`, `RoutingConfig`.
- Produces: `personas::{Persona, default_reviewers(), truth() -> BTreeMap<&'static str, f64>}`; `generate::{Corpus, load_corpus(root) -> Corpus, Lab::new(seed), Lab::run(store, &Corpus, decisions_target, &RoutingConfig, route: Option<&Value /*snapshot*/>) -> Summary}`.

- [ ] **Step 1: Crate skeleton.** `rust/crates/cg-sim/Cargo.toml`:

```toml
[package]
name = "cg-sim"
version.workspace = true
edition.workspace = true

[dependencies]
cg-queue = { path = "../cg-queue" }
serde.workspace = true
serde_json.workspace = true
rand = "0.8"
rand_chacha = "0.3"

[dev-dependencies]
tempfile = "3"
```

`src/lib.rs`: `pub mod personas; pub mod generate; pub mod experiments;` (experiments added in Task 9; create it empty now: `//! Experiments (Task 9).`).

- [ ] **Step 2: Write the failing tests** in `src/generate.rs` (`#[cfg(test)]`), after writing `personas.rs`:

`personas.rs`:

```rust
//! The simulated reviewers. Each rule has a hidden true upheld rate; a reviewer's answer is the truth plus that person's bias and noise.
//! Everything here is invented for a demonstration: no real reviewer behaves like this.

use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct Persona {
    pub badge: String,
    pub senior: bool,
    /// Added to the probability of confirming (negative: dismisses more).
    pub bias: f64,
    /// Chance of answering at random instead of by the truth.
    pub noise: f64,
    /// Dismisses everything.
    pub adversarial: bool,
}

/// The hidden truth: most rules are upheld about 97 % of the time; R006 only 35 % and R013 50 %.
pub fn truth() -> BTreeMap<&'static str, f64> {
    let mut t: BTreeMap<&'static str, f64> = (1..=15).map(|i| (Box::leak(format!("R{i:03}").into_boxed_str()) as &'static str, 0.97)).collect();
    t.insert("R006", 0.35);
    t.insert("R013", 0.50);
    t
}

/// Twelve people: eight juniors (careful, lenient, noisy, slow) and four seniors, one of whom can be switched to the adversarial persona.
pub fn default_reviewers(with_adversary: bool) -> Vec<Persona> {
    let mut v = vec![];
    for i in 0..8 {
        let (bias, noise) = match i % 4 { 0 => (0.0, 0.02), 1 => (0.05, 0.03), 2 => (-0.05, 0.03), _ => (0.0, 0.15) };
        v.push(Persona { badge: format!("SIM-2{i:03}"), senior: false, bias, noise, adversarial: false });
    }
    for i in 0..4 {
        v.push(Persona { badge: format!("SIM-3{i:03}"), senior: true, bias: 0.0, noise: 0.02, adversarial: with_adversary && i == 0 });
    }
    v
}
```

Because `truth()` leaks strings, replace the `Box::leak` construction with a plain list of the fifteen static ids if the reviewer prefers: `const RULES: [&str; 15] = ["R001", ... "R015"]` and build the map from it. Do that instead (simpler, no leak).

`generate.rs` tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use cg_queue::store::MemoryQueueStore;

    fn root() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
    }

    #[test]
    fn the_generator_is_deterministic_and_every_record_is_marked_simulated() {
        let corpus = load_corpus(&root()).unwrap();
        let run = |seed| {
            let store = MemoryQueueStore::new();
            let s = Lab::new(seed, false).run(&store, &corpus, 600, &RoutingConfig::default(), None).unwrap();
            let decided = store.by_state("decided", 100_000).unwrap();
            (s, decided)
        };
        let (a, da) = run(5);
        let (b, db) = run(5);
        assert_eq!(a.decisions, b.decisions);
        assert_eq!(da.iter().map(|d| d["decisions"].clone()).collect::<Vec<_>>(), db.iter().map(|d| d["decisions"].clone()).collect::<Vec<_>>(), "same seed, same decisions");
        assert!(a.decisions >= 600);
        assert!(da.iter().all(|d| d["claim"]["simulated"] == true && d["decisions"].as_array().unwrap().iter().all(|x| x["simulated"] == true)));
        let (c, _) = run(6);
        assert_ne!(a.fingerprint, c.fingerprint, "a different seed gives different decisions");
    }

    #[test]
    fn reviewers_follow_the_hidden_truth_and_the_learner_recovers_it() {
        let corpus = load_corpus(&root()).unwrap();
        let store = MemoryQueueStore::new();
        Lab::new(1, false).run(&store, &corpus, 4000, &RoutingConfig::default(), None).unwrap();
        let counted = reliability::counted_decisions(&store.by_state("decided", 100_000).unwrap());
        let snap = reliability::make_snapshot(&counted, &reliability::Params::from_config(&RoutingConfig::default()), 1, 0.0, 1);
        let r006 = &snap["rules"]["R006"];
        assert!(r006["lower"].as_f64().unwrap() < 0.35 && 0.35 < r006["upper"].as_f64().unwrap(), "the interval for R006 contains the hidden 0.35: {r006}");
        assert_eq!(r006["status"], "doubtful");
        assert_ne!(snap["rules"]["R001"]["status"], "doubtful");
    }

    #[test]
    fn juniors_never_decide_a_claim_that_needs_a_senior() {
        let corpus = load_corpus(&root()).unwrap();
        let store = MemoryQueueStore::new();
        Lab::new(2, false).run(&store, &corpus, 800, &RoutingConfig::default(), None).unwrap();
        for d in store.by_state("decided", 100_000).unwrap() {
            if d["receipt"]["eligibility"] == "decide_high" {
                assert!(d["decided_by"].as_str().unwrap().starts_with("SIM-3"), "{}", d["claim_id"]);
            }
        }
    }
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p cg-sim`
Expected: compile errors.

- [ ] **Step 4: Implement `generate.rs`.**

```rust
//! Builds the simulated review database. The claims and their findings are real (data/development: the claim and the engine's expected
//! results); only the people are invented. The 400 claims carry 499 flagged findings, so claims are recycled under new ids to reach a few
//! thousand decisions. Recycled claims repeat their findings: that is stated in the evidence, and it is why this shows a mechanism, not accuracy.

use crate::personas::{default_reviewers, truth, Persona};
use cg_queue::reliability;
use cg_queue::routing_config::RoutingConfig;
use cg_queue::store::QueueStore;
use cg_queue::triage;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;

pub struct Corpus {
    /// (claim, 15 result rows) for every claim that has at least one flagged finding.
    pub items: Vec<(Value, Value)>,
}

pub fn load_corpus(root: &Path) -> Result<Corpus, String> {
    let read = |name: &str| -> Result<Vec<Value>, String> {
        std::fs::read_to_string(root.join("data/development").join(name)).map_err(|e| e.to_string())?.lines().filter(|l| !l.trim().is_empty()).map(|l| serde_json::from_str(l).map_err(|e| e.to_string())).collect()
    };
    let (claims, results) = (read("claims.jsonl")?, read("expected_results.jsonl")?);
    let mut by_claim: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for r in results {
        by_claim.entry(r["claim_id"].as_str().unwrap_or("").to_string()).or_default().push(r);
    }
    let mut items = vec![];
    for c in claims {
        let Some(rows) = by_claim.remove(c["claim_id"].as_str().unwrap_or("")) else { continue };
        let rows = Value::Array(rows);
        if !triage::flagged(&rows).is_empty() && !triage::degraded(&rows) {
            items.push((c, rows));
        }
    }
    if items.is_empty() {
        return Err("no flagged claims in data/development".into());
    }
    Ok(Corpus { items })
}

#[derive(Debug, Clone)]
pub struct Summary {
    pub claims: usize,
    pub decisions: usize,
    pub fingerprint: String,
}

pub struct Lab {
    pub seed: u64,
    pub reviewers: Vec<Persona>,
}

impl Lab {
    pub fn new(seed: u64, with_adversary: bool) -> Lab {
        Lab { seed, reviewers: default_reviewers(with_adversary) }
    }

    /// The simulated answer of one person to one finding: the truth (confirm with probability `truth[rule]`), shifted by their bias, or a coin flip.
    fn answer(&self, rng: &mut ChaCha8Rng, who: &Persona, rule: &str) -> bool {
        if who.adversarial {
            return false;
        }
        if rng.gen_bool(who.noise.clamp(0.0, 1.0)) {
            return rng.gen_bool(0.5);
        }
        rng.gen_bool((truth().get(rule).copied().unwrap_or(0.9) + who.bias).clamp(0.0, 1.0))
    }

    /// Feeds claims until at least `target` decisions exist. Each claim gets a receipt (with `snapshot` applied when given, which is how routing
    /// changes who may take it), then is leased to a simulated reviewer allowed to take it, decided finding by finding, and marked decided.
    pub fn run(&self, store: &dyn QueueStore, corpus: &Corpus, target: usize, cfg: &RoutingConfig, snapshot: Option<&Value>) -> Result<Summary, String> {
        let mut rng = ChaCha8Rng::seed_from_u64(self.seed);
        let (mut decisions, mut claims, mut t) = (0usize, 0usize, 1_000.0f64);
        let mut trail = String::new();
        while decisions < target {
            let (base, results) = &corpus.items[claims % corpus.items.len()];
            let id = format!("SIM-{:06}", claims);
            let mut claim = base.clone();
            claim["claim_id"] = json!(id);
            claim["simulated"] = json!(true);
            let mut receipt = triage::make_receipt(&claim, results, cfg, "sim-pack", "sim-engine", t);
            if let Some(s) = snapshot {
                reliability::apply(&mut receipt, results, s)?;
            }
            let doc = json!({"claim_id": id, "version": 1, "input_hash": triage::input_hash(&claim), "claim": claim, "results": results, "receipt": receipt});
            if !store.put_triaged(&doc).map_err(|e| e.to_string())? {
                return Err(format!("{id} was stored before"));
            }
            for (from, to) in [("triaged", "explanation_skipped"), ("explanation_skipped", "ready")] {
                t += 0.001;
                store.transition(&id, 1, from, to, "system:sim", t, None, None, None).map_err(|e| e.to_string())?.ok_or("transition refused")?;
            }
            let senior_needed = receipt["eligibility"] == "decide_high";
            let pool: Vec<&Persona> = self.reviewers.iter().filter(|p| p.senior || !senior_needed).collect();
            let who = pool[rng.gen_range(0..pool.len())];
            t += 0.001;
            store.lease(&id, 1, &who.badge, t, t + 1800.0).map_err(|e| e.to_string())?.ok_or("lease refused")?;
            for f in triage::flagged(results) {
                let rule = f["rule_id"].as_str().unwrap_or("");
                let upheld = self.answer(&mut rng, who, rule);
                t += 0.001;
                let d = json!({"rule_id": rule, "action": if upheld { "confirm_issue" } else { "dismiss_with_reason" }, "actor": who.badge, "reason": if upheld { "" } else { "simulated dismissal" }, "at": t, "round": 1, "simulated": true});
                store.add_decision(&id, 1, &who.badge, t, &d).map_err(|e| e.to_string())?.ok_or("decision refused")?;
                trail.push_str(&format!("{id}|{rule}|{}|{upheld};", who.badge));
                decisions += 1;
            }
            let mut set = serde_json::Map::new();
            set.insert("decided_by".into(), json!(who.badge));
            t += 0.001;
            store.transition(&id, 1, "leased", "decided", &who.badge, t, None, Some(set), Some(&who.badge)).map_err(|e| e.to_string())?.ok_or("finish refused")?;
            claims += 1;
        }
        Ok(Summary { claims, decisions, fingerprint: cg_queue::reliability::hash_text(&trail) })
    }
}
```

Add to `reliability.rs`: `pub fn hash_text(s: &str) -> String { hex::encode(Sha256::digest(s.as_bytes())) }` (and use it in `inputs_hash`). The `add_decision` store check requires `decision` to pass `states::check_detail` (flat, shallow): the object above is flat. The claim is stamped `simulated: true` and `check_decision` is not applied to claims.

- [ ] **Step 5: Run tests**

Run: `cargo test -p cg-sim`
Expected: 3 PASS. If `the learner recovers the hidden truth` is flaky on the R006 interval, raise the target decisions rather than widening the assertion. R006 has 32 flagged findings in 400 claims, so 4000 decisions recycle each claim about 8 times and give R006 roughly 260 decisions; the interval half-width is about 0.06, so 0.35 should be inside it.

- [ ] **Step 6: Commit**

```bash
git add rust/Cargo.toml rust/Cargo.lock rust/crates/cg-sim rust/crates/cg-queue/src/reliability.rs
git commit -m "cg-sim: seeded simulated reviewers and review database (real findings, invented people)

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 9: Experiments and the evidence file

**Files:**
- Create: `rust/crates/cg-sim/src/experiments.rs`
- Modify: `rust/crates/cg-cli/src/main.rs`, `rust/crates/cg-cli/Cargo.toml` (add `cg-sim` dependency), new `rust/crates/cg-cli/src/sim_cmd.rs`

**Interfaces:**
- Produces: `experiments::{convergence, routing_effect, active_learning, poisoning, invariants, all}` each returning `serde_json::Value`; `cg sim make-reviews --out DIR [--seed N] [--decisions N]`, `cg sim experiments [--out FILE]`, `cg sim fast-forward ...` (Task 10).

- [ ] **Step 1: Write the failing tests** (in `experiments.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
    }

    #[test]
    fn r006_becomes_doubtful_after_a_stated_number_of_decisions_and_never_before_the_minimum() {
        let c = convergence(&root(), 1).unwrap();
        let first = c["r006_first_doubtful_at_decisions"].as_i64().expect("R006 becomes doubtful");
        assert!(first >= 20, "never before the minimum number of decisions: {first}");
        assert!(c["rules_wrongly_doubtful"].as_array().unwrap().is_empty(), "no rule with a true rate of 0.97 is ever marked doubtful: {c}");
    }

    #[test]
    fn routing_on_changes_who_reviews_but_never_a_verdict_and_reports_senior_load() {
        let r = routing_effect(&root(), 1).unwrap();
        assert_eq!(r["verdict_hashes_equal"], true);
        assert!(r["senior_share_on"].as_f64().unwrap() >= r["senior_share_off"].as_f64().unwrap());
        assert!(r["senior_share_off"].is_number() && r["senior_queue_on"].is_number());
    }

    #[test]
    fn one_adversarial_reviewer_is_stopped_by_the_cap_and_not_without_it() {
        let p = poisoning(&root(), 1).unwrap();
        assert_eq!(p["with_cap_and_minimum"]["wrongly_doubtful"].as_array().unwrap().len(), 0, "{p}");
        assert!(p["without_protection"]["wrongly_doubtful"].as_array().unwrap().len() > 0, "the attack must succeed without the protections, or the demo proves nothing: {p}");
    }

    #[test]
    fn every_invariant_holds() {
        let i = invariants(&root()).unwrap();
        for (k, v) in i.as_object().unwrap() {
            assert_eq!(v, &json!(true), "{k}");
        }
    }
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test -p cg-sim experiments` (compile error).

- [ ] **Step 3: Implement.** Design of each function (all seeded, all return plain JSON and write nothing):

  - `convergence(root, seed)`: run `Lab::new(seed,false)` in batches of 100 decisions into one store by calling `run` repeatedly with increasing `target` on a store (note `run` restarts claim numbering at 0, so add `Lab::run_from(store, corpus, start_claim_index, target_additional, ...)`: give `run` a `start: usize` parameter and have `run` pass 0; the loop uses `start + claims` for the id and `claims % len` for the corpus index offset by `start`). After each batch, `make_snapshot` from `counted_decisions` with default `Params`; record per rule `(decisions n, rate, lower, upper, status)` at checkpoints 100, 200, 500, 1000, 2000, 4000; fields: `curve` (list of `{decisions, R006:{rate,lower,upper,status}, R013:{...}, R001:{...}}`), `r006_first_doubtful_at_decisions` (counted decisions of the whole store when R006 first reads doubtful), `rules_wrongly_doubtful` (every rule with hidden truth ≥ 0.9 that is ever `doubtful` in any checkpoint; expected `[]`), `hidden_truth` (the table), `note: "simulated reviewers; shows the mechanism, not real-world accuracy"`.
  - `routing_effect(root, seed)`: (1) store A: 2000 decisions with `None` snapshot, learn a snapshot S from it (`make_snapshot`, mark `approved`); (2) store B and store C: the same 1500 further decisions with the same seed, B with `None`, C with `Some(&S)` and `route_by_reliability` irrelevant (the Lab applies the snapshot directly). Report `senior_share_off/on` = share of claims with eligibility `decide_high`, `senior_queue_off/on` = number of `decide_high` claims per senior reviewer (senior reviewers = 4, a rough load figure), `newly_senior`, and `verdict_hashes_equal` = for every claim id present in B and C, `triage::result_hash(results)` equal. Add `note` if on > off with the same wording as `replay_report`.
  - `active_learning(root, seed)`: fixed budget of 600 decisions starting from an empty store; strategy `arrival` (corpus order) vs `uncertainty_first` (at each batch of 50, order the remaining corpus items by the widest `width` among their flagged rules in the snapshot learned so far, widest first; items with unseen rules count as width 1.0). Report mean width over the 15 rules (rules with no row count as 1.0) and the number of decisions until every rule that has any flagged claim is `measured` or `doubtful` (or `null` if never within 4000). Word it in `note` as a demonstration, not a finding.
  - `poisoning(root, seed)`: `Lab::new(seed, true)` (one of four seniors adversarial) but the adversary is also given `weight`: make it decide 40 % of all claims by giving it extra weight in reviewer choice (`Lab.adversary_share: Option<f64>`; when set, with that probability the adversary is chosen if the claim allows seniors). Compute snapshots two ways from the same decisions: `without_protection` = `Params{reviewer_cap:1.0, min_reviewers:1, ..}`, `with_cap_and_minimum` = default `Params`. Report `wrongly_doubtful` = rules with hidden truth ≥ 0.9 that read `doubtful`, in each.
  - `invariants(root)`: object of booleans: `feature_off_receipts_equal_baseline` (for the first 200 corpus items, `make_receipt` output equals itself after the intake code path with `route_by_reliability=false`, i.e. no `reliability_version` key: build with `Lab::run(..., None)` and compare to a direct `make_receipt`), `tampered_snapshot_detected` (flip a status, `verify` errs), `replay_of_uncertainty_deal_matches` (build a `Dispatcher` over a `MemoryQueueStore` with 30 ready claims carrying an `uncertainty`, two agents, `uncertainty_weight=3`, `rng_seed=Some(9)`, call `deal(None,true)`, then `verify(deal_id)` returns `Ok(true)`; copy the construction from the dispatcher tests), `verdicts_unchanged_by_apply` (for 200 receipts, `result_hash`, `facts_hash`, `statuses`, `lane`, `score` identical before and after `apply`).
  - `all(root, seed)`: `{"simulated": true, "seed": seed, "decisions_note": "...", "convergence":..., "routing_effect":..., "active_learning":..., "poisoning":..., "invariants":...}`.

Implement them with the helper `snapshot_from(store, params) -> Value` (`make_snapshot(&counted_decisions(&store.by_state("decided",100000)), params, 1, 0.0, 1)`).

- [ ] **Step 4: `cg sim` subcommands.** In `main.rs` add `Some("sim") => std::process::exit(sim_cmd::run(&args[2..])),` and `mod sim_cmd;`. `sim_cmd.rs`:

```rust
//! `cg sim ...`: the simulated review database and its experiments (docs/47). Nothing here touches a real queue unless `fast-forward` is aimed at one on purpose.

use serde_json::json;
use std::path::PathBuf;

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

pub fn run(args: &[String]) -> i32 {
    let root = match crate::boot::repo_root() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let seed: u64 = flag(args, "--seed").and_then(|s| s.parse().ok()).unwrap_or(1);
    match args.first().map(String::as_str) {
        Some("make-reviews") => {
            let out = PathBuf::from(flag(args, "--out").unwrap_or_else(|| "outputs/sim".into()));
            let target: usize = flag(args, "--decisions").and_then(|s| s.parse().ok()).unwrap_or(5000);
            let corpus = match cg_sim::generate::load_corpus(&root) { Ok(c) => c, Err(e) => { eprintln!("error: {e}"); return 2; } };
            let store = cg_queue::store::MemoryQueueStore::new();
            let summary = match cg_sim::generate::Lab::new(seed, args.iter().any(|a| a == "--adversary")).run(&store, &corpus, target, &cg_queue::routing_config::RoutingConfig::default(), None) {
                Ok(s) => s,
                Err(e) => { eprintln!("error: {e}"); return 2; }
            };
            if std::fs::create_dir_all(&out).is_err() { return 2; }
            let docs = store.by_state("decided", 1_000_000).unwrap_or_default();
            let lines: Vec<String> = docs.iter().map(|d| d.to_string()).collect();
            if std::fs::write(out.join("decided.jsonl"), lines.join("\n") + "\n").is_err() { return 2; }
            println!("{}", json!({"claims": summary.claims, "decisions": summary.decisions, "fingerprint": summary.fingerprint, "simulated": true, "file": out.join("decided.jsonl").display().to_string()}));
            0
        }
        Some("experiments") => {
            let out = PathBuf::from(flag(args, "--out").unwrap_or_else(|| "outputs/defense/learning.json".into()));
            match cg_sim::experiments::all(&root, seed) {
                Ok(v) => {
                    if let Some(parent) = out.parent() { let _ = std::fs::create_dir_all(parent); }
                    if std::fs::write(&out, serde_json::to_string_pretty(&v).unwrap_or_default() + "\n").is_err() { return 2; }
                    println!("wrote {}", out.display());
                    0
                }
                Err(e) => { eprintln!("error: {e}"); 2 }
            }
        }
        _ => {
            println!("usage: cg sim make-reviews [--out DIR] [--seed N] [--decisions N] [--adversary] | experiments [--out FILE] [--seed N] | fast-forward ...");
            2
        }
    }
}
```

(`boot::repo_root` is currently private (`repo_root()?` used inside `boot`); make it `pub` or call the same helper `ingest_cmd.rs` uses. Check with `grep -n "fn repo_root" rust/crates/cg-cli/src/*.rs`.)

- [ ] **Step 5: Run tests and the command**

Run: `cargo test -p cg-sim && cargo run -p cg-cli -- sim experiments --out ../outputs/defense/learning.json`
Expected: tests PASS; the JSON file exists and contains `"simulated": true`. Read the file: if `routing_effect.senior_share_on` exceeds `senior_share_off`, that is a finding to put in the document, not a failure.

- [ ] **Step 6: Commit** (the evidence file is under a gitignored folder, so force-add it)

```bash
git add rust/crates/cg-sim rust/crates/cg-cli rust/Cargo.lock
git add -f outputs/defense/learning.json
git commit -m "cg-sim experiments: convergence, routing effect with senior load, active learning, poisoning, invariants; evidence file

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 10: Demo fast-forward

**Files:**
- Modify: `rust/crates/cg-sim/src/generate.rs` (a `feed` function), `rust/crates/cg-cli/src/sim_cmd.rs`

**Interfaces:**
- Produces: `cg sim fast-forward --badge CG-4004 [--dev] [--decisions N] [--seed N] [--pace-ms M]`: opens the real queue store as an operator, feeds simulated decided claims into it (ids `SIM-*`, flagged `simulated`), then runs `learn` so a proposal appears in the Learning panel.

- [ ] **Step 1: Test.** In `generate.rs` tests:

```rust
    #[test]
    fn feeding_a_store_in_slices_gives_the_same_decisions_as_one_run() {
        let corpus = load_corpus(&root()).unwrap();
        let a = MemoryQueueStore::new();
        let s1 = Lab::new(3, false).run(&a, &corpus, 300, &RoutingConfig::default(), None).unwrap();
        let b = MemoryQueueStore::new();
        let lab = Lab::new(3, false);
        let part = lab.run_from(&b, &corpus, 0, 100, &RoutingConfig::default(), None).unwrap();
        let rest = lab.run_from(&b, &corpus, part.claims, 200, &RoutingConfig::default(), None).unwrap();
        assert_eq!(s1.decisions, part.decisions + rest.decisions);
    }
```

(The two runs use one RNG each, so the decisions differ in value but not in count; the test only guards that numbering continues without a clash. If you want identical values, derive the per-claim RNG from `seed ^ claim_index` instead of one running RNG; do that, since it also makes slices reproducible, and then assert equal fingerprints trails.)

- [ ] **Step 2: Implement** `run_from(store, corpus, start, target, cfg, snapshot)` (rename the Task 8 body: `claims` starts at `start`; `run` calls `run_from(.., 0, ..)`), per-claim RNG `ChaCha8Rng::seed_from_u64(self.seed ^ (claims as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15))`. `fast-forward` in `sim_cmd.rs` follows the `queue` command's sign-in sequence (copy the operator login from `tools::queue`; require `routing.manage`; refuse unless `--dev` or the operator explicitly passes `--i-know-this-is-a-demo`; refuse if the store already holds a non-`SIM-` decided claim and `--force` is absent, so it cannot pollute a real database), then loops slices of 50 decisions with `--pace-ms` sleep between, printing a progress line, then calls `reliability::learn`.

- [ ] **Step 3: Run** `cargo test -p cg-sim` — Expected PASS. Smoke test by hand: start `cg serve --demo`, run `cg sim fast-forward` against it, open the Learning panel (Task 11).

- [ ] **Step 4: Commit**

```bash
git add rust/crates/cg-sim rust/crates/cg-cli
git commit -m "cg sim fast-forward: feed simulated decisions into a demo queue

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 11: Console: attention badges and the Learning panel

**Files:**
- Modify: `src/access/ui/views-work.js` (badges), `src/access/ui/app.js` (route + menu entry), `src/access/ui/index.html` (script tag), `src/access/ui/style.css` (badge style)
- Create: `src/access/ui/views-learning.js`

**Interfaces:**
- Consumes: `GET /api/v1/queue/reliability`, `GET /queue/reliability/{v}/verify`, `POST /queue/reliability/learn`, `POST /queue/reliability/{v}/activate`, the existing `/approvals` pages.
- Rules for this folder (read the header of `views-ops.js`): server text only ever becomes text, never markup; use `CG.h`, `CG.table`, `CG.clear`, `CG.status`; no `innerHTML`; the test scan for markup sinks must still pass.

- [ ] **Step 1: Badges in the inbox.** In `views-work.js`, where a claim card header is built, add after the lane badge (find it with `grep -n "lane" src/access/ui/views-work.js`):

```js
  var ATTENTION_TEXT = { high_severity: 'High severity', low_confidence: 'Low confidence', could_not_assess: 'Could not assess', degraded: 'Incomplete result', disagreement: 'Seniors disagreed' };
  function attentionBadges(list) {
    return (list || []).map(function (a) { return h('span', { class: 'chip attn-' + a, text: ATTENTION_TEXT[a] || a }); });
  }
```

and append `attentionBadges(claim.attention)` to the header row. Add to `style.css`: `.chip.attn-low_confidence{background:var(--warn-bg);color:var(--warn)}` using the variables already defined in that file (check `grep -n "\-\-warn" style.css`; reuse whichever warning/info variables exist).

- [ ] **Step 2: The panel.** `views-learning.js`:

```js
/* ClaimGuard console: the learning panel (administrators). Shows what reviewers' decisions have taught the system about each rule, the proposal
 * waiting for approval with its replay, and lets an administrator re-derive every number. Same rule as every file here: text only, never markup. */
(function () {
  'use strict';
  var CG = window.CG, h = CG.h;
  CG.views = CG.views || {};

  var STATUS_TEXT = { unmeasured: 'Not enough decisions yet', measured: 'Reliable', doubtful: 'Doubtful (usually overridden)' };
  var STATUS_KIND = { unmeasured: 'info', measured: 'good', doubtful: 'warn' };

  function pct(v) { return (100 * v).toFixed(0) + '%'; }

  // an interval drawn as a bar: the line is the 95% interval, the dot the observed rate
  function bar(r) {
    var ns = 'http://www.w3.org/2000/svg', svg = document.createElementNS(ns, 'svg');
    svg.setAttribute('viewBox', '0 0 100 10'); svg.setAttribute('width', '160'); svg.setAttribute('height', '16'); svg.setAttribute('role', 'img');
    svg.setAttribute('aria-label', 'upheld rate ' + pct(r.rate) + ', interval ' + pct(r.lower) + ' to ' + pct(r.upper));
    function el(tag, a) { var e = document.createElementNS(ns, tag); Object.keys(a).forEach(function (k) { e.setAttribute(k, a[k]); }); svg.appendChild(e); }
    el('line', { x1: 0, y1: 5, x2: 100, y2: 5, stroke: 'currentColor', 'stroke-opacity': '0.15', 'stroke-width': 2 });
    el('line', { x1: 100 * r.lower, y1: 5, x2: 100 * r.upper, y2: 5, stroke: 'currentColor', 'stroke-width': 3 });
    el('circle', { cx: 100 * r.rate, cy: 5, r: 3, fill: 'currentColor' });
    return svg;
  }

  function rulesTable(rules) {
    var ids = Object.keys(rules || {}).sort();
    if (!ids.length) return h('p', { class: 'muted', text: 'No rule has been decided yet.' });
    return CG.table([
      { label: 'Rule', get: function (id) { return id; } },
      { label: 'Decisions', num: true, get: function (id) { return rules[id].n + ' by ' + rules[id].reviewers; } },
      { label: 'Upheld', num: true, get: function (id) { return pct(rules[id].rate); } },
      { label: '95% interval', get: function (id) { return h('span', null, bar(rules[id]), ' ' + pct(rules[id].lower) + ' to ' + pct(rules[id].upper)); } },
      { label: 'Status', get: function (id) { return h('span', { class: 'chip ' + STATUS_KIND[rules[id].status], text: STATUS_TEXT[rules[id].status] }); } }
    ], ids);
  }

  function replayCard(rep) {
    if (!rep) return null;
    var rows = [['Recent claims replayed', rep.claims], ['Need a senior now', rep.senior_before + ' (' + pct(rep.senior_share_before) + ')'],
      ['Would need a senior', rep.senior_after + ' (' + pct(rep.senior_share_after) + ')'], ['Verdicts changed', rep.verdicts_unchanged ? 'none' : 'YES - do not approve']];
    return h('div', { class: 'card' }, h('h3', { text: 'Replay: what this would change' }),
      h('dl', { class: 'kv' }, rows.map(function (r) { return [h('dt', { text: r[0] }), h('dd', { text: String(r[1]) })]; }).reduce(function (a, b) { return a.concat(b); }, [])),
      rep.note ? h('div', { class: 'notice warn', text: rep.note }) : null);
  }

  CG.views.learning = function (root) {
    var body = h('div'), msg = h('div');
    function load() {
      CG.api('GET', '/api/v1/queue/reliability').then(function (r) {
        CG.clear(body);
        var s = r.settings;
        body.appendChild(h('div', { class: 'notice info' }, 'Learning is ' + (s.learn_enabled ? 'on' : 'off') + ' and routing by reliability is ' + (s.route_by_reliability ? 'on' : 'off') +
          '. A learned snapshot changes nothing until a second administrator approves it.'));
        body.appendChild(h('h2', { class: 'sect', text: r.active ? 'In use: version ' + r.active.version + (r.active.verified ? '' : ' (FAILED verification, ignored)') : 'No learned snapshot is in use' }));
        if (r.active) body.appendChild(rulesTable(r.active.rules));
        var latest = r.latest;
        if (latest && latest.status === 'proposed') {
          body.appendChild(h('h2', { class: 'sect', text: 'Waiting for approval: version ' + latest.version }));
          body.appendChild(rulesTable(latest.rules));
          body.appendChild(replayCard(latest.replay));
          body.appendChild(h('div', { class: 'row' },
            h('button', { class: 'btn', text: 'Re-derive every number', onclick: function () { verify(latest.version); } }),
            h('button', { class: 'btn primary', text: 'Ask for approval to use it', onclick: function () { activate(latest.version); } })));
        }
        body.appendChild(h('div', { class: 'row' }, h('button', { class: 'btn', text: 'Learn from decisions now', onclick: learn })));
      }).catch(function (e) { CG.clear(body); body.appendChild(h('div', { class: 'notice bad', text: CG.errorText(e) })); });
    }
    function say(text, kind) { CG.clear(msg); msg.appendChild(h('div', { class: 'notice ' + kind, text: text })); }
    function learn() { CG.api('POST', '/api/v1/queue/reliability/learn').then(function (r) { say(r.proposed ? 'Proposal ' + r.proposed + ' created.' : 'Nothing new to learn.', 'info'); load(); }).catch(function (e) { say(CG.errorText(e), 'bad'); }); }
    function verify(v) {
      CG.api('GET', '/api/v1/queue/reliability/' + v + '/verify').then(function (r) {
        var rows = Object.keys(r.rules).sort().map(function (id) { return { id: id, s: r.rules[id].stored, c: r.rules[id].recomputed }; });
        CG.clear(msg);
        msg.appendChild(h('div', { class: 'notice ' + (r.ok ? 'good' : 'bad'), text: r.ok ? 'Every figure was recomputed from the counts and matches.' : 'Mismatch: ' + r.problem }));
        msg.appendChild(CG.table([
          { label: 'Rule', get: function (x) { return x.id; } },
          { label: 'Stored interval', get: function (x) { return pct(x.s.lower) + ' to ' + pct(x.s.upper); } },
          { label: 'Recomputed', get: function (x) { return x.c ? pct(x.c.lower) + ' to ' + pct(x.c.upper) : '-'; } }
        ], rows));
      }).catch(function (e) { say(CG.errorText(e), 'bad'); });
    }
    function activate(v) {
      CG.api('POST', '/api/v1/queue/reliability/' + v + '/activate').then(function (r) {
        say(r.status === 'applied' ? 'Version ' + v + ' is now in use.' : 'Waiting for a second administrator to approve it under Users, approvals.', r.status === 'applied' ? 'good' : 'info');
        load();
      }).catch(function (e) { say(CG.errorText(e), 'bad'); });
    }
    root.appendChild(h('h1', { text: 'What the system has learned' }));
    root.appendChild(msg);
    root.appendChild(body);
    load();
  };
})();
```

Before using `CG.api`, `CG.errorText`, `CG.table`, `CG.status`, check their real names and signatures in `lib.js` (`grep -n "CG\.\(api\|errorText\|table\|clear\|status\)\s*=" src/access/ui/lib.js`) and adapt this file to them; `views-ops.js` is the model. If `h` does not accept `onclick`, use the same event-attach style as `views-ops.js`.

- [ ] **Step 3: Register.** `index.html`: add `<script src="views-learning.js"></script>` after `views-ops.js`. `app.js`: add to the menu list `['#/learning', 'Learned reliability', 'queue.view']` under Administration, and the route `[/^#\/learning$/, 'queue.view', function (m, el) { CG.views.learning(el); }, '#/learning']`.

- [ ] **Step 4: Verify.** Run the existing console checks: `cargo test -p cg-api` (the test that scans the UI files for markup sinks and the CSP test must pass) and `rust/tools/console_review.py` / `import_ui_test.py` if they run headless in this environment (they need playwright from `.venv\Scripts\python.exe`). Then do a visual check in the real browser if the Chrome extension is connected: `cg serve --demo`, `cg sim fast-forward`, open `#/learning`, take a screenshot. If the extension is not connected, say so in the final report: the panel was checked by the sink scan only.

- [ ] **Step 5: Commit**

```bash
git add src/access/ui
git commit -m "Console: attention badges and the learning panel

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

---

### Task 12: Documents, mutation check, full run, review

**Files:**
- Create: `docs/47_HITL_Confidence_and_Learning_Loop.md`
- Modify: `SPECS.md` (new section 10h), `README.md`, `SPECIFICATION.md`, `docs/superpowers/specs/2026-10-14-hitl-confidence-and-learning-design.md` (section 5a), `rust/tools/mutate.py` (if it takes a mutant list)

- [ ] **Step 1: Mutation check.** Read `rust/tools/mutate.py` header; add mutants for `reliability.rs` (flip `<` to `<=` in `status_of` upper test; remove the `.max(1)` in the cap; replace `floor` with `ceil`; drop `verify` call in `apply`; invert `upheld`; make `activate` skip the newer-active check; drop the status check). Run it; every mutant must be killed by `cargo test -p cg-queue`. A surviving mutant means a missing test: add the test, not an exclusion.

- [ ] **Step 2: Full run.** Start Docker Desktop, `docker start cg-test-mongo`, source the Mongo env, then `REQUIRE_MONGO=1 REQUIRE_REDIS=1 cargo test --workspace > /tmp/full.out 2>&1` (use the scratchpad path; Python in Git Bash cannot read `/tmp`). `grep "^test result\|FAILED\|panicked" full.out`. Record the new total test count. Re-run `cg sim experiments` and confirm `outputs/defense/learning.json` is unchanged (determinism) with `git diff --stat`. Run `python tools/compare_reliability.py` once more.

- [ ] **Step 3: Write `docs/47_HITL_Confidence_and_Learning_Loop.md`** with: what problem (the 10-point clause and the 2-point item), the definition of confidence (measured reliability, not a model), how routing and uncertainty-first work, the governance chain (proposal, replay, two administrators, verify), what reviewers see, the failure table, the evidence (copy the real numbers from `learning.json`: the convergence point for R006, senior load off vs on and what that means, the poisoning result with and without protection, the invariants), the plain statement that **reviewers are simulated and the claims are recycled**, label shift, and the open mentor question. Write the numbers from the file, not from memory.

- [ ] **Step 4: Update the others.** `SPECS.md` section 10h (same facts, shorter); `README.md`: the two Phase 2 rubric rows currently say "Not built: routing by model confidence" and the status paragraph says "Not built: routing by model confidence, the active learning loop": change them to what now exists, keep the Phase 2 block directly after the Phase 1 deliverables (standing user rule), add a briefing line and a docs/47 link; `SPECIFICATION.md`: FR-22, a new requirement, a use case "Learn from overrides" with main/secondary/extension/failure/guarantee parts (a script check enforces all five), changes to UC12 and UC20; spec section 5a: say activation is by two different administrators through the existing approval gate. Do not claim numbers that are not in the evidence file.

- [ ] **Step 5: Commit** the docs and evidence.

```bash
git add docs SPECS.md README.md SPECIFICATION.md rust/tools
git add -f outputs/defense/learning.json
git commit -m "Docs: HITL confidence and learning loop (docs/47, SPECS 10h, README, SPECIFICATION); mutants

Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>"
```

- [ ] **Step 6: Independent review.** Dispatch one fresh reviewer (model fable, the user's standing workflow) over the whole branch diff against `4a780d4~`. Focus: store atomicity of the status moves, the activation race, whether any path lets a snapshot change a verdict, hide-not-disable on the four routes and the inbox view, honesty of the documents against `learning.json`. Apply one fix pass, re-run the full suite, commit.

- [ ] **Step 7: Report and ask before pushing.** State the test count, what the experiments showed (including any increase in senior load), and what was not verified (visual check of the panel if the browser extension was not connected; the Mongo run if Docker was unavailable).

---

## Self-review

- **Spec coverage:** criteria 1 and 2 (flagging, senior routing, medium needs one senior) Tasks 4 and 6; 3 (overrides are feedback records) Task 2 (`counted_decisions`) and existing `decide_finding`; 4 (doubtful, cap) Tasks 2 and 9; 5 (no verdict changes) Tasks 4, 5 (`verdicts_unchanged`), 9 (invariants); 6 (versioned, re-derivable) Tasks 2, 3, 6, 7; 7 (defaults off) Tasks 1 and 4. Section 5a governance Tasks 5, 6, 7; 5b console Task 11; section 9 simulated database Tasks 8 and 10; section 10 evidence Task 9; section 11 testing Tasks 2 to 12; section 12 documentation Task 12. The Stage 2 logistic-regression experiment (spec section 10 item 6) is NOT in this plan: it is optional, exploratory and was marked "only if everything else works"; add it as Task 13 after the review if time allows.
- **Placeholders:** the only open items are adaptations to names I could not read without compiling (`QueueStack` field names, `CG.api`/`CG.errorText`, `to_doc`/`duplicate` helpers in cg-mongo, the `run_job` closure shape); each says where to look and what the real code already does. No TBD.
- **Types:** `Params`, `Counted`, `row`, `make_snapshot`, `verify`, `apply`, `attention`, `active`, `learn`, `replay_report`, `activate`, `reject`, `hash_text`, `REPLAY_LIMIT` are named the same in every task that uses them; `Snap.uncertainty` in Tasks 4 and 9; store method names identical in Tasks 3, 5, 6, 7.
