//! The rule pack (`rules/core.yar`), compiled and run by YARA-X, the same engine the Python build uses.
//!
//! The pack is declarative: a rule can only match or not match the facts text, it cannot open a file, call the network or loop.
//! Each rule carries `rule_id` and `outcome` in its metadata; for one rulebook rule several packs rules can match, and the
//! strongest outcome wins (FAIL, then UNABLE_TO_ASSESS, then NOT_APPLICABLE, then PASS).

use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub const PRECEDENCE: [&str; 4] = ["FAIL", "UNABLE_TO_ASSESS", "NOT_APPLICABLE", "PASS"];

#[derive(Debug, thiserror::Error)]
pub enum PackError {
    #[error("cannot read the rule pack {0}: {1}")]
    Read(String, String),
    #[error("the rule pack does not compile: {0}")]
    Compile(String),
    #[error("scanning failed: {0}")]
    Scan(String),
}

pub struct Pack {
    rules: &'static yara_x::Rules,
    path: PathBuf,
    bytes: Vec<u8>,
}

impl Pack {
    pub fn load(root: &Path) -> Result<Pack, PackError> {
        let path = root.join("rules").join("core.yar");
        let bytes = std::fs::read(&path).map_err(|e| PackError::Read(path.display().to_string(), e.to_string()))?;
        let text = String::from_utf8_lossy(&bytes).to_string();
        // Leaked on purpose: a pack lives as long as the process, and a scanner (one per thread, reused) borrows it for good.
        let rules: &'static yara_x::Rules = Box::leak(Box::new(yara_x::compile(text.as_str()).map_err(|e| PackError::Compile(e.to_string()))?));
        Ok(Pack { rules, path, bytes })
    }

    /// `yara_engine.pack_hash`: sha256 of the pack file.
    pub fn hash(&self) -> String {
        hex::encode(Sha256::digest(&self.bytes))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Scans the facts text and returns, per rulebook rule id, the set of outcomes whose pack rules matched.
    pub fn scan(&self, blob: &str) -> Result<BTreeMap<String, BTreeSet<String>>, PackError> {
        thread_local! {
            static SCANNER: RefCell<Option<(usize, yara_x::Scanner<'static>)>> = const { RefCell::new(None) };
        }
        SCANNER.with(|cell| {
            let mut slot = cell.borrow_mut();
            let key = self.rules as *const yara_x::Rules as usize;
            if slot.as_ref().map(|(k, _)| *k) != Some(key) {
                *slot = Some((key, yara_x::Scanner::new(self.rules)));
            }
            let scanner = &mut slot.as_mut().expect("just set").1;
            Self::collect(scanner.scan(blob.as_bytes()).map_err(|e| PackError::Scan(e.to_string()))?)
        })
    }

    fn collect(results: yara_x::ScanResults<'_, '_>) -> Result<BTreeMap<String, BTreeSet<String>>, PackError> {
        let mut by_rule: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for rule in results.matching_rules() {
            let (mut id, mut outcome) = (None, None);
            for (key, value) in rule.metadata() {
                if let yara_x::MetaValue::String(text) = value {
                    match key {
                        "rule_id" => id = Some(text.to_string()),
                        "outcome" => outcome = Some(text.to_string()),
                        _ => {}
                    }
                }
            }
            if let (Some(id), Some(outcome)) = (id, outcome) {
                by_rule.entry(id).or_default().insert(outcome);
            }
        }
        Ok(by_rule)
    }
}

/// The strongest outcome of a set (`next(o for o in PRECEDENCE if o in outcomes)`).
pub fn strongest(outcomes: &BTreeSet<String>) -> Option<&'static str> {
    PRECEDENCE.iter().copied().find(|o| outcomes.contains(*o))
}
