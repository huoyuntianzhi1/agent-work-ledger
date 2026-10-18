// Empty stub lib so the root crate of this workspace has at
// least one target to satisfy `cargo metadata`. The real
// functionality (the `awl-desktop` binary plus every
// supporting crate) lives under `crates/awl-*/`. Adding this
// stub means `tauri build` can locate the root `[package]`
// and dispatch to the right member crate.
//
// Do NOT add code here. If you find yourself wanting to put
// logic in the root crate, it should live in `awl-core` or
// `awl-desktop` instead.
