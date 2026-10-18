//! Stage 4-D.5b V8-decoder probe.
//!
//! Reads every record in 王总's WorkBuddy web IndexedDB LevelDB roots and
//! dumps:
//!   - key prefix distribution (which prefix is real data vs metadata)
//!   - first 16 bytes of value (so we can recognise the V8 value tag)
//!   - first 128 bytes of any "candidate" record (key_prefix in 0x00..0x0f
//!     and value length > 32 bytes)
//!
//! Run from the src-tauri/ directory:
//!
//!   cargo run --release --example probe_workbuddy_idb
//!
//! Output goes to stderr so the parent shell can redirect it. The probe
//! never writes to the app database — it's read-only on disk.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use agent_work_ledger_lib::idb;

// Stage 4-D.5b — also probe Local Storage with bytewise comparator to see
// if WorkBuddy web stashes anything there.

fn probe(path: &Path) {
    eprintln!("\n========== {} ==========", path.display());

    // First pass: collect distribution + samples.
    let mut prefix_dist: HashMap<u8, (usize, usize)> = HashMap::new(); // (count, total_value_bytes)
    let mut candidates: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    let mut value_size_buckets: HashMap<String, usize> = HashMap::new();
    let _ = value_size_buckets; // suppress unused warning

    let visited = match idb::iter_store(
        path,
        &mut |key, value| {
            let prefix = if key.is_empty() { 0xff } else { key[0] };
            let entry = prefix_dist.entry(prefix).or_insert((0, 0));
            entry.0 += 1;
            entry.1 += value.len();

            // Bucket by value size to see distribution.
            let bucket = match value.len() {
                0 => "0",
                1..=8 => "1-8",
                9..=16 => "9-16",
                17..=32 => "17-32",
                33..=64 => "33-64",
                65..=128 => "65-128",
                129..=256 => "129-256",
                257..=1024 => "257-1024",
                1025..=4096 => "1025-4096",
                _ => "4097+",
            };
            *value_size_buckets.entry(bucket.to_string()).or_insert(0) += 1;

            // Save EVERY record for full dump (probe is debug tool, not production).
            if prefix < 0x10 {
                candidates.push((key.to_vec(), value.to_vec()));
            }
            true
        },
        None,
    ) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("iter_store failed: {e}");
            return;
        }
    };

    eprintln!("\nvalue size distribution:");
    let order = ["0", "1-8", "9-16", "17-32", "33-64", "65-128", "129-256", "257-1024", "1025-4096", "4097+"];
    for k in order.iter() {
        if let Some(c) = value_size_buckets.get(*k) {
            eprintln!("  {:>12} bytes  count={}", k, c);
        }
    }

    eprintln!("total records: {visited}");

    let mut p: Vec<_> = prefix_dist.iter().collect();
    p.sort_by_key(|(k, _)| **k);
    eprintln!("\nprefix distribution (prefix -> (count, total_value_bytes)):");
    for (k, (count, bytes)) in p {
        eprintln!("  0x{:02x}  count={:5}  bytes={:8}", k, count, bytes);
    }

    eprintln!("\ncandidate records (prefix in 0x00..0x10, value >= 32 bytes): {}",
        candidates.len());

    // Sort candidates by key for stable output.
    candidates.sort_by(|a, b| a.0.cmp(&b.0));

    // Show first 100 candidates in full (first 128 bytes) so we can
    // see V8 value tags.
    for (i, (key, value)) in candidates.iter().take(100).enumerate() {
        let v_show = if value.len() > 128 { &value[..128] } else { &value[..] };
        let v_tail = if value.len() > 128 {
            format!(" ...(+{} bytes)", value.len() - 128)
        } else {
            String::new()
        };
        eprintln!(
            "\n--- candidate #{} (value_len={}) ---\n  KEY[{}] {}\n  VAL[{}] {}{}",
            i,
            value.len(),
            key.len(),
            hex(key),
            value.len(),
            hex(v_show),
            v_tail
        );
        // Show first 64 bytes as a UTF-8 hint (catches string fields).
        if value.len() > 0 {
            if let Ok(s) = std::str::from_utf8(&value[..value.len().min(64)]) {
                if !s.is_empty() && s.chars().all(|c| !c.is_control()) {
                    eprintln!("  VAL utf-8 hint: {:?}", s);
                }
            }
        }
    }

    if candidates.len() > 100 {
        eprintln!("\n... ({} more candidates not shown)", candidates.len() - 100);
    }
}

fn hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for byte in b {
        s.push_str(&format!("{:02x}", byte));
    }
    s
}

fn main() {
    let Some(home) = dirs::home_dir() else {
        eprintln!("no home dir");
        std::process::exit(1);
    };

    // Discover ALL Chrome IndexedDB LevelDB roots. Print one-line summary
    // per root, then deep-probe any that contain > 10 KB total data.
    let base = home.join("Library/Application Support/Google/Chrome/Default/IndexedDB");
    let read_dir = match std::fs::read_dir(&base) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("cannot read {}: {e}", base.display());
            std::process::exit(1);
        }
    };
    let mut all: Vec<std::path::PathBuf> = read_dir
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.file_name().map_or(false, |n| {
            let s = n.to_string_lossy();
            s.ends_with(".indexeddb.leveldb")
        }))
        .collect();
    all.sort();

    eprintln!("=== found {} Chrome IndexedDB roots ===", all.len());
    for p in &all {
        if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
            eprintln!("  {}", name);
        }
    }
    eprintln!();

    for r in &all {
        probe(r);
    }

    // Stage 4-D.5b follow-up: probe Chrome Local Storage with bytewise
    // comparator. The Local Storage leveldb holds ALL origins in one
    // directory; we filter for workbuddy keys. Chrome locks the dir
    // while running, so we copy to /tmp first.
    let local_storage = home.join("Library/Application Support/Google/Chrome/Default/Local Storage/leveldb");
    probe_localstorage(&home, &local_storage, "workbuddy");
    probe_localstorage(&home, &local_storage, "doubao");
    probe_localstorage(&home, &local_storage, "minimax");
    probe_localstorage(&home, &local_storage, "MiniMax");
    probe_localstorage(&home, &local_storage, "bytedance");
    probe_localstorage(&home, &local_storage, "codebuddy");
}

fn probe_localstorage(home: &Path, local_storage: &Path, substring: &str) {
    if !local_storage.is_dir() {
        return;
    }
    eprintln!(
        "\n\n========== localstorage filter={:?} ==========",
        substring
    );
    let tmp = std::path::PathBuf::from(format!("/tmp/wb_ls_{}_{}", std::process::id(), substring));
    let _ = std::fs::remove_dir_all(&tmp);
    if let Err(e) = copy_dir_files(local_storage, &tmp) {
        eprintln!("copy failed: {e}");
        return;
    }

    let mut records: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    let mut total = 0usize;
    match idb::iter_store_bytewise(&tmp, &mut |key, value| {
        total += 1;
        let key_str = String::from_utf8_lossy(key);
        if key_str.to_lowercase().contains(&substring.to_lowercase()) {
            records.push((key.to_vec(), value.to_vec()));
        }
        true
    }, None) {
        Ok(_) => {}
        Err(e) => {
            eprintln!("localstorage iter failed: {e}");
            let _ = std::fs::remove_dir_all(&tmp);
            return;
        }
    }
    eprintln!("total records scanned: {total}");
    eprintln!("matching records: {}", records.len());
    for (i, (k, v)) in records.iter().take(40).enumerate() {
        let k_str = String::from_utf8_lossy(k);
        let v_str = if v.len() > 200 {
            format!("{} ...(+{} bytes)", String::from_utf8_lossy(&v[..200]), v.len() - 200)
        } else {
            String::from_utf8_lossy(v).into_owned()
        };
        eprintln!(
            "\n  --- match #{} (val_len={}) ---\n  KEY: {}\n  VAL: {}",
            i,
            v.len(),
            k_str,
            v_str
        );
    }
    if records.len() > 40 {
        eprintln!("\n  ... ({} more records)", records.len() - 40);
    }
    let _ = std::fs::remove_dir_all(&tmp);
    if local_storage.is_dir() {
        eprintln!("\n\n========== {} (bytewise cmp) ==========", local_storage.display());
        let tmp = std::path::PathBuf::from(format!("/tmp/wb_localstorage_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        if let Err(e) = copy_dir_files(&local_storage, &tmp) {
            eprintln!("copy failed: {e}");
            return;
        }
        eprintln!("copied to {}", tmp.display());

        let mut workbuddy_records: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        let mut total = 0usize;
        match idb::iter_store_bytewise(&tmp, &mut |key, value| {
            total += 1;
            let key_str = String::from_utf8_lossy(key);
            if key_str.contains("workbuddy") {
                workbuddy_records.push((key.to_vec(), value.to_vec()));
            }
            true
        }, None) {
            Ok(_) => {}
            Err(e) => eprintln!("localstorage iter failed: {e}"),
        }
        eprintln!("localstorage total records: {total}");
        eprintln!("workbuddy records: {}", workbuddy_records.len());
        for (i, (k, v)) in workbuddy_records.iter().enumerate() {
            eprintln!(
                "\n--- wb-localstorage #{} (key_len={}, val_len={}) ---\n  KEY: {}\n  VAL: {}",
                i,
                k.len(),
                v.len(),
                String::from_utf8_lossy(k),
                if v.len() > 512 {
                    format!("{} ...(+{} bytes)", String::from_utf8_lossy(&v[..512]), v.len() - 512)
                } else {
                    String::from_utf8_lossy(v).into_owned()
                }
            );
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }
}

fn copy_dir_files(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        if ft.is_file() {
            std::fs::copy(entry.path(), dst.join(entry.file_name()))?;
        }
    }
    Ok(())
}