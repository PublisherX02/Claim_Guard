//! The Redis Streams broker: `cg_queue::broker::Broker` over `XADD` / `XREADGROUP` / `XACK` / `XPENDING` / `XCLAIM`.
//!
//! * `publish` is `XADD`; the caller clears its outbox marker only after this returns, so a Redis outage leaves the markers in place.
//! * `read` is `XREADGROUP ... BLOCK`; a message stays in the group's pending list until `ack`.
//! * `ack` is `XACK` plus `XDEL` in one transaction, so the stream holds only unfinished work and never needs trimming (a trim could
//!   silently drop a pending message).
//! * `reclaim` lists pending entries idle for at least `min_idle_ms` (`XPENDING ... IDLE`) and takes each over with `XCLAIM ... MINIDLE`,
//!   which refuses if another consumer took it first and which counts the delivery.
//!
//! Needs Redis 6.2 or later (`XPENDING ... IDLE`). The address can carry a password, so no error text ever contains it.

use cg_queue::broker::{BResult, Broker, BrokerStats, Delivery};
use redis::Value;
use std::sync::Mutex;
use std::time::Duration;

pub const DEFAULT_STREAM: &str = "cg:queue:claims";
pub const DEFAULT_GROUP: &str = "cg-workers";

pub struct RedisBroker {
    client: redis::Client,
    url: String,
    stream: String,
    group: String,
    idle: Mutex<Vec<redis::Connection>>,
}

fn text(v: &Value) -> Option<String> {
    match v {
        Value::BulkString(b) => String::from_utf8(b.clone()).ok(),
        Value::SimpleString(s) => Some(s.clone()),
        Value::Int(i) => Some(i.to_string()),
        _ => None,
    }
}

fn int(v: &Value) -> Option<i64> {
    match v {
        Value::Int(i) => Some(*i),
        other => text(other)?.parse().ok(),
    }
}

/// `[id, [field, value, ...]]` -> (id, fields). A deleted entry comes back with nil fields and is skipped by the callers.
fn entry(v: &Value) -> Option<(String, Vec<(String, String)>)> {
    let Value::Array(parts) = v else { return None };
    let id = text(parts.first()?)?;
    let Value::Array(kv) = parts.get(1)? else { return None };
    let mut fields = vec![];
    for pair in kv.chunks(2) {
        if let [k, val] = pair {
            fields.push((text(k)?, text(val)?));
        }
    }
    Some((id, fields))
}

fn delivery(id: String, fields: &[(String, String)], attempts: u32) -> Option<Delivery> {
    let get = |name: &str| fields.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
    Some(Delivery { msg_id: id, claim_id: get("claim_id")?, version: get("version")?.parse().ok()?, input_hash: get("input_hash")?, attempts })
}

impl RedisBroker {
    /// Connects (so a wrong address fails now) and creates the stream and consumer group if they are not there yet.
    pub fn connect(url: &str, stream: &str, group: &str) -> BResult<Self> {
        let client = redis::Client::open(url).map_err(|_| "the Redis address is not valid".to_string())?;
        let b = RedisBroker { client, url: url.to_string(), stream: stream.to_string(), group: group.to_string(), idle: Mutex::new(vec![]) };
        b.ensure_group()?;
        Ok(b)
    }

    /// Like `connect` but does not touch the network, so a worker starts even while Redis is briefly down. The consumer group is created
    /// the first time it is needed (`healing`), and every call reports an unreachable Redis as an error the caller retries.
    pub fn lazy(url: &str, stream: &str, group: &str) -> BResult<Self> {
        let client = redis::Client::open(url).map_err(|_| "the Redis address is not valid".to_string())?;
        Ok(RedisBroker { client, url: url.to_string(), stream: stream.to_string(), group: group.to_string(), idle: Mutex::new(vec![]) })
    }

    fn clean(&self, op: &str, e: redis::RedisError) -> String {
        let msg = e.to_string().replace(&self.url, "<redis>");
        format!("redis {op} failed ({:?}): {}", e.kind(), msg.chars().take(160).collect::<String>())
    }

    fn take_conn(&self, extra_ms: u64) -> BResult<redis::Connection> {
        let pooled = self.idle.lock().unwrap_or_else(|p| p.into_inner()).pop();
        let conn = match pooled {
            Some(c) => c,
            None => self.client.get_connection_with_timeout(Duration::from_secs(3)).map_err(|e| self.clean("connect", e))?,
        };
        conn.set_read_timeout(Some(Duration::from_millis(extra_ms + 5000))).map_err(|e| self.clean("connect", e))?;
        conn.set_write_timeout(Some(Duration::from_secs(5))).map_err(|e| self.clean("connect", e))?;
        Ok(conn)
    }

    /// Runs `f` on a connection. A connection that failed is dropped, not returned to the pool.
    fn with<T>(&self, op: &str, block_ms: u64, f: impl FnOnce(&mut redis::Connection) -> redis::RedisResult<T>) -> BResult<T> {
        let mut conn = self.take_conn(block_ms)?;
        match f(&mut conn) {
            Ok(v) => {
                let mut idle = self.idle.lock().unwrap_or_else(|p| p.into_inner());
                if idle.len() < 32 {
                    idle.push(conn);
                }
                Ok(v)
            }
            Err(e) => Err(self.clean(op, e)),
        }
    }

    fn ensure_group(&self) -> BResult<()> {
        let r = self.with("group", 0, |c| redis::cmd("XGROUP").arg("CREATE").arg(&self.stream).arg(&self.group).arg("0").arg("MKSTREAM").query::<Value>(c));
        match r {
            Ok(_) => Ok(()),
            Err(m) if m.contains("BUSYGROUP") => Ok(()),
            Err(m) => Err(m),
        }
    }

    /// Runs `f`; if the stream or group has vanished (Redis was flushed or restarted empty) creates them and tries once more.
    fn healing<T>(&self, op: &str, block_ms: u64, f: impl Fn(&mut redis::Connection) -> redis::RedisResult<T>) -> BResult<T> {
        match self.with(op, block_ms, &f) {
            Err(m) if m.contains("NOGROUP") => {
                self.ensure_group()?;
                self.with(op, block_ms, &f)
            }
            other => other,
        }
    }
}

impl Broker for RedisBroker {
    fn publish(&self, claim_id: &str, version: i64, input_hash: &str) -> BResult<()> {
        self.with("publish", 0, |c| {
            redis::cmd("XADD").arg(&self.stream).arg("*").arg("claim_id").arg(claim_id).arg("version").arg(version).arg("input_hash").arg(input_hash).query::<Value>(c)
        })
        .map(|_| ())
    }

    fn read(&self, consumer: &str, block_ms: u64) -> BResult<Option<Delivery>> {
        let reply = self.healing("read", block_ms, |c| {
            redis::cmd("XREADGROUP").arg("GROUP").arg(&self.group).arg(consumer).arg("COUNT").arg(1).arg("BLOCK").arg(block_ms.max(1)).arg("STREAMS").arg(&self.stream).arg(">").query::<Value>(c)
        })?;
        // [[stream, [[id, [field, value, ...]]]]], or nil when nothing arrived before the block time ran out
        let Value::Array(streams) = reply else { return Ok(None) };
        let Some(Value::Array(one)) = streams.first() else { return Ok(None) };
        let Some(Value::Array(entries)) = one.get(1) else { return Ok(None) };
        let Some((id, fields)) = entries.first().and_then(entry) else { return Ok(None) };
        match delivery(id.clone(), &fields, 1) {
            Some(d) => Ok(Some(d)),
            None => {
                // not a message this system wrote: take it off the list so it cannot sit pending forever
                self.ack(&id)?;
                Ok(None)
            }
        }
    }

    fn reclaim(&self, consumer: &str, min_idle_ms: u64, count: usize) -> BResult<Vec<Delivery>> {
        let listed = self.healing("reclaim", 0, |c| {
            redis::cmd("XPENDING").arg(&self.stream).arg(&self.group).arg("IDLE").arg(min_idle_ms).arg("-").arg("+").arg(count).query::<Value>(c)
        })?;
        let Value::Array(rows) = listed else { return Ok(vec![]) };
        let mut out = vec![];
        for row in rows {
            let Value::Array(cols) = row else { continue };
            let (Some(id), Some(before)) = (cols.first().and_then(text), cols.get(3).and_then(int)) else { continue };
            let claimed = self.with("reclaim", 0, |c| redis::cmd("XCLAIM").arg(&self.stream).arg(&self.group).arg(consumer).arg(min_idle_ms).arg(&id).query::<Value>(c))?;
            let Value::Array(entries) = claimed else { continue };
            if let Some((cid, fields)) = entries.first().and_then(entry) {
                match delivery(cid.clone(), &fields, (before.max(0) as u32).saturating_add(1)) {
                    Some(d) => out.push(d),
                    None => self.ack(&cid)?,
                }
            }
        }
        Ok(out)
    }

    fn ack(&self, msg_id: &str) -> BResult<()> {
        self.with("ack", 0, |c| redis::pipe().atomic().cmd("XACK").arg(&self.stream).arg(&self.group).arg(msg_id).ignore().cmd("XDEL").arg(&self.stream).arg(msg_id).ignore().query::<()>(c))
    }

    fn stats(&self) -> BResult<BrokerStats> {
        let length = self.healing("stats", 0, |c| redis::cmd("XLEN").arg(&self.stream).query::<u64>(c))?;
        // [count, smallest id, largest id, [[consumer, count], ...]] (the last three are nil when nothing is pending)
        let summary = self.healing("stats", 0, |c| redis::cmd("XPENDING").arg(&self.stream).arg(&self.group).query::<Value>(c))?;
        let Value::Array(parts) = summary else { return Ok(BrokerStats { length, ..Default::default() }) };
        let pending = parts.first().and_then(int).unwrap_or(0).max(0) as u64;
        let consumers = match parts.get(3) {
            Some(Value::Array(list)) => list.len() as u64,
            _ => 0,
        };
        let mut oldest_idle = 0;
        if pending > 0 {
            let oldest = self.healing("stats", 0, |c| redis::cmd("XPENDING").arg(&self.stream).arg(&self.group).arg("-").arg("+").arg(1).query::<Value>(c))?;
            if let Value::Array(rows) = oldest {
                if let Some(Value::Array(cols)) = rows.first() {
                    oldest_idle = cols.get(2).and_then(int).unwrap_or(0).max(0) as u64;
                }
            }
        }
        Ok(BrokerStats { length, pending, oldest_pending_idle_ms: oldest_idle, consumers })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static N: AtomicU64 = AtomicU64::new(0);

    /// REDIS_URL for the tests. With REQUIRE_REDIS=1 a missing address is a failure, otherwise the test is skipped (and says so).
    fn url() -> Option<String> {
        match std::env::var("REDIS_URL").ok().filter(|s| !s.is_empty()) {
            Some(u) => Some(u),
            None => {
                assert!(std::env::var("REQUIRE_REDIS").ok().as_deref() != Some("1"), "REQUIRE_REDIS=1 but REDIS_URL is not set");
                eprintln!("SKIPPED: REDIS_URL is not set");
                None
            }
        }
    }

    fn fresh_stream() -> String {
        format!("cgtest:{}:{}:{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0), N.fetch_add(1, Ordering::SeqCst))
    }

    fn drop_stream(url: &str, stream: &str) {
        if let Ok(c) = redis::Client::open(url) {
            if let Ok(mut conn) = c.get_connection() {
                let _: redis::RedisResult<i64> = redis::cmd("DEL").arg(stream).query(&mut conn);
            }
        }
    }

    #[test]
    fn real_redis_meets_the_broker_contract() {
        let Some(url) = url() else { return };
        let mut used = vec![];
        let used_ref = std::sync::Mutex::new(&mut used);
        cg_queue::broker::contract(&|| {
            let s = fresh_stream();
            used_ref.lock().unwrap().push(s.clone());
            Box::new(RedisBroker::connect(&url, &s, DEFAULT_GROUP).expect("connects"))
        });
        for s in used {
            drop_stream(&url, &s);
        }
    }

    #[test]
    fn a_stream_that_vanishes_is_recreated_and_work_continues() {
        let Some(url) = url() else { return };
        let s = fresh_stream();
        let b = RedisBroker::connect(&url, &s, DEFAULT_GROUP).unwrap();
        b.publish("C1", 1, "h").unwrap();
        drop_stream(&url, &s); // Redis restarted empty, or someone flushed it
        assert!(b.read("a", 20).unwrap().is_none(), "the lost message is gone, but reading does not fail");
        b.publish("C2", 1, "h").unwrap();
        assert_eq!(b.read("a", 200).unwrap().unwrap().claim_id, "C2");
        drop_stream(&url, &s);
    }

    #[test]
    fn two_brokers_on_one_stream_share_the_work_without_overlap() {
        let Some(url) = url() else { return };
        let s = fresh_stream();
        let (a, b) = (RedisBroker::connect(&url, &s, DEFAULT_GROUP).unwrap(), RedisBroker::connect(&url, &s, DEFAULT_GROUP).unwrap());
        for i in 0..20 {
            a.publish(&format!("C{i}"), 1, "h").unwrap();
        }
        let mut seen = std::collections::HashSet::new();
        for turn in 0..20 {
            let d = if turn % 2 == 0 { a.read("pa", 200) } else { b.read("pb", 200) }.unwrap().expect("a message");
            assert!(seen.insert(d.claim_id.clone()), "{} delivered twice", d.claim_id);
            a.ack(&d.msg_id).unwrap();
        }
        assert_eq!(a.stats().unwrap().length, 0);
        drop_stream(&url, &s);
    }

    #[test]
    fn an_unreachable_redis_is_an_error_and_the_address_never_appears_in_it() {
        let secret = "redis://:hunter2-not-a-real-password@127.0.0.1:1/0";
        let err = RedisBroker::connect(secret, "s", "g").err().expect("nothing listens on port 1");
        assert!(!err.contains("hunter2") && !err.contains("127.0.0.1:1"), "leaked: {err}");
        let bad = RedisBroker::connect("not a url", "s", "g").err().unwrap();
        assert!(!bad.contains("not a url") || bad.contains("not valid"));
    }

    #[test]
    fn stats_report_pending_work_and_who_holds_it() {
        let Some(url) = url() else { return };
        let s = fresh_stream();
        let b = RedisBroker::connect(&url, &s, DEFAULT_GROUP).unwrap();
        assert_eq!(b.stats().unwrap(), BrokerStats::default());
        for i in 0..3 {
            b.publish(&format!("C{i}"), 1, "h").unwrap();
        }
        let d = b.read("holder", 200).unwrap().unwrap();
        std::thread::sleep(Duration::from_millis(60));
        let st = b.stats().unwrap();
        assert_eq!((st.length, st.pending, st.consumers), (3, 1, 1));
        assert!(st.oldest_pending_idle_ms >= 50, "idle {}", st.oldest_pending_idle_ms);
        b.ack(&d.msg_id).unwrap();
        drop_stream(&url, &s);
    }
}
