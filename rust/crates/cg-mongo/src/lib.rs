//! MongoDB stores. The guarantees that matter are enforced by the database, not by code that could race:
//!   * one user per badge: a unique index on `users.badge_id`;
//!   * a one-time code is accepted once: a unique index on `(badge_id, step)` in `used_totp`, so of 50 parallel submissions exactly one
//!     insert succeeds;
//!   * failed-login counting and locking are one atomic update pipeline, so parallel failures lose no count;
//!   * used codes and revoked tokens disappear on their own through TTL indexes.
//!
//! Any driver failure except a duplicate key becomes `StoreError::Unavailable`, which callers treat as "refuse".

pub mod bsonconv;
pub mod queue;
pub mod users;

pub use queue::MongoQueueStore;
pub use users::MongoUserStore;

use mongodb::sync::Client;

/// A client with short timeouts, so an unreachable database fails a request quickly instead of hanging it.
pub fn connect(uri: &str, timeout_ms: u64) -> Result<Client, mongodb::error::Error> {
    let mut opts = mongodb::options::ClientOptions::parse(uri).run()?;
    let t = std::time::Duration::from_millis(timeout_ms);
    opts.server_selection_timeout = Some(t);
    opts.connect_timeout = Some(t);
    opts.app_name = Some("claimguard-rust".into());
    Client::with_options(opts)
}
