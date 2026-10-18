//! `awl-core` — Agent Work Ledger domain logic.
//!
//! Pure, Tauri-free testable surface for cross-tool aggregation,
//! classification, and search. `awl-ipc` and (future) `awl-cli`
//! both drive this crate.

pub mod ai;
pub mod app_settings;
pub mod brief;
pub mod connectors;
pub mod eisenhower;
pub mod error;
pub mod etl;
pub mod idb;
pub mod planner;
pub mod redactor;
pub mod reports;
pub mod search;
pub mod summary;
