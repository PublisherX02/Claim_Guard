//! The rulebook configuration: `rules/rules.json` (the fifteen rules' metadata), `policies.json` and `services.json`
//! (`engine_core.config`). Read once; the engine never mutates it.

use crate::Num;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Debug, Clone)]
pub struct RuleDef {
    pub rule_id: String,
    pub version: String,
    pub severity: String,
    pub source: String,
    pub corrective_action: String,
}

#[derive(Debug, Clone)]
pub struct Policy {
    pub currency: String,
    pub submission_window_days: i64,
    pub allowed_providers: Vec<String>,
    pub auth_required_services: BTreeSet<String>,
    pub required_documents: BTreeMap<String, String>,
    pub max_unit_price: BTreeMap<String, Num>,
    pub max_quantity_per_line: BTreeMap<String, Num>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub rules: Vec<RuleDef>,
    pub policies: BTreeMap<String, Policy>,
    pub services: BTreeSet<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {0}: {1}")]
    Read(String, String),
    #[error("{0} is not what the engine expects: {1}")]
    Shape(String, String),
}

fn read(root: &Path, name: &str) -> Result<Value, ConfigError> {
    let path = root.join("rules").join(format!("{name}.json"));
    let text = std::fs::read_to_string(&path).map_err(|e| ConfigError::Read(path.display().to_string(), e.to_string()))?;
    serde_json::from_str(&text).map_err(|e| ConfigError::Shape(name.into(), e.to_string()))
}

fn text(v: &Value, key: &str, file: &str) -> Result<String, ConfigError> {
    v.get(key).and_then(Value::as_str).map(str::to_string).ok_or_else(|| ConfigError::Shape(file.into(), format!("missing text field {key}")))
}

fn num_map(v: &Value, key: &str) -> Result<BTreeMap<String, Num>, ConfigError> {
    let obj = v.get(key).and_then(Value::as_object).ok_or_else(|| ConfigError::Shape("policies".into(), format!("missing object {key}")))?;
    obj.iter()
        .map(|(k, n)| Num::from_json(n).map(|n| (k.clone(), n)).ok_or_else(|| ConfigError::Shape("policies".into(), format!("{key}.{k} is not a number"))))
        .collect()
}

impl Config {
    /// Loads from `<root>/rules/{rules,policies,services}.json`.
    pub fn load(root: &Path) -> Result<Config, ConfigError> {
        let rules = read(root, "rules")?
            .as_array()
            .ok_or_else(|| ConfigError::Shape("rules".into(), "not a list".into()))?
            .iter()
            .map(|r| {
                Ok(RuleDef {
                    rule_id: text(r, "rule_id", "rules")?,
                    version: text(r, "version", "rules")?,
                    severity: text(r, "severity", "rules")?,
                    source: text(r, "source", "rules")?,
                    corrective_action: text(r, "corrective_action", "rules")?,
                })
            })
            .collect::<Result<Vec<_>, ConfigError>>()?;
        let mut policies = BTreeMap::new();
        for (id, p) in read(root, "policies")?.as_object().ok_or_else(|| ConfigError::Shape("policies".into(), "not an object".into()))? {
            let strings = |key: &str| -> Result<Vec<String>, ConfigError> {
                p.get(key)
                    .and_then(Value::as_array)
                    .ok_or_else(|| ConfigError::Shape("policies".into(), format!("missing list {key}")))?
                    .iter()
                    .map(|s| s.as_str().map(str::to_string).ok_or_else(|| ConfigError::Shape("policies".into(), format!("{key} holds a non-text item"))))
                    .collect()
            };
            let docs = p
                .get("required_documents")
                .and_then(Value::as_object)
                .ok_or_else(|| ConfigError::Shape("policies".into(), "missing required_documents".into()))?
                .iter()
                .map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())).ok_or_else(|| ConfigError::Shape("policies".into(), "document type is not text".into())))
                .collect::<Result<BTreeMap<_, _>, _>>()?;
            policies.insert(
                id.clone(),
                Policy {
                    currency: text(p, "currency", "policies")?,
                    submission_window_days: p
                        .get("submission_window_days")
                        .and_then(Value::as_i64)
                        .ok_or_else(|| ConfigError::Shape("policies".into(), "submission_window_days".into()))?,
                    allowed_providers: strings("allowed_providers")?,
                    auth_required_services: strings("auth_required_services")?.into_iter().collect(),
                    required_documents: docs,
                    max_unit_price: num_map(p, "max_unit_price")?,
                    max_quantity_per_line: num_map(p, "max_quantity_per_line")?,
                },
            );
        }
        let services = read(root, "services")?
            .as_object()
            .ok_or_else(|| ConfigError::Shape("services".into(), "not an object".into()))?
            .keys()
            .cloned()
            .collect();
        Ok(Config { rules, policies, services })
    }

    pub fn rule(&self, id: &str) -> Option<&RuleDef> {
        self.rules.iter().find(|r| r.rule_id == id)
    }
}
