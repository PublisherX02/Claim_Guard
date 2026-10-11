//! Identity and access: the Rust counterpart of `src/access/` (everything except the HTTP layer).
//!
//! A badge number, a password (bcrypt), a one-time code from an authenticator app (RFC 6238, the seed encrypted at rest), a signed
//! session token, four clearance levels with per-user grants and revokes, masked identifiers, and a signed security log. No HTTP in here:
//! the API crate only translates requests into calls on `AccessService`.

pub mod contract;
pub mod masking;
pub mod passwords;
pub mod permissions;
pub mod securitylog;
pub mod service;
pub mod settings;
pub mod store;
pub mod tokens;
pub mod totp;

pub use service::{AccessService, AuthError, Forbidden, Principal, Session};
pub use settings::Settings;
pub use store::{MemoryStore, StoreError, User, UserStore};

use std::sync::Arc;

/// Seconds since the Unix epoch, as a float: replaceable in tests.
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;

pub fn system_clock() -> Clock {
    Arc::new(|| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0))
}
