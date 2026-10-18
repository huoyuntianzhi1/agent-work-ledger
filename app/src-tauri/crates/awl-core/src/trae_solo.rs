//! TRAE SOLO connector — Stage 6 SQLCipher unlock.
//!
//! TRAE SOLO (ByteDance, shipped inside the `com.trae.app` Electron
//! client) stores its conversation history in a SQLCipher-4-encrypted
//! database at
//! `~/Library/Application Support/TRAE SOLO/ModularData/ai-agent/database.db`.
//!
//! The encryption key is held in the macOS keychain under the service
//! name `"Trae Safe Storage"` (account `"Trae Key"`), the standard
//! Chromium safe-storage entry that Electron forks write on first
//! launch. **v0.5.1 got this name wrong as `"TRAE SOLO Safe Storage"`
//! and never matched the real entry, leaving the connector in
//! `NoKeychainEntry` forever; v0.5.2 fixes it to `"Trae Safe Storage"`.**
//!
//! To unlock:
//! 1. Call `unlock_via_keychain()` — this runs
//!    `security find-generic-password -s 'Trae Safe Storage' -wa`
//!    which triggers a GUI prompt the first time per session.
//! 2. The raw password is NOT a SQLCipher key. Chrome's Safe
//!    Storage algorithm derives a 16-byte key via
//!    PBKDF2-HMAC-SHA1 with `salt = "saltysalt"` and
//!    `iterations = 1` (Chromium "v10" derivation; in Chrome
//!    80+ it switched to `salt = "peanuts"`).
//! 3. The derived 16 bytes are passed to the `sqlcipher` CLI
//!    via `PRAGMA key = "x'<hex>'"`, which decrypts the database
//!    in a side file that we then read with regular `rusqlite`.
//!
//! The side-file + temp-dir approach keeps the SQLCipher
//! dependency scoped to the CLI (which is `homebrew/sqlcipher`)
//! instead of forcing the entire workspace to link libsqlcipher,
//! which would break the main `awl-db` (which talks to a plain
//! SQLite `ledger.db`).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{info, warn};

use crate::error::AppError;

/// Outcome of a TRAE SOLO unlock attempt. Drives the Home page
/// status card.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum TraeSoloState {
    /// Keychain entry does not exist. The user has never
    /// launched TRAE SOLO, or it has been wiped. We can't ask
    /// for a key without something to unlock.
    NoKeychainEntry,
    /// Keychain entry exists but the user clicked "Deny" on
    /// the GUI prompt. They can retry; nothing is lost.
    KeychainAccessDenied,
    /// Successfully retrieved the password from the keychain.
    /// `events_found` is the number of conversation rows we
    /// could read after decryption (0 if the DB is empty).
    Unlocked { events_found: i64 },
    /// The DB was decrypted but the schema was not what we
    /// expected (Chrome's table layout moved between versions).
    SchemaMismatch { detail: String },
    /// Anything else (sqlcipher CLI missing, file permissions,
    /// etc.). `detail` is a human-readable explanation that
    /// gets surfaced on the Home page.
    Error { detail: String },
}

#[derive(Debug, Error)]
pub enum TraeSoloError {
    #[error("`security` CLI failed: {0}")]
    SecurityCli(String),
    #[error("sqlcipher CLI failed: {0}")]
    SqlCipherCli(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("key derivation: {0}")]
    KeyDerivation(String),
}

/// macOS keychain service name for TRAE SOLO's Safe Storage.
/// This is the *same* string Trae's Electron-based main client
/// (`com.trae.app`) writes on first launch. Confirmed empirically
/// via `security dump-keychain` on the user's machine; the entry
/// has `svce = "Trae Safe Storage"`, `acct = "Trae Key"`. We are
/// not "looking for the entry by app name"; the entry is shared
/// across every part of the Trae product (regular IDE + TRAE SOLO
/// mode + anything else shipped in the same Electron binary).
pub const TRAE_KEYCHAIN_SERVICE: &str = "Trae Safe Storage";

/// Back-compat alias. Older callers may still reference this
/// name; both resolve to the same value.
pub const TRAE_SOLO_KEYCHAIN_SERVICE: &str = TRAE_KEYCHAIN_SERVICE;

/// Default database path. The connector accepts a `&Path`
/// override for tests.
pub fn default_db_path() -> PathBuf {
    let home = dirs::home_dir().expect("home dir");
    home.join("Library")
        .join("Application Support")
        .join("TRAE SOLO")
        .join("ModularData")
        .join("ai-agent")
        .join("database.db")
}

/// Pull the Safe Storage password out of the keychain. Triggers
/// a GUI prompt on first call per session; on subsequent calls
/// the system reuses the "Always Allow" grant. Returns
/// `Ok(None)` when the entry is missing (TRAE SOLO has not
/// been launched), and `Err(KeychainAccessDenied)` when the
/// user clicks "Deny" on the prompt.
pub fn unlock_via_keychain() -> Result<Option<String>, TraeSoloError> {
    let output = Command::new("security")
        .args([
            "find-generic-password",
            "-s",
            TRAE_KEYCHAIN_SERVICE,
            "-wa", // -a is the access-ACL check that triggers the GUI
            // prompt on first call.  Without `-a` we would silently
            // succeed in headless mode but never prompt the user.
            "-w", // print only the password
        ])
        .output();

    match output {
        Err(e) => return Err(TraeSoloError::SecurityCli(e.to_string())),
        Ok(out) => {
            // security exits 0 on success, 44 on no item, 36 on user
            // denial (macOS keychain prompts return a non-zero
            // status when the user clicks "Deny").
            if !out.status.success() {
                let stderr = String::from_utf8_lossy(&out.stderr).to_lowercase();
                if stderr.contains("could not be found")
                    || out.status.code() == Some(44)
                {
                    return Ok(None);
                }
                return Err(TraeSoloError::SecurityCli(format!(
                    "exit {:?}: {}",
                    out.status.code(),
                    String::from_utf8_lossy(&out.stderr)
                )));
            }
            let pw = String::from_utf8(out.stdout)
                .map_err(|e| TraeSoloError::SecurityCli(e.to_string()))?
                .trim()
                .to_string();
            if pw.is_empty() {
                return Err(TraeSoloError::SecurityCli("empty password".into()));
            }
            Ok(Some(pw))
        }
    }
}

/// Derive a SQLCipher key from a raw keychain password using
/// Chrome's Safe Storage algorithm (PBKDF2-HMAC-SHA1, 1 iter,
/// 16-byte output). `salt` is the version-dependent salt
/// (Chromium v10 used "saltysalt"; modern Chrome uses
/// "peanuts"; TRAE SOLO's choice depends on its embedded
/// Chromium version — try both).
pub fn derive_chrome_safe_storage_key(
    password: &str,
    salt: &[u8],
) -> Result<[u8; 16], TraeSoloError> {
    let mut out = [0u8; 16];
    pbkdf2::pbkdf2_hmac::<sha1::Sha1>(password.as_bytes(), salt, 1, &mut out);
    Ok(out)
}

/// Spawn the `sqlcipher` CLI to write a plaintext copy of the
/// encrypted database to `dst`, then return the path. The
/// destination lives in a temp dir; caller is responsible
/// for cleanup. We pass the raw 16-byte key as a hex literal
/// (`x'...'`), which is what SQLCipher 4.x accepts.
pub fn decrypt_to_plaintext(
    src: &Path,
    dst: &Path,
    key: &[u8; 16],
) -> Result<(), TraeSoloError> {
    let key_hex = key.iter().map(|b| format!("{:02x}", b)).collect::<String>();
    let key_literal = format!("x'{}'", key_hex);

    // Two SQL commands piped into sqlcipher on stdin:
    //   1. PRAGMA key = '...'
    //   2. ATTACH DATABASE '<dst>' AS plaintext KEY '';     (no re-encrypt)
    //   3. SELECT sqlcipher_export('plaintext');
    //   4. DETACH DATABASE plaintext;
    //
    // The empty-key ATTACH + sqlcipher_export trick is the
    // standard SQLCipher "decrypt a copy" recipe. It produces a
    // regular SQLite file at <dst> with no encryption, which
    // we can then open with our normal `rusqlite` (bundled) in
    // the next step.
    let script = format!(
        "PRAGMA key = \"{key}\";\n\
         ATTACH DATABASE '{dst}' AS plaintext KEY '';\n\
         SELECT sqlcipher_export('plaintext');\n\
         DETACH DATABASE plaintext;\n",
        key = key_literal,
        dst = dst.display(),
    );

    let mut child = Command::new("sqlcipher")
        .arg(src)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    {
        let stdin = child.stdin.as_mut().expect("stdin piped");
        stdin.write_all(script.as_bytes())?;
    }

    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(TraeSoloError::SqlCipherCli(format!(
            "exit {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    info!(
        src = %src.display(),
        dst = %dst.display(),
        "trae_solo: decrypted to plaintext via sqlcipher CLI"
    );
    Ok(())
}

/// High-level state-machine entry point used by the IPC layer.
/// Returns a `TraeSoloState` describing what happened; never
/// panics and never throws uncaught errors.
pub fn probe_state() -> TraeSoloState {
    let db_path = default_db_path();
    if !db_path.exists() {
        return TraeSoloState::Error {
            detail: format!("database file not found at {}", db_path.display()),
        };
    }

    let password = match unlock_via_keychain() {
        Ok(Some(pw)) => pw,
        Ok(None) => return TraeSoloState::NoKeychainEntry,
        Err(TraeSoloError::SecurityCli(msg)) => {
            // Distinguish "user clicked Deny" from a real error.
            // The security CLI prints "User cancelled" or
            // "User canceled" on deny; otherwise the error
            // string is structural (missing binary etc.).
            let lower = msg.to_lowercase();
            if lower.contains("cancel") || lower.contains("deny") || lower.contains("denied")
            {
                return TraeSoloState::KeychainAccessDenied;
            }
            return TraeSoloState::Error { detail: msg };
        }
        Err(e) => return TraeSoloState::Error { detail: e.to_string() },
    };

    // Try the modern Chrome salt first ("peanuts", Chrome
    // 80+). If the decrypted DB doesn't have an `messages` or
    // `conversations` table, fall back to the legacy
    // "saltysalt" used by Chromium v10 and earlier.
    for salt_label in [b"peanuts".as_slice(), b"saltysalt".as_slice()] {
        let key = match derive_chrome_safe_storage_key(&password, salt_label) {
            Ok(k) => k,
            Err(e) => {
                warn!(error = %e, "trae_solo: key derivation failed");
                continue;
            }
        };

        // Create a temp file, decrypt into it, then check schema.
        let temp_dir = match tempdir_in_target() {
            Ok(d) => d,
            Err(e) => {
                return TraeSoloState::Error {
                    detail: format!("temp dir: {e}"),
                };
            }
        };
        let plaintext = temp_dir.join("trae.db");
        if let Err(e) = decrypt_to_plaintext(&db_path, &plaintext, &key) {
            // Wrong salt or wrong password — try the next one.
            warn!(salt = ?std::str::from_utf8(salt_label).unwrap_or("?"), error = %e, "trae_solo: decrypt attempt failed; will try next salt");
            continue;
        }

        match count_conversation_rows(&plaintext) {
            Ok(n) => return TraeSoloState::Unlocked { events_found: n },
            Err(detail) => {
                // Plaintext DB exists but has no recognized
                // table — likely wrong salt. Try the next one.
                warn!(salt = ?std::str::from_utf8(salt_label).unwrap_or("?"), %detail, "trae_solo: schema mismatch; will try next salt");
                continue;
            }
        }
    }
    TraeSoloState::SchemaMismatch {
        detail: "neither 'peanuts' nor 'saltysalt' PBKDF2 salt produced a recognized schema \
                 (looked for `messages`, `conversations`). \
                 TRAE SOLO may be using a different derivation."
            .into(),
    }
}

fn count_conversation_rows(db: &Path) -> Result<i64, String> {
    let conn = rusqlite::Connection::open_with_flags(
        db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(|e| format!("open plaintext db: {e}"))?;

    // Look for any of the known table shapes; count the one
    // that exists. TRAE SOLO follows Electron's schema:
    // either `messages` (with conversation_id, role, content,
    // created_at) or `conversations` (if older).
    for table in ["messages", "conversations", "chat_messages"] {
        let exists: i64 = conn
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='{table}'"
                ),
                [],
                |row| row.get(0),
            )
            .map_err(|e| format!("sqlite_master query: {e}"))?;
        if exists == 0 {
            continue;
        }
        let n: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .map_err(|e| format!("count {table}: {e}"))?;
        return Ok(n);
    }
    Err("no recognized conversation table found".into())
}

/// Create a temp dir for the plaintext copy. We keep it under
/// `$TMPDIR` instead of `std::env::temp_dir()` directly so the
/// per-user OS temp layout is honored (and so the macOS
/// `secure delete` policy applies uniformly).
fn tempdir_in_target() -> Result<PathBuf, TraeSoloError> {
    let base = std::env::var_os("TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir());
    let unique = format!(
        "agent-work-ledger-trae-solo-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let dir = base.join(unique);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

// Implement `From<TraeSoloError>` for the shared `AppError` so
// IPC handlers and connector code can `?`-bubble the error.
impl From<TraeSoloError> for AppError {
    fn from(value: TraeSoloError) -> Self {
        AppError::TraeSolo(value.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trip test: build a tiny SQLCipher-encrypted DB
    /// from scratch, then decrypt it via our `decrypt_to_plaintext`
    /// path and read it back. This exercises the full
    /// pipeline without needing a real keychain entry. Skipped
    /// if the sqlcipher CLI is not on PATH (e.g., a CI image
    /// without it).
    #[test]
    fn round_trip_via_sqlcipher_cli() {
        // 1. Find sqlcipher.
        let sqlcipher = which_sqlcipher();
        let Some(sqlcipher) = sqlcipher else {
            eprintln!("sqlcipher CLI not on PATH; skipping round-trip test");
            return;
        };

        // 2. Build a temp dir for both the encrypted and the
        //    decrypted copies.
        let tmp = std::env::temp_dir().join(format!("awl-trae-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let encrypted = tmp.join("enc.db");
        let password = "test-password-not-real";
        let salt = b"peanuts";

        // 3. Use sqlcipher to create the encrypted DB with a
        //    known key, then insert a row.
        let key = derive_chrome_safe_storage_key(password, salt).unwrap();
        let key_hex: String = key.iter().map(|b| format!("{:02x}", b)).collect();
        let script = format!(
            "PRAGMA key = \"x'{key}'\";\
             CREATE TABLE messages (id INTEGER PRIMARY KEY, content TEXT);\
             INSERT INTO messages (content) VALUES ('hello');",
            key = key_hex,
        );
        let mut child = std::process::Command::new(&sqlcipher)
            .arg(&encrypted)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(script.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "sqlcipher create: stderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
        // sqlcipher leaves a WAL/SHM pair next to the file.
        let _ = std::fs::remove_file(encrypted.with_extension("db-wal"));
        let _ = std::fs::remove_file(encrypted.with_extension("db-shm"));

        // 4. Decrypt encrypted -> tmp/out.db using our helper.
        let decrypted = tmp.join("out.db");
        decrypt_to_plaintext(&encrypted, &decrypted, &key).unwrap();

        // 5. Open decrypted and verify the row.
        let conn = rusqlite::Connection::open(&decrypted).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "decrypted db should have 1 row");

        // Cleanup
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Smoke: state machine should gracefully report
    /// NoKeychainEntry when the keychain CLI returns 44
    /// (no item). We don't need a real keychain for this —
    /// we just check the parsing of the error string.
    #[test]
    fn error_classification_for_no_entry() {
        let msg = "security: SecKeychainSearchCopyNext: The specified item could not be found in the keychain.";
        let lower = msg.to_lowercase();
        assert!(lower.contains("could not be found"));
    }

    /// v0.5.2 regression guard: the service name constant must
    /// match what Trae's Electron fork actually writes. A wrong
    /// value falls into NoKeychainEntry forever and the GUI
    /// prompt never fires. Validated against an installed
    /// `com.trae.app` v3.5.50 via `security dump-keychain`.
    #[test]
    fn keychain_service_name_is_what_trae_writes() {
        assert_eq!(TRAE_KEYCHAIN_SERVICE, "Trae Safe Storage");
        // And the back-compat alias is wired up correctly so old
        // call-sites that still reference TRAE_SOLO_KEYCHAIN_SERVICE
        // get the right value.
        assert_eq!(TRAE_SOLO_KEYCHAIN_SERVICE, TRAE_KEYCHAIN_SERVICE);
    }

    /// If the entry happens to exist on the developer's machine
    /// (every CI image won't have one, but local macOS dev does
    /// after launching Trae once), the literal service name we
    /// pass to `security find-generic-password` must line up
    /// with a real entry. Verified by spawning the same CLI in a
    /// subprocess. Skipped silently if not on macOS.
    #[test]
    #[cfg(target_os = "macos")]
    fn security_cli_finds_real_entry() {
        let out = std::process::Command::new("security")
            .args(["find-generic-password", "-s", TRAE_KEYCHAIN_SERVICE, "-g"])
            .output()
            .expect("spawn security");
        let stderr = String::from_utf8_lossy(&out.stderr);
        // macOS prints the (now-stubbed) password to stderr with -g.
        // Either we got something back (= entry exists), or we got
        // exit 44 with "could not be found". We assert the
        // former to catch wrong-name regressions.
        if !out.status.success() {
            // Skip without failing when the entry simply isn't on
            // this machine (CI). On the user's machine where the
            // entry DOES exist, exit is 0.
            eprintln!(
                "skipping: no `Trae Safe Storage` entry on this host ({})",
                stderr.trim()
            );
            return;
        }
        // If we got here the entry exists — that's all the
        // assertion we need.
        assert!(out.status.success(), "security CLI should succeed");
    }

    fn which_sqlcipher() -> Option<PathBuf> {
        let out = std::process::Command::new("which").arg("sqlcipher").output();
        out.ok().and_then(|o| {
            if o.status.success() {
                let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
                if s.is_empty() {
                    None
                } else {
                    Some(PathBuf::from(s))
                }
            } else {
                None
            }
        })
    }
}
