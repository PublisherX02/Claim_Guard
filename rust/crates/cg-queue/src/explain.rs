//! The AI explanation step. It can only ever add words; it can never hold a claim up (`src/workqueue/explain.py`).
//!
//! For each flagged finding the step asks a model for an explanation *template*: text with the placeholders `{value}` and `{line}` in
//! place of anything from the claim. The model never sees a claim value, which is also what makes the answer cacheable: the template is
//! keyed by (rule, failure shape, prompt version, model), claim values are filled in afterwards, and the grounding guard runs on the
//! *filled* text every time, cached or not. Only guard-approved text is cached.
//!
//! Whatever goes wrong (budget spent, rate limit, breaker open, 90-second ceiling passed, model error, guard refusal) the claim keeps the
//! engine's own deterministic explanation, moves on to `explanation_skipped`, and the next step carries on. The step does not fail for
//! any model behaviour.

use crate::breaker::{CallError, CircuitBreaker, ModelError};
use crate::routing_config::RoutingConfig;
use crate::store::QueueStore;
use crate::triage;
use cg_access::Clock;
use fancy_regex::Regex;
use rand::Rng;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;

pub const PLACEHOLDERS: [&str; 2] = ["{value}", "{line}"];
pub const MAX_TEMPLATE_CHARS: usize = 1500;
pub const MAX_VALUE_CHARS: usize = 200;

/// A model that answers a template request. It never sees a claim value.
pub trait Model: Send + Sync {
    fn ask(&self, request: &Value, deadline: f64) -> Result<String, ModelError>;
    fn name(&self) -> String;
}

/// The model used when none is configured: every claim keeps the engine's own explanation.
pub struct NoModel;

impl Model for NoModel {
    fn ask(&self, _: &Value, _: f64) -> Result<String, ModelError> {
        Err(ModelError::NotConfigured)
    }
    fn name(&self) -> String {
        "none".into()
    }
}

pub type Guard = Arc<dyn Fn(&str, &Value) -> bool + Send + Sync>;

/// The grounding guard applied to a filled-in explanation.
pub fn default_guard() -> Guard {
    Arc::new(|text, result| crate::guard::check_grounding(text, result, None).is_ok())
}

/// The engine's own explanation of a finding (what a claim keeps whenever the AI step is skipped).
pub fn deterministic_text(result: &Value) -> String {
    match result["explanation"].as_str() {
        Some(t) if !t.trim().is_empty() => t.to_string(),
        _ => format!("{}: {}", py_str(&result["rule_id"]), py_str(&result["status"])),
    }
}

/// Python's `str(value)` for the JSON shapes that can appear in evidence.
pub fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => match cg_core::Num::from_text(&n.to_string()) {
            Some(cg_core::Num::Int(i)) => i.to_string(),
            Some(cg_core::Num::Float(f)) => cg_audit::canonical::py_float_repr(f),
            None => n.to_string(),
        },
        Value::Array(a) => format!("[{}]", a.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(m) => format!("{{{}}}", m.iter().map(|(k, v)| format!("{}: {}", py_repr(&Value::String(k.clone())), py_repr(v))).collect::<Vec<_>>().join(", ")),
    }
}

fn py_repr(v: &Value) -> String {
    match v {
        Value::String(s) => {
            let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
            let mut out = String::from(quote);
            for c in s.chars() {
                match c {
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    c if c == quote => {
                        out.push('\\');
                        out.push(c);
                    }
                    c => out.push(c),
                }
            }
            out.push(quote);
            out
        }
        other => py_str(other),
    }
}

/// Full jitter: a random wait between zero and the capped exponential.
pub fn backoff(attempt: u32, base: f64, cap: f64, rng: &mut impl Rng) -> f64 {
    rng.gen_range(0.0..=cap.min(base * 2f64.powi(attempt as i32)))
}

pub fn failure_shape(result: &Value) -> String {
    let lines = result["affected_line_ids"].as_array().map_or(0, Vec::len);
    format!("{}/{}/{}", py_str(&result["status"]), py_str(&result["severity"]), match lines { 0 => "0", 1 => "1", _ => "n" })
}

pub fn template_key(rule_id: &str, shape: &str, prompt_version: &str, model_name: &str) -> String {
    hex::encode(Sha256::digest([rule_id, shape, prompt_version, model_name].join("|").as_bytes()))
}

/// A claim's value made safe to show inside a sentence: control characters, zero-width and direction-changing characters are removed
/// (they can hide text or reverse it on screen), white space is collapsed, quotation marks are dropped so the value cannot close its own
/// quote, the length is capped, and anything that is not a plain code, number or date is put in quotation marks so that it reads as data
/// the submitter wrote and never as part of the explanation.
pub fn display_value(raw: &str) -> String {
    use unicode_general_category::{get_general_category, GeneralCategory};
    let kept: String = raw
        .chars()
        .filter(|c| !matches!(get_general_category(*c), GeneralCategory::Format | GeneralCategory::Control | GeneralCategory::PrivateUse | GeneralCategory::Surrogate | GeneralCategory::Unassigned) || c.is_whitespace())
        .filter(|c| !matches!(*c, '\u{201c}' | '\u{201d}' | '"' | '`'))
        .map(|c| match c {
            '<' => '\u{2039}', // markup brackets become look-alike angle quotes: harmless as text, never a tag
            '>' => '\u{203a}',
            other => other,
        })
        .collect();
    let collapsed: String = kept.split_whitespace().collect::<Vec<_>>().join(" ");
    let capped: String = collapsed.chars().take(MAX_VALUE_CHARS).collect();
    let plain = !capped.is_empty() && capped.chars().count() <= 64 && capped.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':' | '/'));
    if capped.is_empty() || plain { capped } else { format!("\u{201c}{capped}\u{201d}") }
}

/// Puts this finding's own values into a template. One pass, plain replacement: a value is never searched for placeholders.
pub fn fill(template: &str, result: &Value) -> String {
    let value = result["evidence"].as_array().and_then(|e| e.first()).filter(|e| e.is_object()).map(|e| py_str(&e["value"])).unwrap_or_default();
    let value = display_value(&value);
    let line = display_value(&result["affected_line_ids"].as_array().and_then(|l| l.first()).map(py_str).unwrap_or_default());
    let re = Regex::new(r"\{value\}|\{line\}").expect("static pattern");
    let mut out = String::new();
    let mut last = 0;
    for m in re.find_iter(template).flatten() {
        out.push_str(&template[last..m.start()]);
        out.push_str(if m.as_str() == "{value}" { &value } else { &line });
        last = m.end();
    }
    out.push_str(&template[last..]);
    out
}

pub struct ExplainStep {
    pub store: Arc<dyn QueueStore>,
    pub model: Arc<dyn Model>,
    pub guard: Guard,
    pub breaker: Arc<CircuitBreaker>,
    pub clock: Clock,
    pub cfg_source: Arc<dyn Fn() -> RoutingConfig + Send + Sync>,
    pub sleep: Arc<dyn Fn(f64) + Send + Sync>,
    pub max_attempts: u32,
    pub prompt_version: String,
    pub deadline_seconds: f64,
}

impl ExplainStep {
    pub fn new(store: Arc<dyn QueueStore>, model: Arc<dyn Model>, guard: Guard, breaker: Arc<CircuitBreaker>, clock: Clock, cfg_source: Arc<dyn Fn() -> RoutingConfig + Send + Sync>) -> ExplainStep {
        ExplainStep {
            store, model, guard, breaker, clock, cfg_source, sleep: Arc::new(|s| std::thread::sleep(std::time::Duration::from_secs_f64(s.max(0.0)))), max_attempts: 3,
            prompt_version: "2.1.0".into(), deadline_seconds: 90.0,
        }
    }

    pub fn model_name(&self) -> String {
        self.model.name()
    }

    fn now(&self) -> f64 {
        (self.clock)()
    }

    fn limits_ok(&self, cfg: &RoutingConfig, now: f64) -> Option<&'static str> {
        if !self.store.bump("ai_day", &((now / 86400.0) as i64).to_string(), cfg.ai_daily_budget).unwrap_or(false) {
            return Some("skipped_budget");
        }
        if !self.store.bump("ai_minute", &((now / 60.0) as i64).to_string(), cfg.ai_per_minute).unwrap_or(false) {
            return Some("skipped_rate");
        }
        None
    }

    /// (template text, None) or (None, skip reason)
    fn ask(&self, result: &Value, deadline: f64) -> (Option<String>, Option<&'static str>) {
        let request = json!({"rule_id": result["rule_id"], "failure_shape": failure_shape(result), "prompt_version": self.prompt_version, "placeholders": PLACEHOLDERS});
        for attempt in 0..self.max_attempts {
            if self.now() >= deadline {
                return (None, Some("skipped_timeout"));
            }
            match self.breaker.call(|| self.model.ask(&request, deadline)) {
                Err(CallError::Open) => return (None, Some("skipped_breaker")),
                Err(CallError::Model(ModelError::NotConfigured)) => return (None, Some("skipped_no_model")),
                Err(CallError::Model(ModelError::Fatal(_))) => return (None, Some("skipped_error")),
                Err(CallError::Model(e)) => {
                    if attempt + 1 >= self.max_attempts {
                        return (None, Some(if matches!(e, ModelError::Timeout(_)) { "skipped_timeout" } else { "skipped_error" }));
                    }
                    let wait = backoff(attempt, 1.0, 30.0, &mut rand::thread_rng());
                    (self.sleep)(wait.min(deadline - self.now()).max(0.0));
                }
                Ok(text) => {
                    if self.now() > deadline {
                        return (None, Some("skipped_timeout"));
                    }
                    if text.trim().is_empty() || text.chars().count() > MAX_TEMPLATE_CHARS {
                        return (None, Some("skipped_error"));
                    }
                    return (Some(text.trim().to_string()), None);
                }
            }
        }
        (None, Some("skipped_error"))
    }

    /// (text, source, outcome, stop): stop is a reason that applies to the rest of the claim.
    fn one(&self, result: &Value, cfg: &RoutingConfig, deadline: f64, stopped: Option<&'static str>) -> (String, &'static str, &'static str, Option<&'static str>) {
        let fallback = deterministic_text(result);
        let key = template_key(result["rule_id"].as_str().unwrap_or(""), &failure_shape(result), &self.prompt_version, &self.model.name());
        if let Ok(Some(cached)) = self.store.cache_get(&key) {
            let filled = fill(&cached, result);
            return if (self.guard)(&filled, result) { (filled, "cache", "cache_hit", stopped) } else { (fallback, "template", "skipped_guard", stopped) };
        }
        if let Some(s) = stopped {
            return (fallback, "template", s, Some(s));
        }
        if let Some(limited) = self.limits_ok(cfg, self.now()) {
            return (fallback, "template", limited, Some(limited));
        }
        let (template, why) = self.ask(result, deadline);
        let Some(template) = template else {
            let why = why.unwrap_or("skipped_error");
            let stop = ["skipped_breaker", "skipped_timeout", "skipped_no_model"].contains(&why).then_some(why);
            return (fallback, "template", why, stop);
        };
        let filled = fill(&template, result);
        if !(self.guard)(&filled, result) {
            return (fallback, "template", "skipped_guard", stopped);
        }
        let _ = self.store.cache_put(&key, &template);
        (filled, "ai", "ai_used", stopped)
    }

    /// Explains one stored claim. Returns the state it moved to ("explained" or "explanation_skipped"), or "noop".
    pub fn run(&self, claim_id: &str, version: i64) -> &'static str {
        let Ok(Some(doc)) = self.store.get(claim_id, Some(version)) else { return "noop" };
        if doc["state"] != "triaged" || doc["receipt"]["lane"] == "green" {
            return "noop";
        }
        let cfg = (self.cfg_source)();
        let deadline = self.now() + self.deadline_seconds;
        let (mut findings, mut outcomes, mut stopped) = (Map::new(), Vec::new(), None);
        for result in triage::flagged(&doc["results"]) {
            let Some(rule_id) = result["rule_id"].as_str() else { continue };
            let (text, source, outcome, stop) = self.one(result, &cfg, deadline, stopped);
            stopped = stop;
            findings.insert(rule_id.to_string(), json!({"text": text, "source": source}));
            outcomes.push(outcome);
        }
        let skipped: Vec<&&str> = outcomes.iter().filter(|o| o.starts_with("skipped")).collect();
        let (final_outcome, to) = if findings.is_empty() {
            ("skipped_none", "explanation_skipped")
        } else if let Some(first) = skipped.first() {
            (**first, "explanation_skipped")
        } else {
            (if outcomes.contains(&"ai_used") { "ai_used" } else { "cache_hit" }, "explained")
        };
        let mut set = Map::new();
        set.insert("explanation".into(), json!({"outcome": final_outcome, "findings": findings}));
        let moved = self
            .store
            .transition(claim_id, version, "triaged", to, "system:explain", self.now(), Some(json!({"outcome": final_outcome, "findings": findings.len(), "skipped": skipped.len()})), Some(set), None);
        if matches!(moved, Ok(Some(_))) { to } else { "noop" }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::doc;
    use crate::store::MemoryQueueStore;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Scripted(Box<dyn Fn(usize) -> Result<String, ModelError> + Send + Sync>, AtomicUsize);
    impl Model for Scripted {
        fn ask(&self, _: &Value, _: f64) -> Result<String, ModelError> {
            (self.0)(self.1.fetch_add(1, Ordering::SeqCst))
        }
        fn name(&self) -> String {
            "scripted".into()
        }
    }

    fn rows(flags: &[(&str, &str, &str)]) -> Value {
        let mut v: Vec<Value> = triage::rule_ids().into_iter().map(|id| json!({"rule_id": id, "status": "PASS", "severity": "medium", "affected_line_ids": [], "evidence": [], "explanation": "engine text"})).collect();
        for (id, st, sev) in flags {
            let i = triage::rule_ids().iter().position(|r| r == id).unwrap();
            v[i]["status"] = json!(st);
            v[i]["severity"] = json!(sev);
            v[i]["affected_line_ids"] = json!(["L1"]);
            v[i]["evidence"] = json!([{"path": "/lines/0/quantity", "value": 3}]);
        }
        Value::Array(v)
    }

    fn setup(model: Arc<dyn Model>, flags: &[(&str, &str, &str)]) -> (ExplainStep, Arc<MemoryQueueStore>) {
        let store = Arc::new(MemoryQueueStore::new());
        let mut d = doc("C1");
        d["results"] = rows(flags);
        d["receipt"]["lane"] = json!(if flags.is_empty() { "green" } else { "A" });
        store.put_triaged(&d).unwrap();
        let clock: Clock = Arc::new(|| 1_700_000_000.0);
        let mut step = ExplainStep::new(store.clone(), model, default_guard(), Arc::new(CircuitBreaker::new(clock.clone())), clock, Arc::new(RoutingConfig::default));
        step.sleep = Arc::new(|_| {});
        (step, store)
    }

    #[test]
    fn a_good_template_is_filled_guarded_cached_and_the_claim_moves_on() {
        let (step, store) = setup(Arc::new(Scripted(Box::new(|_| Ok("The quantity is {value} on line {line}; ask the provider to confirm it.".into())), AtomicUsize::new(0))), &[("R013", "FAIL", "medium")]);
        assert_eq!(step.run("C1", 1), "explained");
        let d = store.get("C1", None).unwrap().unwrap();
        assert_eq!(d["explanation"]["findings"]["R013"]["text"], "The quantity is 3 on line L1; ask the provider to confirm it.");
        assert_eq!(d["explanation"]["findings"]["R013"]["source"], "ai");
        assert!(store.cache_get(&template_key("R013", "FAIL/medium/1", &step.prompt_version, "scripted")).unwrap().is_some());
        assert_eq!(step.run("C1", 1), "noop", "no longer triaged");
    }

    #[test]
    fn no_model_keeps_the_engine_text_and_never_blocks() {
        let (step, store) = setup(Arc::new(NoModel), &[("R013", "FAIL", "medium")]);
        assert_eq!(step.run("C1", 1), "explanation_skipped");
        let d = store.get("C1", None).unwrap().unwrap();
        assert_eq!((d["explanation"]["outcome"].as_str(), d["explanation"]["findings"]["R013"]["text"].as_str(), d["explanation"]["findings"]["R013"]["source"].as_str()), (Some("skipped_no_model"), Some("engine text"), Some("template")));
    }

    #[test]
    fn a_template_the_guard_refuses_is_replaced_by_the_engine_text_and_not_cached() {
        let (step, store) = setup(Arc::new(Scripted(Box::new(|_| Ok("The claim is approved for payment.".into())), AtomicUsize::new(0))), &[("R013", "FAIL", "medium")]);
        assert_eq!(step.run("C1", 1), "explanation_skipped");
        assert_eq!(store.get("C1", None).unwrap().unwrap()["explanation"]["outcome"], "skipped_guard");
        assert!(store.cache_get(&template_key("R013", "FAIL/medium/1", &step.prompt_version, "scripted")).unwrap().is_none());
    }

    #[test]
    fn transient_failures_retry_then_give_up_and_an_open_breaker_stops_asking() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c2 = calls.clone();
        let (step, store) = setup(Arc::new(Scripted(Box::new(move |_| { c2.fetch_add(1, Ordering::SeqCst); Err(ModelError::Transient("503".into())) }), AtomicUsize::new(0))), &[("R013", "FAIL", "medium"), ("R001", "FAIL", "medium")]);
        assert_eq!(step.run("C1", 1), "explanation_skipped");
        assert_eq!(store.get("C1", None).unwrap().unwrap()["explanation"]["outcome"], "skipped_error");
        assert_eq!(calls.load(Ordering::SeqCst), 5, "three attempts, then two more until the fifth failure opens the breaker, which refuses the last try");
    }

    #[test]
    fn the_daily_budget_and_the_rate_limit_stop_the_model_calls() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c2 = calls.clone();
        let (mut step, store) = setup(Arc::new(Scripted(Box::new(move |_| { c2.fetch_add(1, Ordering::SeqCst); Ok("Check {value}.".into()) }), AtomicUsize::new(0))), &[("R013", "FAIL", "medium")]);
        step.cfg_source = Arc::new(|| RoutingConfig { ai_per_minute: 0, ..RoutingConfig::default() });
        assert_eq!(step.run("C1", 1), "explanation_skipped");
        assert_eq!(store.get("C1", None).unwrap().unwrap()["explanation"]["outcome"], "skipped_rate");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_green_claim_is_left_alone() {
        let (step, _) = setup(Arc::new(NoModel), &[]);
        assert_eq!(step.run("C1", 1), "noop");
    }

    #[test]
    fn a_claims_value_is_made_safe_before_it_is_shown_inside_a_sentence() {
        assert_eq!(display_value("SVC-EDU-01"), "SVC-EDU-01");
        assert_eq!(display_value("2026-03-02"), "2026-03-02");
        assert_eq!(display_value("12.50"), "12.50");
        assert_eq!(display_value("two words"), "\u{201c}two words\u{201d}", "anything that could read as a sentence is quoted");
        assert_eq!(display_value("a\u{202e}b\u{200b}c\u{0}d\u{2060}"), "abcd", "direction, zero-width and control characters are removed");
        assert_eq!(display_value("say \"hi\"\n\n  there"), "\u{201c}say hi there\u{201d}", "white space collapses and the value cannot close its own quote");
        assert_eq!(display_value(&"A".repeat(5000)).chars().count(), MAX_VALUE_CHARS + 2, "capped, then quoted");
        assert_eq!(display_value("<img src=x onerror=alert(1)>"), "\u{201c}\u{2039}img src=x onerror=alert(1)\u{203a}\u{201d}", "angle brackets cannot form a tag");
        assert_eq!(display_value(""), "");
        assert_eq!(display_value("\u{200b}\u{202e}"), "", "only invisible characters leave nothing");
    }

    #[test]
    fn fill_replaces_once_and_never_re_scans_a_value() {
        let r = json!({"affected_line_ids": ["L2"], "evidence": [{"path": "/x", "value": "{line} and {value}"}]});
        assert_eq!(fill("see {value} / {line} / {line}", &r), "see \u{201c}{line} and {value}\u{201d} / L2 / L2", "a value that is not a plain code is quoted, and its braces are not expanded");
        assert_eq!(fill("no placeholders {braces}", &r), "no placeholders {braces}");
        assert_eq!(py_str(&json!({"a": [1, "x"], "b": null})), "{'a': [1, 'x'], 'b': None}");
        assert_eq!(py_str(&json!(2.0)), "2.0");
        assert_eq!(failure_shape(&json!({"status": "FAIL", "severity": "high", "affected_line_ids": ["a", "b"]})), "FAIL/high/n");
    }
}
