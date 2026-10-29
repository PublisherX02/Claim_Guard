//! The demo's intake log (`workqueue/demo.py` ArrivalFeed), for the console's notification bell (`GET /demo/feed`) and its "Simulate new
//! claims" button (`POST /demo/arrivals`). It listens on the intake, so every new claim counts whichever way it came in: the button, the
//! supervisor's Submit page or a file import. A resubmission of a claim already stored is not news and is not logged. The pool is the sample
//! claims the demo did not load at start. `cg serve --demo` only.

use cg_access::Clock;
use cg_queue::flow::{Intake, IntakeError};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, Weak};

const KEEP: usize = 40;
const SHOW: usize = 20;

pub struct ArrivalFeed {
    intake: Arc<Intake>,
    clock: Clock,
    pool: Mutex<VecDeque<Value>>,
    /// (sequence number of the last arrival, the latest arrivals)
    log: Mutex<(u64, VecDeque<Value>)>,
}

impl ArrivalFeed {
    /// Starts listening on `intake`; claims stored before this call are not logged.
    pub fn attach(intake: Arc<Intake>, pool: Vec<Value>, clock: Clock) -> Arc<ArrivalFeed> {
        let feed = Arc::new(ArrivalFeed { intake: intake.clone(), clock, pool: Mutex::new(pool.into()), log: Mutex::new((0, VecDeque::new())) });
        let weak: Weak<ArrivalFeed> = Arc::downgrade(&feed);
        let _ = intake.on_stored.set(Arc::new(move |claim_id, receipt| {
            if let Some(f) = weak.upgrade() {
                f.note(claim_id, &receipt["lane"]);
            }
        }));
        feed
    }

    fn note(&self, claim_id: &str, lane: &Value) {
        let at = (self.clock)();
        let mut log = self.log.lock().unwrap_or_else(|e| e.into_inner());
        log.0 += 1;
        let seq = log.0;
        log.1.push_back(json!({"seq": seq, "claim_id": claim_id, "lane": lane, "at": at}));
        while log.1.len() > KEEP {
            log.1.pop_front();
        }
    }

    pub fn snapshot(&self) -> Value {
        let log = self.log.lock().unwrap_or_else(|e| e.into_inner());
        let recent: Vec<&Value> = log.1.iter().skip(log.1.len().saturating_sub(SHOW)).collect();
        json!({"seq": log.0, "recent": recent})
    }

    /// Sends the next n sample claims through the real intake. Returns how many were accepted.
    pub fn arrive(&self, n: usize) -> usize {
        let mut accepted = 0;
        while accepted < n {
            let Some(claim) = self.pool.lock().unwrap_or_else(|e| e.into_inner()).pop_front() else { break };
            match self.intake.submit(&claim) {
                Ok(_) => accepted += 1,
                Err(IntakeError::Refused(_)) => continue,
                Err(IntakeError::Failed(_)) => break,
            }
        }
        accepted
    }

    pub fn left(&self) -> usize {
        self.pool.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}
