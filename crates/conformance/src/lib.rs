//! # nanosb-conformance
//!
//! A language-agnostic, CI-safe protocol conformance harness. The
//! `nanosb-conformance` binary serves a scripted NDJSON exchange over a Unix
//! stream socket so that every SDK driver can be tested against a real,
//! byte-level protocol target without a VM or network.

pub mod matcher;
pub mod scenario;
pub mod server;

pub use scenario::Scenario;
pub use server::ConformanceServer;
