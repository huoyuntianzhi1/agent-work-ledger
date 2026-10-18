//! Stage 1-I: thin FFI wrapper over system `libleveldb` for reading
//! Chromium IndexedDB LevelDB stores.
//!
//! Why a hand-rolled wrapper rather than the `leveldb` crate? The crate
//! version locked in our Cargo.lock (0.8) ships with `leveldb-sys` 2.0
//! which insists on building its own copy of snappy via cmake — fragile
//! on Apple Silicon because the bundled snappy's CMakeLists trips on
//! modern CMake. Since we already know from the C PoC that Chromium's
//! IDB only needs a handful of entry points and a single custom
//! comparator, the surface area is small enough to maintain directly.
//!
//! Public surface:
//! - [`iter_store`] walks every record in a `.indexeddb.leveldb/` dir,
//!   invoking the callback for each (key, value) pair.
//! - [`extract_utf16le_string`] decodes Chromium's object-store-name
//!   encoding from the tail of a key.

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::path::Path;

use crate::error::{AppError, AppResult};
use sha2::{Digest, Sha256};

// ---------------------------------------------------------------------------
// leveldb C API (only the entry points we actually use). All pointers are
// owned and must be destroyed with the matching `*_destroy` call. The
// wrapper RAII types below enforce that.
// ---------------------------------------------------------------------------

#[repr(C)]
pub struct leveldb_options_t {
    _private: [u8; 0],
}
#[repr(C)]
pub struct leveldb_comparator_t {
    _private: [u8; 0],
}
#[repr(C)]
pub struct leveldb_t {
    _private: [u8; 0],
}
#[repr(C)]
pub struct leveldb_readoptions_t {
    _private: [u8; 0],
}
#[repr(C)]
pub struct leveldb_iterator_t {
    _private: [u8; 0],
}

#[link(name = "leveldb")]
extern "C" {
    fn leveldb_options_create() -> *mut leveldb_options_t;
    fn leveldb_options_destroy(opt: *mut leveldb_options_t);
    fn leveldb_options_set_create_if_missing(opt: *mut leveldb_options_t, v: u8);
    fn leveldb_options_set_error_if_exists(opt: *mut leveldb_options_t, v: u8);
    fn leveldb_options_set_paranoid_checks(opt: *mut leveldb_options_t, v: u8);
    fn leveldb_options_set_comparator(
        opt: *mut leveldb_options_t,
        cmp: *mut leveldb_comparator_t,
    );

    fn leveldb_comparator_create(
        state: *mut c_void,
        destructor: Option<unsafe extern "C" fn(*mut c_void)>,
        compare: Option<
            unsafe extern "C" fn(
                *mut c_void,
                *const c_char,
                usize,
                *const c_char,
                usize,
            ) -> c_int,
        >,
        name: Option<unsafe extern "C" fn(*mut c_void) -> *const c_char>,
    ) -> *mut leveldb_comparator_t;
    fn leveldb_comparator_destroy(cmp: *mut leveldb_comparator_t);

    fn leveldb_open(
        opt: *mut leveldb_options_t,
        name: *const c_char,
        errptr: *mut *mut c_char,
    ) -> *mut leveldb_t;
    fn leveldb_close(db: *mut leveldb_t);
    fn leveldb_free(ptr: *mut c_void);

    fn leveldb_readoptions_create() -> *mut leveldb_readoptions_t;
    fn leveldb_readoptions_destroy(opt: *mut leveldb_readoptions_t);

    fn leveldb_create_iterator(
        db: *mut leveldb_t,
        ropt: *mut leveldb_readoptions_t,
    ) -> *mut leveldb_iterator_t;
    fn leveldb_iter_destroy(it: *mut leveldb_iterator_t);
    fn leveldb_iter_seek_to_first(it: *mut leveldb_iterator_t);
    fn leveldb_iter_valid(it: *mut leveldb_iterator_t) -> u8;
    fn leveldb_iter_next(it: *mut leveldb_iterator_t);
    fn leveldb_iter_key(it: *mut leveldb_iterator_t, klen: *mut usize) -> *const c_char;
    fn leveldb_iter_value(it: *mut leveldb_iterator_t, vlen: *mut usize) -> *const c_char;
    fn leveldb_iter_get_error(it: *mut leveldb_iterator_t, errptr: *mut *mut c_char);
}

// ---------------------------------------------------------------------------
// `idb_cmp1` comparator — bytewise memcmp. Chromium's IndexedDB pre-encodes
// keys so plain bytewise ordering already gives IDB's logical order.
// ---------------------------------------------------------------------------

unsafe extern "C" fn idb_cmp1_destructor(_state: *mut c_void) {}
unsafe extern "C" fn idb_cmp1_compare(
    _state: *mut c_void,
    a: *const c_char,
    alen: usize,
    b: *const c_char,
    blen: usize,
) -> c_int {
    // Inline bytewise compare instead of calling libc `memcmp` — we don't
    // pull in the libc crate to keep this module dependency-free, and the
    // forwarder below is the only place that needs it.
    let ap = unsafe { std::slice::from_raw_parts(a as *const u8, alen) };
    let bp = unsafe { std::slice::from_raw_parts(b as *const u8, blen) };
    let n = alen.min(blen);
    for i in 0..n {
        let av = ap[i];
        let bv = bp[i];
        if av != bv {
            return (av as c_int) - (bv as c_int);
        }
    }
    if alen < blen {
        -1
    } else if alen > blen {
        1
    } else {
        0
    }
}
unsafe extern "C" fn idb_cmp1_name(_state: *mut c_void) -> *const c_char {
    // Static, never freed by leveldb. Returning the same bytes as
    // Chromium's own idb_cmp1 is what lets `leveldb_open` accept the
    // existing MANIFEST/OPTIONS files written by the browser.
    b"idb_cmp1\0".as_ptr() as *const c_char
}

/// RAII-owned options + comparator with the requested flavour. Returns
/// `(opt, cmp, needs_comparator_destroy)` — when `cmp` is null (the
/// bytewise case), the caller skips `leveldb_comparator_destroy`.
fn build_options(
    comparator: Comparator,
) -> AppResult<(*mut leveldb_options_t, *mut leveldb_comparator_t)> {
    let opt = unsafe { leveldb_options_create() };
    if opt.is_null() {
        return Err(AppError::Connector(
            "idb".into(),
            "leveldb_options_create returned null".into(),
        ));
    }
    unsafe {
        leveldb_options_set_create_if_missing(opt, 0);
        leveldb_options_set_error_if_exists(opt, 0);
        leveldb_options_set_paranoid_checks(opt, 1);
    }
    let cmp: *mut leveldb_comparator_t = match comparator {
        Comparator::IdbCmp1 => unsafe {
            let c = leveldb_comparator_create(
                std::ptr::null_mut(),
                Some(idb_cmp1_destructor),
                Some(idb_cmp1_compare),
                Some(idb_cmp1_name),
            );
            if !c.is_null() {
                leveldb_options_set_comparator(opt, c);
            }
            c
        },
        // Bytewise: do NOT call `leveldb_options_set_comparator` at all.
        // leveldb then uses its built-in `BytewiseComparator` and there's
        // nothing for us to destroy. The non-Null placeholder below is
        // never dereferenced.
        Comparator::Bytewise => std::ptr::null_mut(),
    };
    if cmp.is_null() && matches!(comparator, Comparator::IdbCmp1) {
        unsafe {
            leveldb_options_destroy(opt);
        }
        return Err(AppError::Connector(
            "idb".into(),
            "leveldb_comparator_create returned null".into(),
        ));
    }
    Ok((opt, cmp))
}

/// Destroy whatever `build_options` returned. For bytewise, `cmp` is
/// null and we skip the destroy.
unsafe fn destroy_options(
    opt: *mut leveldb_options_t,
    cmp: *mut leveldb_comparator_t,
    _comparator: Comparator,
) {
    leveldb_options_destroy(opt);
    if !cmp.is_null() {
        leveldb_comparator_destroy(cmp);
    }
}

/// Open the LevelDB at `path` (typically a directory like
/// `chrome_doubao-chat_0.indexeddb.leveldb`) and invoke `visit` for every
/// (key, value) pair. Iteration stops when `visit` returns `false` or when
/// the underlying cursor is exhausted.
///
/// `limit` is an optional safety bound; `None` means "read everything",
/// which is what we want for connector scans since each Chromium store holds
/// at most a few thousand records.
pub fn iter_store(
    path: &Path,
    visit: &mut dyn FnMut(&[u8], &[u8]) -> bool,
    limit: Option<usize>,
) -> AppResult<usize> {
    iter_store_with(path, visit, limit, Comparator::IdbCmp1)
}

/// Open a LevelDB at `path` using the bytewise comparator instead of
/// `idb_cmp1`. Chromium uses bytewise for Local Storage, Cookies, and
/// several other stores — IndexedDB is the only one that uses `idb_cmp1`.
/// Stage 4-D.5b added this so the connector pipeline can scan those
/// stores too.
pub fn iter_store_bytewise(
    path: &Path,
    visit: &mut dyn FnMut(&[u8], &[u8]) -> bool,
    limit: Option<usize>,
) -> AppResult<usize> {
    iter_store_with(path, visit, limit, Comparator::Bytewise)
}

/// Which Chromium LevelDB comparator to use.
#[derive(Clone, Copy)]
enum Comparator {
    IdbCmp1,
    Bytewise,
}

fn iter_store_with(
    path: &Path,
    visit: &mut dyn FnMut(&[u8], &[u8]) -> bool,
    limit: Option<usize>,
    comparator: Comparator,
) -> AppResult<usize> {
    let (opt, cmp) = build_options(comparator)?;

    let cpath = match CString::new(path.as_os_str().as_encoded_bytes()) {
        Ok(s) => s,
        Err(e) => {
            unsafe {
                destroy_options(opt, cmp, comparator);
            }
            return Err(AppError::Io(std::io::Error::other(format!(
                "invalid leveldb path: {e}"
            ))));
        }
    };

    let mut err: *mut c_char = std::ptr::null_mut();
    let db = unsafe { leveldb_open(opt, cpath.as_ptr(), &mut err) };
    if !err.is_null() {
        let msg = unsafe { CStr::from_ptr(err) }
            .to_string_lossy()
            .into_owned();
        unsafe {
            leveldb_free(err as *mut c_void);
            destroy_options(opt, cmp, comparator);
        }
        return Err(AppError::Connector(
            "idb".into(),
            format!("leveldb_open {}: {msg}", path.display()),
        ));
    }
    if db.is_null() {
        unsafe {
            destroy_options(opt, cmp, comparator);
        }
        return Err(AppError::Connector(
            "idb".into(),
            format!("leveldb_open {} returned null db", path.display()),
        ));
    }

    let ropt = unsafe { leveldb_readoptions_create() };
    let it = unsafe { leveldb_create_iterator(db, ropt) };
    unsafe {
        leveldb_iter_seek_to_first(it);
    }

    let mut visited = 0usize;
    while unsafe { leveldb_iter_valid(it) } != 0 {
        let mut klen: usize = 0;
        let mut vlen: usize = 0;
        let kptr = unsafe { leveldb_iter_key(it, &mut klen) };
        let vptr = unsafe { leveldb_iter_value(it, &mut vlen) };
        let key = unsafe { std::slice::from_raw_parts(kptr as *const u8, klen) };
        let val = unsafe { std::slice::from_raw_parts(vptr as *const u8, vlen) };
        let keep_going = visit(key, val);
        visited += 1;
        if !keep_going {
            break;
        }
        if let Some(max) = limit {
            if visited >= max {
                break;
            }
        }
        unsafe {
            leveldb_iter_next(it);
        }
    }

    let mut ierr: *mut c_char = std::ptr::null_mut();
    unsafe {
        leveldb_iter_get_error(it, &mut ierr);
    }
    if !ierr.is_null() {
        let msg = unsafe { CStr::from_ptr(ierr) }
            .to_string_lossy()
            .into_owned();
        unsafe {
            leveldb_free(ierr as *mut c_void);
        }
        unsafe {
            leveldb_iter_destroy(it);
            leveldb_readoptions_destroy(ropt);
            leveldb_close(db);
            destroy_options(opt, cmp, comparator);
        }
        return Err(AppError::Connector(
            "idb".into(),
            format!("leveldb iter {}: {msg}", path.display()),
        ));
    }

    unsafe {
        leveldb_iter_destroy(it);
        leveldb_readoptions_destroy(ropt);
        leveldb_close(db);
        destroy_options(opt, cmp, comparator);
    }
    Ok(visited)
}

/// Debug-only: dump every record (key + value) as hex so we can
/// hand-inspect what a Chromium IDB LevelDB directory actually
/// contains. Used by the Stage 4-D.5b V8-decoder probe; gated behind
/// the `RUSTY_IDB_DUMP` env var so it never fires in production.
pub fn dump_records(path: &Path) -> AppResult<()> {
    let (opt, cmp) = build_options(Comparator::IdbCmp1)?;
    let cpath = match CString::new(path.as_os_str().as_encoded_bytes()) {
        Ok(s) => s,
        Err(e) => {
            unsafe {
                destroy_options(opt, cmp, Comparator::IdbCmp1);
            }
            return Err(AppError::Io(std::io::Error::other(format!(
                "invalid leveldb path: {e}"
            ))));
        }
    };
    let mut err: *mut c_char = std::ptr::null_mut();
    let db = unsafe { leveldb_open(opt, cpath.as_ptr(), &mut err) };
    if !err.is_null() || db.is_null() {
        unsafe {
            destroy_options(opt, cmp, Comparator::IdbCmp1);
        }
        return Err(AppError::Connector(
            "idb".into(),
            format!("dump_records open failed for {}", path.display()),
        ));
    }
    let ropt = unsafe { leveldb_readoptions_create() };
    let it = unsafe { leveldb_create_iterator(db, ropt) };
    unsafe { leveldb_iter_seek_to_first(it) };

    let mut total = 0usize;
    let mut prefix_dist: std::collections::HashMap<u8, usize> = std::collections::HashMap::new();
    eprintln!("=== idb dump {} ===", path.display());
    while unsafe { leveldb_iter_valid(it) } != 0 {
        let mut klen: usize = 0;
        let mut vlen: usize = 0;
        let kptr = unsafe { leveldb_iter_key(it, &mut klen) };
        let vptr = unsafe { leveldb_iter_value(it, &mut vlen) };
        let key = unsafe { std::slice::from_raw_parts(kptr as *const u8, klen) };
        let val = unsafe { std::slice::from_raw_parts(vptr as *const u8, vlen) };
        let prefix = if key.is_empty() { 0xff } else { key[0] };
        *prefix_dist.entry(prefix).or_insert(0) += 1;
        eprintln!(
            "K[{:02x}|len={}] {}\n  V[len={}] {}",
            prefix,
            key.len(),
            hex(key),
            val.len(),
            hex(val)
        );
        total += 1;
        unsafe { leveldb_iter_next(it); }
    }
    eprintln!("=== total: {} ===", total);
    let mut p: Vec<_> = prefix_dist.into_iter().collect();
    p.sort_by_key(|(k, _)| *k);
    eprintln!("prefix distribution: {:?}", p);
    unsafe {
        leveldb_iter_destroy(it);
        leveldb_readoptions_destroy(ropt);
        leveldb_close(db);
        destroy_options(opt, cmp, Comparator::IdbCmp1);
    }
    Ok(())
}

fn hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for byte in b {
        s.push_str(&format!("{:02x}", byte));
    }
    s
}

/// Best-effort UTF-16BE string decoder for the tail of an IDB key.
///
/// Verified against `chrome_doubao-chat_0` on Apple Silicon: object-store
/// names like `chrome_doubao-chat_0`, `flow-draft-conversations`, and
/// `overall` all decode correctly as UTF-16BE (i.e. the high byte comes
/// first — `00 63 00 68 00 72 00 6f 00 6d 00 65` reads as "chrome").
///
/// Surrogate halves and invalid code points are replaced with `U+FFFD`
/// so the decoder never panics on malformed bytes. Connector code should
/// treat the result as best-effort, not authoritative.
pub fn extract_utf16be_string(key: &[u8]) -> String {
    if key.is_empty() {
        return String::new();
    }
    let mut out = String::with_capacity(key.len() / 2);
    let mut i = 0;
    while i + 1 < key.len() {
        let cu = u16::from_be_bytes([key[i], key[i + 1]]);
        if let Some(c) = char::from_u32(cu as u32) {
            out.push(c);
        } else {
            out.push('\u{FFFD}');
        }
        i += 2;
    }
    out
}

/// Insert one Chromium IndexedDB record into `raw_events` if it looks like
/// real data (i.e. the key resolves to a UTF-16BE object-store name and
/// the value is at least 32 bytes). Returns `Some(1)` on insert,
/// `Some(0)` when the row already existed (UNIQUE constraint), and `None`
/// when the record should be skipped silently — bookkeeping rows,
/// metadata descriptors, and undecodable tails all return `None`.
///
/// This is the shared extraction core used by every Chromium IDB connector
/// (Doubao, MiniMax Agent, Trae, CodeBuddy CN once we add it). Each
/// connector is responsible for the iterator plumbing around this call;
/// the row schema inside `raw_events` stays uniform.
pub fn upsert_chromium_idb_record(
    conn: &mut rusqlite::Connection,
    source_id: &str,
    key: &[u8],
    value: &[u8],
) -> Option<i64> {
    // Filter: skip descriptor rows (<8 bytes) and empty values.
    //
    // Stage 4-D.5a broadened this from a strict `key[0] == 0x00` +
    // printable-ASCII user_key gate to a loose "any non-trivial record".
    // The Chromium IndexedDB key-prefix space is heterogeneous across
    // vendors — Doubao / MiniMax emit simple primary keys, but
    // WorkBuddy's web client stores most rows under prefix 0x00..0x07
    // with non-ASCII user keys (UUIDs / hex digests). 拒掉所有这些
    // means 王总's web work was silently dropped, even after we found
    // the right LevelDB root. We accept everything that looks like
    // real data; the downstream AI summariser is what decides what's
    // a conversation transcript vs bookkeeping. Over-ingesting is
    // cheap; under-ingesting is data loss.
    if key.len() < 8 || value.is_empty() {
        return None;
    }
    // Anything outside 0x00..0x0f is almost certainly a LevelDB
    // metadata row from a non-Chromium writer (Chrome FileSystem,
    // etc.) — skip those so we don't drown the user in noise.
    if key[0] > 0x0f {
        return None;
    }
    let user_key = extract_utf16be_string(&key[6..]);
    let preview = String::from_utf8_lossy(&value[..value.len().min(240)]).into_owned();

    let id = format!("idb_{}", short_hash(&[key, value].concat()));
    let occurred_at = chrono::Utc::now().to_rfc3339();
    let payload_text = serde_json::json!({
        "key_user": user_key,
        "key_prefix": key[0],
        "key_hex": hex::encode(&key[..key.len().min(64)]),
        "value_size": value.len(),
        "value_preview": preview,
    })
    .to_string();
    let payload_hash = short_hash(payload_text.as_bytes());

    let inserted = conn
        .execute(
            "INSERT OR IGNORE INTO raw_events
                (id, source_id, event_type, occurred_at, project_key, payload, redacted, created_at, payload_hash)
             VALUES (?1, ?2, 'session_meta', ?3, '', ?4, 0, ?5, ?6)",
            rusqlite::params![
                id,
                source_id,
                occurred_at,
                payload_text,
                occurred_at,
                payload_hash
            ],
        )
        .ok()?;
    if inserted == 0 {
        Some(0)
    } else {
        Some(1)
    }
}

fn short_hash(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    let digest = h.finalize();
    let mut out = String::with_capacity(16);
    for byte in digest.iter().take(8) {
        use std::fmt::Write;
        let _ = write!(out, "{:02x}", byte);
    }
    out
}

mod hex {
    pub fn encode(bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len() * 2);
        for b in bytes {
            use std::fmt::Write;
            let _ = write!(out, "{:02x}", b);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16be_roundtrip_ascii() {
        // UTF-16BE: high byte first. "chat" -> 00 63 00 68 00 61 00 74.
        let key = b"\x00\x63\x00\x68\x00\x61\x00\x74";
        assert_eq!(extract_utf16be_string(key), "chat");
    }

    #[test]
    fn utf16be_handles_odd_trailing() {
        // 5 bytes: trailing single byte is dropped, "ch" recovered.
        let key = b"\x00\x63\x00\x68\x00";
        assert_eq!(extract_utf16be_string(key), "ch");
        // A real CJK code unit must not be confused with a surrogate
        // half. U+00D8 = "Ø" is a valid Latin letter.
        let key = b"\x00\xD8";
        assert_eq!(extract_utf16be_string(key), "\u{00D8}");
    }

    #[test]
    fn utf16be_empty_input() {
        assert_eq!(extract_utf16be_string(b""), "");
    }

    #[test]
    fn utf16be_replaces_unpaired_surrogate() {
        let key = b"\xD8\x00";
        assert_eq!(extract_utf16be_string(key), "\u{FFFD}");
    }

    #[test]
    fn short_hash_is_deterministic() {
        let a = short_hash(b"hello");
        let b = short_hash(b"hello");
        assert_eq!(a, b);
        assert_eq!(a.len(), 16);
        assert_ne!(a, short_hash(b"world"));
    }

    // Stage 6 diagnostic — `cargo test --lib -- --ignored dump_doubao_work_chat --nocapture`
    // prints every LevelDB entry key + value hex under 王总's actual
    // DoubaoWork install. The prefix distribution and any plaintext
    // ascii leaking through tells us whether real chat is stored in
    // this IDB and what serialization shape (Blink V8 vs JSON vs CBOR)
    // it uses. Marked `#[ignore]` so it stays out of the default run
    // and only fires when an analyst wants a fresh snapshot.
    #[test]
    #[ignore]
    fn dump_doubao_work_chat() {
        let p = std::path::Path::new(
            "/Users/apple/Library/Application Support/DoubaoWork/Default/IndexedDB/chrome_doubaowork-chat_0.indexeddb.leveldb",
        );
        if !p.is_dir() {
            eprintln!("[dump] missing path: {}", p.display());
            return;
        }
        dump_records(p).expect("dump DoubaoWork chat IDB");
    }
}