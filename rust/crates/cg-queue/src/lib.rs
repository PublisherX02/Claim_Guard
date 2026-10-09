//! The work queue: the Rust counterpart of `src/workqueue/`. Every claim goes to a person; triage decides the lane and who may take it,
//! the dispatcher keeps each reviewer's inbox topped up, leases bring a claim back when someone walks away, high-severity claims need two
//! different seniors, and every move is one append-only event.

pub mod contract;
pub mod routing_config;
pub mod states;
pub mod store;
pub mod triage;
