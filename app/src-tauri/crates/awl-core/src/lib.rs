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
// v0.5.6: GUI lifecycle log + quit-confirmed flag. Lives in
// `awl-core` (not `awl-desktop`) so both `awl-ipc` and
// `awl-desktop` can import it without a workspace cycle.
pub mod lifecycle;
pub mod error;
pub mod etl;
pub mod idb;
pub mod planner;
pub mod redactor;
pub mod reports;
pub mod search;
pub mod summary;
pub mod trae_solo;
