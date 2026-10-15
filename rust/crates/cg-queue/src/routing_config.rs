//! The routing configuration (`src/workqueue/routing_config.py`): every number that decides where a claim goes and how much work an
//! agent holds. Nothing here is a constant of the code paths. An administrator changes these values (through the queue service, which
//! writes a new numbered version and records before and after in the security log), each triage receipt names the version it used, and
//! an invalid configuration is refused whole, never applied in part.

use serde_json::{json, Map, Value};

pub const STATUSES: [&str; 2] = ["FAIL", "UNABLE_TO_ASSESS"];
pub const SEVERITIES: [&str; 2] = ["high", "medium"];
const NAMES: [&str; 22] = [
    "version", "points", "lane_b_flagged", "lane_b_score", "slice_size", "low_water", "lease_seconds", "aging_per_hour", "ai_per_minute", "ai_daily_budget", "on_shift", "exclusions",
    "learn_enabled", "route_by_reliability", "reliability_min_decisions", "reliability_min_reviewers", "reliability_reviewer_cap", "doubtful_below", "uncertainty_weight",
    "auto_clear_green", "audit_percent", "bulk_confirm_skip_lower",
];

/// Points per flagged finding: `[status][severity]`, statuses in `STATUSES` order, severities in `SEVERITIES` order.
#[derive(Debug, Clone, PartialEq)]
pub struct Points(pub [[i64; 2]; 2]);

impl Points {
    pub fn get(&self, status: &str, severity: &str) -> i64 {
        let s = STATUSES.iter().position(|x| *x == status).unwrap_or(0);
        let v = SEVERITIES.iter().position(|x| *x == severity).unwrap_or(0);
        self.0[s][v]
    }

    fn to_doc(&self) -> Value {
        let mut m = Map::new();
        for (i, status) in STATUSES.iter().enumerate() {
            m.insert((*status).into(), json!({"high": self.0[i][0], "medium": self.0[i][1]}));
        }
        Value::Object(m)
    }

    fn from_doc(v: &Value) -> Result<Points, String> {
        let obj = v.as_object().filter(|o| o.len() == 2 && STATUSES.iter().all(|s| o.contains_key(*s))).ok_or("points must hold exactly FAIL and UNABLE_TO_ASSESS")?;
        let mut out = [[0i64; 2]; 2];
        for (i, status) in STATUSES.iter().enumerate() {
            let row = obj[*status].as_object().filter(|r| r.len() == 2 && SEVERITIES.iter().all(|s| r.contains_key(*s))).ok_or(format!("points for {status} must hold exactly high and medium"))?;
            for (j, sev) in SEVERITIES.iter().enumerate() {
                out[i][j] = int_in(&format!("points {status} {sev}"), &row[*sev], 0, 100)?;
            }
        }
        Ok(Points(out))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RoutingConfig {
    pub version: i64,
    pub points: Points,
    pub lane_b_flagged: i64,
    pub lane_b_score: i64,
    pub slice_size: i64,
    pub low_water: i64,
    pub lease_seconds: i64,
    pub aging_per_hour: f64,
    pub ai_per_minute: i64,
    pub ai_daily_budget: i64,
    pub on_shift: Vec<String>,
    pub exclusions: Vec<(String, String)>,
    pub learn_enabled: bool,
    pub route_by_reliability: bool,
    pub reliability_min_decisions: i64,
    pub reliability_min_reviewers: i64,
    pub reliability_reviewer_cap: f64,
    pub doubtful_below: f64,
    pub uncertainty_weight: f64,
    /// A green claim (nothing flagged, result set intact) is decided by the system instead of waiting for a person, except for the random audit sample.
    pub auto_clear_green: bool,
    /// Percent of green claims still sent to a person while `auto_clear_green` is on: the measurement of what the engine might miss.
    pub audit_percent: i64,
    /// 0 = no bulk confirm. Otherwise a rule whose exact lower bound of the upheld rate is at least this may be confirmed in bulk after a lighter look.
    pub bulk_confirm_skip_lower: f64,
}

impl Default for RoutingConfig {
    fn default() -> Self {
        RoutingConfig {
            version: 1,
            points: Points([[4, 2], [2, 1]]),
            lane_b_flagged: 4,
            lane_b_score: 10,
            slice_size: 25,
            low_water: 10,
            lease_seconds: 1800,
            aging_per_hour: 0.5,
            ai_per_minute: 30,
            ai_daily_budget: 2000,
            on_shift: vec![],
            exclusions: vec![],
            learn_enabled: false,
            route_by_reliability: false,
            reliability_min_decisions: 20,
            reliability_min_reviewers: 3,
            reliability_reviewer_cap: 0.4,
            doubtful_below: 0.6,
            uncertainty_weight: 0.0,
            auto_clear_green: false,
            audit_percent: 5,
            bulk_confirm_skip_lower: 0.0,
        }
    }
}

fn int_in(name: &str, v: &Value, low: i64, high: i64) -> Result<i64, String> {
    match v {
        Value::Number(n) if n.is_i64() || n.is_u64() => n.as_i64().filter(|x| (low..=high).contains(x)).ok_or_else(|| format!("{name} must be a whole number from {low} to {high}")),
        _ => Err(format!("{name} must be a whole number from {low} to {high}")),
    }
}

fn check_range(name: &str, v: i64, low: i64, high: i64) -> Result<(), String> {
    if (low..=high).contains(&v) { Ok(()) } else { Err(format!("{name} must be a whole number from {low} to {high}")) }
}

/// Ok(()) or the first problem.
pub fn validate(c: &RoutingConfig) -> Result<(), String> {
    check_range("version", c.version, 1, 1_000_000_000)?;
    for (i, status) in STATUSES.iter().enumerate() {
        for (j, sev) in SEVERITIES.iter().enumerate() {
            check_range(&format!("points {status} {sev}"), c.points.0[i][j], 0, 100)?;
        }
    }
    for (j, sev) in SEVERITIES.iter().enumerate() {
        if c.points.0[0][j] < c.points.0[1][j] {
            return Err(format!("a failed {sev} finding must not score less than an unassessable one"));
        }
    }
    check_range("lane_b_flagged", c.lane_b_flagged, 1, 15)?;
    check_range("lane_b_score", c.lane_b_score, 1, 1000)?;
    check_range("slice_size", c.slice_size, 1, 200)?;
    check_range("low_water", c.low_water, 1, 200)?;
    if c.low_water > c.slice_size {
        return Err("low_water must not exceed slice_size".into());
    }
    check_range("lease_seconds", c.lease_seconds, 60, 86400)?;
    if !c.aging_per_hour.is_finite() || !(0.0..=100.0).contains(&c.aging_per_hour) {
        return Err("aging_per_hour must be a number from 0 to 100".into());
    }
    check_range("ai_per_minute", c.ai_per_minute, 0, 600)?;
    check_range("ai_daily_budget", c.ai_daily_budget, 0, 100000)?;
    if c.on_shift.iter().any(String::is_empty) {
        return Err("on_shift must be a tuple of non-empty badge ids".into());
    }
    let mut seen = std::collections::HashSet::new();
    if !c.on_shift.iter().all(|b| seen.insert(b)) {
        return Err("on_shift must not repeat a badge".into());
    }
    if c.exclusions.iter().any(|(a, b)| a.is_empty() || b.is_empty()) {
        return Err("each exclusion must be a (badge, patient) pair of non-empty text".into());
    }
    check_range("reliability_min_decisions", c.reliability_min_decisions, 5, 1000)?;
    check_range("reliability_min_reviewers", c.reliability_min_reviewers, 1, 50)?;
    for (name, v, low, high) in [("reliability_reviewer_cap", c.reliability_reviewer_cap, 0.1, 1.0), ("doubtful_below", c.doubtful_below, 0.05, 0.95), ("uncertainty_weight", c.uncertainty_weight, 0.0, 20.0)] {
        if !v.is_finite() || !(low..=high).contains(&v) {
            return Err(format!("{name} must be a number from {low} to {high}"));
        }
    }
    check_range("audit_percent", c.audit_percent, 1, 100)?;
    if !c.bulk_confirm_skip_lower.is_finite() || !(c.bulk_confirm_skip_lower == 0.0 || (0.8..=0.999).contains(&c.bulk_confirm_skip_lower)) {
        return Err("bulk_confirm_skip_lower must be 0 (off) or a number from 0.8 to 0.999".into());
    }
    Ok(())
}

/// Plain data for storage and the audit log.
pub fn to_doc(c: &RoutingConfig) -> Result<Value, String> {
    validate(c)?;
    Ok(json!({"version": c.version, "points": c.points.to_doc(), "lane_b_flagged": c.lane_b_flagged, "lane_b_score": c.lane_b_score, "slice_size": c.slice_size,
        "low_water": c.low_water, "lease_seconds": c.lease_seconds, "aging_per_hour": c.aging_per_hour, "ai_per_minute": c.ai_per_minute, "ai_daily_budget": c.ai_daily_budget,
        "on_shift": c.on_shift, "exclusions": c.exclusions.iter().map(|(a, b)| json!([a, b])).collect::<Vec<_>>(),
        "learn_enabled": c.learn_enabled, "route_by_reliability": c.route_by_reliability, "reliability_min_decisions": c.reliability_min_decisions, "reliability_min_reviewers": c.reliability_min_reviewers,
        "reliability_reviewer_cap": c.reliability_reviewer_cap, "doubtful_below": c.doubtful_below, "uncertainty_weight": c.uncertainty_weight,
        "auto_clear_green": c.auto_clear_green, "audit_percent": c.audit_percent, "bulk_confirm_skip_lower": c.bulk_confirm_skip_lower}))
}

/// Fields left out take their default; unknown fields are refused; the result is validated whole.
pub fn from_doc(doc: &Value) -> Result<RoutingConfig, String> {
    let obj = doc.as_object().filter(|o| o.keys().all(|k| NAMES.contains(&k.as_str()))).ok_or("unknown routing configuration field")?;
    let mut c = RoutingConfig::default();
    let int = |name: &str, low: i64, high: i64| -> Result<Option<i64>, String> { obj.get(name).map(|v| int_in(name, v, low, high)).transpose() };
    if let Some(v) = obj.get("version") { c.version = int_in("version", v, 1, 1_000_000_000)?; }
    if let Some(v) = obj.get("points") { c.points = Points::from_doc(v)?; }
    if let Some(v) = int("lane_b_flagged", 1, 15)? { c.lane_b_flagged = v; }
    if let Some(v) = int("lane_b_score", 1, 1000)? { c.lane_b_score = v; }
    if let Some(v) = int("slice_size", 1, 200)? { c.slice_size = v; }
    if let Some(v) = int("low_water", 1, 200)? { c.low_water = v; }
    if let Some(v) = int("lease_seconds", 60, 86400)? { c.lease_seconds = v; }
    if let Some(v) = obj.get("aging_per_hour") {
        c.aging_per_hour = match v { Value::Number(n) => n.as_f64().ok_or("aging_per_hour must be a number from 0 to 100")?, _ => return Err("aging_per_hour must be a number from 0 to 100".into()) };
    }
    if let Some(v) = int("ai_per_minute", 0, 600)? { c.ai_per_minute = v; }
    if let Some(v) = int("ai_daily_budget", 0, 100000)? { c.ai_daily_budget = v; }
    let boolean = |name: &str| -> Result<Option<bool>, String> { obj.get(name).map(|v| v.as_bool().ok_or(format!("{name} must be true or false"))).transpose() };
    let float = |name: &str| -> Result<Option<f64>, String> { obj.get(name).map(|v| match v { Value::Number(n) => n.as_f64().ok_or(format!("{name} must be a number")), _ => Err(format!("{name} must be a number")) }).transpose() };
    if let Some(v) = boolean("learn_enabled")? { c.learn_enabled = v; }
    if let Some(v) = boolean("route_by_reliability")? { c.route_by_reliability = v; }
    if let Some(v) = int("reliability_min_decisions", 5, 1000)? { c.reliability_min_decisions = v; }
    if let Some(v) = int("reliability_min_reviewers", 1, 50)? { c.reliability_min_reviewers = v; }
    if let Some(v) = float("reliability_reviewer_cap")? { c.reliability_reviewer_cap = v; }
    if let Some(v) = float("doubtful_below")? { c.doubtful_below = v; }
    if let Some(v) = float("uncertainty_weight")? { c.uncertainty_weight = v; }
    if let Some(v) = boolean("auto_clear_green")? { c.auto_clear_green = v; }
    if let Some(v) = int("audit_percent", 1, 100)? { c.audit_percent = v; }
    if let Some(v) = float("bulk_confirm_skip_lower")? { c.bulk_confirm_skip_lower = v; }
    if let Some(v) = obj.get("on_shift") {
        c.on_shift = v.as_array().ok_or("on_shift must be a tuple of non-empty badge ids")?.iter().map(|b| b.as_str().map(str::to_string).ok_or("on_shift must be a tuple of non-empty badge ids")).collect::<Result<_, _>>()?;
    }
    if let Some(v) = obj.get("exclusions") {
        c.exclusions = v
            .as_array()
            .ok_or("exclusions must be a tuple of (badge, patient) pairs")?
            .iter()
            .map(|p| match p.as_array().map(Vec::as_slice) {
                Some([Value::String(a), Value::String(b)]) => Ok((a.clone(), b.clone())),
                _ => Err("each exclusion must be a (badge, patient) pair of non-empty text".to_string()),
            })
            .collect::<Result<_, _>>()?;
    }
    validate(&c)?;
    Ok(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_valid_and_round_trips() {
        let c = RoutingConfig::default();
        validate(&c).unwrap();
        let doc = to_doc(&c).unwrap();
        assert_eq!(doc["points"]["FAIL"]["high"], 4);
        assert_eq!(from_doc(&doc).unwrap(), c);
    }

    #[test]
    fn partial_documents_take_defaults_and_unknown_fields_are_refused() {
        let c = from_doc(&json!({"slice_size": 6, "low_water": 3, "on_shift": ["CG-2002"], "exclusions": [["CG-2002", "P1"]]})).unwrap();
        assert_eq!((c.slice_size, c.low_water, c.on_shift.clone(), c.exclusions.clone()), (6, 3, vec!["CG-2002".to_string()], vec![("CG-2002".to_string(), "P1".to_string())]));
        assert!(from_doc(&json!({"slice": 6})).is_err());
        assert!(from_doc(&json!("x")).is_err());
    }

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

    #[test]
    fn the_workload_switches_default_off_round_trip_and_refuse_bad_values() {
        let c = RoutingConfig::default();
        assert!(!c.auto_clear_green && c.audit_percent == 5 && c.bulk_confirm_skip_lower == 0.0, "nothing changes who looks at a claim until an administrator says so");
        assert_eq!(from_doc(&to_doc(&c).unwrap()).unwrap(), c);
        let on = from_doc(&json!({"auto_clear_green": true, "audit_percent": 10, "bulk_confirm_skip_lower": 0.95})).unwrap();
        assert!(on.auto_clear_green && on.audit_percent == 10 && on.bulk_confirm_skip_lower == 0.95);
        assert!(from_doc(&json!({"slice_size": 6, "low_water": 3})).unwrap().audit_percent == 5, "a document written before the fields existed loads with the defaults");
        for bad in [json!({"audit_percent": 0}), json!({"audit_percent": 101}), json!({"audit_percent": 2.5}), json!({"auto_clear_green": 1}), json!({"auto_clear_green": "yes"}),
                    json!({"bulk_confirm_skip_lower": 0.5}), json!({"bulk_confirm_skip_lower": 0.79}), json!({"bulk_confirm_skip_lower": 1.0}), json!({"bulk_confirm_skip_lower": -0.1}), json!({"bulk_confirm_skip_lower": "x"})] {
            assert!(from_doc(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn bad_values_are_refused_whole() {
        for bad in [json!({"slice_size": 0}), json!({"slice_size": 201}), json!({"slice_size": 4.0}), json!({"slice_size": true}), json!({"slice_size": 5, "low_water": 6}),
                    json!({"lease_seconds": 59}), json!({"aging_per_hour": -1}), json!({"aging_per_hour": "x"}), json!({"ai_per_minute": 601}),
                    json!({"on_shift": ["a", "a"]}), json!({"on_shift": [""]}), json!({"on_shift": [1]}), json!({"exclusions": [["a"]]}), json!({"exclusions": [["a", ""]]}),
                    json!({"points": {"FAIL": {"high": 1, "medium": 1}}}), json!({"points": {"FAIL": {"high": 1, "medium": 1}, "UNABLE_TO_ASSESS": {"high": 5, "medium": 1}}}),
                    json!({"points": {"FAIL": {"high": 101, "medium": 1}, "UNABLE_TO_ASSESS": {"high": 1, "medium": 1}}}), json!({"lane_b_flagged": 16}), json!({"version": 0})] {
            assert!(from_doc(&bad).is_err(), "{bad}");
        }
    }
}
