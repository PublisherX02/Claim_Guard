//! Knowledge-graph answers for "Ask the rulebook" (docs/60): the questions that are about how the rulebook's parts connect, not about one passage.
//!
//! "Which rules can fire for SVC-IMAGE under EDU-BASIC?" is not in any single passage. It is a path: the policy lists the service as needing an
//! authorization, rule R008 reads that list, rule R009 is named by R008. Vector search finds passages that mention these things; this module
//! walks the connections and returns every fact on the path, each with the rule, policy, field or catalogue entry it came from.
//!
//! * **The graph is derived, not extracted.** Its sources are files that are already structured: the rules (their text names the policy
//!   attributes they read, `policy.allowed_providers`, and the rules they point to, `R009`), the field dictionary (which rules read which
//!   field), the policy table in force, the service catalogue, the providers and the diagnoses. Nothing is learned, so nothing is guessed.
//! * **Answers are computed.** A question becomes one of a closed set of queries; the executor returns facts. A fact is never written by a model.
//! * **Who turns the question into a query** is a separate, replaceable step. The built-in planner reads identifiers and keywords. A language
//!   model may propose a query instead (`Query::from_json`), and its proposal is accepted only if every identifier in it exists in the graph.
//!   A bad proposal costs nothing: the question falls through to passage search.
//! * It never sees a claim. The graph holds the rulebook, the policies and the catalogue.

use fancy_regex::Regex;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};

/// The policy attributes a rule can read, with the words people use for them and a short label.
pub const ATTRS: [(&str, &str, &[&str]); 7] = [
    ("allowed_providers", "the allowed providers (the network)", &["network", "provider", "providers", "in-network", "allowed provider"]),
    ("auth_required_services", "the services that need an authorization reference", &["authorization", "authorisation", "pre-auth", "preauth", "prior approval", "approval"]),
    ("required_documents", "the supporting document each service needs", &["document", "documents", "attachment", "supporting", "report", "paperwork"]),
    ("max_unit_price", "the highest unit price per service", &["price", "prices", "cost", "unit price", "tariff", "expensive", "pricing"]),
    ("max_quantity_per_line", "the highest quantity on one line per service", &["quantity", "quantities", "units per line", "how many units"]),
    ("submission_window_days", "the submission window in days", &["window", "deadline", "days", "late", "timely"]),
    ("currency", "the currency", &["currency", "sar", "exchange"]),
];

/// What a fact came from, for the citation. The same strings the passage index uses, so a reader finds them in the same place.
#[derive(Debug, Clone, PartialEq)]
pub struct Fact {
    pub text: String,
    pub source: String,
}

impl Fact {
    pub fn new(text: impl Into<String>, source: impl Into<String>) -> Fact {
        Fact { text: text.into(), source: source.into() }
    }
    pub fn to_json(&self) -> Value {
        json!({"text": self.text, "source": self.source})
    }
}

/// The closed set of questions the graph answers.
#[derive(Debug, Clone, PartialEq)]
pub enum Query {
    RuleInfo(String),
    RuleFields(String),
    FieldRules(String),
    RulesBySeverity(String),
    RulesConsulting(String),
    PolicyAttr { policy: Option<String>, attr: String, service: Option<String> },
    ServiceRules { service: String, policy: Option<String> },
    ProviderPolicies(String),
    ComparePolicies(String, String),
    RelatedRules(String),
    List(String),
}

#[derive(Debug, Clone)]
struct RuleNode {
    title: String,
    severity: String,
    logic: String,
    corrective: String,
    /// policy attributes it reads, plus "catalogue" if it reads the service catalogue
    consults: BTreeSet<String>,
    /// rules its text names
    mentions: BTreeSet<String>,
}

#[derive(Debug, Clone)]
struct FieldNode {
    path: String,
    meaning: String,
    ty: String,
    used_by: Vec<String>,
}

pub struct Graph {
    rules: BTreeMap<String, RuleNode>,
    fields: Vec<FieldNode>,
    services: BTreeMap<String, Value>,
    providers: BTreeMap<String, String>,
    diagnoses: BTreeMap<String, String>,
    policies: BTreeMap<String, Value>,
    version: u64,
}

fn all(re: &Regex, text: &str) -> Vec<String> {
    re.find_iter(text).filter_map(|m| m.ok()).map(|m| m.as_str().to_string()).collect()
}

fn num(v: &Value) -> String {
    v.as_f64().map(|f| if f.fract() == 0.0 { format!("{}", f as i64) } else { format!("{f}") }).unwrap_or_else(|| v.to_string())
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect()
}

fn words(text: &str) -> Vec<String> {
    text.to_lowercase().split(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_' || c == '.')).filter(|w| !w.is_empty()).map(str::to_string).collect()
}

fn has_phrase(q: &str, phrases: &[&str]) -> bool {
    phrases.iter().any(|p| q.contains(p))
}

impl Graph {
    /// `glossary`: the field dictionary (`cg_core::localize::glossary_json()`). `policies`: the table in force, at `version`.
    pub fn build(rules_json: &Value, policies: &Value, services_json: &Value, providers_json: &Value, diagnoses_json: &Value, glossary: &Value, version: u64) -> Graph {
        let policy_ref = Regex::new(r"policy\.([a-z_]+)").expect("a valid pattern");
        let rule_ref = Regex::new(r"\bR\d{3}\b").expect("a valid pattern");
        let mut rules = BTreeMap::new();
        for r in rules_json.as_array().into_iter().flatten() {
            let id = r["rule_id"].as_str().unwrap_or("").to_string();
            let logic = r["logic"].as_str().unwrap_or("").to_string();
            let mut consults: BTreeSet<String> = policy_ref
                .captures_iter(&logic)
                .filter_map(|c| c.ok())
                .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
                .filter(|a| ATTRS.iter().any(|(n, _, _)| n == a))
                .collect();
            if logic.contains("policy.currency") || logic.contains("(SAR)") && logic.to_lowercase().contains("currency must equal") {
                consults.insert("currency".into());
            }
            if logic.contains("rules/services.json") {
                consults.insert("catalogue".into());
            }
            let mentions: BTreeSet<String> = all(&rule_ref, &logic).into_iter().filter(|m| *m != id).collect();
            rules.insert(id, RuleNode { title: r["title"].as_str().unwrap_or("").to_string(), severity: r["severity"].as_str().unwrap_or("").to_string(), logic, corrective: r["corrective_action"].as_str().unwrap_or("").to_string(), consults, mentions });
        }
        let mut fields = vec![];
        for (scope, prefix) in [("claim", ""), ("coverage", "coverage."), ("line", "lines[]."), ("authorization", "authorizations[]."), ("attachment", "attachments[].")] {
            for (key, f) in glossary["scopes"][scope].as_object().into_iter().flatten() {
                fields.push(FieldNode { path: format!("{prefix}{key}"), meaning: f["meaning"].as_str().unwrap_or("").to_string(), ty: f["type"].as_str().unwrap_or("").to_string(), used_by: strings(&f["used_by"]) });
            }
        }
        let map_of = |v: &Value, k: &str, d: &str| -> BTreeMap<String, String> { v.as_array().into_iter().flatten().filter_map(|x| Some((x[k].as_str()?.to_string(), x[d].as_str().unwrap_or("").to_string()))).collect() };
        Graph {
            rules,
            fields,
            services: services_json.as_object().map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default(),
            providers: map_of(providers_json, "provider_id", "display"),
            diagnoses: map_of(diagnoses_json, "code", "display"),
            policies: policies.as_object().map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default(),
            version,
        }
    }

    pub fn counts(&self) -> Value {
        let edges: usize = self.rules.values().map(|r| r.consults.len() + r.mentions.len()).sum::<usize>() + self.fields.iter().map(|f| f.used_by.len()).sum::<usize>();
        json!({"rules": self.rules.len(), "fields": self.fields.len(), "services": self.services.len(), "providers": self.providers.len(), "diagnoses": self.diagnoses.len(), "policies": self.policies.len(), "rule_links": edges})
    }

    fn policy_src(&self, id: &str) -> String {
        format!("policy {id} (table version {})", self.version)
    }

    fn rule_src(id: &str) -> String {
        format!("rule {id}")
    }

    /// Rules that name `id`, and rules `id` names.
    fn linked(&self, id: &str) -> BTreeSet<String> {
        let mut out: BTreeSet<String> = self.rules.get(id).map(|r| r.mentions.clone()).unwrap_or_default();
        out.extend(self.rules.iter().filter(|(_, r)| r.mentions.contains(id)).map(|(k, _)| k.clone()));
        out.remove(id);
        out
    }

    fn rule_line(&self, id: &str) -> String {
        let r = &self.rules[id];
        format!("{id} {} ({} severity)", r.title, r.severity)
    }

    // ---------------------------------------------------------------- entity lookup

    pub fn rule_ids(&self) -> Vec<String> {
        self.rules.keys().cloned().collect()
    }
    pub fn service_ids(&self) -> Vec<String> {
        self.services.keys().cloned().collect()
    }
    pub fn policy_ids(&self) -> Vec<String> {
        self.policies.keys().cloned().collect()
    }
    pub fn provider_ids(&self) -> Vec<String> {
        self.providers.keys().cloned().collect()
    }
    pub fn field_paths(&self) -> Vec<String> {
        self.fields.iter().map(|f| f.path.clone()).collect()
    }

    fn canon(list: impl Iterator<Item = String>, wanted: &str) -> Option<String> {
        list.into_iter().find(|x| x.eq_ignore_ascii_case(wanted.trim()))
    }
    fn rule(&self, s: &str) -> Option<String> {
        Self::canon(self.rules.keys().cloned(), s)
    }
    fn service(&self, s: &str) -> Option<String> {
        Self::canon(self.services.keys().cloned(), s)
    }
    fn policy(&self, s: &str) -> Option<String> {
        Self::canon(self.policies.keys().cloned(), s)
    }
    fn provider(&self, s: &str) -> Option<String> {
        Self::canon(self.providers.keys().cloned(), s)
    }
    fn attr(&self, s: &str) -> Option<String> {
        ATTRS.iter().map(|(n, _, _)| n.to_string()).find(|n| n == s.trim())
    }
    /// A field by its full path, or by its bare name when that is unambiguous.
    fn field(&self, s: &str) -> Option<String> {
        let s = s.trim().to_lowercase();
        if let Some(f) = self.fields.iter().find(|f| f.path.to_lowercase() == s) {
            return Some(f.path.clone());
        }
        let bare: Vec<&FieldNode> = self.fields.iter().filter(|f| f.path.rsplit('.').next().unwrap_or("").to_lowercase() == s).collect();
        (bare.len() == 1).then(|| bare[0].path.clone())
    }

    // ---------------------------------------------------------------- the built-in planner

    /// Reads identifiers and keywords in a question and picks a query, or None when the question does not clearly ask for one of them.
    pub fn plan(&self, question: &str) -> Option<Query> {
        let q = question.to_lowercase();
        let ws = words(&q);
        let rules: Vec<String> = ws.iter().filter_map(|w| self.rule(w)).collect::<BTreeSet<_>>().into_iter().collect();
        let services: Vec<String> = ws.iter().filter_map(|w| self.service(w)).collect::<BTreeSet<_>>().into_iter().collect();
        let providers: Vec<String> = ws.iter().filter_map(|w| self.provider(w)).collect::<BTreeSet<_>>().into_iter().collect();
        let policies: Vec<String> = ws.iter().filter_map(|w| self.policy(w)).collect::<BTreeSet<_>>().into_iter().collect();
        let mut fields: Vec<String> = vec![];
        for f in &self.fields {
            let bare = f.path.rsplit('.').next().unwrap_or("").to_string();
            let spaced = bare.replace('_', " ");
            let hit = q.contains(&f.path.to_lowercase()) || ws.iter().any(|w| *w == bare) || (bare.contains('_') && q.contains(&spaced));
            // a bare name shared by several scopes (service_code, patient_id) is ambiguous: only the full path counts for those
            let shared = self.fields.iter().filter(|g| g.path.rsplit('.').next() == Some(bare.as_str())).count() > 1;
            if hit && (!shared || q.contains(&f.path.to_lowercase())) && !fields.contains(&f.path) {
                fields.push(f.path.clone());
            }
        }
        // a bare name shared by several scopes: if the question names it and nothing narrower, take the line-level or claim-level field
        if fields.is_empty() {
            for f in &self.fields {
                let bare = f.path.rsplit('.').next().unwrap_or("");
                if ws.iter().any(|w| w == bare) && self.fields.iter().filter(|g| g.path.rsplit('.').next() == Some(bare)).count() > 1 {
                    let pick = self.fields.iter().find(|g| g.path.rsplit('.').next() == Some(bare) && (g.path == bare || g.path.starts_with("lines[]."))).map(|g| g.path.clone());
                    if let Some(p) = pick.filter(|p| !fields.contains(p)) {
                        fields.push(p);
                    }
                }
            }
        }
        let attr = ATTRS.iter().find(|(name, _, syn)| q.contains(name) || has_phrase(&format!(" {q} "), &syn.iter().map(|s| format!(" {s}")).collect::<Vec<_>>().iter().map(String::as_str).collect::<Vec<_>>())).map(|(n, _, _)| n.to_string());
        let asks_rules = has_phrase(&q, &["which rule", "what rule", "rules", "rule ", "check", "flag", "fire", "apply", "applies", "trigger", "validate", "règle", "regle", "قاعد"]);
        let asks_fields = has_phrase(&q, &["field", "fields", "input", "inputs", "read", "reads", "uses what", "data does", "information does", "look at", "looks at"]);
        let asks_related = has_phrase(&q, &["related", "depends", "dependent", "other rules", "see also", "overlap", "work together", "refer", "relationship", "connected", "linked"]);

        if policies.len() >= 2 && has_phrase(&q, &["compare", "differ", "difference", "versus", " vs", "between", "same as"]) {
            return Some(Query::ComparePolicies(policies[0].clone(), policies[1].clone()));
        }
        if let Some(s) = services.first() {
            let pol = policies.first().cloned();
            if asks_rules || has_phrase(&q, &["what happens", "what can go wrong", "what could", "anything to watch", "need for", "requirements for", "required for", "what do i need"]) {
                return Some(Query::ServiceRules { service: s.clone(), policy: pol });
            }
            if let Some(a) = attr.filter(|a| a != "allowed_providers" && a != "currency" && a != "submission_window_days") {
                return Some(Query::PolicyAttr { policy: pol, attr: a, service: Some(s.clone()) });
            }
            if has_phrase(&q, &["limit", "maximum", "highest", "most", "cap", "allowed", "needs", "require", "requires"]) {
                return Some(Query::ServiceRules { service: s.clone(), policy: pol });
            }
            return None;
        }
        if let Some(p) = providers.first() {
            return Some(Query::ProviderPolicies(p.clone()));
        }
        if let (Some(f), true) = (fields.first(), asks_rules || has_phrase(&q, &["who uses", "used by", "depends on", "reads this", "which rule"])) {
            return Some(Query::FieldRules(f.clone()));
        }
        if let Some(r) = rules.first() {
            if rules.len() >= 2 && asks_related {
                return Some(Query::RelatedRules(r.clone()));
            }
            if asks_related {
                return Some(Query::RelatedRules(r.clone()));
            }
            if asks_fields {
                return Some(Query::RuleFields(r.clone()));
            }
            if has_phrase(&q, &["policy", "attribute", "table"]) && !self.rules[r].consults.is_empty() {
                return Some(Query::RuleInfo(r.clone()));
            }
            return Some(Query::RuleInfo(r.clone()));
        }
        if let Some(sev) = ["high", "medium", "low"].iter().find(|s| ws.iter().any(|w| w == *s)) {
            if has_phrase(&q, &["severity", "rules", "rule "]) {
                return Some(Query::RulesBySeverity(sev.to_string()));
            }
        }
        if let Some(a) = &attr {
            if asks_rules {
                return Some(Query::RulesConsulting(a.clone()));
            }
            // "which documents does a patient bring to the clinic" shares a word with the rulebook and is not about it: a bare attribute word needs a policy, insurer or claim beside it
            let about_rulebook = ["polic", "insur", "claim", "payer", "rulebook", "reimburs"].iter().any(|w| q.contains(w));
            if !policies.is_empty() || (about_rulebook && has_phrase(&q, &["what is", "what are", "which"])) {
                return Some(Query::PolicyAttr { policy: policies.first().cloned(), attr: a.clone(), service: None });
            }
        }
        if has_phrase(&q, &["list", "all the", "every ", "how many", "what are the"]) {
            for (kind, syn) in [("rules", &["rules"][..]), ("services", &["services", "service codes"][..]), ("providers", &["providers", "clinics"][..]), ("policies", &["policies"][..]), ("diagnoses", &["diagnoses", "diagnosis codes"][..])] {
                if has_phrase(&q, syn) {
                    return Some(Query::List(kind.into()));
                }
            }
        }
        None
    }

    // ---------------------------------------------------------------- queries from outside (a language model's proposal)

    /// A query from JSON, such as a model proposes: `{"op": "service_rules", "service": "SVC-IMAGE", "policy": "EDU-BASIC"}`. Every identifier must
    /// exist in the graph, and unknown operations and extra keys are refused, so the proposal can only ask for something the graph can answer.
    pub fn query_from_json(&self, v: &Value) -> Result<Query, String> {
        let o = v.as_object().ok_or("a query must be a JSON object")?;
        let op = o.get("op").and_then(Value::as_str).ok_or("a query needs an op")?;
        let allowed: &[&str] = match op {
            "rule_info" | "rule_fields" | "related_rules" => &["op", "rule"],
            "field_rules" => &["op", "field"],
            "rules_by_severity" => &["op", "severity"],
            "rules_consulting" => &["op", "attr"],
            "policy_attr" => &["op", "policy", "attr", "service"],
            "service_rules" => &["op", "service", "policy"],
            "provider_policies" => &["op", "provider"],
            "compare_policies" => &["op", "a", "b"],
            "list" => &["op", "kind"],
            _ => return Err(format!("unknown operation {op:?}")),
        };
        if let Some(extra) = o.keys().find(|k| !allowed.contains(&k.as_str())) {
            return Err(format!("unexpected key {extra:?}"));
        }
        let get = |k: &str| o.get(k).and_then(Value::as_str).filter(|s| !s.trim().is_empty());
        let need = |k: &str| get(k).ok_or_else(|| format!("{op} needs {k}"));
        let opt_policy = || -> Result<Option<String>, String> { get("policy").map(|p| self.policy(p).ok_or_else(|| format!("no policy {p:?}"))).transpose() };
        Ok(match op {
            "rule_info" => Query::RuleInfo(self.rule(need("rule")?).ok_or("no such rule")?),
            "rule_fields" => Query::RuleFields(self.rule(need("rule")?).ok_or("no such rule")?),
            "related_rules" => Query::RelatedRules(self.rule(need("rule")?).ok_or("no such rule")?),
            "field_rules" => Query::FieldRules(self.field(need("field")?).ok_or("no such field")?),
            "rules_by_severity" => {
                let s = need("severity")?.to_lowercase();
                if !["high", "medium", "low"].contains(&s.as_str()) {
                    return Err("severity must be high, medium or low".into());
                }
                Query::RulesBySeverity(s)
            }
            "rules_consulting" => {
                let a = need("attr")?;
                Query::RulesConsulting(self.attr(a).ok_or("no such policy attribute")?)
            }
            "policy_attr" => Query::PolicyAttr { policy: opt_policy()?, attr: self.attr(need("attr")?).ok_or("no such policy attribute")?, service: get("service").map(|s| self.service(s).ok_or_else(|| format!("no service {s:?}"))).transpose()? },
            "service_rules" => Query::ServiceRules { service: self.service(need("service")?).ok_or("no such service")?, policy: opt_policy()? },
            "provider_policies" => Query::ProviderPolicies(self.provider(need("provider")?).ok_or("no such provider")?),
            "compare_policies" => Query::ComparePolicies(self.policy(need("a")?).ok_or("no such policy")?, self.policy(need("b")?).ok_or("no such policy")?),
            _ => {
                let k = need("kind")?.to_lowercase();
                if !["rules", "services", "providers", "policies", "diagnoses"].contains(&k.as_str()) {
                    return Err("kind must be rules, services, providers, policies or diagnoses".into());
                }
                Query::List(k)
            }
        })
    }

    /// The prompt-side description of what a model may propose, with the real identifiers (so it picks, and does not invent).
    pub fn planner_catalogue(&self) -> String {
        format!(
            "Operations (reply with one JSON object):\n\
             {{\"op\":\"rule_info\",\"rule\":ID}}  what a rule checks, its severity and corrective action\n\
             {{\"op\":\"rule_fields\",\"rule\":ID}}  which claim fields a rule reads\n\
             {{\"op\":\"related_rules\",\"rule\":ID}}  rules that name it or are named by it\n\
             {{\"op\":\"field_rules\",\"field\":NAME}}  which rules read a claim field\n\
             {{\"op\":\"rules_by_severity\",\"severity\":\"high|medium|low\"}}\n\
             {{\"op\":\"rules_consulting\",\"attr\":ATTR}}  which rules read a policy attribute\n\
             {{\"op\":\"policy_attr\",\"attr\":ATTR,\"policy\":ID?,\"service\":CODE?}}  the value of a policy attribute (optionally for one service)\n\
             {{\"op\":\"service_rules\",\"service\":CODE,\"policy\":ID?}}  everything that applies to a service and the rules that can fire for it\n\
             {{\"op\":\"provider_policies\",\"provider\":ID}}  the policies that allow a provider\n\
             {{\"op\":\"compare_policies\",\"a\":ID,\"b\":ID}}\n\
             {{\"op\":\"list\",\"kind\":\"rules|services|providers|policies|diagnoses\"}}\n\
             Rules: {}\nPolicies: {}\nServices: {}\nProviders: {}\nATTR is one of: {}\nFields: {}",
            self.rule_ids().join(", "),
            self.policy_ids().join(", "),
            self.service_ids().join(", "),
            self.provider_ids().join(", "),
            ATTRS.iter().map(|(n, _, _)| *n).collect::<Vec<_>>().join(", "),
            self.field_paths().join(", ")
        )
    }

    // ---------------------------------------------------------------- the executor

    /// The facts that answer a query. Empty when the graph holds nothing for it.
    pub fn run(&self, q: &Query) -> Vec<Fact> {
        match q {
            Query::RuleInfo(id) => {
                let Some(r) = self.rules.get(id) else { return vec![] };
                let mut out = vec![Fact::new(format!("{} checks: {}", self.rule_line(id), r.logic), Self::rule_src(id)), Fact::new(format!("{id} corrective action: {}", r.corrective), Self::rule_src(id))];
                let attrs: Vec<&str> = r.consults.iter().filter(|a| *a != "catalogue").map(String::as_str).collect();
                if !attrs.is_empty() {
                    out.push(Fact::new(format!("{id} reads these policy attributes: {}.", attrs.join(", ")), Self::rule_src(id)));
                }
                out
            }
            Query::RuleFields(id) => {
                let Some(_) = self.rules.get(id) else { return vec![] };
                let mut out: Vec<Fact> = self.fields.iter().filter(|f| f.used_by.iter().any(|u| u == id)).map(|f| Fact::new(format!("{id} reads field {} ({}): {}", f.path, f.ty, f.meaning), format!("field {}", f.path))).collect();
                for a in self.rules[id].consults.iter().filter(|a| *a != "catalogue") {
                    out.push(Fact::new(format!("{id} also reads the policy attribute {a}."), Self::rule_src(id)));
                }
                out
            }
            Query::FieldRules(path) => {
                let Some(f) = self.fields.iter().find(|f| &f.path == path) else { return vec![] };
                if f.used_by.is_empty() {
                    return vec![Fact::new(format!("No rule reads field {}.", f.path), format!("field {}", f.path))];
                }
                f.used_by.iter().filter(|u| self.rules.contains_key(*u)).map(|u| Fact::new(format!("Field {} is read by {}.", f.path, self.rule_line(u)), format!("field {}", f.path))).collect()
            }
            Query::RulesBySeverity(sev) => self.rules.iter().filter(|(_, r)| &r.severity == sev).map(|(id, _)| Fact::new(format!("{} has {sev} severity.", self.rule_line(id)), Self::rule_src(id))).collect(),
            Query::RulesConsulting(attr) => {
                let label = ATTRS.iter().find(|(n, _, _)| n == attr).map(|(_, l, _)| *l).unwrap_or("");
                let mut out: Vec<Fact> = self.rules.iter().filter(|(_, r)| r.consults.contains(attr)).map(|(id, _)| Fact::new(format!("{} reads {attr} ({label}).", self.rule_line(id)), Self::rule_src(id))).collect();
                // the rules those rules point to apply to the same data
                let direct: Vec<String> = self.rules.iter().filter(|(_, r)| r.consults.contains(attr)).map(|(id, _)| id.clone()).collect();
                for id in &direct {
                    for l in self.linked(id).into_iter().filter(|l| !direct.contains(l) && self.rules[id].mentions.contains(l)) {
                        out.push(Fact::new(format!("{} is named by {id}, which reads {attr}, so it follows up on the same data.", self.rule_line(&l)), Self::rule_src(&l)));
                    }
                }
                out
            }
            Query::PolicyAttr { policy, attr, service } => self.policy_attr(policy.as_deref(), attr, service.as_deref()),
            Query::ServiceRules { service, policy } => self.service_rules(service, policy.as_deref()),
            Query::ProviderPolicies(p) => {
                let Some(name) = self.providers.get(p) else { return vec![] };
                let mut out = vec![Fact::new(format!("Provider {p} is {name}."), format!("provider {p}"))];
                for (id, pol) in &self.policies {
                    let yes = strings(&pol["allowed_providers"]).contains(p);
                    out.push(Fact::new(format!("Policy {id} {} provider {p} in its network.", if yes { "allows" } else { "does not allow" }), self.policy_src(id)));
                }
                if let Some((rid, _)) = self.rules.iter().find(|(_, r)| r.consults.contains("allowed_providers")) {
                    out.push(Fact::new(format!("{} is the rule that checks a provider against the network.", self.rule_line(rid)), Self::rule_src(rid)));
                }
                out
            }
            Query::ComparePolicies(a, b) => {
                let (Some(pa), Some(pb)) = (self.policies.get(a), self.policies.get(b)) else { return vec![] };
                let mut out = vec![];
                for (key, _, _) in ATTRS {
                    if pa[key] != pb[key] {
                        out.push(Fact::new(format!("{key} differs: {a} has {}, {b} has {}.", short(&pa[key]), short(&pb[key])), format!("{} and {}", self.policy_src(a), self.policy_src(b))));
                    }
                }
                if out.is_empty() {
                    out.push(Fact::new(format!("Policies {a} and {b} have the same values for every attribute the rules read."), format!("{} and {}", self.policy_src(a), self.policy_src(b))));
                }
                out
            }
            Query::RelatedRules(id) => {
                let Some(r) = self.rules.get(id) else { return vec![] };
                let mut out = vec![];
                for m in &r.mentions {
                    if self.rules.contains_key(m) {
                        out.push(Fact::new(format!("{id} names {}.", self.rule_line(m)), Self::rule_src(id)));
                    }
                }
                for (other, o) in &self.rules {
                    if o.mentions.contains(id) {
                        out.push(Fact::new(format!("{} names {id}.", self.rule_line(other)), Self::rule_src(other)));
                    }
                }
                // rules that read the same policy attribute or the same field
                for (other, o) in &self.rules {
                    if other != id {
                        let shared: Vec<&String> = r.consults.intersection(&o.consults).filter(|a| *a != "catalogue").collect();
                        if !shared.is_empty() {
                            out.push(Fact::new(format!("{} reads the same policy attribute as {id}: {}.", self.rule_line(other), shared.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")), Self::rule_src(other)));
                        }
                    }
                }
                out
            }
            Query::List(kind) => match kind.as_str() {
                "rules" => self.rules.keys().map(|id| Fact::new(self.rule_line(id), Self::rule_src(id))).collect(),
                "services" => self.services.iter().map(|(id, s)| Fact::new(format!("{id}: {}", s["description"].as_str().unwrap_or("")), format!("service {id}"))).collect(),
                "providers" => self.providers.iter().map(|(id, n)| Fact::new(format!("{id}: {n}"), format!("provider {id}"))).collect(),
                "policies" => self.policies.keys().map(|id| Fact::new(format!("Policy {id} of payer {}", self.policies[id]["payer_id"].as_str().unwrap_or("")), self.policy_src(id))).collect(),
                "diagnoses" => self.diagnoses.iter().map(|(id, n)| Fact::new(format!("{id}: {n}"), format!("diagnosis {id}"))).collect(),
                _ => vec![],
            },
        }
    }

    fn policy_attr(&self, policy: Option<&str>, attr: &str, service: Option<&str>) -> Vec<Fact> {
        let label = ATTRS.iter().find(|(n, _, _)| *n == attr).map(|(_, l, _)| *l).unwrap_or("");
        let ids: Vec<&String> = match policy {
            Some(p) => self.policies.keys().filter(|k| k.as_str() == p).collect(),
            None => self.policies.keys().collect(),
        };
        let mut out = vec![];
        for id in ids {
            let v = &self.policies[id][attr];
            let text = match (service, v) {
                (Some(s), Value::Array(_)) => format!("Policy {id}: {s} {} in {label}.", if strings(v).iter().any(|x| x == s) { "is listed" } else { "is not listed" }),
                (Some(s), Value::Object(m)) => match m.get(s) {
                    Some(x) => format!("Policy {id}: {label} for {s} is {}.", if attr == "required_documents" { x.as_str().unwrap_or("").to_string() } else { num(x) }),
                    None => format!("Policy {id}: no entry for {s} in {label}."),
                },
                _ => format!("Policy {id}: {label} {}.", short(v)),
            };
            out.push(Fact::new(text, self.policy_src(id)));
        }
        if let Some((rid, _)) = self.rules.iter().find(|(_, r)| r.consults.contains(attr)) {
            let all: Vec<String> = self.rules.iter().filter(|(_, r)| r.consults.contains(attr)).map(|(id, _)| id.clone()).collect();
            out.push(Fact::new(format!("{} read{} {attr}.", all.iter().map(|id| self.rule_line(id)).collect::<Vec<_>>().join("; "), if all.len() == 1 { "s" } else { "" }), Self::rule_src(rid)));
        }
        out
    }

    /// The multi-hop query: a service, the policies' facts about it, and the rules that can fire because of those facts.
    fn service_rules(&self, service: &str, policy: Option<&str>) -> Vec<Fact> {
        let Some(svc) = self.services.get(service) else { return vec![] };
        let mut out = vec![Fact::new(format!("Service {service}: {} (catalogue price {}, at most {}, quantity at most {}).", svc["description"].as_str().unwrap_or(""), num(&svc["base_price"]), num(&svc["max_price"]), num(&svc["max_quantity"])), format!("service {service}"))];
        let mut involved: BTreeMap<String, Vec<String>> = BTreeMap::new(); // rule -> why
        let mut add = |rule: &str, why: String| involved.entry(rule.to_string()).or_default().push(why);
        for (rid, r) in &self.rules {
            if r.consults.contains("catalogue") {
                add(rid, format!("{service} must exist in the service catalogue"));
            }
        }
        let ids: Vec<&String> = match policy {
            Some(p) => self.policies.keys().filter(|k| k.as_str() == p).collect(),
            None => self.policies.keys().collect(),
        };
        for id in ids {
            let pol = &self.policies[id];
            let src = self.policy_src(id);
            if strings(&pol["auth_required_services"]).iter().any(|s| s == service) {
                out.push(Fact::new(format!("Policy {id}: {service} needs an authorization reference."), src.clone()));
                for (rid, r) in &self.rules {
                    if r.consults.contains("auth_required_services") {
                        add(rid, format!("policy {id} requires an authorization for {service}"));
                        for l in r.mentions.iter().filter(|l| self.rules.contains_key(*l)) {
                            add(l, format!("it follows up {rid}, which checks the authorization policy {id} requires for {service}"));
                        }
                    }
                }
            } else {
                out.push(Fact::new(format!("Policy {id}: {service} does not need an authorization reference."), src.clone()));
            }
            if let Some(d) = pol["required_documents"][service].as_str() {
                out.push(Fact::new(format!("Policy {id}: {service} needs a supporting document of type {d}."), src.clone()));
                for (rid, r) in &self.rules {
                    if r.consults.contains("required_documents") {
                        add(rid, format!("policy {id} requires a {d} for {service}"));
                    }
                }
            }
            if !pol["max_unit_price"][service].is_null() {
                out.push(Fact::new(format!("Policy {id}: the highest unit price for {service} is {} {}; the highest quantity on one line is {}.", num(&pol["max_unit_price"][service]), pol["currency"].as_str().unwrap_or(""), num(&pol["max_quantity_per_line"][service])), src.clone()));
                for (rid, r) in &self.rules {
                    if r.consults.contains("max_unit_price") {
                        add(rid, format!("policy {id} caps the unit price and quantity of {service}"));
                    }
                }
            }
        }
        for (rid, whys) in involved {
            let mut whys = whys;
            whys.sort();
            whys.dedup();
            out.push(Fact::new(format!("{} can fire for {service}: {}.", self.rule_line(&rid), whys.join("; ")), Self::rule_src(&rid)));
        }
        out.push(Fact::new(format!("Rules that read nothing about {service} in the catalogue or the policy (the checks on dates, identifiers, arithmetic, totals and duplicates) apply to every claim line and are not listed."), "the rulebook"));
        out
    }

    /// Every fact as plain text, numbered, with its citation: the answer when nothing writes it up.
    pub fn answer_text(facts: &[Fact]) -> String {
        facts.iter().map(|f| format!("- {} [{}]", f.text, f.source)).collect::<Vec<_>>().join("\n")
    }

    /// A short description of what was asked, shown beside the answer so a reader sees which question the graph understood.
    pub fn describe(q: &Query) -> String {
        match q {
            Query::RuleInfo(r) => format!("what rule {r} checks"),
            Query::RuleFields(r) => format!("which fields rule {r} reads"),
            Query::FieldRules(f) => format!("which rules read field {f}"),
            Query::RulesBySeverity(s) => format!("which rules have {s} severity"),
            Query::RulesConsulting(a) => format!("which rules read the policy attribute {a}"),
            Query::PolicyAttr { policy, attr, service } => format!("{attr}{}{}", service.as_ref().map(|s| format!(" for {s}")).unwrap_or_default(), policy.as_ref().map(|p| format!(" in policy {p}")).unwrap_or_else(|| " in every policy".into())),
            Query::ServiceRules { service, policy } => format!("what applies to {service}{} and which rules can fire for it", policy.as_ref().map(|p| format!(" under policy {p}")).unwrap_or_default()),
            Query::ProviderPolicies(p) => format!("which policies allow provider {p}"),
            Query::ComparePolicies(a, b) => format!("how policies {a} and {b} differ"),
            Query::RelatedRules(r) => format!("which rules are connected to {r}"),
            Query::List(k) => format!("the list of {k}"),
        }
    }

    pub fn query_json(q: &Query) -> Value {
        let mut m = Map::new();
        let mut put = |k: &str, v: &str| {
            m.insert(k.into(), json!(v));
        };
        match q {
            Query::RuleInfo(r) => {
                put("op", "rule_info");
                put("rule", r);
            }
            Query::RuleFields(r) => {
                put("op", "rule_fields");
                put("rule", r);
            }
            Query::FieldRules(f) => {
                put("op", "field_rules");
                put("field", f);
            }
            Query::RulesBySeverity(s) => {
                put("op", "rules_by_severity");
                put("severity", s);
            }
            Query::RulesConsulting(a) => {
                put("op", "rules_consulting");
                put("attr", a);
            }
            Query::PolicyAttr { policy, attr, service } => {
                put("op", "policy_attr");
                put("attr", attr);
                if let Some(p) = policy {
                    put("policy", p);
                }
                if let Some(s) = service {
                    put("service", s);
                }
            }
            Query::ServiceRules { service, policy } => {
                put("op", "service_rules");
                put("service", service);
                if let Some(p) = policy {
                    put("policy", p);
                }
            }
            Query::ProviderPolicies(p) => {
                put("op", "provider_policies");
                put("provider", p);
            }
            Query::ComparePolicies(a, b) => {
                put("op", "compare_policies");
                put("a", a);
                put("b", b);
            }
            Query::RelatedRules(r) => {
                put("op", "related_rules");
                put("rule", r);
            }
            Query::List(k) => {
                put("op", "list");
                put("kind", k);
            }
        }
        Value::Object(m)
    }
}

fn short(v: &Value) -> String {
    match v {
        Value::Array(a) => format!("[{}]", a.iter().map(|x| x.as_str().map(str::to_string).unwrap_or_else(|| x.to_string())).collect::<Vec<_>>().join(", ")),
        Value::Object(m) => format!("{{{}}}", m.iter().map(|(k, x)| format!("{k}: {}", x.as_str().map(str::to_string).unwrap_or_else(|| num(x)))).collect::<Vec<_>>().join(", ")),
        Value::String(s) => s.clone(),
        other => num(other),
    }
}

/// Checks a model's written-up answer against the facts it was given. The answer must cite facts as [1], [2] ..., every sentence must carry at least
/// one citation to a fact that exists, and every identifier and number it states must occur in the facts it was given. Anything else is refused, and
/// the caller shows the facts themselves.
pub fn check_generated(text: &str, facts: &[Fact]) -> Result<(), String> {
    let t = text.trim();
    if t.is_empty() {
        return Err("empty".into());
    }
    if t.chars().count() > 1500 {
        return Err("too long".into());
    }
    let cite = Regex::new(r"\[(\d{1,2})\]").expect("a valid pattern");
    let mut cited = 0;
    for m in cite.captures_iter(t).filter_map(|c| c.ok()) {
        let n: usize = m.get(1).and_then(|g| g.as_str().parse().ok()).unwrap_or(0);
        if n == 0 || n > facts.len() {
            return Err(format!("it cites fact [{n}], which does not exist"));
        }
        cited += 1;
    }
    if cited == 0 {
        return Err("it cites no fact".into());
    }
    for s in t.split(|c| c == '.' || c == '\n').map(str::trim).filter(|s| s.chars().filter(|c| c.is_alphabetic()).count() >= 12) {
        if !cite.is_match(s).unwrap_or(false) {
            return Err(format!("a sentence has no citation: {}", s.chars().take(60).collect::<String>()));
        }
    }
    let haystack = facts.iter().map(|f| format!("{} {}", f.text, f.source)).collect::<Vec<_>>().join(" ").to_lowercase();
    let token = Regex::new(r"(?i)\b(?:R\d{3}|SVC-[A-Z]+|EDU-[A-Z0-9-]+|DX-[A-Z0-9-]+|\d+(?:\.\d+)?)\b").expect("a valid pattern");
    let without_cites = cite.replace_all(t, "").to_string();
    for m in token.find_iter(&without_cites).filter_map(|m| m.ok()) {
        if !haystack.contains(&m.as_str().to_lowercase()) {
            return Err(format!("it states {:?}, which is in none of the facts", m.as_str()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph() -> Graph {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let read = |p: &str| -> Value { serde_json::from_str(&std::fs::read_to_string(root.join(p)).unwrap()).unwrap() };
        Graph::build(&read("rules/rules.json"), &read("rules/policies.json"), &read("rules/services.json"), &read("rules/providers.json"), &read("rules/diagnoses.json"), cg_core::localize::glossary_json(), 1)
    }

    fn sources(f: &[Fact]) -> String {
        f.iter().map(|f| f.source.clone()).collect::<Vec<_>>().join(" | ")
    }

    #[test]
    fn the_graph_is_derived_from_the_rulebook_files() {
        let g = graph();
        let c = g.counts();
        assert_eq!((c["rules"].as_u64(), c["services"].as_u64(), c["policies"].as_u64(), c["providers"].as_u64()), (Some(15), Some(6), Some(2), Some(3)));
        assert!(g.rules["R008"].consults.contains("auth_required_services") && g.rules["R008"].mentions.contains("R009"));
        assert!(g.rules["R013"].consults.contains("max_unit_price") && g.rules["R013"].consults.contains("max_quantity_per_line"));
        assert!(g.rules["R011"].consults.contains("catalogue") && g.rules["R015"].consults.contains("currency") && g.rules["R001"].consults.is_empty());
    }

    #[test]
    fn a_multi_hop_question_finds_the_rules_through_the_policy() {
        let g = graph();
        let q = g.plan("Which rules can fire for SVC-IMAGE under EDU-BASIC?").unwrap();
        assert_eq!(q, Query::ServiceRules { service: "SVC-IMAGE".into(), policy: Some("EDU-BASIC".into()) });
        let facts = g.run(&q);
        let text = Graph::answer_text(&facts);
        for rule in ["R008", "R009", "R010", "R013", "R011"] {
            assert!(text.contains(&format!("{rule} ")), "{rule} missing:\n{text}");
        }
        assert!(text.contains("needs an authorization reference") && text.contains("imaging-report") && text.contains("2200"));
        assert!(!text.contains("R005 "), "the network rule does not depend on the service");
        assert!(sources(&facts).contains("policy EDU-BASIC (table version 1)") && sources(&facts).contains("rule R008"));
        // a service the policy does not restrict: no authorization hop
        let q = g.plan("which rules apply to SVC-LAB under EDU-BASIC").unwrap();
        let text = Graph::answer_text(&g.run(&q));
        assert!(text.contains("does not need an authorization reference") && !text.contains("R008 "), "{text}");
    }

    #[test]
    fn rule_field_and_policy_questions() {
        let g = graph();
        let facts = g.run(&g.plan("what fields does R003 read").unwrap());
        let t = Graph::answer_text(&facts);
        assert!(t.contains("coverage.status") && t.contains("coverage.start_date") && t.contains("lines[].service_date"), "{t}");
        let t = Graph::answer_text(&g.run(&g.plan("which rules use the service_date field").unwrap()));
        for r in ["R001", "R002", "R003", "R006", "R009", "R010", "R014"] {
            assert!(t.contains(&format!("{r} ")), "{r}: {t}");
        }
        let t = Graph::answer_text(&g.run(&g.plan("which rules read the network of allowed providers").unwrap()));
        assert!(t.contains("R005"), "{t}");
        let t = Graph::answer_text(&g.run(&g.plan("what is the highest unit price for SVC-THERAPY").unwrap()));
        assert!(t.contains("450"), "{t}");
        let t = Graph::answer_text(&g.run(&g.plan("how do policies EDU-BASIC and EDU-PLUS differ").unwrap()));
        assert!(t.contains("submission_window_days differs") && t.contains("30") && t.contains("60"), "{t}");
        let t = Graph::answer_text(&g.run(&g.plan("which policies allow EDU-PROV-02").unwrap()));
        assert!(t.contains("allows provider EDU-PROV-02") && t.contains("R005"), "{t}");
        let t = Graph::answer_text(&g.run(&g.plan("which rules have high severity").unwrap()));
        assert!(t.contains("R001") && !t.contains("R006 "), "R006 is medium: {t}");
        let t = Graph::answer_text(&g.run(&g.plan("how are R008 and R009 related").unwrap()));
        assert!(t.contains("R008 names R009") && t.contains("names R008."), "{t}");
    }

    #[test]
    fn questions_the_graph_cannot_answer_are_left_to_passage_search() {
        let g = graph();
        for q in ["how do I appeal a rejected claim", "what is the weather today", "", "tell me something interesting", "ما هي قاعدة التفويض المسبق"] {
            assert_eq!(g.plan(q), None, "{q}");
        }
    }

    #[test]
    fn a_models_proposal_is_accepted_only_if_every_identifier_exists() {
        let g = graph();
        let ok = g.query_from_json(&json!({"op": "service_rules", "service": "svc-image", "policy": "EDU-PLUS"})).unwrap();
        assert_eq!(ok, Query::ServiceRules { service: "SVC-IMAGE".into(), policy: Some("EDU-PLUS".into()) });
        for bad in [
            json!({"op": "service_rules", "service": "SVC-MADE-UP"}),
            json!({"op": "rule_info", "rule": "R999"}),
            json!({"op": "drop_database"}),
            json!({"op": "rule_info", "rule": "R001", "extra": 1}),
            json!({"op": "rules_by_severity", "severity": "critical"}),
            json!({"op": "policy_attr", "attr": "password"}),
            json!("rule_info"),
            json!({"rule": "R001"}),
        ] {
            assert!(g.query_from_json(&bad).is_err(), "{bad}");
        }
        let q = g.plan("which rules can fire for SVC-IMAGE under EDU-BASIC").unwrap();
        assert_eq!(g.query_from_json(&Graph::query_json(&q)).unwrap(), q, "a query survives its own JSON");
    }

    #[test]
    fn a_written_up_answer_is_checked_against_its_facts() {
        let facts = vec![Fact::new("R008 Required authorization reference (high severity) checks the authorization reference.", "rule R008"), Fact::new("Policy EDU-BASIC: SVC-IMAGE needs an authorization reference.", "policy EDU-BASIC (table version 1)")];
        assert!(check_generated("Policy EDU-BASIC requires an authorization reference for SVC-IMAGE [2]. Rule R008 checks that the reference is present [1].", &facts).is_ok());
        for (bad, why) in [
            ("Rule R008 checks the reference.", "cites no fact"),
            ("Rule R008 checks the reference [3].", "does not exist"),
            ("Rule R009 checks the approval record on file [1].", "R009"),
            ("Rule R008 checks it [1]. The highest unit price is 9999 SAR for this service [2].", "9999"),
            ("Rule R008 checks it [1]. Claims are paid within a very short time frame otherwise.", "no citation"),
            ("", "empty"),
        ] {
            let e = check_generated(bad, &facts).unwrap_err();
            assert!(e.contains(why), "{bad:?}: {e}");
        }
    }
}
