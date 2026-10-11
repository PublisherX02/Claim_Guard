//! The consumer pool: N threads that take messages from the broker and run the pipeline (triage -> explanation -> ready) for each.
//!
//! N is the cap on how many claims are being processed at the same moment, so it is also the cap on concurrent calls to the AI model:
//! a burst of claims waits in the broker, never in front of the model. The rules, each with a test:
//! * a message is acknowledged only after `process` returned; a thread that panics leaves it pending and it is retried;
//! * a message pending longer than `min_idle_ms` (its consumer died) is taken over; `min_idle_ms` must exceed the explanation ceiling,
//!   or a slow but live consumer would have its message stolen and the model would be asked twice;
//! * a message delivered more than `max_attempts` times is parked as a dead letter and acknowledged, so one poison claim cannot loop;
//! * a message for a claim this process is already working on is acknowledged without a second run;
//! * an unreachable broker slows the threads down (backoff) but never stops them.

use crate::broker::{Broker, Delivery};
use crate::flow::{self, Runtime};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// Threads, and so the most claims processed at once.
    pub concurrency: usize,
    /// Deliveries after which a message is parked as a dead letter.
    pub max_attempts: u32,
    /// How long a message may stay pending before another consumer may take it over.
    pub min_idle_ms: u64,
    /// How long one read waits for a message.
    pub block_ms: u64,
    /// How often a thread looks for abandoned messages.
    pub reclaim_every_ms: u64,
    /// The longest pause after a broker error.
    pub max_backoff_ms: u64,
}

impl PoolConfig {
    /// Defaults for production: four at once, three tries, ninety-second ceiling plus a margin before a takeover.
    pub fn production(deadline_seconds: f64) -> Self {
        PoolConfig {
            concurrency: 4,
            max_attempts: 3,
            min_idle_ms: ((deadline_seconds + 30.0).max(60.0) * 1000.0) as u64,
            block_ms: 1000,
            reclaim_every_ms: 15_000,
            max_backoff_ms: 5000,
        }
    }
}

/// Counters the health panel and the tests read.
#[derive(Default)]
pub struct PoolStats {
    pub in_flight: AtomicUsize,
    pub peak_in_flight: AtomicUsize,
    pub processed: AtomicU64,
    pub panicked: AtomicU64,
    pub dead_lettered: AtomicU64,
    pub duplicates_skipped: AtomicU64,
    pub reclaimed: AtomicU64,
    pub broker_errors: AtomicU64,
}

pub struct Pool {
    handles: Vec<std::thread::JoinHandle<()>>,
    pub stats: Arc<PoolStats>,
}

impl Pool {
    /// Waits for every thread; they stop when the `stop` flag given to `start` is set.
    pub fn join(self) {
        for h in self.handles {
            let _ = h.join();
        }
    }
}

type InFlight = Arc<Mutex<HashSet<(String, i64)>>>;

/// Handles one delivery. Returns true when the message was acknowledged.
pub fn handle(broker: &dyn Broker, rt: &Runtime, cfg: &PoolConfig, stats: &PoolStats, in_flight: &InFlight, d: &Delivery) -> bool {
    if d.attempts > cfg.max_attempts {
        flow::dead_letter(rt, &d.claim_id, d.version, &d.input_hash, "delivery_attempts_exceeded", d.attempts);
        stats.dead_lettered.fetch_add(1, Ordering::SeqCst);
        return broker.ack(&d.msg_id).is_ok();
    }
    let key = (d.claim_id.clone(), d.version);
    if !in_flight.lock().unwrap_or_else(|p| p.into_inner()).insert(key.clone()) {
        // the same claim is being run by another thread of this process: that run covers this message
        stats.duplicates_skipped.fetch_add(1, Ordering::SeqCst);
        return broker.ack(&d.msg_id).is_ok();
    }
    let now = stats.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
    stats.peak_in_flight.fetch_max(now, Ordering::SeqCst);
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| flow::process(rt, &d.claim_id, d.version, &d.input_hash)));
    stats.in_flight.fetch_sub(1, Ordering::SeqCst);
    in_flight.lock().unwrap_or_else(|p| p.into_inner()).remove(&key);
    match outcome {
        Ok(_) => {
            stats.processed.fetch_add(1, Ordering::SeqCst);
            broker.ack(&d.msg_id).is_ok()
        }
        Err(_) => {
            // not acknowledged: it stays pending and comes back after min_idle_ms with its attempt count up by one
            stats.panicked.fetch_add(1, Ordering::SeqCst);
            false
        }
    }
}

/// `handle` behind a guard: nothing a delivery does may end a consumer thread.
fn safely(broker: &dyn Broker, rt: &Runtime, cfg: &PoolConfig, stats: &PoolStats, in_flight: &InFlight, d: &Delivery) {
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handle(broker, rt, cfg, stats, in_flight, d))).is_err() {
        stats.panicked.fetch_add(1, Ordering::SeqCst);
    }
}

fn sleep_unless_stopped(ms: u64, stop: &AtomicBool) {
    let mut left = ms;
    while left > 0 && !stop.load(Ordering::SeqCst) {
        let step = left.min(50);
        std::thread::sleep(Duration::from_millis(step));
        left -= step;
    }
}

/// Starts `cfg.concurrency` consumer threads. `name` makes the consumer names unique per process.
pub fn start(broker: Arc<dyn Broker>, rt: Arc<Runtime>, cfg: PoolConfig, stop: Arc<AtomicBool>, name: &str) -> Pool {
    let stats = Arc::new(PoolStats::default());
    let in_flight: InFlight = Arc::new(Mutex::new(HashSet::new()));
    let mut handles = vec![];
    for i in 0..cfg.concurrency.max(1) {
        let (broker, rt, cfg, stop, stats, in_flight) = (broker.clone(), rt.clone(), cfg.clone(), stop.clone(), stats.clone(), in_flight.clone());
        let consumer = format!("{name}-{i}");
        let h = std::thread::Builder::new()
            .name(format!("claimguard-consumer-{i}"))
            .spawn(move || {
                let mut errors: u32 = 0;
                let mut last_reclaim = std::time::Instant::now() - Duration::from_millis(cfg.reclaim_every_ms);
                while !stop.load(Ordering::SeqCst) {
                    let mut failed = false;
                    if last_reclaim.elapsed() >= Duration::from_millis(cfg.reclaim_every_ms) {
                        last_reclaim = std::time::Instant::now();
                        match broker.reclaim(&consumer, cfg.min_idle_ms, 10) {
                            Ok(taken) => {
                                stats.reclaimed.fetch_add(taken.len() as u64, Ordering::SeqCst);
                                for d in taken {
                                    safely(&*broker, &rt, &cfg, &stats, &in_flight, &d);
                                }
                            }
                            Err(_) => failed = true,
                        }
                    }
                    if !failed {
                        match broker.read(&consumer, cfg.block_ms) {
                            Ok(Some(d)) => safely(&*broker, &rt, &cfg, &stats, &in_flight, &d),
                            Ok(None) => {}
                            Err(_) => failed = true,
                        }
                    }
                    if failed {
                        stats.broker_errors.fetch_add(1, Ordering::SeqCst);
                        errors = errors.saturating_add(1);
                        sleep_unless_stopped((100u64 << errors.min(10)).min(cfg.max_backoff_ms), &stop);
                    } else {
                        errors = 0;
                    }
                }
            })
            .expect("a consumer thread starts");
        handles.push(h);
    }
    Pool { handles, stats }
}

#[cfg(test)]
#[path = "consumer_tests.rs"]
mod tests;
