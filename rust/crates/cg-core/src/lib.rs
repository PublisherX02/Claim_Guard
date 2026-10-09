//! Building blocks shared by the ClaimGuard crates: numbers and dates that behave exactly like the Python reference, text helpers,
//! the cleaned "rule view" of a claim, and the rulebook configuration.
//!
//! Nothing here knows about a particular rule. Every function says which Python function it mirrors, because the Python
//! implementation (src/engine_core.py, src/facts_extractor.py) is the specification the Rust code is checked against.

pub mod config;
pub mod date;
pub mod num;
pub mod text;
pub mod transport;
pub mod view;

pub use date::Day;
pub use num::Num;
