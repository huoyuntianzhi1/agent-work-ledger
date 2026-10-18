//! Stage 1-I integration: prove the idb thin-ffi wrapper can open a real
//! Chromium IndexedDB LevelDB. Skipped by default because the test relies
//! on the user's local Doubao install path; pass the path via the env var
//! `AWL_DOBAO_IDB_DIR` to run.

use std::path::PathBuf;

#[test]
#[ignore]
fn idb_iter_doubao_chat() {
    let dir = match std::env::var_os("AWL_DOBAO_IDB_DIR") {
        Some(v) => PathBuf::from(v),
        None => {
            eprintln!("skipping; set AWL_DOBAO_IDB_DIR to a *.indexeddb.leveldb directory");
            return;
        }
    };
    if !dir.is_dir() {
        eprintln!("skipping; {} is not a directory", dir.display());
        return;
    }
    let mut total = 0usize;
    let mut big = 0usize;
    agent_work_ledger_lib::idb::iter_store(
        &dir,
        &mut |_k, v| {
            total += 1;
            if v.len() > 1024 {
                big += 1;
            }
            true
        },
        None,
    )
    .expect("iter_store");
    eprintln!("doubao-chat leveldb @ {}: total={total} big(>1KB)={big}", dir.display());
    assert!(total > 0, "expected non-empty leveldb");
}