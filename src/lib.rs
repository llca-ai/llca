//! llca (Local Ledger for Coding Agents): a local, private log of what AI coding agents do on this machine.
//!
//! No third-party crates: the collector runs on every agent action, so all
//! the code it executes lives in this repository. SQLite is the system library.

pub mod audit;
pub mod cli;
pub mod demo;
pub mod doctor;
pub mod icons;
pub mod install;
pub mod json;
pub mod redact;
pub mod resources;
pub mod rules;
pub mod server;
pub mod sqlite;
pub mod store;
pub mod util;
pub mod watch;
