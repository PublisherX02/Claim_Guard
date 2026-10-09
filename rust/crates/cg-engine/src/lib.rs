//! The rule engine: the Rust counterpart of `src/yara_engine.py`.
//!
//! For one claim it extracts the fact lines of each rule (`details`), lets the YARA-X rule pack decide each rule's outcome from those
//! facts (`pack`), and assembles the fifteen schema-exact results. Anything unexpected inside one rule makes that rule UNABLE_TO_ASSESS
//! (a person looks at it) and never a PASS; the other fourteen are unaffected.

pub mod details;
pub mod pack;

use cg_core::config::{Config, ConfigError};
use cg_core::text::pointer;
use cg_core::transport;
use cg_core::view::View;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;

use details::Details;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Pack(#[from] pack::PackError),
    #[error("{0} produced no outcome - extractor and rule pack are out of sync")]
    OutOfSync(String),
    #[error("the claim is not an object with a claim_id")]
    NoClaim,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Evidence {
    pub path: String,
    pub value: Value,
}

/// One rule's verdict on one claim; field order and names are the `schemas/result.schema.json` record.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RuleResult {
    pub claim_id: Value,
    pub rule_id: String,
    pub rule_version: String,
    pub status: String,
    pub severity: String,
    pub affected_line_ids: Vec<String>,
    pub evidence: Vec<Evidence>,
    pub rule_source: String,
    pub explanation: String,
    pub corrective_action: String,
    pub confidence: Option<f64>,
    pub confidence_kind: String,
    pub requires_human_review: bool,
    pub method: String,
    pub review_status: String,
}

pub struct Engine {
    pub cfg: Config,
    pub pack: pack::Pack,
    root: std::path::PathBuf,
}

const CRASH_MESSAGE: &str = "An internal error prevented this check; it is treated as unable to assess.";

impl Engine {
    /// `root` is the project directory that holds `rules/`.
    pub fn load(root: &Path) -> Result<Engine, EngineError> {
        Ok(Engine { cfg: Config::load(root)?, pack: pack::Pack::load(root)?, root: root.to_path_buf() })
    }

    /// `yara_engine.engine_code_hash`: tells two builds of the engine apart. The pack plus the two Rust modules that feed and assemble it;
    /// line endings are normalised so a checkout with LF or CRLF gives the same digest.
    pub fn code_hash(&self) -> String {
        let mut h = Sha256::new();
        let parts: [(&str, Vec<u8>); 3] = [
            ("core.yar", std::fs::read(self.pack.path()).unwrap_or_default()),
            ("details.rs", include_bytes!("details.rs").to_vec()),
            ("lib.rs", include_bytes!("lib.rs").to_vec()),
        ];
        for (name, bytes) in parts {
            let text = String::from_utf8_lossy(&bytes).replace("\r\n", "\n").replace('\r', "\n");
            let mut lines: Vec<&str> = text.split('\n').collect();
            if lines.last() == Some(&"") {
                lines.pop();
            }
            let mut joined = vec![name.to_string()];
            joined.extend(lines.iter().map(|l| l.to_string()));
            h.update(joined.join("|").as_bytes());
        }
        hex::encode(h.finalize())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Evidence = direct pointer lookups on the original claim. A path that cannot be resolved is dropped; if none resolve,
    /// `/claim_id` (always present) keeps the result scorable.
    fn evidence(claim: &Value, paths: &[String]) -> Vec<Evidence> {
        let mut seen = HashSet::new();
        let mut out: Vec<Evidence> = paths
            .iter()
            .filter(|p| seen.insert((*p).clone()))
            .filter_map(|p| pointer(claim, p).map(|v| Evidence { path: p.clone(), value: v.clone() }))
            .collect();
        if out.is_empty() {
            out.push(Evidence { path: "/claim_id".into(), value: claim.get("claim_id").cloned().unwrap_or(Value::Null) });
        }
        out
    }

    /// Evaluates all fifteen rules for one claim that passed `transport::validate`.
    /// `tool_errors` receives one line per rule that crashed (kept out of the result record, which is schema-exact).
    pub fn evaluate(&self, claim: &Value, tool_errors: &mut Vec<String>) -> Result<Vec<RuleResult>, EngineError> {
        let view = View::of(claim).ok_or(EngineError::NoClaim)?;
        let mut details: Vec<(&'static str, Result<Details, String>)> = vec![];
        for (rid, run) in details::all(&view, &self.cfg) {
            let outcome = catch_unwind(AssertUnwindSafe(|| run())).map_err(|e| {
                let text = e.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| e.downcast_ref::<String>().cloned()).unwrap_or_else(|| "panic".into());
                tool_errors.push(format!("{rid}: engine exception {text}"));
                text
            });
            details.push((rid, outcome));
        }
        let mut blob = String::new();
        for (_, d) in &details {
            if let Ok(d) = d {
                for fact in &d.facts {
                    blob.push_str(fact);
                    blob.push('\n');
                }
            }
        }
        let matched = self.pack.scan(&blob)?;
        let mut results = Vec::with_capacity(15);
        for (rid, outcome) in details {
            let def = self.cfg.rule(rid).ok_or_else(|| EngineError::OutOfSync(rid.to_string()))?;
            let (status, d) = match outcome {
                Err(_) => (
                    "UNABLE_TO_ASSESS",
                    Details { facts: vec![], evidence_paths: vec!["/claim_id".into()], line_ids: vec![], message: CRASH_MESSAGE.into() },
                ),
                Ok(d) => {
                    let outcomes = matched.get(rid).ok_or_else(|| EngineError::OutOfSync(rid.to_string()))?;
                    let status = pack::strongest(outcomes).ok_or_else(|| EngineError::OutOfSync(rid.to_string()))?;
                    (status, d)
                }
            };
            let flagged = status == "FAIL" || status == "UNABLE_TO_ASSESS";
            results.push(RuleResult {
                claim_id: claim.get("claim_id").cloned().unwrap_or(Value::Null),
                rule_id: rid.to_string(),
                rule_version: def.version.clone(),
                status: status.to_string(),
                severity: def.severity.clone(),
                affected_line_ids: d.line_ids,
                evidence: Self::evidence(claim, &d.evidence_paths),
                rule_source: def.source.clone(),
                explanation: d.message,
                corrective_action: if flagged { def.corrective_action.clone() } else { String::new() },
                confidence: None,
                confidence_kind: "not_probabilistic".into(),
                requires_human_review: flagged,
                method: "deterministic".into(),
                review_status: "unreviewed".into(),
            });
        }
        Ok(results)
    }

    /// Fifteen UNABLE_TO_ASSESS results for a claim that could not be evaluated at all (it failed the transport contract).
    pub fn fail_closed(&self, claim: &Value) -> Vec<RuleResult> {
        let mut rules: Vec<_> = self.cfg.rules.iter().collect();
        rules.sort_by(|a, b| a.rule_id.cmp(&b.rule_id));
        let claim_id = claim.get("claim_id").cloned().unwrap_or(Value::Null);
        rules
            .into_iter()
            .map(|r| RuleResult {
                claim_id: claim_id.clone(),
                rule_id: r.rule_id.clone(),
                rule_version: r.version.clone(),
                status: "UNABLE_TO_ASSESS".into(),
                severity: r.severity.clone(),
                affected_line_ids: vec![],
                evidence: vec![Evidence { path: "/claim_id".into(), value: claim_id.clone() }],
                rule_source: r.source.clone(),
                explanation: "The claim failed the input contract, so this check could not be performed.".into(),
                corrective_action: r.corrective_action.clone(),
                confidence: None,
                confidence_kind: "not_probabilistic".into(),
                requires_human_review: true,
                method: "deterministic".into(),
                review_status: "unreviewed".into(),
            })
            .collect()
    }

    /// What the pipeline does for one claim line: validate the envelope, evaluate, or fail closed.
    pub fn review(&self, claim: &Value, tool_errors: &mut Vec<String>) -> Result<Vec<RuleResult>, EngineError> {
        match transport::validate(claim) {
            Ok(()) => self.evaluate(claim, tool_errors),
            Err(_) => Ok(self.fail_closed(claim)),
        }
    }
}
