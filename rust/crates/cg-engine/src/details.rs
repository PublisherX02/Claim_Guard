//! The fifteen rules' fact extraction: a line-for-line port of `src/facts_extractor.py` (R001..R015).
//!
//! Each `rNNN` function is the single source of truth for its rule. One pass over the claim yields the fact lines the rule pack
//! pattern-matches, the evidence paths and line ids the result reports, and the message, so what the pack sees and what the result says
//! cannot drift apart. Facts are percent-encoded wherever a claim value enters one, so a value cannot forge another rule's tag.

use bigdecimal::BigDecimal;
use cg_core::config::Config;
use cg_core::num::{cents, plain};
use cg_core::text::{empty, quote};
use cg_core::view::View;
use cg_core::{Day, Num};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::str::FromStr;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Details {
    pub facts: Vec<String>,
    pub evidence_paths: Vec<String>,
    pub line_ids: Vec<String>,
    pub message: String,
}

fn day(s: &Option<String>) -> Option<Day> {
    s.as_deref().and_then(Day::parse)
}

fn uniq(paths: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    paths.into_iter().filter(|p| seen.insert(p.clone())).collect()
}

/// `sep.join(sorted(set(items)))`
fn joined(items: &[&str], sep: &str) -> String {
    let set: BTreeSet<&str> = items.iter().copied().collect();
    set.into_iter().collect::<Vec<_>>().join(sep)
}

fn line_ids(v: &View, indices: &BTreeSet<usize>) -> Vec<String> {
    indices.iter().map(|i| v.lines[*i].line_id.clone()).collect()
}

fn d(facts: Vec<String>, paths: Vec<String>, ids: Vec<String>, message: impl Into<String>) -> Details {
    Details { facts, evidence_paths: paths, line_ids: ids, message: message.into() }
}

fn one(fact: &str, paths: Vec<String>, message: &str) -> Details {
    d(vec![fact.to_string()], paths, vec![], message)
}

fn s(x: &str) -> String {
    x.to_string()
}

pub fn r001(v: &View) -> Details {
    let mut paths = vec![];
    let mut ids = BTreeSet::new();
    for (name, value) in [("invoice_number", &v.invoice_number), ("member_id", &v.member_id), ("diagnosis_code", &v.diagnosis_code)] {
        if empty(value) {
            paths.push(format!("/{name}"));
        }
    }
    for (i, l) in v.lines.iter().enumerate() {
        let missing = [
            ("service_date", empty(&l.service_date)),
            ("service_code", empty(&l.service_code)),
            ("quantity", l.quantity.is_none()),
            ("unit_price", l.unit_price.is_none()),
            ("net_amount", l.net_amount.is_none()),
        ];
        for (name, is_missing) in missing {
            if is_missing {
                paths.push(format!("/lines/{i}/{name}"));
                ids.insert(l.line_id.clone());
            }
        }
    }
    if !paths.is_empty() {
        return d(
            paths.iter().map(|p| format!("R001:MISSING:{p}")).collect(),
            uniq(paths),
            ids.into_iter().collect(),
            "Required information is missing.",
        );
    }
    d(
        vec![s("R001:OK")],
        vec![s("/invoice_number"), s("/member_id"), s("/diagnosis_code"), s("/lines")],
        vec![],
        "Required information is present.",
    )
}

pub fn r002(v: &View) -> Details {
    let sub = day(&v.submission_date);
    let mut paths = vec![s("/submission_date")];
    let mut unknown: Vec<&str> = vec![];
    let mut facts = vec![];
    let mut ids = vec![];
    if sub.is_none() {
        unknown.push("submission date");
    }
    for (i, l) in v.lines.iter().enumerate() {
        paths.push(format!("/lines/{i}/service_date"));
        let Some(dt) = day(&l.service_date) else {
            unknown.push("service date");
            continue;
        };
        if let Some(sub) = sub {
            if dt > sub {
                ids.push(l.line_id.clone());
                facts.push(format!("R002:LATE:/lines/{i}/service_date:service={dt}:submission={sub}"));
            }
        }
    }
    let message;
    if !facts.is_empty() {
        message = format!(
            "Service date is after the submission date.{}",
            if unknown.is_empty() { String::new() } else { format!(" Additional unknown inputs: {}", joined(&unknown, ", ")) }
        );
    } else if !unknown.is_empty() {
        facts = vec![s("R002:UNKNOWN")];
        message = joined(&unknown, "; ");
    } else {
        facts = vec![s("R002:OK")];
        message = s("All service dates are on or before the submission date.");
    }
    d(facts, paths, ids, message)
}

pub fn r003(v: &View) -> Details {
    let cv = &v.coverage;
    let start = day(&cv.start_date);
    let end = day(&cv.end_date);
    let mut paths = vec![s("/coverage/status"), s("/coverage/start_date"), s("/coverage/end_date")];
    let mut failed: Vec<&str> = vec![];
    let mut unknown: Vec<&str> = vec![];
    let mut facts = vec![];
    let mut ids = vec![];
    if empty(&cv.status) {
        unknown.push("coverage status");
    } else if cv.status.as_deref() != Some("active") {
        failed.push("coverage status is not active");
        facts.push(format!("R003:INACTIVE:status={}", quote(cv.status.as_deref().unwrap_or(""))));
    }
    if start.is_none() || end.is_none() {
        unknown.push("coverage period");
    }
    for (i, l) in v.lines.iter().enumerate() {
        paths.push(format!("/lines/{i}/service_date"));
        let Some(dt) = day(&l.service_date) else {
            unknown.push("service date");
            continue;
        };
        if start.is_some_and(|st| dt < st) || end.is_some_and(|en| dt > en) {
            failed.push("service outside coverage period");
            ids.push(l.line_id.clone());
            facts.push(format!(
                "R003:OUT_OF_PERIOD:/lines/{i}/service_date:service={dt}:start={}:end={}",
                start.map(|x| x.to_string()).unwrap_or_default(),
                end.map(|x| x.to_string()).unwrap_or_default()
            ));
        }
    }
    let message;
    if !failed.is_empty() {
        message = format!(
            "{}{}",
            joined(&failed, "; "),
            if unknown.is_empty() { String::new() } else { format!("; Additional unknown inputs: {}", joined(&unknown, ", ")) }
        );
    } else if !unknown.is_empty() {
        facts = vec![s("R003:UNKNOWN")];
        message = joined(&unknown, "; ");
    } else {
        facts = vec![s("R003:OK")];
        message = s("All service dates are within active coverage, including boundaries.");
    }
    d(facts, paths, ids, message)
}

pub fn r004(v: &View) -> Details {
    let paths = vec![s("/patient_id"), s("/coverage/beneficiary_patient_id"), s("/member_id"), s("/coverage/member_id")];
    let mut unknown: Vec<&str> = vec![];
    let mut mismatches = vec![];
    let (pid, bpid) = (&v.patient_id, &v.coverage.beneficiary_patient_id);
    let (mid, cmid) = (&v.member_id, &v.coverage.member_id);
    if empty(pid) || empty(bpid) {
        unknown.push("patient identifiers");
    } else if pid != bpid {
        mismatches.push(format!(
            "R004:PATIENT_MISMATCH:patient_id={}:beneficiary_patient_id={}",
            quote(pid.as_deref().unwrap_or("")),
            quote(bpid.as_deref().unwrap_or(""))
        ));
    }
    if empty(mid) || empty(cmid) {
        unknown.push("member identifiers");
    } else if mid != cmid {
        mismatches.push(format!(
            "R004:MEMBER_MISMATCH:member_id={}:coverage_member_id={}",
            quote(mid.as_deref().unwrap_or("")),
            quote(cmid.as_deref().unwrap_or(""))
        ));
    }
    if !mismatches.is_empty() {
        d(mismatches, paths, vec![], "Patient or member identifiers do not match coverage.")
    } else if !unknown.is_empty() {
        d(vec![s("R004:UNKNOWN")], paths, vec![], joined(&unknown, "; "))
    } else {
        d(vec![s("R004:OK")], paths, vec![], "Patient and member identifiers match coverage.")
    }
}

pub fn r005(v: &View, cfg: &Config) -> Details {
    let paths = vec![s("/provider_id"), s("/policy_id")];
    let policy = v.policy_id.as_ref().and_then(|p| cfg.policies.get(p));
    match policy {
        Some(p) if !empty(&v.provider_id) => {
            let pid = v.provider_id.as_deref().unwrap_or("");
            if p.allowed_providers.iter().any(|a| a == pid) {
                one("R005:OK", paths, "Provider is in the allowed network.")
            } else {
                one(&format!("R005:UNLISTED:provider_id={}", quote(pid)), paths, "Provider is not in the allowed network.")
            }
        }
        _ => one("R005:UNKNOWN", paths, "Provider or policy is unknown."),
    }
}

pub fn r006(v: &View) -> Details {
    let mut seen: HashMap<(String, String, String), usize> = HashMap::new();
    let mut dups: Vec<usize> = vec![];
    let mut missing = false;
    let mut facts = vec![];
    for (i, l) in v.lines.iter().enumerate() {
        if empty(&l.service_code) || day(&l.service_date).is_none() {
            missing = true;
            continue;
        }
        let key = (l.service_code.clone().unwrap_or_default(), l.service_date.clone().unwrap_or_default(), l.modifier.clone().unwrap_or_default());
        match seen.get(&key) {
            Some(a) => {
                dups.extend([*a, i]);
                facts.push(format!("R006:DUPLICATE:{a},{i}:key={}|{}|{}", quote(&key.0), quote(&key.1), quote(&key.2)));
            }
            None => {
                seen.insert(key, i);
            }
        }
    }
    let set: BTreeSet<usize> = dups.iter().copied().collect();
    let ids = line_ids(v, &set);
    let mut paths = vec![];
    for i in &set {
        for k in ["service_code", "service_date", "modifier"] {
            paths.push(format!("/lines/{i}/{k}"));
        }
    }
    let paths = if paths.is_empty() { vec![s("/lines")] } else { paths };
    if !dups.is_empty() {
        let message = format!("Possible duplicate lines require review.{}", if missing { " Additional lines have missing inputs." } else { "" });
        d(facts, paths, ids, message)
    } else if missing {
        d(vec![s("R006:UNKNOWN")], paths, ids, "Missing inputs prevent a complete duplicate check.")
    } else {
        d(vec![s("R006:OK")], paths, ids, "No duplicate service/date/modifier combinations.")
    }
}

pub fn r007(v: &View) -> Details {
    let mut paths = vec![];
    let mut unknown: Vec<&str> = vec![];
    let mut facts = vec![];
    let mut ids = vec![];
    for (i, l) in v.lines.iter().enumerate() {
        paths.extend([format!("/lines/{i}/quantity"), format!("/lines/{i}/unit_price"), format!("/lines/{i}/net_amount")]);
        let (Some(q), Some(u), Some(n)) = (&l.quantity, &l.unit_price, &l.net_amount) else {
            unknown.push("line amount inputs");
            continue;
        };
        let expected = cents(&(q.to_decimal() * u.to_decimal()));
        let actual = n.to_decimal();
        let off = (&expected - &actual).abs() > BigDecimal::from_str("0.01").unwrap_or_default();
        if off {
            ids.push(l.line_id.clone());
            facts.push(format!("R007:MISMATCH:/lines/{i}/net_amount:expected={}:actual={}", plain(&expected), plain(&actual)));
        }
    }
    if !facts.is_empty() {
        d(facts, paths, ids, "Line net amount does not equal quantity times unit price.")
    } else if !unknown.is_empty() {
        d(vec![s("R007:UNKNOWN")], paths, ids, joined(&unknown, "; "))
    } else {
        d(vec![s("R007:OK")], paths, ids, "All line amounts equal quantity times unit price.")
    }
}

fn catalogued(code: &Option<String>, cfg: &Config) -> bool {
    !empty(code) && code.as_ref().is_some_and(|c| cfg.services.contains(c))
}

pub fn r008(v: &View, cfg: &Config) -> Details {
    let Some(policy) = v.policy_id.as_ref().and_then(|p| cfg.policies.get(p)) else {
        return one("R008:UNKNOWN", vec![s("/policy_id")], "Policy is unknown.");
    };
    let mut paths = vec![];
    let mut unknown_lines = false;
    let mut failed: BTreeSet<usize> = BTreeSet::new();
    let mut relevant = false;
    for (i, l) in v.lines.iter().enumerate() {
        if !catalogued(&l.service_code, cfg) {
            unknown_lines = true;
            paths.push(format!("/lines/{i}/service_code"));
            continue;
        }
        if !policy.auth_required_services.contains(l.service_code.as_deref().unwrap_or("")) {
            continue;
        }
        relevant = true;
        paths.extend([format!("/lines/{i}/service_code"), format!("/lines/{i}/authorization_id")]);
        if empty(&l.authorization_id) {
            failed.insert(i);
        }
    }
    let ids = line_ids(v, &failed);
    let paths = if paths.is_empty() { vec![s("/policy_id")] } else { paths };
    if !failed.is_empty() {
        d(failed.iter().map(|i| format!("R008:MISSING_AUTH:/lines/{i}/authorization_id")).collect(), paths, ids, "Required authorization reference is missing.")
    } else if unknown_lines {
        d(vec![s("R008:UNKNOWN")], paths, ids, "Some line service codes are unknown; cannot determine authorization requirement.")
    } else if relevant {
        d(vec![s("R008:OK")], paths, ids, "All authorization-required lines carry an authorization reference.")
    } else {
        d(vec![s("R008:NOT_APPLICABLE")], paths, ids, "No line requires an authorization reference under this policy.")
    }
}

pub fn r009(v: &View, cfg: &Config) -> Details {
    let Some(policy) = v.policy_id.as_ref().and_then(|p| cfg.policies.get(p)) else {
        return one("R009:UNKNOWN", vec![s("/policy_id")], "Policy is unknown.");
    };
    // dict comprehension semantics: a later record with the same id replaces an earlier one
    let mut auths_by_id: HashMap<Option<&str>, &cg_core::view::AuthView> = HashMap::new();
    for a in &v.authorizations {
        auths_by_id.insert(a.authorization_id.as_deref(), a);
    }
    let mut qty_by_auth: HashMap<&str, Num> = HashMap::new();
    let mut qty_unknown_auth: HashSet<&str> = HashSet::new();
    for l in &v.lines {
        let Some(aid) = l.authorization_id.as_deref().filter(|a| !a.is_empty()) else { continue };
        match &l.quantity {
            Some(q) => {
                let total = qty_by_auth.get(aid).cloned().unwrap_or_else(|| Num::int(0)).add(q);
                qty_by_auth.insert(aid, total);
            }
            None => {
                qty_unknown_auth.insert(aid);
            }
        }
    }
    let mut paths = vec![s("/policy_id")];
    let mut unknown_lines = false;
    let mut failed: BTreeSet<usize> = BTreeSet::new();
    let mut relevant = false;
    for (i, l) in v.lines.iter().enumerate() {
        if !catalogued(&l.service_code, cfg) {
            unknown_lines = true;
            continue;
        }
        let code = l.service_code.as_deref().unwrap_or("");
        if !policy.auth_required_services.contains(code) {
            continue;
        }
        relevant = true;
        paths.extend([format!("/lines/{i}/service_code"), format!("/lines/{i}/authorization_id")]);
        if empty(&l.authorization_id) {
            unknown_lines = true;
            continue;
        }
        let aid = l.authorization_id.as_deref().unwrap_or("");
        let Some(auth) = auths_by_id.get(&Some(aid)) else {
            failed.insert(i);
            paths.push(s("/authorizations"));
            continue;
        };
        let sd = day(&l.service_date);
        let (vf, vt) = (day(&auth.valid_from), day(&auth.valid_to));
        let mut mismatch = false;
        let mut unknown = false;
        for (mine, theirs) in [(&v.patient_id, &auth.patient_id), (&l.service_code, &auth.service_code)] {
            if empty(theirs) {
                unknown = true;
            } else if theirs != mine {
                mismatch = true;
            }
        }
        if empty(&auth.status) {
            unknown = true;
        } else if auth.status.as_deref() != Some("approved") {
            mismatch = true;
        }
        match (sd, vf, vt) {
            (Some(sd), Some(vf), Some(vt)) => {
                if sd < vf || sd > vt {
                    mismatch = true;
                }
            }
            _ => unknown = true,
        }
        match &auth.max_quantity {
            Some(cap) if !qty_unknown_auth.contains(aid) => {
                let total = qty_by_auth.get(aid).cloned().unwrap_or_else(|| Num::int(0));
                if total.gt(cap) {
                    mismatch = true;
                }
            }
            _ => unknown = true,
        }
        if mismatch || !unknown {
            paths.push(format!("/lines/{i}/service_date"));
        }
        if mismatch {
            failed.insert(i);
        } else if unknown {
            unknown_lines = true;
        }
    }
    let ids = line_ids(v, &failed);
    if !failed.is_empty() {
        d(failed.iter().map(|i| format!("R009:MISMATCH:/lines/{i}/authorization_id")).collect(), paths, ids, "Authorization record does not match the service.")
    } else if unknown_lines {
        d(vec![s("R009:UNKNOWN")], paths, ids, "Missing inputs prevent matching the authorization record.")
    } else if relevant {
        d(vec![s("R009:OK")], paths, ids, "Authorization records match the required services.")
    } else {
        d(vec![s("R009:NOT_APPLICABLE")], paths, ids, "No line requires an authorization record under this policy.")
    }
}

pub fn r010(v: &View, cfg: &Config) -> Details {
    let Some(policy) = v.policy_id.as_ref().and_then(|p| cfg.policies.get(p)) else {
        return one("R010:UNKNOWN", vec![s("/policy_id")], "Policy is unknown.");
    };
    let mut paths = vec![s("/attachments")];
    let mut unknown_lines = false;
    let mut failed: BTreeSet<usize> = BTreeSet::new();
    let mut relevant = false;
    for (i, l) in v.lines.iter().enumerate() {
        if !catalogued(&l.service_code, cfg) {
            unknown_lines = true;
            continue;
        }
        let code = l.service_code.as_deref().unwrap_or("");
        let Some(req_type) = policy.required_documents.get(code) else { continue };
        relevant = true;
        paths.extend([format!("/lines/{i}/service_code"), format!("/lines/{i}/service_date")]);
        if empty(&l.service_date) || day(&l.service_date).is_none() {
            unknown_lines = true;
            continue;
        }
        let matches: Vec<_> = v
            .attachments
            .iter()
            .filter(|a| {
                a.kind.as_deref() == Some(req_type.as_str()) && a.patient_id == v.patient_id && a.service_code.as_deref() == Some(code) && a.service_date == l.service_date
            })
            .collect();
        if matches.is_empty() {
            failed.insert(i);
        } else if matches.iter().any(|a| a.document_status.as_deref() == Some("final")) {
            continue;
        } else {
            unknown_lines = true;
        }
    }
    let ids = line_ids(v, &failed);
    if !failed.is_empty() {
        d(failed.iter().map(|i| format!("R010:MISSING_DOC:/lines/{i}/service_code")).collect(), paths, ids, "Required supporting document is absent or mismatched.")
    } else if unknown_lines {
        d(vec![s("R010:UNKNOWN")], paths, ids, "Only draft or uncertain matching documentation is available.")
    } else if relevant {
        d(vec![s("R010:OK")], paths, ids, "Required supporting documents are present and final.")
    } else {
        d(vec![s("R010:NOT_APPLICABLE")], paths, ids, "No line requires a supporting document under this policy.")
    }
}

pub fn r011(v: &View, cfg: &Config) -> Details {
    let mut paths = vec![];
    let mut unknown_lines = false;
    let mut failed: BTreeSet<usize> = BTreeSet::new();
    for (i, l) in v.lines.iter().enumerate() {
        paths.push(format!("/lines/{i}/service_code"));
        if empty(&l.service_code) {
            unknown_lines = true;
        } else if !l.service_code.as_ref().is_some_and(|c| cfg.services.contains(c)) {
            failed.insert(i);
        }
    }
    let ids = line_ids(v, &failed);
    if !failed.is_empty() {
        d(failed.iter().map(|i| format!("R011:NOT_CATALOGUED:/lines/{i}/service_code")).collect(), paths, ids, "Service code is not in the fictional catalogue.")
    } else if unknown_lines {
        d(vec![s("R011:UNKNOWN")], paths, ids, "Service code is missing.")
    } else {
        d(vec![s("R011:OK")], paths, ids, "All service codes are in the fictional catalogue.")
    }
}

pub fn r012(v: &View) -> Details {
    let mut paths = vec![s("/total_amount")];
    paths.extend((0..v.lines.len()).map(|i| format!("/lines/{i}/net_amount")));
    let amounts: Vec<&Option<Num>> = v.lines.iter().map(|l| &l.net_amount).collect();
    let Some(total) = &v.total_amount else {
        return one("R012:UNKNOWN", paths, "Missing amount inputs prevent totalling.");
    };
    if amounts.iter().any(|a| a.is_none()) {
        return one("R012:UNKNOWN", paths, "Missing amount inputs prevent totalling.");
    }
    let mut sum = BigDecimal::from(0);
    for a in amounts.iter().copied().flatten() {
        sum += a.to_decimal();
    }
    let expected = cents(&sum);
    let actual = total.to_decimal();
    if (&expected - &actual).abs() > BigDecimal::from_str("0.01").unwrap_or_default() {
        one(&format!("R012:MISMATCH:total_amount={}:expected={}", plain(&actual), plain(&expected)), paths, "Claim total does not equal the sum of line amounts.")
    } else {
        one("R012:OK", paths, "Claim total equals the sum of line amounts.")
    }
}

pub fn r013(v: &View, cfg: &Config) -> Details {
    let policy = v.policy_id.as_ref().and_then(|p| cfg.policies.get(p));
    let mut paths = vec![];
    let mut unknown_lines = false;
    let mut failed: BTreeSet<usize> = BTreeSet::new();
    for (i, l) in v.lines.iter().enumerate() {
        paths.extend([format!("/lines/{i}/quantity"), format!("/lines/{i}/unit_price"), format!("/lines/{i}/service_code")]);
        let mut line_failed = false;
        let mut line_unknown = l.quantity.is_none() || l.unit_price.is_none();
        if let Some(q) = &l.quantity {
            if !(q.is_whole() && !q.is_zero_or_negative()) {
                line_failed = true;
            }
        }
        if let Some(u) = &l.unit_price {
            if u.is_zero_or_negative() {
                line_failed = true;
            }
        }
        match (empty(&l.service_code), policy) {
            (false, Some(p)) => {
                let code = l.service_code.as_deref().unwrap_or("");
                match (p.max_unit_price.get(code), p.max_quantity_per_line.get(code)) {
                    (Some(max_price), Some(max_qty)) => {
                        if l.unit_price.as_ref().is_some_and(|u| u.gt(max_price)) {
                            line_failed = true;
                        }
                        if l.quantity.as_ref().is_some_and(|q| q.gt(max_qty)) {
                            line_failed = true;
                        }
                    }
                    _ => line_unknown = true,
                }
            }
            _ => line_unknown = true,
        }
        if line_failed {
            failed.insert(i);
        } else if line_unknown {
            unknown_lines = true;
        }
    }
    let ids = line_ids(v, &failed);
    if !failed.is_empty() {
        d(failed.iter().map(|i| format!("R013:LIMIT:/lines/{i}/quantity")).collect(), paths, ids, "Quantity or unit price violates the fictional limits.")
    } else if unknown_lines {
        d(vec![s("R013:UNKNOWN")], paths, ids, "Missing inputs, unknown code, or unavailable policy prevent checking limits.")
    } else {
        d(vec![s("R013:OK")], paths, ids, "All quantities and unit prices are within the fictional limits.")
    }
}

pub fn r014(v: &View, cfg: &Config) -> Details {
    let policy = v.policy_id.as_ref().and_then(|p| cfg.policies.get(p));
    let sub = day(&v.submission_date);
    let service_dates: Vec<Option<Day>> = v.lines.iter().map(|l| day(&l.service_date)).collect();
    let mut paths = vec![s("/submission_date"), s("/policy_id")];
    paths.extend((0..v.lines.len()).map(|i| format!("/lines/{i}/service_date")));
    let (Some(policy), Some(sub)) = (policy, sub) else {
        return one("R014:UNKNOWN", paths, "Missing dates or unavailable policy prevent checking the submission window.");
    };
    if service_dates.is_empty() || service_dates.iter().any(|x| x.is_none()) {
        return one("R014:UNKNOWN", paths, "Missing dates or unavailable policy prevent checking the submission window.");
    }
    let latest = service_dates.iter().flatten().max().copied();
    let lag = latest.map(|l| sub.days_since(&l)).unwrap_or(0);
    if lag < 0 {
        one("R014:NOT_APPLICABLE", paths, "Service date after submission date is handled by R002.")
    } else if lag > policy.submission_window_days {
        one(&format!("R014:LATE:lag={lag}:window={}", policy.submission_window_days), paths, "Submission window exceeded.")
    } else {
        one("R014:OK", paths, "Submission is within the allowed window.")
    }
}

pub fn r015(v: &View, cfg: &Config) -> Details {
    let policy = v.policy_id.as_ref().and_then(|p| cfg.policies.get(p));
    let paths = vec![s("/currency"), s("/policy_id")];
    match policy {
        Some(p) if !empty(&v.currency) => {
            let cur = v.currency.as_deref().unwrap_or("");
            if cur != p.currency {
                one(&format!("R015:MISMATCH:currency={}", quote(cur)), paths, "Currency does not match the policy currency.")
            } else {
                one("R015:OK", paths, "Currency matches the policy currency.")
            }
        }
        _ => one("R015:UNKNOWN", paths, "Currency or policy is unknown."),
    }
}

/// All fifteen, in rule order, each isolated: a panic inside one rule is reported for that rule only (see `evaluate`).
pub fn all<'a>(v: &'a View, cfg: &'a Config) -> Vec<(&'static str, Box<dyn Fn() -> Details + 'a>)> {
    vec![
        ("R001", Box::new(move || r001(v))),
        ("R002", Box::new(move || r002(v))),
        ("R003", Box::new(move || r003(v))),
        ("R004", Box::new(move || r004(v))),
        ("R005", Box::new(move || r005(v, cfg))),
        ("R006", Box::new(move || r006(v))),
        ("R007", Box::new(move || r007(v))),
        ("R008", Box::new(move || r008(v, cfg))),
        ("R009", Box::new(move || r009(v, cfg))),
        ("R010", Box::new(move || r010(v, cfg))),
        ("R011", Box::new(move || r011(v, cfg))),
        ("R012", Box::new(move || r012(v))),
        ("R013", Box::new(move || r013(v, cfg))),
        ("R014", Box::new(move || r014(v, cfg))),
        ("R015", Box::new(move || r015(v, cfg))),
    ]
}
