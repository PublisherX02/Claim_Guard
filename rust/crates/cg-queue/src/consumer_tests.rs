//! Tests of the consumer pool, against the in-memory broker and store. `cg-redis` runs the broker contract against real Redis.

use super::*;
use crate::breaker::{CircuitBreaker, ModelError};
use crate::broker::MemoryBroker;
use crate::contract::doc;
use crate::explain::{default_guard, ExplainStep, Model};
use crate::routing_config::RoutingConfig;
use crate::store::{MemoryQueueStore, QueueStore};
use crate::triage;
use cg_access::Clock;
use serde_json::{json, Value};
use std::time::Instant;

fn wall() -> Clock {
    Arc::new(|| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0))
}

/// Records how many `ask` calls overlap, and how many there were in all. `panic_always` makes every call panic.
struct Probe {
    running: AtomicUsize,
    peak: AtomicUsize,
    calls: AtomicUsize,
    pause_ms: u64,
    panic_always: bool,
}

impl Model for Probe {
    fn ask(&self, _: &Value, _: f64) -> Result<String, ModelError> {
        if self.panic_always {
            panic!("the model client crashed");
        }
        self.calls.fetch_add(1, Ordering::SeqCst);
        let now = self.running.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(now, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(self.pause_ms));
        self.running.fetch_sub(1, Ordering::SeqCst);
        Ok("The quantity is {value} on line {line}; ask the provider to confirm it.".into())
    }
    fn name(&self) -> String {
        "probe".into()
    }
}

fn probe(pause_ms: u64, panic_always: bool) -> Arc<Probe> {
    Arc::new(Probe { running: AtomicUsize::new(0), peak: AtomicUsize::new(0), calls: AtomicUsize::new(0), pause_ms, panic_always })
}

/// A flagged claim whose explanation template key differs for every i below 45, so no claim is served from the template cache.
fn put_flagged(store: &MemoryQueueStore, i: usize) -> String {
    let id = format!("C{i}");
    let rule = format!("R{:03}", i % 15 + 1);
    let sev = ["high", "medium", "low"][(i / 15) % 3];
    let mut d = doc(&id);
    let mut rows: Vec<Value> = triage::rule_ids()
        .into_iter()
        .map(|r| json!({"rule_id": r, "status": "PASS", "severity": "medium", "affected_line_ids": [], "evidence": [], "explanation": "engine text"}))
        .collect();
    let at = triage::rule_ids().iter().position(|r| *r == rule).unwrap();
    rows[at] = json!({"rule_id": rule, "status": "FAIL", "severity": sev, "affected_line_ids": ["L1"], "evidence": [{"path": "/lines/0/quantity", "value": 3}], "explanation": "engine text"});
    d["results"] = Value::Array(rows);
    d["receipt"]["lane"] = json!("A");
    assert!(store.put_triaged(&d).unwrap());
    id
}

fn put_green(store: &MemoryQueueStore, i: usize) -> String {
    let id = format!("G{i}");
    let mut d = doc(&id);
    d["receipt"]["lane"] = json!("green");
    assert!(store.put_triaged(&d).unwrap());
    id
}

struct World {
    store: Arc<MemoryQueueStore>,
    broker: Arc<MemoryBroker>,
    rt: Arc<Runtime>,
}

fn world(model: Arc<dyn Model>) -> World {
    let store = Arc::new(MemoryQueueStore::new());
    let clock = wall();
    let cfg = RoutingConfig { ai_per_minute: 600, ai_daily_budget: 100_000, ..RoutingConfig::default() };
    let mut step = ExplainStep::new(store.clone(), model, default_guard(), Arc::new(CircuitBreaker::new(clock.clone())), clock.clone(), Arc::new(move || cfg.clone()));
    step.sleep = Arc::new(|_| {});
    let rt = Arc::new(Runtime { store: store.clone(), explain: Arc::new(step), clock: clock.clone(), max_retries: 3 });
    World { store, broker: Arc::new(MemoryBroker::new(clock)), rt }
}

fn fast(concurrency: usize) -> PoolConfig {
    PoolConfig { concurrency, max_attempts: 3, min_idle_ms: 80, block_ms: 20, reclaim_every_ms: 30, max_backoff_ms: 100 }
}

fn publish_all(w: &World, ids: &[String]) {
    for id in ids {
        w.broker.publish(id, 1, &format!("h-{id}")).unwrap();
    }
}

fn state_of(store: &MemoryQueueStore, id: &str) -> String {
    store.get(id, None).unwrap().unwrap()["state"].as_str().unwrap_or("").to_string()
}

fn wait_until(what: &str, f: impl Fn() -> bool) {
    let end = Instant::now() + Duration::from_secs(30);
    while !f() {
        assert!(Instant::now() < end, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn stop_and_join(pool: Pool, stop: &AtomicBool) {
    stop.store(true, Ordering::SeqCst);
    pool.join();
}

#[test]
fn a_burst_never_puts_more_claims_in_front_of_the_model_than_the_pool_size() {
    for n in [1usize, 3, 4] {
        let m = probe(25, false);
        let w = world(m.clone());
        let ids: Vec<String> = (0..40).map(|i| put_flagged(&w.store, i)).collect();
        publish_all(&w, &ids);
        let stop = Arc::new(AtomicBool::new(false));
        let pool = start(w.broker.clone(), w.rt.clone(), fast(n), stop.clone(), "t");
        wait_until("every claim ready", || ids.iter().all(|id| state_of(&w.store, id) == "ready"));
        wait_until("broker drained", || w.broker.stats().unwrap().length == 0);
        let stats = pool.stats.clone();
        stop_and_join(pool, &stop);
        let peak = m.peak.load(Ordering::SeqCst);
        assert!(peak <= n, "pool of {n}: the model saw {peak} at once");
        assert!(stats.peak_in_flight.load(Ordering::SeqCst) <= n);
        assert!(peak >= n.min(2), "pool of {n}: the calls did overlap, so the cap was really exercised (peak {peak})");
        assert_eq!(m.calls.load(Ordering::SeqCst), 40, "one model call per flagged claim, none repeated");
        assert_eq!(stats.processed.load(Ordering::SeqCst), 40);
        for id in &ids {
            let d = w.store.get(id, None).unwrap().unwrap();
            let moved = d["events"].as_array().unwrap().iter().filter(|e| e["to"] == "ready").count();
            assert_eq!(moved, 1, "{id} reached ready exactly once");
        }
    }
}

#[test]
fn green_claims_never_reach_the_model() {
    let m = probe(5, false);
    let w = world(m.clone());
    let ids: Vec<String> = (0..30).map(|i| put_green(&w.store, i)).collect();
    publish_all(&w, &ids);
    let stop = Arc::new(AtomicBool::new(false));
    let pool = start(w.broker.clone(), w.rt.clone(), fast(4), stop.clone(), "t");
    wait_until("every claim ready", || ids.iter().all(|id| state_of(&w.store, id) == "ready"));
    stop_and_join(pool, &stop);
    assert_eq!(m.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn a_message_left_pending_by_a_dead_consumer_is_taken_over_and_finished() {
    let w = world(probe(1, false));
    let id = put_flagged(&w.store, 0);
    publish_all(&w, &[id.clone()]);
    let lost = w.broker.read("dead-consumer", 100).unwrap().expect("taken by the consumer that then dies");
    assert_eq!(lost.attempts, 1);
    assert_eq!(state_of(&w.store, &id), "triaged");
    let stop = Arc::new(AtomicBool::new(false));
    let pool = start(w.broker.clone(), w.rt.clone(), fast(2), stop.clone(), "t");
    wait_until("the abandoned claim is finished", || state_of(&w.store, &id) == "ready");
    wait_until("and acknowledged", || w.broker.stats().unwrap().length == 0);
    let stats = pool.stats.clone();
    stop_and_join(pool, &stop);
    assert!(stats.reclaimed.load(Ordering::SeqCst) >= 1);
}

#[test]
fn a_message_that_keeps_failing_is_parked_after_the_attempt_limit_and_does_not_loop() {
    let w = world(probe(1, true)); // the model client panics on every call
    let id = put_flagged(&w.store, 0);
    publish_all(&w, &[id.clone()]);
    let stop = Arc::new(AtomicBool::new(false));
    let pool = start(w.broker.clone(), w.rt.clone(), fast(2), stop.clone(), "t");
    wait_until("parked", || state_of(&w.store, &id) == "dead_lettered");
    wait_until("and acknowledged", || w.broker.stats().unwrap().length == 0);
    let stats = pool.stats.clone();
    stop_and_join(pool, &stop);
    assert_eq!(stats.dead_lettered.load(Ordering::SeqCst), 1);
    assert_eq!(stats.panicked.load(Ordering::SeqCst), 3, "tried exactly max_attempts times, never acknowledged in between");
    let dead = w.store.dead_letters(10).unwrap();
    assert_eq!(dead.len(), 1);
    assert_eq!((dead[0]["claim_id"].as_str(), dead[0]["reason"].as_str(), dead[0]["attempts"].as_i64()), (Some(id.as_str()), Some("delivery_attempts_exceeded"), Some(4)));
}

#[test]
fn stale_forged_and_unknown_messages_change_nothing_and_are_acknowledged() {
    let m = probe(1, false);
    let w = world(m.clone());
    let id = put_flagged(&w.store, 0);
    w.broker.publish(&id, 1, "not-the-stored-hash").unwrap(); // forged: the hash does not match the stored claim
    w.broker.publish("NO-SUCH-CLAIM", 1, "h").unwrap();
    w.broker.publish(&id, 7, "h-C0").unwrap(); // a version that does not exist
    let stop = Arc::new(AtomicBool::new(false));
    let pool = start(w.broker.clone(), w.rt.clone(), fast(2), stop.clone(), "t");
    wait_until("all three acknowledged", || w.broker.stats().unwrap().length == 0);
    stop_and_join(pool, &stop);
    assert_eq!(state_of(&w.store, &id), "triaged", "nothing moved");
    assert_eq!(m.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn the_same_claim_published_twice_is_finished_once() {
    let m = probe(15, false);
    let w = world(m.clone());
    let id = put_flagged(&w.store, 0);
    publish_all(&w, &[id.clone(), id.clone(), id.clone()]);
    let stop = Arc::new(AtomicBool::new(false));
    let pool = start(w.broker.clone(), w.rt.clone(), fast(3), stop.clone(), "t");
    wait_until("ready", || state_of(&w.store, &id) == "ready");
    wait_until("all copies acknowledged", || w.broker.stats().unwrap().length == 0);
    stop_and_join(pool, &stop);
    let d = w.store.get(&id, None).unwrap().unwrap();
    assert_eq!(d["events"].as_array().unwrap().iter().filter(|e| e["to"] == "ready").count(), 1);
    assert_eq!(m.calls.load(Ordering::SeqCst), 1, "the copies did not ask the model again");
}

#[test]
fn an_unreachable_broker_slows_the_threads_down_but_nothing_is_lost_when_it_returns() {
    let w = world(probe(1, false));
    let ids: Vec<String> = (0..10).map(|i| put_flagged(&w.store, i)).collect();
    publish_all(&w, &ids);
    w.broker.set_down(true);
    let stop = Arc::new(AtomicBool::new(false));
    let pool = start(w.broker.clone(), w.rt.clone(), fast(3), stop.clone(), "t");
    wait_until("errors are being counted", || pool.stats.broker_errors.load(Ordering::SeqCst) >= 3);
    assert!(ids.iter().all(|id| state_of(&w.store, id) == "triaged"), "nothing processed while the broker is down");
    w.broker.set_down(false);
    wait_until("every claim ready after the broker returned", || ids.iter().all(|id| state_of(&w.store, id) == "ready"));
    stop_and_join(pool, &stop);
}

/// Takes every message out of the broker without processing it: what a Redis that restarted empty looks like to the claims.
fn lose_everything(b: &MemoryBroker) {
    while let Some(d) = b.read("void", 5).unwrap() {
        b.ack(&d.msg_id).unwrap();
    }
}

#[test]
fn claims_whose_messages_were_lost_are_published_again_and_finished() {
    let m = probe(1, false);
    let w = world(m.clone());
    let ids: Vec<String> = (0..6).map(|i| put_flagged(&w.store, i)).collect();
    publish_all(&w, &ids);
    lose_everything(&w.broker);
    assert_eq!(w.broker.stats().unwrap().length, 0);
    let r = crate::flow::republish_lost(&*w.store, &*w.broker, (w.rt.clock)(), 900.0);
    assert_eq!(r["republished"], 6, "{r}");
    assert_eq!(w.broker.stats().unwrap().length, 6);
    let stop = Arc::new(AtomicBool::new(false));
    let pool = start(w.broker.clone(), w.rt.clone(), fast(2), stop.clone(), "t");
    wait_until("every claim ready", || ids.iter().all(|id| state_of(&w.store, id) == "ready"));
    stop_and_join(pool, &stop);
    assert_eq!(m.calls.load(Ordering::SeqCst), 6);
}

#[test]
fn a_backlog_is_not_loss_and_is_never_republished() {
    let w = world(probe(1, false));
    let ids: Vec<String> = (0..4).map(|i| put_flagged(&w.store, i)).collect();
    publish_all(&w, &ids[..2]); // two are still waiting in the broker, two are unaccounted for
    let r = crate::flow::republish_lost(&*w.store, &*w.broker, (w.rt.clock)(), 900.0);
    assert_eq!(r["republished"], 0);
    assert_eq!(w.broker.stats().unwrap().length, 2, "nothing was added");
}

#[test]
fn young_finished_and_unreachable_cases_are_left_alone() {
    let w = world(probe(1, false));
    let ids: Vec<String> = (0..3).map(|i| put_flagged(&w.store, i)).collect();
    let now = (w.rt.clock)();
    // younger than the threshold: the claim's state is from the fixed past, so a huge threshold makes it young
    assert_eq!(crate::flow::republish_lost(&*w.store, &*w.broker, now, 1.0e12)["republished"], 0);
    // a finished claim is not in a pipeline state
    assert_eq!(crate::flow::process(&w.rt, &ids[0], 1, &format!("h-{}", ids[0])), "ready");
    assert_eq!(crate::flow::republish_lost(&*w.store, &*w.broker, now, 900.0)["republished"], 2, "only the two unfinished claims");
    // a broker that does not answer: no panic, nothing published
    w.broker.set_down(true);
    let r = crate::flow::republish_lost(&*w.store, &*w.broker, now, 900.0);
    assert_eq!(r["republished"], 0);
    assert!(r["skipped"].is_string());
}

fn late_delivery(id: &str, attempts: u32) -> Delivery {
    Delivery { msg_id: "1-0".into(), claim_id: id.into(), version: 1, input_hash: format!("h-{id}"), attempts }
}

fn idle_pool_parts() -> (PoolStats, InFlight) {
    (PoolStats::default(), Arc::new(Mutex::new(HashSet::new())))
}

#[test]
fn dead_lettering_never_touches_a_finished_claim_and_never_repeats() {
    let w = world(probe(1, false));
    let (stats, in_flight) = idle_pool_parts();
    // a claim that already finished: a late copy of its message past the attempt limit parks nothing and is simply acknowledged
    let done = put_flagged(&w.store, 0);
    assert_eq!(crate::flow::process(&w.rt, &done, 1, &format!("h-{done}")), "ready");
    assert!(handle(&*w.broker, &w.rt, &fast(1), &stats, &in_flight, &late_delivery(&done, 4)));
    assert_eq!(state_of(&w.store, &done), "ready");
    assert!(w.store.dead_letters(10).unwrap().is_empty(), "no dead letter for a claim that finished");
    // an unfinished claim: parked once; the same late message again (its acknowledgement failed) writes no second record
    let stuck = put_flagged(&w.store, 1);
    assert!(handle(&*w.broker, &w.rt, &fast(1), &stats, &in_flight, &late_delivery(&stuck, 4)));
    assert!(handle(&*w.broker, &w.rt, &fast(1), &stats, &in_flight, &late_delivery(&stuck, 5)));
    assert_eq!(state_of(&w.store, &stuck), "dead_lettered");
    assert_eq!(w.store.dead_letters(10).unwrap().len(), 1, "one record for one parking");
    assert_eq!(stats.dead_lettered.load(Ordering::SeqCst), 1);
}

#[test]
fn a_late_message_never_takes_a_claim_out_of_a_reviewers_hands() {
    let w = world(probe(1, false));
    let (stats, in_flight) = idle_pool_parts();
    let id = put_flagged(&w.store, 0);
    assert_eq!(crate::flow::process(&w.rt, &id, 1, &format!("h-{id}")), "ready");
    let mut lease = serde_json::Map::new();
    lease.insert("lease".into(), json!({"badge_id": "CG-2002", "expires_at": 4_000_000_000.0}));
    assert!(w.store.transition(&id, 1, "ready", "leased", "CG-2002", 2000.0, None, Some(lease), None).unwrap().is_some());
    assert!(handle(&*w.broker, &w.rt, &fast(1), &stats, &in_flight, &late_delivery(&id, 9)), "acknowledged");
    assert_eq!(state_of(&w.store, &id), "leased", "a reviewer holds this claim; a stale delivery must not park it");
    assert!(w.store.dead_letters(10).unwrap().is_empty());
}

#[test]
fn a_copy_past_the_attempt_limit_waits_while_another_copy_of_the_claim_is_being_worked_on() {
    let w = world(probe(1, false));
    let (stats, in_flight) = idle_pool_parts();
    let id = put_flagged(&w.store, 0);
    in_flight.lock().unwrap().insert((id.clone(), 1)); // another thread holds this claim
    let acked = handle(&*w.broker, &w.rt, &fast(1), &stats, &in_flight, &late_delivery(&id, 4));
    assert!(!acked, "left pending, not acknowledged");
    assert_eq!(state_of(&w.store, &id), "triaged", "not parked while another thread may still finish it");
    assert!(w.store.dead_letters(10).unwrap().is_empty());
}

/// A broker whose client panics once on the first read, like a bug in a driver would.
struct PanicsOnFirstRead {
    inner: MemoryBroker,
    armed: AtomicBool,
}

impl Broker for PanicsOnFirstRead {
    fn publish(&self, c: &str, v: i64, h: &str) -> crate::broker::BResult<()> {
        self.inner.publish(c, v, h)
    }
    fn read(&self, consumer: &str, block_ms: u64) -> crate::broker::BResult<Option<Delivery>> {
        if self.armed.swap(false, Ordering::SeqCst) {
            panic!("the driver crashed");
        }
        self.inner.read(consumer, block_ms)
    }
    fn reclaim(&self, consumer: &str, min_idle_ms: u64, count: usize) -> crate::broker::BResult<Vec<Delivery>> {
        self.inner.reclaim(consumer, min_idle_ms, count)
    }
    fn ack(&self, msg_id: &str) -> crate::broker::BResult<()> {
        self.inner.ack(msg_id)
    }
    fn stats(&self) -> crate::broker::BResult<crate::broker::BrokerStats> {
        self.inner.stats()
    }
}

#[test]
fn a_crash_inside_the_broker_client_does_not_end_the_consumer_threads() {
    let w = world(probe(1, false));
    let id = put_flagged(&w.store, 0);
    let broker = Arc::new(PanicsOnFirstRead { inner: MemoryBroker::new(wall()), armed: AtomicBool::new(true) });
    broker.publish(&id, 1, &format!("h-{id}")).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let pool = start(broker.clone(), w.rt.clone(), fast(1), stop.clone(), "t"); // one thread: if it died there would be nobody left
    wait_until("the claim is finished after the crash", || state_of(&w.store, &id) == "ready");
    let stats = pool.stats.clone();
    stop_and_join(pool, &stop);
    assert!(stats.broker_errors.load(Ordering::SeqCst) >= 1, "the crash was counted as a broker failure");
}

#[test]
fn setting_the_stop_flag_ends_every_thread() {
    let w = world(probe(1, false));
    let stop = Arc::new(AtomicBool::new(false));
    let pool = start(w.broker.clone(), w.rt.clone(), fast(4), stop.clone(), "t");
    std::thread::sleep(Duration::from_millis(60));
    stop_and_join(pool, &stop); // join returns only if every thread has ended
}
