//! A circuit breaker for the AI call, so a failing model never slows the queue down (`src/workqueue/breaker.py`).
//!
//! * closed: calls go through. The breaker opens after `consecutive` transient failures in a row, or when failures / calls over the last
//!   `window` seconds reaches `error_rate` with at least `min_calls` calls in that window.
//! * open: calls are refused at once without running the function, for `cooldown` seconds.
//! * half_open: after the cooldown exactly one probe call is let through; its success closes the breaker, its failure reopens it for a new
//!   cooldown. A second caller during the probe is refused.
//!
//! Only transient failures (timeouts, 5xx, rate limits) count. Fatal ones (a rejected request) are the caller's bug, not the model being
//! down: they pass through without counting and are never retried.

use cg_access::Clock;
use std::collections::VecDeque;
use std::sync::Mutex;

#[derive(Debug, Clone, PartialEq)]
pub enum ModelError {
    /// May go away on its own and counts toward opening the breaker.
    Transient(String),
    /// The model did not answer in time (also transient).
    Timeout(String),
    /// Retrying cannot fix it; never counts.
    Fatal(String),
    /// No model has been set up; claims keep the engine's own explanation.
    NotConfigured,
}

impl ModelError {
    pub fn counts(&self) -> bool {
        matches!(self, ModelError::Transient(_) | ModelError::Timeout(_))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum CallError {
    /// The breaker is open: the call was not made.
    Open,
    Model(ModelError),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum State {
    Closed,
    Open,
    HalfOpen,
}

struct Inner {
    state: State,
    opened_at: f64,
    streak: i64,
    calls: VecDeque<(f64, bool)>,
    probing: bool,
}

pub struct CircuitBreaker {
    clock: Clock,
    pub consecutive: i64,
    pub error_rate: f64,
    pub window: f64,
    pub min_calls: usize,
    pub cooldown: f64,
    inner: Mutex<Inner>,
}

impl CircuitBreaker {
    pub fn new(clock: Clock) -> CircuitBreaker {
        CircuitBreaker::with(clock, 5, 0.5, 60.0, 20, 20.0)
    }

    pub fn with(clock: Clock, consecutive: i64, error_rate: f64, window: f64, min_calls: usize, cooldown: f64) -> CircuitBreaker {
        CircuitBreaker {
            clock,
            consecutive,
            error_rate,
            window,
            min_calls,
            cooldown,
            inner: Mutex::new(Inner { state: State::Closed, opened_at: 0.0, streak: 0, calls: VecDeque::new(), probing: false }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn state(&self) -> &'static str {
        let g = self.lock();
        match g.state {
            State::Open if (self.clock)() - g.opened_at >= self.cooldown => "half_open",
            State::Open => "open",
            State::HalfOpen => "half_open",
            State::Closed => "closed",
        }
    }

    pub fn call<T>(&self, f: impl FnOnce() -> Result<T, ModelError>) -> Result<T, CallError> {
        let probe;
        {
            let mut g = self.lock();
            let now = (self.clock)();
            if g.state == State::Open {
                if now - g.opened_at < self.cooldown {
                    return Err(CallError::Open);
                }
                g.state = State::HalfOpen;
                g.probing = false;
            }
            probe = g.state == State::HalfOpen;
            if probe {
                if g.probing {
                    return Err(CallError::Open);
                }
                g.probing = true;
            }
        }
        match f() {
            Ok(v) => {
                self.record(true, probe);
                Ok(v)
            }
            Err(e) if e.counts() => {
                self.record(false, probe);
                Err(CallError::Model(e))
            }
            Err(e) => {
                if probe {
                    self.lock().probing = false; // neither success nor failure: let the next caller probe again
                }
                Err(CallError::Model(e))
            }
        }
    }

    fn record(&self, ok: bool, probe: bool) {
        let mut g = self.lock();
        let now = (self.clock)();
        if probe {
            g.probing = false;
            if ok {
                g.state = State::Closed;
                g.streak = 0;
                g.calls.clear();
            } else {
                g.state = State::Open;
                g.opened_at = now;
            }
            return;
        }
        if g.state != State::Closed {
            return; // a straggler that finished after the breaker moved on
        }
        g.calls.push_back((now, ok));
        while g.calls.front().is_some_and(|(t, _)| now - t > self.window) {
            g.calls.pop_front();
        }
        g.streak = if ok { 0 } else { g.streak + 1 };
        if ok {
            return;
        }
        let failures = g.calls.iter().filter(|(_, good)| !good).count();
        if g.streak >= self.consecutive || (g.calls.len() >= self.min_calls && failures as f64 / g.calls.len() as f64 >= self.error_rate) {
            g.state = State::Open;
            g.opened_at = now;
            g.probing = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    fn clock() -> (Clock, Arc<AtomicU64>) {
        let t = Arc::new(AtomicU64::new(1000));
        let t2 = t.clone();
        (Arc::new(move || t2.load(Ordering::SeqCst) as f64), t)
    }

    fn fail() -> Result<(), ModelError> {
        Err(ModelError::Transient("x".into()))
    }

    #[test]
    fn five_failures_in_a_row_open_it_and_a_success_in_between_resets_the_streak() {
        let (c, _) = clock();
        let b = CircuitBreaker::new(c);
        for _ in 0..4 {
            let _ = b.call(fail);
        }
        assert!(b.call(|| Ok(())).is_ok());
        for _ in 0..4 {
            let _ = b.call(fail);
        }
        assert_eq!(b.state(), "closed");
        let _ = b.call(fail);
        assert_eq!(b.state(), "open");
        assert_eq!(b.call(|| Ok(())), Err(CallError::Open), "refused without running");
    }

    #[test]
    fn after_the_cooldown_one_probe_decides() {
        let (c, t) = clock();
        let b = CircuitBreaker::new(c);
        for _ in 0..5 {
            let _ = b.call(fail);
        }
        t.fetch_add(19, Ordering::SeqCst);
        assert_eq!(b.call(|| Ok(())), Err(CallError::Open));
        t.fetch_add(1, Ordering::SeqCst);
        assert_eq!(b.state(), "half_open");
        let _ = b.call(fail);
        assert_eq!(b.state(), "open", "a failed probe reopens for a new cooldown");
        t.fetch_add(20, Ordering::SeqCst);
        assert!(b.call(|| Ok(())).is_ok());
        assert_eq!(b.state(), "closed");
    }

    #[test]
    fn a_second_caller_during_the_probe_is_refused() {
        let (c, t) = clock();
        let b = CircuitBreaker::new(c);
        for _ in 0..5 {
            let _ = b.call(fail);
        }
        t.fetch_add(20, Ordering::SeqCst);
        let first = b.call(|| {
            assert_eq!(b.call(|| Ok(())), Err(CallError::Open));
            Ok(())
        });
        assert!(first.is_ok());
    }

    #[test]
    fn fatal_errors_never_count_and_the_error_rate_opens_it_too() {
        let (c, _) = clock();
        let b = CircuitBreaker::new(c);
        for _ in 0..50 {
            assert_eq!(b.call::<()>(|| Err(ModelError::Fatal("bad request".into()))), Err(CallError::Model(ModelError::Fatal("bad request".into()))));
        }
        assert_eq!(b.state(), "closed");
        let (c, _) = clock();
        let b = CircuitBreaker::new(c);
        for i in 0..40 {
            let _ = b.call(|| if i % 2 == 0 { Ok(()) } else { fail() });
            if b.state() == "open" {
                break;
            }
        }
        assert_eq!(b.state(), "open", "half of at least twenty calls failed");
    }
}
