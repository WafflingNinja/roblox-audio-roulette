// Reads the .ROBLOSECURITY cookie straight from a Firefox-family profile.
// Firefox stores cookies in a plaintext sqlite DB, so no decryption is needed.
// The live DB is WAL-locked while the browser runs, so we copy it to a temp
// file (with its -wal/-shm siblings) before opening.

use std::fs;
use std::path::{Path, PathBuf};

pub struct FoundCookie {
    pub value: String,
    pub profile: String, // path we read it from, shown to the user for transparency
}

/// Candidate roots that hold `<profile>/cookies.sqlite` on this box.
/// Order = preference: the PSD tmpfs copy is the live one.
fn candidate_cookie_dbs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let home = dirs::home_dir().unwrap_or_default();

    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(uid) = std::env::var("UID").ok().or_else(|| {
        // UID isn't exported by default; fall back to the runtime dir name.
        std::env::var("XDG_RUNTIME_DIR")
            .ok()
            .and_then(|d| Path::new(&d).file_name().map(|s| s.to_string_lossy().into_owned()))
    }) {
        roots.push(PathBuf::from(format!("/run/user/{uid}/psd")));
    }
    roots.push(home.join(".config/mozilla/firefox"));
    roots.push(home.join(".mozilla/firefox"));

    for root in roots {
        let Ok(entries) = fs::read_dir(&root) else { continue };
        for e in entries.flatten() {
            let db = e.path().join("cookies.sqlite");
            if db.is_file() {
                out.push(db);
            }
        }
    }
    out
}

fn read_roblosecurity(db: &Path) -> Option<String> {
    // Copy the DB + WAL/SHM to a private temp file so we can open a locked,
    // in-use Firefox profile without fighting the browser for the lock.
    let tmp_dir = std::env::temp_dir().join(format!("rar-cookie-{}", std::process::id()));
    let _ = fs::create_dir_all(&tmp_dir);
    let tmp_db = tmp_dir.join("cookies.sqlite");
    fs::copy(db, &tmp_db).ok()?;
    for ext in ["-wal", "-shm"] {
        let src = PathBuf::from(format!("{}{}", db.display(), ext));
        if src.is_file() {
            let _ = fs::copy(&src, format!("{}{}", tmp_db.display(), ext));
        }
    }

    let conn = rusqlite::Connection::open(&tmp_db).ok()?;
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM moz_cookies WHERE name='.ROBLOSECURITY' AND host LIKE '%roblox%' LIMIT 1",
            [],
            |row| row.get(0),
        )
        .ok();
    let _ = fs::remove_dir_all(&tmp_dir);
    value.filter(|v| !v.is_empty())
}

/// Scan every Firefox-family profile and return the first logged-in cookie.
pub fn find() -> Option<FoundCookie> {
    for db in candidate_cookie_dbs() {
        if let Some(value) = read_roblosecurity(&db) {
            return Some(FoundCookie {
                value,
                profile: db.display().to_string(),
            });
        }
    }
    None
}
