//! Advisory extension rules E001 to E005 and E101 to E103 (`src/extension_rules.py`, `src/claim_history.py`).
//!
//! Same design as the official engine: each `eNNN` function is the single source of truth for its rule (fact lines, evidence paths,
//! affected line ids, message from one pass over the claim) and a YARA-X pack generated from the catalogue decides the outcome. Nothing
//! here changes the official fifteen: extension results have their own ids and never enter them.
//!
//! Status semantics follow the official engine: a missing input gives UNABLE_TO_ASSESS (never PASS), an absent trigger NOT_APPLICABLE,
//! and a failure inside one rule UNABLE_TO_ASSESS for that rule only.

use crate::details::Details;
use crate::pack::{strongest, Pack};
use bigdecimal::BigDecimal;
use cg_core::text::{pointer, quote};
use cg_core::{Day, Num};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};

const OUTCOMES: [(&str, &str); 4] = [("FAIL", "/{rid}:FAIL:/"), ("PASS", "\"{rid}:OK\""), ("UNABLE_TO_ASSESS", "\"{rid}:UNKNOWN\""), ("NOT_APPLICABLE", "\"{rid}:NA\"")];
pub const RULE_IDS: [&str; 8] = ["E001", "E002", "E003", "E004", "E005", "E101", "E102", "E103"];
const NO_HISTORY: &str = "Claim history is not available, so earlier claims could not be checked.";
const NO_PARTY: &str = "The claim has no readable patient, provider or submission date, so earlier claims could not be matched or ordered.";

#[derive(Debug, thiserror::Error)]
pub enum ExtError {
    #[error("cannot read the extension catalogue: {0}")]
    Catalogue(String),
    #[error(transparent)]
    Pack(#[from] crate::pack::PackError),
    #[error("{0} produced no outcome - extractor and rule pack are out of sync")]
    OutOfSync(String),
}

/// Earlier claims of the same patient. "Earlier" is defined once, in `order_key`: strictly before by (submission date, claim id).
pub trait History {
    fn earlier_claims(&self, claim: &Value) -> Result<Vec<Value>, String>;
}

fn text(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}

/// (day, claim id), or None when the submission date cannot be read.
pub fn order_key(claim: &Value) -> Option<(Day, String)> {
    let day = Day::parse(claim.get("submission_date")?.as_str()?)?;
    Some((day, claim.get("claim_id").map(text).unwrap_or("").to_string()))
}

/// What `claim_history.InMemoryHistory` does, for batch files and tests.
pub struct InMemoryHistory {
    by_patient: BTreeMap<String, Vec<Value>>,
}

impl InMemoryHistory {
    pub fn new(claims: &[Value]) -> InMemoryHistory {
        let mut by_patient: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        for c in claims {
            let pid = c.get("patient_id").and_then(Value::as_str).filter(|s| !s.is_empty());
            let cid = c.get("claim_id").and_then(Value::as_str).filter(|s| !s.is_empty());
            if let (Some(pid), Some(_), Some(_)) = (pid, cid, order_key(c)) {
                by_patient.entry(pid.to_string()).or_default().push(c.clone());
            }
        }
        for rows in by_patient.values_mut() {
            rows.sort_by_key(|c| order_key(c));
        }
        InMemoryHistory { by_patient }
    }
}

/// The ordering rule over any list of a patient's claims (used by the database-backed history too).
pub fn earlier_of(claim: &Value, candidates: Vec<Value>) -> Vec<Value> {
    let Some(me) = order_key(claim) else { return vec![] };
    let mine = claim.get("claim_id").and_then(Value::as_str);
    let mut rows: Vec<Value> = candidates
        .into_iter()
        .filter(|c| c.get("claim_id").and_then(Value::as_str).is_some_and(|id| !id.is_empty() && Some(id) != mine) && order_key(c).is_some_and(|k| k < me))
        .collect();
    rows.sort_by_key(|c| order_key(c));
    rows
}

impl History for InMemoryHistory {
    fn earlier_claims(&self, claim: &Value) -> Result<Vec<Value>, String> {
        let Some(pid) = claim.get("patient_id").and_then(Value::as_str).filter(|s| !s.is_empty()) else { return Ok(vec![]) };
        Ok(earlier_of(claim, self.by_patient.get(pid).cloned().unwrap_or_default()))
    }
}

// ---- the cleaned view (`facts_extractor.rule_view`) -----------------------------------------------------------------------------------
const TOP_STR: [&str; 9] = ["invoice_number", "member_id", "diagnosis_code", "patient_id", "provider_id", "payer_id", "policy_id", "submission_date", "currency"];
const COVERAGE_STR: [&str; 6] = ["coverage_id", "status", "beneficiary_patient_id", "member_id", "start_date", "end_date"];
const LINE_NUM: [&str; 3] = ["quantity", "unit_price", "net_amount"];
const LINE_STR: [&str; 4] = ["service_code", "service_date", "modifier", "authorization_id"];
const AUTH_STR: [&str; 6] = ["authorization_id", "patient_id", "service_code", "status", "valid_from", "valid_to"];
const ATT_STR: [&str; 7] = ["attachment_id", "type", "patient_id", "service_code", "service_date", "document_status", "text"];

fn clean_num(v: Option<&Value>) -> Value {
    match v {
        Some(n @ Value::Number(_)) if Num::from_json(n).is_some() => n.clone(),
        _ => Value::Null,
    }
}

fn clean_row(row: Option<&Value>, nums: &[&str], strs: &[&str]) -> Map<String, Value> {
    let mut out = match row {
        Some(Value::Object(m)) => m.clone(),
        _ => Map::new(),
    };
    let src = row.and_then(Value::as_object);
    for k in nums {
        out.insert((*k).into(), clean_num(src.and_then(|m| m.get(*k))));
    }
    for k in strs {
        out.insert((*k).into(), match src.and_then(|m| m.get(*k)) { Some(s @ Value::String(_)) => s.clone(), _ => Value::Null });
    }
    out
}

pub fn clean_view(c: &Value) -> Value {
    let mut v = clean_row(Some(c), &["total_amount"], &TOP_STR);
    v.insert("coverage".into(), Value::Object(clean_row(c.get("coverage"), &[], &COVERAGE_STR)));
    for (key, nums, strs) in [("lines", &LINE_NUM[..], &LINE_STR[..]), ("authorizations", &["max_quantity"][..], &AUTH_STR[..]), ("attachments", &[][..], &ATT_STR[..])] {
        if let Some(Value::Array(rows)) = c.get(key) {
            v.insert(key.into(), Value::Array(rows.iter().map(|r| Value::Object(clean_row(Some(r), nums, strs))).collect()));
        }
    }
    Value::Object(v)
}

// ---- catalogue ---------------------------------------------------------------------------------------------------------------------
pub struct Rule {
    pub id: String,
    pub severity: String,
    pub version: String,
    pub source: String,
    pub corrective_action: String,
}

pub struct Catalogue {
    raw: Value,
    rules: Vec<Rule>,
    limits: BTreeMap<String, BigDecimal>,
}

impl Catalogue {
    pub fn load(root: &Path) -> Result<Catalogue, ExtError> {
        let read = |p: PathBuf| -> Result<Value, ExtError> {
            serde_json::from_str(&std::fs::read_to_string(&p).map_err(|e| ExtError::Catalogue(format!("{}: {e}", p.display())))?).map_err(|e| ExtError::Catalogue(e.to_string()))
        };
        let raw = read(root.join("rules/extensions/catalogue.json"))?;
        let services = read(root.join("rules/services.json"))?;
        let limits = services
            .as_object()
            .ok_or_else(|| ExtError::Catalogue("services.json is not an object".into()))?
            .iter()
            .filter_map(|(k, v)| Num::from_json(&v["max_quantity"]).map(|n| (k.clone(), n.to_decimal())))
            .collect();
        let rules = raw["rules"]
            .as_array()
            .ok_or_else(|| ExtError::Catalogue("no rules".into()))?
            .iter()
            .map(|r| Rule { id: text(&r["rule_id"]).into(), severity: text(&r["severity"]).into(), version: text(&r["version"]).into(), source: text(&r["source"]).into(), corrective_action: text(&r["corrective_action"]).into() })
            .collect();
        Ok(Catalogue { raw, rules, limits })
    }

    /// `extension_rules.pack_source`
    pub fn pack_source(&self) -> String {
        let mut blocks = vec![];
        for r in &self.rules {
            for (outcome, pattern) in OUTCOMES {
                blocks.push(format!(
                    "rule {id}_{lo} {{\n  meta:\n    rule_id = \"{id}\"\n    outcome = \"{outcome}\"\n    severity = \"{sev}\"\n    rule_version = \"{ver}\"\n  strings:\n    $m = {pat}\n  condition:\n    $m\n}}\n",
                    id = r.id,
                    lo = outcome.to_lowercase(),
                    sev = r.severity,
                    ver = r.version,
                    pat = pattern.replace("{rid}", &r.id)
                ));
            }
        }
        blocks.join("\n")
    }

    fn list(&self, key: &str) -> Vec<&str> {
        self.raw[key].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default()
    }
}

// ---- helpers on cleaned rows -------------------------------------------------------------------------------------------------------
type Row<'a> = &'a Map<String, Value>;

fn rows<'a>(view: &'a Value, key: &str) -> Vec<(usize, Row<'a>)> {
    match view.get(key) {
        Some(Value::Array(items)) => items.iter().enumerate().filter_map(|(i, r)| r.as_object().map(|m| (i, m))).collect(),
        _ => vec![],
    }
}

fn lines(view: &Value) -> Vec<(usize, Row<'_>)> {
    rows(view, "lines")
}

fn sget<'a>(r: Row<'a>, k: &str) -> Option<&'a str> {
    r.get(k).and_then(Value::as_str)
}

fn day_of(r: Row<'_>, k: &str) -> Option<Day> {
    sget(r, k).and_then(Day::parse)
}

fn line_id(r: Row<'_>) -> Option<String> {
    sget(r, "line_id").filter(|s| !s.is_empty()).map(str::to_string)
}

fn dec(r: Row<'_>, k: &str) -> Option<BigDecimal> {
    r.get(k).and_then(Num::from_json).map(|n| n.to_decimal())
}

/// A quantity or limit usable in a sum: finite and not negative.
fn qty(r: Row<'_>, k: &str) -> Option<BigDecimal> {
    dec(r, k).filter(|d| *d >= BigDecimal::from(0))
}

fn line_paths(i: usize, names: &[&str]) -> Vec<String> {
    names.iter().map(|n| format!("/lines/{i}/{n}")).collect()
}

fn dedupe<T: Clone + Ord>(items: Vec<T>) -> Vec<T> {
    let mut seen = BTreeSet::new();
    items.into_iter().filter(|x| seen.insert(x.clone())).collect()
}

fn dx(view: &Value) -> Option<&str> {
    view.get("diagnosis_code").and_then(Value::as_str).filter(|s| !s.is_empty())
}

fn unknown(rid: &str, message: &str) -> Details {
    Details { facts: vec![format!("{rid}:UNKNOWN")], evidence_paths: vec!["/claim_id".into()], line_ids: vec![], message: message.into() }
}

fn na(rid: &str, message: &str) -> Details {
    Details { facts: vec![format!("{rid}:NA")], evidence_paths: vec!["/claim_id".into()], line_ids: vec![], message: message.into() }
}

fn ok(rid: &str, message: &str) -> Details {
    Details { facts: vec![format!("{rid}:OK")], evidence_paths: vec!["/claim_id".into()], line_ids: vec![], message: message.into() }
}

// ---- E001, E002 --------------------------------------------------------------------------------------------------------------------
struct PairFound<'a> {
    primary: &'a str,
    secondary: &'a str,
    combos: Vec<(usize, usize)>,
    unreadable: bool,
}

fn pair_scan<'a>(view: &Value, cat: &'a Catalogue, only_never: bool) -> Vec<PairFound<'a>> {
    let all = lines(view);
    let mut found = vec![];
    for pair in cat.raw["pairs"].as_array().into_iter().flatten() {
        if only_never && pair["modifier_allowed"] == true {
            continue;
        }
        let (p, s) = (text(&pair["primary"]), text(&pair["secondary"]));
        let prim: Vec<_> = all.iter().filter(|(_, r)| sget(r, "service_code") == Some(p)).collect();
        let sec: Vec<_> = all.iter().filter(|(_, r)| sget(r, "service_code") == Some(s)).collect();
        if prim.is_empty() || sec.is_empty() {
            continue;
        }
        let unreadable = prim.iter().chain(sec.iter()).any(|(_, r)| day_of(r, "service_date").is_none());
        let mut combos = vec![];
        for (pi, pr) in &prim {
            for (si, sr) in &sec {
                if let (Some(a), Some(b)) = (day_of(pr, "service_date"), day_of(sr, "service_date")) {
                    if a == b {
                        combos.push((*pi, *si));
                    }
                }
            }
        }
        found.push(PairFound { primary: p, secondary: s, combos, unreadable });
    }
    found
}

fn is_exception(row: Row<'_>, cat: &Catalogue) -> bool {
    sget(row, "modifier").is_some_and(|m| cat.list("exception_modifiers").contains(&m))
}

fn pair_result(rid: &str, view: &Value, cat: &Catalogue, only_never: bool, violation_if_exception: bool) -> Details {
    let found = pair_scan(view, cat, only_never);
    if found.is_empty() {
        return na(rid, "No listed procedure pair is present on this claim.");
    }
    let all: BTreeMap<usize, Row<'_>> = lines(view).into_iter().collect();
    let (mut facts, mut idx) = (vec![], vec![]);
    for f in &found {
        for (pi, si) in &f.combos {
            if is_exception(all[si], cat) == violation_if_exception {
                facts.push(format!("{rid}:FAIL:{pi},{si}:pair={}|{}", quote(f.primary), quote(f.secondary)));
                idx.push(*pi);
                idx.push(*si);
            }
        }
    }
    let unreadable = found.iter().any(|f| f.unreadable);
    if unreadable {
        facts.push(format!("{rid}:UNKNOWN"));
    }
    let (mut paths, mut ids) = (vec![], vec![]);
    for i in dedupe(idx).into_iter().collect::<BTreeSet<_>>() {
        paths.extend(line_paths(i, &["service_code", "service_date", "modifier"]));
        if let Some(id) = line_id(all[&i]) {
            ids.push(id);
        }
    }
    if facts.iter().any(|x| x.starts_with(&format!("{rid}:FAIL:"))) {
        let message = if rid == "E001" {
            "Two services that form a listed pair were billed on the same date without an exception modifier on the component."
        } else {
            "An exception modifier was used on a pair that does not allow one to separate its services."
        };
        return Details { facts, evidence_paths: paths, line_ids: dedupe(ids), message: message.into() };
    }
    if unreadable {
        let mut up = vec![];
        for f in &found {
            for (i, r) in &all {
                if sget(r, "service_code") == Some(f.primary) || sget(r, "service_code") == Some(f.secondary) {
                    up.extend(line_paths(*i, &["service_code", "service_date"]));
                }
            }
        }
        return Details { facts, evidence_paths: dedupe(up), line_ids: vec![], message: "A line of a listed pair has no readable service date, so the check could not be completed.".into() };
    }
    ok(rid, if rid == "E001" { "Listed pairs are not billed together on one date, or the component carries an exception modifier." } else { "No exception modifier is used on a pair that forbids one." })
}

fn e001(view: &Value, cat: &Catalogue, _h: &Hist) -> Result<Details, String> {
    Ok(pair_result("E001", view, cat, false, false))
}

fn e002(view: &Value, cat: &Catalogue, _h: &Hist) -> Result<Details, String> {
    Ok(pair_result("E002", view, cat, true, true))
}

// ---- E003, E004, E005 --------------------------------------------------------------------------------------------------------------
fn event_dates(text: &str) -> Vec<Day> {
    thread_local! {
        static RE: fancy_regex::Regex = fancy_regex::Regex::new(r"Event date: ?([0-9]{4}-[0-9]{2}-[0-9]{2})(?![0-9])").expect("the event-date pattern compiles");
    }
    RE.with(|re| re.captures_iter(text).flatten().filter_map(|c| c.get(1).and_then(|m| Day::parse(m.as_str()))).collect())
}

fn e003(view: &Value, cat: &Catalogue, _h: &Hist) -> Result<Details, String> {
    let Some(code) = dx(view) else { return Ok(unknown("E003", "The claim has no readable diagnosis code, so the event-date requirement could not be checked.")) };
    if cat.raw["diagnoses"][code]["requires_event_date"] != true {
        return Ok(na("E003", "This diagnosis does not require an event date."));
    }
    let service_dates: Vec<Day> = lines(view).iter().filter_map(|(_, r)| day_of(r, "service_date")).collect();
    let Some(first) = service_dates.iter().min().copied() else {
        return Ok(Details { facts: vec!["E003:UNKNOWN".into()], evidence_paths: vec!["/diagnosis_code".into()], line_ids: vec![], message: "No line has a readable service date, so the event date could not be compared with it.".into() });
    };
    let mut sources: Vec<(String, Option<&str>)> = vec![("/notes".into(), view.get("notes").and_then(Value::as_str))];
    for (i, r) in rows(view, "attachments") {
        sources.push((format!("/attachments/{i}/text"), sget(r, "text")));
    }
    let (mut events, mut paths) = (vec![], vec!["/diagnosis_code".to_string()]);
    for (path, t) in sources {
        let found = t.map(event_dates).unwrap_or_default();
        if !found.is_empty() {
            events.extend(found);
            paths.push(path);
        }
    }
    if events.iter().any(|d| *d <= first) {
        return Ok(Details { facts: vec!["E003:OK".into()], evidence_paths: paths, line_ids: vec![], message: "An event date on or before the first service is stated.".into() });
    }
    if let Some((i, _)) = lines(view).first() {
        paths.push(format!("/lines/{i}/service_date"));
    }
    Ok(Details {
        facts: vec![format!("E003:FAIL:dx={}", quote(code))],
        evidence_paths: paths,
        line_ids: vec![],
        message: "This diagnosis requires an event date (written \"Event date: YYYY-MM-DD\") on or before the first service date, and none was found.".into(),
    })
}

fn e004(view: &Value, cat: &Catalogue, _h: &Hist) -> Result<Details, String> {
    let codes: Vec<&str> = cat.raw["services"].as_object().into_iter().flatten().filter(|(_, s)| s["category"] == "pharmaceutical").map(|(k, _)| k.as_str()).collect();
    let rx: Vec<_> = lines(view).into_iter().filter(|(_, r)| sget(r, "service_code").is_some_and(|c| codes.contains(&c))).collect();
    if rx.is_empty() {
        return Ok(na("E004", "No pharmaceutical line from the extension catalogue is present."));
    }
    let routes = cat.list("route_modifiers");
    let bad: Vec<_> = rx.iter().filter(|(_, r)| !sget(r, "modifier").is_some_and(|m| routes.contains(&m))).collect();
    if bad.is_empty() {
        return Ok(Details {
            facts: vec!["E004:OK".into()],
            evidence_paths: rx.iter().flat_map(|(i, _)| line_paths(*i, &["service_code", "modifier"])).collect(),
            line_ids: vec![],
            message: "Every pharmaceutical line carries a route-of-administration modifier.".into(),
        });
    }
    Ok(Details {
        facts: bad.iter().map(|(i, _)| format!("E004:FAIL:{i}")).collect(),
        evidence_paths: bad.iter().flat_map(|(i, _)| line_paths(*i, &["service_code", "modifier"])).collect(),
        line_ids: bad.iter().filter_map(|(_, r)| line_id(r)).collect(),
        message: "A pharmaceutical line has no valid route-of-administration modifier.".into(),
    })
}

fn e005(view: &Value, cat: &Catalogue, _h: &Hist) -> Result<Details, String> {
    let Some(code) = dx(view) else { return Ok(unknown("E005", "The claim has no readable diagnosis code, so its position could not be checked.")) };
    if cat.raw["diagnoses"][code]["secondary_only"] == true {
        return Ok(Details {
            facts: vec![format!("E005:FAIL:dx={}", quote(code))],
            evidence_paths: vec!["/diagnosis_code".into()],
            line_ids: vec![],
            message: "The header diagnosis is reserved for a secondary position and cannot be the primary reason for the claim.".into(),
        });
    }
    Ok(Details { facts: vec!["E005:OK".into()], evidence_paths: vec!["/diagnosis_code".into()], line_ids: vec![], message: "The header diagnosis may be used as the primary diagnosis.".into() })
}

// ---- E101, E102, E103 --------------------------------------------------------------------------------------------------------------
/// Asks the real history at most once per evaluated claim and remembers the answer (or the failure, so every history rule reports it).
pub struct Once<'a> {
    history: Option<&'a dyn History>,
    cached: RefCell<Option<Result<Vec<Value>, String>>>,
}

type Hist<'a> = Once<'a>;

impl Once<'_> {
    fn present(&self) -> bool {
        self.history.is_some()
    }

    fn views(&self, view: &Value) -> Result<Vec<Value>, String> {
        if self.cached.borrow().is_none() {
            let answer = match self.history {
                None => Ok(vec![]),
                Some(h) => h.earlier_claims(view).and_then(|rows| {
                    if rows.iter().all(Value::is_object) {
                        Ok(rows.iter().map(clean_view).collect())
                    } else {
                        Err("claim history returned data that is not a list of claims".to_string())
                    }
                }),
            };
            *self.cached.borrow_mut() = Some(answer);
        }
        self.cached.borrow().clone().unwrap_or_else(|| Ok(vec![]))
    }
}

fn party(view: &Value) -> Option<&str> {
    let s = |k: &str| view.get(k).and_then(Value::as_str).filter(|x| !x.is_empty());
    let (patient, provider) = (s("patient_id"), s("provider_id"));
    let day = view.get("submission_date").and_then(Value::as_str).and_then(Day::parse);
    if patient.is_some() && day.is_some() { provider } else { None }
}

fn line_key(r: Row<'_>) -> Option<(String, Day, BigDecimal, BigDecimal)> {
    let code = sget(r, "service_code").filter(|c| !c.is_empty())?;
    Some((code.to_string(), day_of(r, "service_date")?, dec(r, "quantity")?, dec(r, "net_amount")?))
}

fn e101(view: &Value, _cat: &Catalogue, h: &Hist) -> Result<Details, String> {
    if !h.present() {
        return Ok(unknown("E101", NO_HISTORY));
    }
    let Some(provider) = party(view) else { return Ok(unknown("E101", NO_PARTY)) };
    let mut seen = BTreeSet::new();
    for prev in h.views(view)? {
        if prev.get("provider_id").and_then(Value::as_str) == Some(provider) {
            seen.extend(lines(&prev).iter().filter_map(|(_, r)| line_key(r)));
        }
    }
    let (mut facts, mut paths, mut ids, mut unreadable) = (vec![], vec![], vec![], false);
    for (i, row) in lines(view) {
        match line_key(row) {
            None => unreadable = true,
            Some(k) if seen.contains(&k) => {
                facts.push(format!("E101:FAIL:{i}"));
                paths.extend(line_paths(i, &["service_code", "service_date", "quantity", "net_amount"]));
                ids.extend(line_id(row));
            }
            _ => {}
        }
    }
    if unreadable {
        facts.push("E101:UNKNOWN".into());
    }
    if facts.iter().any(|f| f.starts_with("E101:FAIL:")) {
        return Ok(Details { facts, evidence_paths: paths, line_ids: ids, message: "A line is identical (patient, provider, service, date, quantity and amount) to a line of an earlier claim.".into() });
    }
    if unreadable {
        return Ok(Details { facts, evidence_paths: vec!["/claim_id".into()], line_ids: vec![], message: "A line has an unreadable service, date, quantity or amount, so it could not be compared with earlier claims.".into() });
    }
    Ok(ok("E101", "No line repeats a line of an earlier claim."))
}

fn e102(view: &Value, _cat: &Catalogue, h: &Hist) -> Result<Details, String> {
    let mut groups: Vec<(String, Vec<(usize, Row<'_>)>)> = vec![];
    for (i, row) in lines(view) {
        if let Some(aid) = sget(row, "authorization_id").filter(|s| !s.is_empty()) {
            match groups.iter_mut().find(|(k, _)| k == aid) {
                Some((_, v)) => v.push((i, row)),
                None => groups.push((aid.to_string(), vec![(i, row)])),
            }
        }
    }
    if groups.is_empty() {
        return Ok(na("E102", "No line references an authorization."));
    }
    if !h.present() {
        return Ok(unknown("E102", NO_HISTORY));
    }
    if party(view).is_none() {
        return Ok(unknown("E102", NO_PARTY));
    }
    let mut records: BTreeMap<String, (usize, Row<'_>)> = BTreeMap::new();
    for (k, a) in rows(view, "authorizations") {
        if let Some(id) = sget(a, "authorization_id") {
            records.entry(id.to_string()).or_insert((k, a));
        }
    }
    let earlier = h.views(view)?;
    let (mut facts, mut paths, mut ids, mut unreadable) = (vec![], vec![], vec![], false);
    for (aid, grp) in &groups {
        let rec = records.get(aid);
        let limit = rec.and_then(|(_, a)| qty(a, "max_quantity"));
        let used: Vec<Option<BigDecimal>> = grp.iter().map(|(_, r)| qty(r, "quantity")).collect();
        let mut before: Vec<Option<BigDecimal>> = vec![];
        for prev in &earlier {
            before.extend(lines(prev).iter().filter(|(_, r)| sget(r, "authorization_id") == Some(aid.as_str())).map(|(_, r)| qty(r, "quantity")));
        }
        let all_known = used.iter().chain(before.iter()).all(Option::is_some);
        let Some(limit) = limit.filter(|_| all_known) else {
            unreadable = true;
            continue;
        };
        let sum = |v: &[Option<BigDecimal>]| v.iter().flatten().fold(BigDecimal::from(0), |a, b| a + b);
        let (b, u) = (sum(&before), sum(&used));
        if b > BigDecimal::from(0) && &b + &u > limit {
            facts.push(format!("E102:FAIL:{}", quote(aid)));
            for (i, r) in grp {
                paths.extend(line_paths(*i, &["authorization_id", "quantity"]));
                ids.extend(line_id(r));
            }
            paths.push(format!("/authorizations/{}/max_quantity", rec.map(|(k, _)| *k).unwrap_or(0)));
        }
    }
    if unreadable {
        facts.push("E102:UNKNOWN".into());
    }
    if facts.iter().any(|f| f.starts_with("E102:FAIL:")) {
        return Ok(Details { facts, evidence_paths: paths, line_ids: dedupe(ids), message: "Units under this authorization in earlier claims plus this claim exceed its approved maximum.".into() });
    }
    if unreadable {
        return Ok(Details { facts, evidence_paths: vec!["/claim_id".into()], line_ids: vec![], message: "An authorization record, its maximum, or a quantity is missing or unreadable, so the total could not be checked.".into() });
    }
    Ok(ok("E102", "Units under every referenced authorization stay within its approved maximum."))
}

fn e103(view: &Value, cat: &Catalogue, h: &Hist) -> Result<Details, String> {
    let limits = &cat.limits;
    let mut groups: Vec<((String, Day), Vec<(usize, Row<'_>, BigDecimal)>)> = vec![];
    let mut unreadable = false;
    for (i, row) in lines(view) {
        let Some(code) = sget(row, "service_code").filter(|c| limits.contains_key(*c)) else { continue };
        let (Some(day), Some(q)) = (day_of(row, "service_date"), qty(row, "quantity")) else {
            unreadable = true;
            continue;
        };
        let key = (code.to_string(), day);
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, v)) => v.push((i, row, q)),
            None => groups.push((key, vec![(i, row, q)])),
        }
    }
    if groups.is_empty() && !unreadable {
        return Ok(na("E103", "No line uses a service with a daily limit."));
    }
    if !h.present() {
        return Ok(unknown("E103", NO_HISTORY));
    }
    let Some(provider) = party(view) else { return Ok(unknown("E103", NO_PARTY)) };
    let (mut earlier_units, mut earlier_bad): (BTreeMap<(String, Day), BigDecimal>, BTreeSet<(String, Day)>) = (BTreeMap::new(), BTreeSet::new());
    for prev in h.views(view)? {
        if prev.get("provider_id").and_then(Value::as_str) != Some(provider) {
            continue;
        }
        for (_, r) in lines(&prev) {
            let Some(code) = sget(r, "service_code").filter(|c| limits.contains_key(*c)) else { continue };
            let Some(day) = day_of(r, "service_date") else { continue };
            match qty(r, "quantity") {
                Some(q) => *earlier_units.entry((code.to_string(), day)).or_insert_with(|| BigDecimal::from(0)) += q,
                None => {
                    earlier_bad.insert((code.to_string(), day));
                }
            }
        }
    }
    unreadable = unreadable || groups.iter().any(|(k, _)| earlier_bad.contains(k));
    let (mut facts, mut paths, mut ids) = (vec![], vec![], vec![]);
    for ((code, day), grp) in &groups {
        let key = (code.clone(), *day);
        if earlier_bad.contains(&key) {
            continue;
        }
        let before = earlier_units.get(&key).cloned().unwrap_or_else(|| BigDecimal::from(0));
        let total = grp.iter().fold(BigDecimal::from(0), |a, (_, _, q)| a + q);
        if before > BigDecimal::from(0) && &before + &total > limits[code] {
            facts.push(format!("E103:FAIL:{}:{}", quote(code), day));
            for (i, r, _) in grp {
                paths.extend(line_paths(*i, &["service_code", "service_date", "quantity"]));
                ids.extend(line_id(r));
            }
        }
    }
    if unreadable {
        facts.push("E103:UNKNOWN".into());
    }
    if facts.iter().any(|f| f.starts_with("E103:FAIL:")) {
        return Ok(Details { facts, evidence_paths: paths, line_ids: ids, message: "Units of this service on this date for this patient and provider, in earlier claims plus this claim, exceed the daily limit.".into() });
    }
    if unreadable {
        return Ok(Details { facts, evidence_paths: vec!["/claim_id".into()], line_ids: vec![], message: "A line has an unreadable date or quantity, so the daily total could not be checked.".into() });
    }
    Ok(ok("E103", "Daily totals across claims stay within each service limit."))
}

type DetailFn = fn(&Value, &Catalogue, &Hist) -> Result<Details, String>;
const DETAIL_FUNCS: [(&str, DetailFn); 8] = [("E001", e001), ("E002", e002), ("E003", e003), ("E004", e004), ("E005", e005), ("E101", e101), ("E102", e102), ("E103", e103)];

// ---- assembly ------------------------------------------------------------------------------------------------------------------------
pub struct Extensions {
    pub cat: Catalogue,
    pack: Pack,
}

impl Extensions {
    pub fn load(root: &Path) -> Result<Extensions, ExtError> {
        let cat = Catalogue::load(root)?;
        let pack = Pack::from_source(cat.pack_source(), root.join("rules/extensions/catalogue.json"))?;
        Ok(Extensions { cat, pack })
    }

    /// `extension_rules.pack_hash`: sha256 of the generated pack source.
    pub fn pack_hash(&self) -> String {
        hex::encode(Sha256::digest(self.cat.pack_source().as_bytes()))
    }

    fn evidence(claim: &Value, paths: &[String]) -> Vec<Value> {
        let mut seen = BTreeSet::new();
        let mut out: Vec<Value> = paths.iter().filter(|p| seen.insert((*p).clone())).filter_map(|p| pointer(claim, p).map(|v| json!({"path": p, "value": v}))).collect();
        if out.is_empty() {
            out.push(json!({"path": "/claim_id", "value": claim.get("claim_id").cloned().unwrap_or(Value::Null)}));
        }
        out
    }

    /// The eight advisory results for one claim. Never panics for an object claim: a failure inside one rule makes that rule
    /// UNABLE_TO_ASSESS and is appended to `tool_errors`.
    pub fn evaluate(&self, claim: &Value, history: Option<&dyn History>, tool_errors: &mut Vec<String>) -> Result<Vec<Value>, ExtError> {
        let view = clean_view(claim);
        let once = Once { history, cached: RefCell::new(None) };
        let mut details: Vec<(&str, Result<Details, String>)> = vec![];
        for (rid, f) in DETAIL_FUNCS {
            let r = catch_unwind(AssertUnwindSafe(|| f(&view, &self.cat, &once)))
                .map_err(|e| e.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| e.downcast_ref::<String>().cloned()).unwrap_or_else(|| "panic".into()))
                .and_then(|r| r);
            if let Err(e) = &r {
                tool_errors.push(format!("{rid}: engine exception {e}"));
            }
            details.push((rid, r));
        }
        let mut blob = String::new();
        for (_, d) in details.iter().filter_map(|(r, d)| d.as_ref().ok().map(|d| (r, d))) {
            for fact in &d.facts {
                blob.push_str(fact);
                blob.push('\n');
            }
        }
        let matched = self.pack.scan(&blob)?;
        let phash = self.pack_hash();
        let claim_id = claim.get("claim_id").filter(|v| v.is_string()).cloned().unwrap_or_else(|| json!(""));
        let mut results = vec![];
        for (rid, outcome) in details {
            let def = self.cat.rules.iter().find(|r| r.id == rid).ok_or_else(|| ExtError::OutOfSync(rid.into()))?;
            let (status, d) = match outcome {
                Err(_) => ("UNABLE_TO_ASSESS", Details { facts: vec![], evidence_paths: vec!["/claim_id".into()], line_ids: vec![], message: "An internal error prevented this check; it is treated as unable to assess.".into() }),
                Ok(d) => {
                    let set = matched.get(rid).ok_or_else(|| ExtError::OutOfSync(rid.into()))?;
                    (strongest(set).ok_or_else(|| ExtError::OutOfSync(rid.into()))?, d)
                }
            };
            let flagged = status == "FAIL" || status == "UNABLE_TO_ASSESS";
            results.push(json!({
                "claim_id": claim_id, "rule_id": rid, "rule_version": def.version, "status": status, "severity": def.severity,
                "affected_line_ids": d.line_ids, "evidence": Self::evidence(claim, &d.evidence_paths), "rule_source": def.source,
                "explanation": d.message, "corrective_action": if flagged { def.corrective_action.clone() } else { String::new() },
                "confidence": null, "confidence_kind": "not_probabilistic", "requires_human_review": flagged,
                "method": "deterministic", "review_status": "unreviewed", "rule_family": "extension", "advisory": true, "pack_hash": phash,
            }));
        }
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
    }

    #[test]
    fn the_generated_pack_is_the_committed_pack_and_has_the_python_hash() {
        let ext = Extensions::load(&root()).unwrap();
        let committed = std::fs::read_to_string(root().join("rules/extensions.yar")).unwrap().replace("\r\n", "\n");
        assert_eq!(ext.cat.pack_source().trim_end(), committed.trim_end());
        assert_eq!(ext.pack_hash(), "dafef98d515ff4fda819970692efd5443532164921e8403bcd7cd1df9679eda1", "the Python build computes this hash");
    }

    #[test]
    fn a_missing_history_makes_the_cross_claim_rules_unable_not_pass() {
        let ext = Extensions::load(&root()).unwrap();
        let claim = json!({"claim_id": "C1", "patient_id": "P", "provider_id": "V", "submission_date": "2026-03-10", "diagnosis_code": "DX-EDU-01",
            "lines": [{"line_id": "L1", "service_code": "SVC-EDU-01", "service_date": "2026-03-02", "quantity": 1, "net_amount": 10, "authorization_id": "A1"}], "authorizations": []});
        let r = ext.evaluate(&claim, None, &mut vec![]).unwrap();
        let st = |id: &str| r.iter().find(|x| x["rule_id"] == id).unwrap()["status"].as_str().unwrap().to_string();
        assert_eq!((st("E101").as_str(), st("E102").as_str()), ("UNABLE_TO_ASSESS", "UNABLE_TO_ASSESS"));
        assert_eq!(r.len(), 8);
        assert!(r.iter().all(|x| x["advisory"] == true && x["rule_family"] == "extension"));
    }

    #[test]
    fn a_history_that_returns_garbage_is_unable_to_assess_and_recorded() {
        struct Broken;
        impl History for Broken {
            fn earlier_claims(&self, _: &Value) -> Result<Vec<Value>, String> {
                Ok(vec![json!(5)])
            }
        }
        let ext = Extensions::load(&root()).unwrap();
        let claim = json!({"claim_id": "C1", "patient_id": "P", "provider_id": "V", "submission_date": "2026-03-10",
            "lines": [{"line_id": "L1", "service_code": "SVC-EDU-01", "service_date": "2026-03-02", "quantity": 1, "net_amount": 10}]});
        let mut errors = vec![];
        let r = ext.evaluate(&claim, Some(&Broken), &mut errors).unwrap();
        assert_eq!(r.iter().find(|x| x["rule_id"] == "E101").unwrap()["status"], "UNABLE_TO_ASSESS");
        assert!(errors.iter().any(|e| e.starts_with("E101:")));
    }
}
