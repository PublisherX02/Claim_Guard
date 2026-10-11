//! The message broker seam. The outbox relay publishes "claim X version V needs its pipeline run" and a pool of consumers takes the
//! messages, one at a time each, so the number of claims being explained at the same moment is a setting and not an accident.
//!
//! The contract is the one a Redis Stream with a consumer group gives (`cg-redis`): a message is delivered to one consumer, stays
//! *pending* until it is acknowledged, and a message that has been pending too long can be taken over by another consumer (a crashed
//! consumer's work is not lost). `MemoryBroker` is the twin used by tests and by single-process runs; one contract suite checks both.

use cg_access::Clock;
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub type BResult<T> = Result<T, String>;

/// One message handed to a consumer. `attempts` is how many times it has been delivered, this delivery included.
#[derive(Debug, Clone, PartialEq)]
pub struct Delivery {
    pub msg_id: String,
    pub claim_id: String,
    pub version: i64,
    pub input_hash: String,
    pub attempts: u32,
}

/// What the health panel shows. `length` is every message not yet acknowledged (waiting plus pending).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BrokerStats {
    pub length: u64,
    pub pending: u64,
    pub oldest_pending_idle_ms: u64,
    pub consumers: u64,
}

pub trait Broker: Send + Sync {
    /// Adds a message. Returns only after the broker has it, so the caller may then clear its outbox marker.
    fn publish(&self, claim_id: &str, version: i64, input_hash: &str) -> BResult<()>;
    /// The next message nobody has taken, waiting up to `block_ms` for one. `None` when there was none.
    fn read(&self, consumer: &str, block_ms: u64) -> BResult<Option<Delivery>>;
    /// Takes over up to `count` messages that have been pending for at least `min_idle_ms` (their consumer is presumed dead). The
    /// delivery count of each goes up by one, so a message that keeps killing its consumer can be recognised and parked.
    fn reclaim(&self, consumer: &str, min_idle_ms: u64, count: usize) -> BResult<Vec<Delivery>>;
    /// Marks the message done and removes it. Acknowledging an unknown id is not an error.
    fn ack(&self, msg_id: &str) -> BResult<()>;
    fn stats(&self) -> BResult<BrokerStats>;
}

struct Entry {
    claim_id: String,
    version: i64,
    input_hash: String,
    owner: Option<String>,
    since: f64,
    deliveries: u32,
}

#[derive(Default)]
struct State {
    next: u64,
    entries: BTreeMap<u64, Entry>,
    down: bool,
}

/// The in-process twin. Everything is behind one lock, which is plenty for tests and for a single-process demo.
pub struct MemoryBroker {
    state: Mutex<State>,
    clock: Clock,
}

impl MemoryBroker {
    pub fn new(clock: Clock) -> Self {
        MemoryBroker { state: Mutex::new(State::default()), clock }
    }

    /// Test hook: while down, every call fails like an unreachable broker. Nothing already stored is lost.
    pub fn set_down(&self, down: bool) {
        self.lock().down = down;
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn deliver(id: u64, e: &Entry) -> Delivery {
        Delivery { msg_id: format!("{id}-0"), claim_id: e.claim_id.clone(), version: e.version, input_hash: e.input_hash.clone(), attempts: e.deliveries }
    }
}

fn parse_id(msg_id: &str) -> Option<u64> {
    msg_id.split('-').next()?.parse().ok()
}

impl Broker for MemoryBroker {
    fn publish(&self, claim_id: &str, version: i64, input_hash: &str) -> BResult<()> {
        let mut s = self.lock();
        if s.down {
            return Err("broker unreachable".into());
        }
        s.next += 1;
        let id = s.next;
        s.entries.insert(id, Entry { claim_id: claim_id.into(), version, input_hash: input_hash.into(), owner: None, since: 0.0, deliveries: 0 });
        Ok(())
    }

    fn read(&self, consumer: &str, block_ms: u64) -> BResult<Option<Delivery>> {
        let end = Instant::now() + Duration::from_millis(block_ms);
        loop {
            {
                let mut s = self.lock();
                if s.down {
                    return Err("broker unreachable".into());
                }
                let now = (self.clock)();
                if let Some((&id, e)) = s.entries.iter_mut().find(|(_, e)| e.owner.is_none()) {
                    e.owner = Some(consumer.to_string());
                    e.since = now;
                    e.deliveries = 1;
                    return Ok(Some(Self::deliver(id, e)));
                }
            }
            if Instant::now() >= end {
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn reclaim(&self, consumer: &str, min_idle_ms: u64, count: usize) -> BResult<Vec<Delivery>> {
        let mut s = self.lock();
        if s.down {
            return Err("broker unreachable".into());
        }
        let now = (self.clock)();
        let mut out = vec![];
        for (&id, e) in s.entries.iter_mut() {
            if out.len() >= count {
                break;
            }
            if e.owner.is_some() && (now - e.since) * 1000.0 >= min_idle_ms as f64 {
                e.owner = Some(consumer.to_string());
                e.since = now;
                e.deliveries += 1;
                out.push(Self::deliver(id, e));
            }
        }
        Ok(out)
    }

    fn ack(&self, msg_id: &str) -> BResult<()> {
        let mut s = self.lock();
        if s.down {
            return Err("broker unreachable".into());
        }
        if let Some(id) = parse_id(msg_id) {
            s.entries.remove(&id);
        }
        Ok(())
    }

    fn stats(&self) -> BResult<BrokerStats> {
        let s = self.lock();
        if s.down {
            return Err("broker unreachable".into());
        }
        let now = (self.clock)();
        let pending: Vec<&Entry> = s.entries.values().filter(|e| e.owner.is_some()).collect();
        let owners: std::collections::BTreeSet<&String> = pending.iter().filter_map(|e| e.owner.as_ref()).collect();
        Ok(BrokerStats {
            length: s.entries.len() as u64,
            pending: pending.len() as u64,
            oldest_pending_idle_ms: pending.iter().map(|e| ((now - e.since) * 1000.0).max(0.0) as u64).max().unwrap_or(0),
            consumers: owners.len() as u64,
        })
    }
}

/// The behaviour every broker must have. `fresh` returns an empty broker each time it is called (a new stream for Redis).
pub fn contract(fresh: &dyn Fn() -> Box<dyn Broker>) {
    use std::collections::HashSet;
    use std::sync::Arc;

    // a published message is delivered once, with one attempt, and is not delivered again to anyone while pending
    let b = fresh();
    b.publish("C1", 1, "h1").unwrap();
    let d = b.read("a", 200).unwrap().expect("the message arrives");
    assert_eq!((d.claim_id.as_str(), d.version, d.input_hash.as_str(), d.attempts), ("C1", 1, "h1", 1));
    assert!(b.read("b", 30).unwrap().is_none(), "a pending message is not handed to a second consumer");
    let st = b.stats().unwrap();
    assert_eq!((st.length, st.pending), (1, 1));
    assert!(st.consumers >= 1);

    // not idle long enough: nothing to take over. Idle long enough: taken over once, with the attempt count up by one
    assert!(b.reclaim("b", 60_000, 10).unwrap().is_empty(), "a message that is not idle yet is left alone");
    std::thread::sleep(Duration::from_millis(120));
    let again = b.reclaim("b", 60, 10).unwrap();
    assert_eq!(again.len(), 1);
    assert_eq!((again[0].msg_id.as_str(), again[0].attempts), (d.msg_id.as_str(), 2));
    assert!(b.reclaim("c", 60, 10).unwrap().is_empty(), "taking a message over restarts its idle time");
    std::thread::sleep(Duration::from_millis(120));
    assert_eq!(b.reclaim("c", 60, 10).unwrap()[0].attempts, 3);

    // acknowledged: gone for good, acknowledging twice or an unknown id is harmless
    b.ack(&d.msg_id).unwrap();
    b.ack(&d.msg_id).unwrap();
    b.ack("999999-0").unwrap();
    std::thread::sleep(Duration::from_millis(120));
    assert!(b.reclaim("c", 60, 10).unwrap().is_empty());
    let st = b.stats().unwrap();
    assert_eq!((st.length, st.pending), (0, 0));

    // arrival order is kept, and two consumers reading at once never get the same message
    let b: Arc<dyn Broker> = Arc::from(fresh());
    for i in 0..60 {
        b.publish(&format!("C{i}"), 1, "h").unwrap();
    }
    let first = b.read("solo", 200).unwrap().unwrap();
    assert_eq!(first.claim_id, "C0", "oldest first");
    b.ack(&first.msg_id).unwrap();
    let seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let hs: Vec<_> = ["x", "y"]
        .into_iter()
        .map(|name| {
            let (b, seen) = (b.clone(), seen.clone());
            std::thread::spawn(move || {
                while let Some(d) = b.read(name, 100).unwrap() {
                    seen.lock().unwrap().push(d.claim_id.clone());
                    b.ack(&d.msg_id).unwrap();
                }
            })
        })
        .collect();
    for h in hs {
        h.join().unwrap();
    }
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 59, "everything is delivered");
    assert_eq!(seen.iter().collect::<HashSet<_>>().len(), 59, "and nothing is delivered twice");
    assert_eq!(b.stats().unwrap().length, 0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    pub fn wall_clock() -> Clock {
        Arc::new(|| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0))
    }

    #[test]
    fn the_memory_broker_meets_the_contract() {
        contract(&|| Box::new(MemoryBroker::new(wall_clock())));
    }

    #[test]
    fn a_down_broker_fails_every_call_and_loses_nothing() {
        let b = MemoryBroker::new(wall_clock());
        b.publish("C1", 1, "h").unwrap();
        b.set_down(true);
        assert!(b.publish("C2", 1, "h").is_err());
        assert!(b.read("a", 5).is_err());
        assert!(b.reclaim("a", 0, 1).is_err());
        assert!(b.ack("1-0").is_err());
        assert!(b.stats().is_err());
        b.set_down(false);
        assert_eq!(b.read("a", 50).unwrap().unwrap().claim_id, "C1");
        assert!(b.stats().unwrap().length == 1);
    }
}
