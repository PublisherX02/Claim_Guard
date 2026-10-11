//! The dispatcher: deals ready claims into the personal inboxes of eligible agents, fairly, reproducibly and without waiting
//! (`src/workqueue/dispatcher.py`).
//!
//! An agent never sees the whole pool. Each agent holds at most `slice_size` claims, leased to them one by one; the dispatcher tops an inbox
//! up when it drops below `low_water`, so nobody waits for work while work exists. Nobody assigns a particular claim to a particular
//! person: an administrator sets capacity and shifts, the formula and a logged random seed do the rest.
//!
//! `plan_deal` is pure: the same pool, agents, loads, configuration and seed always give the same plan. The deal document stores exactly
//! those inputs, so `replay` can re-derive the plan later and `verify` can prove the stored assignments are what the algorithm yields.
//! (Replay is reproducible within the Rust build; the random streams of the Python and Rust builds differ, so a deal is replayed by the
//! build that wrote it.)

use crate::flow::{release, routing_config};
use crate::routing_config::{self as rc, RoutingConfig};
use crate::store::QueueStore;
use cg_access::securitylog::SecurityLog;
use cg_access::Clock;
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

pub const MAX_POOL: usize = 5000;
pub const DECIDE: &str = "claims.decide";
pub const DECIDE_HIGH: &str = "claims.decide_high";
const AVOID_EVENTS: [&str; 2] = ["lease_expired", "escalated"]; // whoever held or escalated a claim is the last choice to get it again

#[derive(Debug, Clone, PartialEq)]
pub struct Agent {
    pub badge_id: String,
    pub permissions: BTreeSet<String>,
    pub active: bool,
}

/// The part of a ready claim a plan depends on, small enough to keep in the deal document. The patient identifier is not kept: only the
/// badges the configuration bars from this patient's claims, which is all the plan needs from it.
#[derive(Debug, Clone, PartialEq)]
pub struct Snap {
    pub claim_id: String,
    pub version: i64,
    pub state_at: f64,
    pub score: i64,
    pub eligibility: String,
    pub excluded_for: Vec<String>,
    pub prior_deciders: Vec<String>,
    pub avoid: Vec<String>,
}

impl Snap {
    fn to_json(&self) -> Value {
        json!({"claim_id": self.claim_id, "version": self.version, "state_at": self.state_at, "score": self.score, "eligibility": self.eligibility,
               "excluded_for": self.excluded_for, "prior_deciders": self.prior_deciders, "avoid": self.avoid})
    }

    fn from_json(v: &Value) -> Snap {
        let list = |k: &str| v[k].as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect()).unwrap_or_default();
        Snap {
            claim_id: v["claim_id"].as_str().unwrap_or("").into(), version: v["version"].as_i64().unwrap_or(0), state_at: v["state_at"].as_f64().unwrap_or(0.0),
            score: v["score"].as_i64().unwrap_or(0), eligibility: v["eligibility"].as_str().unwrap_or("decide").into(),
            excluded_for: list("excluded_for"), prior_deciders: list("prior_deciders"), avoid: list("avoid"),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Deal {
    pub deal_id: String,
    pub seed: u64,
    pub config_version: i64,
    pub planned: Vec<(String, String, i64)>,
    /// The part of the plan that was won (another dispatcher may win a claim first).
    pub assigned: Vec<(String, String, i64)>,
}

fn needed(s: &Snap) -> &'static str {
    if s.eligibility == "decide_high" { DECIDE_HIGH } else { DECIDE }
}

fn priority(s: &Snap, cfg: &RoutingConfig, now: f64) -> f64 {
    let waited = ((now - s.state_at) / 3600.0).max(0.0);
    s.score as f64 + cfg.aging_per_hour * waited
}

fn blocked(agent: &Agent, s: &Snap) -> bool {
    s.prior_deciders.contains(&agent.badge_id) || s.excluded_for.contains(&agent.badge_id)
}

/// Returns `(claim_id, badge_id, version)` triples. Pure and deterministic.
///
/// Claims are shuffled by the seed (so ties are random but reproducible), then stably sorted by priority = score plus a little for every
/// hour waited. Each claim goes to the eligible agent with the least work (sum of scores) so far; ties by the seeded order of agents. A
/// claim that was handed back from an agent is offered to someone else first when anyone else is eligible.
pub fn plan_deal(ready: &[Snap], agents: &[Agent], loads: &HashMap<String, (i64, i64)>, cfg: &RoutingConfig, seed: u64, now: f64, max_per_agent: Option<i64>) -> Result<Vec<(String, String, i64)>, String> {
    let mut seen = HashSet::new();
    if !agents.iter().all(|a| seen.insert(&a.badge_id)) {
        return Err("an agent appears twice".into());
    }
    let considered: Vec<&Agent> = agents.iter().filter(|a| a.active && cfg.on_shift.contains(&a.badge_id)).collect();
    let load_of = |b: &str| loads.get(b).copied().unwrap_or((0, 0));
    let mut room: HashMap<&str, i64> = considered.iter().map(|a| (a.badge_id.as_str(), cfg.slice_size - load_of(&a.badge_id).0)).collect();
    if let Some(cap) = max_per_agent {
        room.values_mut().for_each(|n| *n = (*n).min(cap));
    }
    let mut load: HashMap<&str, i64> = considered.iter().map(|a| (a.badge_id.as_str(), load_of(&a.badge_id).1)).collect();
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut order: Vec<&Snap> = ready.iter().collect();
    order.shuffle(&mut rng);
    order.sort_by(|a, b| priority(b, cfg, now).partial_cmp(&priority(a, cfg, now)).unwrap_or(std::cmp::Ordering::Equal));
    let mut pool = considered.clone();
    pool.sort_by(|a, b| a.badge_id.cmp(&b.badge_id));
    pool.shuffle(&mut rng);
    let rank: HashMap<&str, usize> = pool.iter().enumerate().map(|(i, a)| (a.badge_id.as_str(), i)).collect();
    let mut out = vec![];
    for s in order {
        let mut ok: Vec<&&Agent> = pool.iter().filter(|a| a.permissions.contains(needed(s)) && room[a.badge_id.as_str()] > 0 && !blocked(a, s)).collect();
        let fresh: Vec<&&Agent> = ok.iter().filter(|a| !s.avoid.contains(&a.badge_id)).copied().collect();
        if !fresh.is_empty() {
            ok = fresh;
        }
        let Some(pick) = ok.into_iter().min_by_key(|a| (load[a.badge_id.as_str()], rank[a.badge_id.as_str()])) else { continue };
        out.push((s.claim_id.clone(), pick.badge_id.clone(), s.version));
        *room.get_mut(pick.badge_id.as_str()).expect("considered agent") -= 1;
        *load.get_mut(pick.badge_id.as_str()).expect("considered agent") += s.score;
    }
    Ok(out)
}

fn ids_hash(ready: &[Value]) -> String {
    let mut pairs: Vec<(String, i64)> = ready.iter().map(|d| (d["claim_id"].as_str().unwrap_or("").to_string(), d["version"].as_i64().unwrap_or(0))).collect();
    pairs.sort();
    let text = format!("[{}]", pairs.iter().map(|(c, v)| format!("[{},{}]", serde_json::to_string(c).unwrap_or_default(), v)).collect::<Vec<_>>().join(","));
    hex::encode(Sha256::digest(text.as_bytes()))
}

pub fn new_id() -> String {
    let mut b = [0u8; 16];
    rand::thread_rng().fill(&mut b);
    hex::encode(b)
}

pub struct Dispatcher {
    pub store: Arc<dyn QueueStore>,
    pub agents: Arc<dyn Fn() -> Vec<Agent> + Send + Sync>,
    pub clock: Clock,
    pub rng_seed: Option<u64>,
    pub log: Option<Arc<SecurityLog>>,
}

impl Dispatcher {
    pub fn routing_config(&self) -> RoutingConfig {
        routing_config(&*self.store)
    }

    fn config_version(&self, version: i64) -> Result<RoutingConfig, String> {
        for doc in self.store.config_history().map_err(|e| e.to_string())? {
            if doc["version"].as_i64() == Some(version) {
                return rc::from_doc(&doc);
            }
        }
        if version == RoutingConfig::default().version && self.store.latest_config().map_err(|e| e.to_string())?.is_none() {
            return Ok(RoutingConfig::default());
        }
        Err(format!("routing configuration version {version} is not stored"))
    }

    /// Badges that decided an earlier version of this claim (and signers: nobody signs the same claim twice), and badges whose lease on
    /// this version already ran out or who escalated it.
    fn history_of(&self, doc: &Value) -> (BTreeSet<String>, BTreeSet<String>) {
        let mut prior: BTreeSet<String> = ["first_by", "second_by"].iter().filter_map(|k| doc["signoff"][*k].as_str()).filter(|s| !s.is_empty()).map(str::to_string).collect();
        let id = doc["claim_id"].as_str().unwrap_or("");
        for v in 1..doc["version"].as_i64().unwrap_or(1) {
            if let Ok(Some(earlier)) = self.store.get(id, Some(v)) {
                if let Some(b) = earlier["decided_by"].as_str().filter(|s| !s.is_empty()) {
                    prior.insert(b.to_string());
                }
            }
        }
        let avoid = doc["events"].as_array().into_iter().flatten().filter(|e| AVOID_EVENTS.contains(&e["detail"]["event"].as_str().unwrap_or(""))).filter_map(|e| e["detail"]["badge_id"].as_str()).filter(|s| !s.is_empty()).map(str::to_string).collect();
        (prior, avoid)
    }

    fn deal_inner(&self, now: Option<f64>, only: Option<&str>, full: bool, max_per_agent: Option<i64>) -> Deal {
        let now = now.unwrap_or_else(|| (self.clock)());
        self.reclaim_ineligible(now);
        let cfg = self.routing_config();
        let agents: Vec<Agent> = (self.agents)().into_iter().filter(|a| only.map_or(true, |o| a.badge_id == o)).collect();
        let mut considered: Vec<Agent> = agents.into_iter().filter(|a| a.active && cfg.on_shift.contains(&a.badge_id)).collect();
        let loads: HashMap<String, (i64, i64)> = considered.iter().map(|a| (a.badge_id.clone(), self.store.inbox_load(&a.badge_id).unwrap_or((0, 0)))).collect();
        considered.retain(|a| loads[&a.badge_id].0 < cfg.slice_size && (full || loads[&a.badge_id].0 < cfg.low_water));
        let mut pool = self.store.by_state("ready", MAX_POOL).unwrap_or_default();
        pool.extend(self.store.by_state("awaiting_countersign", MAX_POOL).unwrap_or_default());
        let newest = self.store.latest_versions(&pool.iter().filter_map(|d| d["claim_id"].as_str().map(str::to_string)).collect::<Vec<_>>()).unwrap_or_default();
        let ready: Vec<Value> = pool.into_iter().filter(|d| newest.get(d["claim_id"].as_str().unwrap_or("")).copied().unwrap_or(0) <= d["version"].as_i64().unwrap_or(0)).collect(); // only the newest version of a claim is dealt
        let snaps: Vec<Snap> = ready
            .iter()
            .map(|d| {
                let (prior, avoid) = self.history_of(d);
                let patient = d["claim"]["patient_id"].as_str().unwrap_or("");
                let mut snap = Snap {
                    claim_id: d["claim_id"].as_str().unwrap_or("").into(), version: d["version"].as_i64().unwrap_or(0), state_at: d["state_at"].as_f64().unwrap_or(0.0),
                    score: d["receipt"]["score"].as_i64().unwrap_or(0), eligibility: d["receipt"]["eligibility"].as_str().unwrap_or("decide").into(),
                    excluded_for: cfg.exclusions.iter().filter(|(_, who)| who == patient).map(|(b, _)| b.clone()).collect::<BTreeSet<_>>().into_iter().collect(),
                    prior_deciders: prior.into_iter().collect(), avoid: avoid.into_iter().collect(),
                };
                if d["escalated"] == true || !d["signoff"].is_null() {
                    snap.eligibility = "decide_high".into(); // an escalated claim, or one awaiting a countersignature, needs a senior
                }
                snap
            })
            .collect();
        let seed = self.rng_seed.unwrap_or_else(|| rand::thread_rng().gen::<u64>() >> 1);
        let plan = if considered.is_empty() { vec![] } else { plan_deal(&snaps, &considered, &loads, &cfg, seed, now, max_per_agent).unwrap_or_default() };
        let mut deal = Deal { deal_id: new_id(), seed, config_version: cfg.version, planned: plan.clone(), assigned: vec![] };
        if plan.is_empty() {
            return deal;
        }
        let expires = now + cfg.lease_seconds as f64;
        for (claim_id, badge, version) in &plan {
            if matches!(self.store.lease(claim_id, *version, badge, now, expires), Ok(Some(_))) {
                deal.assigned.push((claim_id.clone(), badge.clone(), *version));
                if let Some(log) = &self.log {
                    let _ = log.record("claim_dealt", json!({"deal_id": deal.deal_id, "claim_id": claim_id, "badge_id": badge}));
                }
            }
        }
        let triple = |t: &(String, String, i64)| json!([t.0, t.1, t.2]);
        let _ = self.store.append_deal(&json!({
            "deal_id": deal.deal_id, "seed": seed, "at": now, "config_version": cfg.version, "ids_hash": ids_hash(&ready), "max_per_agent": max_per_agent, "full": full,
            "agents": considered.iter().map(|a| json!({"badge_id": a.badge_id, "permissions": a.permissions, "count": loads[&a.badge_id].0, "points": loads[&a.badge_id].1})).collect::<Vec<_>>(),
            "claims": snaps.iter().map(Snap::to_json).collect::<Vec<_>>(), "planned": plan.iter().map(triple).collect::<Vec<_>>(), "assigned": deal.assigned.iter().map(triple).collect::<Vec<_>>(),
        }));
        deal
    }

    /// Fills inboxes from the pool. `full = false` tops up only agents below the low-water mark (what the scheduler runs).
    pub fn deal(&self, now: Option<f64>, full: bool) -> Deal {
        self.deal_inner(now, None, full, None)
    }

    /// Fills one agent's inbox to the slice size if it holds fewer than `low_water` claims. Returns how many were added.
    pub fn top_up_if_low(&self, badge_id: &str) -> usize {
        let cfg = self.routing_config();
        if self.store.inbox(badge_id).map(|i| i.len() as i64).unwrap_or(0) >= cfg.low_water {
            return 0;
        }
        self.deal_inner(None, Some(badge_id), true, None).assigned.len()
    }

    /// One more claim for an agent who asks for it now (capacity permitting), or None.
    pub fn next_for(&self, badge_id: &str) -> Option<Value> {
        let deal = self.deal_inner(None, Some(badge_id), true, Some(1));
        let (claim_id, _, version) = deal.assigned.first()?;
        self.store.get(claim_id, Some(*version)).ok().flatten()
    }

    /// Returns every lease that ran out to the pool. Returns how many were returned.
    pub fn expire(&self, now: Option<f64>) -> usize {
        let now = now.unwrap_or_else(|| (self.clock)());
        let mut count = 0;
        for doc in self.store.expired(now).unwrap_or_default() {
            let holder = doc["lease"]["badge_id"].as_str().unwrap_or("").to_string();
            if matches!(release(&*self.store, &doc, now, "system:dispatcher", "lease_expired"), Ok(Some(_))) {
                count += 1;
                if let Some(log) = &self.log {
                    let _ = log.record("lease_expired", json!({"claim_id": doc["claim_id"], "badge_id": holder}));
                }
            }
        }
        count + self.reclaim_ineligible(now)
    }

    /// Takes back claims held by an agent who may no longer decide them (demoted, deactivated, a permission revoked). A lease confers no
    /// rights of its own: the permission is checked again here and again when a decision arrives.
    pub fn reclaim_ineligible(&self, now: f64) -> usize {
        let by_badge: HashMap<String, Agent> = (self.agents)().into_iter().map(|a| (a.badge_id.clone(), a)).collect();
        let rows = self.store.leased_summary().unwrap_or_default();
        let newest = self.store.latest_versions(&rows.iter().filter_map(|r| r["claim_id"].as_str().map(str::to_string)).collect::<Vec<_>>()).unwrap_or_default();
        let mut count = 0;
        for row in rows {
            let badge = row["badge_id"].as_str().unwrap_or("");
            let id = row["claim_id"].as_str().unwrap_or("");
            let version = row["version"].as_i64().unwrap_or(0);
            let need = if row["eligibility"] == "decide_high" || row["escalated"] == true || row["signoff"] == true { DECIDE_HIGH } else { DECIDE };
            let superseded = newest.get(id).copied().unwrap_or(version) > version;
            let fine = !superseded && by_badge.get(badge).is_some_and(|a| a.active && a.permissions.contains(need));
            if fine {
                continue;
            }
            let reason = if superseded { "superseded" } else { "agent_ineligible" };
            let held = json!({"claim_id": id, "version": version, "lease": {"badge_id": badge}});
            if matches!(release(&*self.store, &held, now, "system:dispatcher", reason), Ok(Some(_))) {
                count += 1;
                if let Some(log) = &self.log {
                    let _ = log.record("lease_reclaimed", json!({"claim_id": id, "badge_id": badge, "reason": reason}));
                }
            }
        }
        count
    }

    /// Re-derives the plan from the inputs stored in the deal.
    pub fn replay(&self, deal_id: &str) -> Result<Vec<(String, String, i64)>, String> {
        let deal = self.store.get_deal(deal_id).map_err(|e| e.to_string())?.ok_or("no such deal")?;
        let cfg = self.config_version(deal["config_version"].as_i64().unwrap_or(0))?;
        let agents: Vec<Agent> = deal["agents"].as_array().into_iter().flatten().map(|a| Agent {
            badge_id: a["badge_id"].as_str().unwrap_or("").into(),
            permissions: a["permissions"].as_array().into_iter().flatten().filter_map(|p| p.as_str().map(str::to_string)).collect(),
            active: true,
        }).collect();
        let loads = deal["agents"].as_array().into_iter().flatten().map(|a| (a["badge_id"].as_str().unwrap_or("").to_string(), (a["count"].as_i64().unwrap_or(0), a["points"].as_i64().unwrap_or(0)))).collect();
        let ready: Vec<Snap> = deal["claims"].as_array().into_iter().flatten().map(Snap::from_json).collect();
        plan_deal(&ready, &agents, &loads, &cfg, deal["seed"].as_u64().unwrap_or(0), deal["at"].as_f64().unwrap_or(0.0), deal["max_per_agent"].as_i64())
    }

    /// True when the stored plan is exactly what the algorithm gives for the stored inputs and seed.
    pub fn verify(&self, deal_id: &str) -> Result<bool, String> {
        let deal = self.store.get_deal(deal_id).map_err(|e| e.to_string())?.ok_or("no such deal")?;
        let replayed: Vec<Value> = self.replay(deal_id)?.iter().map(|t| json!([t.0, t.1, t.2])).collect();
        Ok(Value::Array(replayed) == deal["planned"])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::make_doc;
    use crate::store::MemoryQueueStore;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn agent(b: &str, high: bool) -> Agent {
        let mut p: BTreeSet<String> = ["claims.decide".to_string()].into_iter().collect();
        if high {
            p.insert("claims.decide_high".into());
        }
        Agent { badge_id: b.into(), permissions: p, active: true }
    }

    fn snap(id: &str, score: i64, high: bool) -> Snap {
        Snap { claim_id: id.into(), version: 1, state_at: 0.0, score, eligibility: if high { "decide_high" } else { "decide" }.into(), excluded_for: vec![], prior_deciders: vec![], avoid: vec![] }
    }

    fn cfg(on: &[&str], slice: i64) -> RoutingConfig {
        RoutingConfig { on_shift: on.iter().map(|s| s.to_string()).collect(), slice_size: slice, low_water: slice.min(3), ..RoutingConfig::default() }
    }

    #[test]
    fn the_plan_is_deterministic_for_a_seed_and_differs_between_seeds() {
        let ready: Vec<Snap> = (0..30).map(|i| snap(&format!("C{i:02}"), 2, false)).collect();
        let agents = [agent("A", false), agent("B", false), agent("C", false)];
        let c = cfg(&["A", "B", "C"], 25);
        let p1 = plan_deal(&ready, &agents, &HashMap::new(), &c, 7, 0.0, None).unwrap();
        assert_eq!(p1, plan_deal(&ready, &agents, &HashMap::new(), &c, 7, 0.0, None).unwrap());
        assert_ne!(p1, plan_deal(&ready, &agents, &HashMap::new(), &c, 8, 0.0, None).unwrap());
        assert_eq!(p1.len(), 30);
        let per: HashMap<&str, usize> = p1.iter().fold(HashMap::new(), |mut m, t| { *m.entry(t.1.as_str()).or_default() += 1; m });
        assert!(per.values().all(|n| *n == 10), "the work is spread evenly: {per:?}");
    }

    #[test]
    fn nobody_gets_more_than_the_slice_and_high_claims_go_only_to_seniors() {
        let ready: Vec<Snap> = (0..20).map(|i| snap(&format!("C{i:02}"), 2, i % 2 == 0)).collect();
        let agents = [agent("J1", false), agent("S1", true)];
        let plan = plan_deal(&ready, &agents, &HashMap::new(), &cfg(&["J1", "S1"], 6), 1, 0.0, None).unwrap();
        assert!(plan.iter().filter(|t| t.1 == "J1").count() <= 6 && plan.iter().filter(|t| t.1 == "S1").count() <= 6);
        for (id, badge, _) in &plan {
            let n: usize = id[1..].parse().unwrap();
            assert!(!(n % 2 == 0 && badge == "J1"), "{id} is high severity and went to a junior");
        }
        let with_load = HashMap::from([("S1".to_string(), (6, 12))]);
        assert!(plan_deal(&ready, &agents, &with_load, &cfg(&["J1", "S1"], 6), 1, 0.0, None).unwrap().iter().all(|t| t.1 != "S1"), "a full inbox gets nothing");
    }

    #[test]
    fn exclusions_prior_deciders_and_off_shift_agents_are_respected_and_handed_back_claims_go_elsewhere_first() {
        let mut s = snap("C1", 2, false);
        s.excluded_for = vec!["A".into()];
        s.prior_deciders = vec!["B".into()];
        let agents = [agent("A", false), agent("B", false), agent("C", false), agent("OFF", false)];
        let plan = plan_deal(&[s.clone()], &agents, &HashMap::new(), &cfg(&["A", "B", "C"], 5), 3, 0.0, None).unwrap();
        assert_eq!(plan, vec![("C1".to_string(), "C".to_string(), 1)]);
        let mut back = snap("C2", 2, false);
        back.avoid = vec!["A".into()];
        for seed in 0..20 {
            let p = plan_deal(&[back.clone()], &[agent("A", false), agent("B", false)], &HashMap::new(), &cfg(&["A", "B"], 5), seed, 0.0, None).unwrap();
            assert_eq!(p[0].1, "B", "someone else first");
        }
        let only = plan_deal(&[back], &[agent("A", false)], &HashMap::new(), &cfg(&["A"], 5), 0, 0.0, None).unwrap();
        assert_eq!(only[0].1, "A", "when nobody else is eligible the same person may take it again");
        assert!(plan_deal(&[], &[agent("A", false), agent("A", true)], &HashMap::new(), &cfg(&["A"], 5), 0, 0.0, None).is_err());
    }

    #[test]
    fn the_higher_score_and_the_longer_wait_come_first() {
        let mut old = snap("OLD", 1, false);
        old.state_at = -36000.0;
        let ready = [snap("BIG", 4, false), snap("SMALL", 1, false), old];
        let plan = plan_deal(&ready, &[agent("A", false)], &HashMap::new(), &RoutingConfig { aging_per_hour: 1.0, ..cfg(&["A"], 25) }, 1, 0.0, Some(2)).unwrap();
        assert_eq!(plan.iter().map(|t| t.0.as_str()).collect::<Vec<_>>(), ["OLD", "BIG"], "ten hours of waiting outweighs three points; max_per_agent caps the deal");
    }

    fn world(agents: Vec<Agent>, config: RoutingConfig) -> (Dispatcher, Arc<MemoryQueueStore>, Arc<AtomicU64>) {
        let store = Arc::new(MemoryQueueStore::new());
        let t = Arc::new(AtomicU64::new(1_000_000));
        let t2 = t.clone();
        let clock: Clock = Arc::new(move || t2.load(Ordering::SeqCst) as f64);
        store.put_config(&rc::to_doc(&RoutingConfig { version: 1, ..config }).unwrap(), 0).unwrap();
        let agents = Arc::new(Mutex::new(agents));
        let a2 = agents.clone();
        (Dispatcher { store: store.clone(), agents: Arc::new(move || a2.lock().unwrap().clone()), clock, rng_seed: Some(5), log: None }, store, t)
    }
    use std::sync::Mutex;

    fn ready(store: &MemoryQueueStore, id: &str, eligibility: &str, score: i64) {
        assert!(store.put_triaged(&make_doc(id, 1, &format!("h{id}"), "A", eligibility, score, "P1", 1000.0, None)).unwrap());
        store.transition(id, 1, "triaged", "ready", "t", 1_000_000.0, None, None, None).unwrap().unwrap();
    }

    #[test]
    fn dealing_leases_claims_records_a_deal_that_replays_and_tops_up_only_low_inboxes() {
        let (d, store, _) = world(vec![agent("A", false), agent("B", false)], cfg(&["A", "B"], 4));
        for i in 0..10 {
            ready(&store, &format!("C{i}"), "decide", 2);
        }
        let deal = d.deal(None, true);
        assert_eq!(deal.assigned.len(), 8, "two agents with room for four each");
        assert_eq!((store.inbox("A").unwrap().len(), store.inbox("B").unwrap().len()), (4, 4));
        assert!(d.verify(&deal.deal_id).unwrap(), "the stored plan is what the algorithm yields");
        assert_eq!(d.replay(&deal.deal_id).unwrap(), deal.planned);
        assert_eq!(d.deal(None, false).assigned.len(), 0, "nobody is below the low-water mark");
        assert!(d.next_for("A").is_none(), "a full inbox gets no more");
    }

    #[test]
    fn expired_leases_return_and_a_demoted_agent_loses_the_claims_it_can_no_longer_decide() {
        let (d, store, t) = world(vec![agent("A", true)], cfg(&["A"], 5));
        ready(&store, "H1", "decide_high", 6);
        ready(&store, "L1", "decide", 2);
        assert_eq!(d.deal(None, true).assigned.len(), 2);
        t.fetch_add(1801, Ordering::SeqCst);
        assert_eq!(d.expire(None), 2);
        assert_eq!(store.get("H1", None).unwrap().unwrap()["state"], "ready");
        assert_eq!(store.get("H1", None).unwrap().unwrap()["events"].as_array().unwrap().last().unwrap()["detail"]["event"], "lease_expired");
        // a junior now: the next deal gives the high claim to nobody and the low one back to A
        let (d2, store2, _) = world(vec![agent("A", true)], cfg(&["A"], 5));
        ready(&store2, "H1", "decide_high", 6);
        d2.deal(None, true);
        let demoted = Dispatcher { agents: Arc::new(|| vec![agent("A", false)]), store: store2.clone(), clock: d2.clock.clone(), rng_seed: Some(5), log: None };
        assert_eq!(demoted.reclaim_ineligible(1_000_100.0), 1);
        assert_eq!(store2.get("H1", None).unwrap().unwrap()["state"], "ready");
    }

    #[test]
    fn only_the_newest_version_of_a_claim_is_dealt() {
        let (d, store, _) = world(vec![agent("A", false)], cfg(&["A"], 5));
        ready(&store, "C1", "decide", 2);
        assert!(store.put_triaged(&make_doc("C1", 2, "h2", "A", "decide", 2, "P1", 1001.0, None)).unwrap());
        assert_eq!(d.deal(None, true).assigned.len(), 0, "version 1 is superseded and version 2 is not ready yet");
    }
}
