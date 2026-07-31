//! Local snapshot backup, restore, and retention.
//!
//! Backups use `VACUUM INTO`, never a file copy: the live database runs in WAL
//! mode, so copying `ledger.db` alone can capture a torn state with unmerged
//! `-wal` content. `VACUUM INTO` writes one consistent, compacted file.

use rusqlite::Connection;
use std::fs;
use std::path::{Path, PathBuf};

/// Prefix for backups the user asked for. Never pruned.
pub const MANUAL_PREFIX: &str = "ledger-";
/// Prefix for backups written automatically on app close. Pruned to `KEEP_AUTO`.
pub const AUTO_PREFIX: &str = "ledger-auto-";
/// Prefix for the safety copy taken before a restore or import.
pub const RESCUE_PREFIX: &str = "pre-restore-";
/// How many automatic backups and rescue copies to keep.
///
/// Three, not ten: the event log is append-only, so each snapshot is a strict
/// superset of the previous one. Extra copies are nested prefixes of one
/// history, not independent versions, so they buy very little.
pub const KEEP_AUTO: usize = 3;

/// Format a Unix-ms timestamp as `YYYYMMDD-HHMMSS` (UTC).
///
/// Hand-rolled because the project has no date dependency, and a backup
/// filename does not justify adding one. Uses the civil-from-days algorithm
/// (Howard Hinnant), valid for all dates after 1970.
pub fn timestamp_utc(unix_ms: i64) -> String {
    let secs = unix_ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (hh, mm, ss) = (tod / 3600, (tod % 3600) / 60, tod % 60);

    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}{m:02}{d:02}-{hh:02}{mm:02}{ss:02}")
}

/// Build the snapshot filename for a backup of the given kind.
pub fn snapshot_name(prefix: &str, unix_ms: i64) -> String {
    format!("{prefix}{}.db", timestamp_utc(unix_ms))
}

/// Write a consistent snapshot of `conn`'s database to `dest`.
///
/// `VACUUM INTO` refuses to overwrite, so `dest` must not exist — callers use a
/// fresh timestamped name. Returns the size of the written file in bytes.
pub fn snapshot_to(conn: &Connection, dest: &Path) -> rusqlite::Result<u64> {
    // Bound parameter, not string interpolation — the path is user-supplied.
    conn.execute("VACUUM INTO ?1", [dest.to_string_lossy().as_ref()])?;
    Ok(fs::metadata(dest).map(|m| m.len()).unwrap_or(0))
}

/// Delete all but the newest `keep` files in `dir` whose name starts with
/// `prefix` and ends with `.db`.
///
/// Names embed a sortable UTC timestamp, so lexical order is chronological.
/// Returns the paths removed.
pub fn prune(dir: &Path, prefix: &str, keep: usize) -> std::io::Result<Vec<PathBuf>> {
    let mut matching: Vec<PathBuf> = fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with(prefix) && n.ends_with(".db"))
                .unwrap_or(false)
        })
        .collect();

    matching.sort();
    let mut removed = Vec::new();
    if matching.len() > keep {
        for path in &matching[..matching.len() - keep] {
            fs::remove_file(path)?;
            removed.push(path.clone());
        }
    }
    Ok(removed)
}

/// Why a candidate file cannot be restored.
#[derive(Debug)]
pub struct InvalidCandidate(pub String);

impl std::fmt::Display for InvalidCandidate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for InvalidCandidate {}

/// Check a candidate backup **before** the live database is touched.
///
/// Requires: the file exists, opens as SQLite, passes `integrity_check`, has an
/// `events` table, and holds at least one event. Opened read-only so validation
/// can never modify the candidate.
pub fn validate_candidate(path: &Path) -> Result<(), InvalidCandidate> {
    if !path.exists() {
        return Err(InvalidCandidate("the file no longer exists".into()));
    }

    let uri = format!("file:{}?mode=ro", path.to_string_lossy());
    let conn = Connection::open_with_flags(
        &uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|e| InvalidCandidate(format!("this file is not a valid backup ({e})")))?;

    let integrity: String = conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .map_err(|e| InvalidCandidate(format!("this file is not a valid backup ({e})")))?;
    if integrity != "ok" {
        return Err(InvalidCandidate(format!("this backup is damaged ({integrity})")));
    }

    let has_events: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='events'",
            [],
            |r| r.get(0),
        )
        .map_err(|e| InvalidCandidate(format!("this file is not a valid backup ({e})")))?;
    if has_events == 0 {
        return Err(InvalidCandidate("this file is not an accounting backup".into()));
    }

    let events: i64 = conn
        .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
        .map_err(|e| InvalidCandidate(format!("this backup cannot be read ({e})")))?;
    if events == 0 {
        return Err(InvalidCandidate("this backup contains no records".into()));
    }

    Ok(())
}

/// Replace `live` with `candidate` and delete the stale WAL sidecars.
///
/// The caller MUST have dropped every connection to `live` first.
///
/// Deleting `-wal` / `-shm` is mandatory, not tidiness: they belong to the file
/// being replaced. Left in place, SQLite may replay the OLD write-ahead log on
/// top of the NEW database and corrupt it.
pub fn swap_in_place(candidate: &Path, live: &Path) -> std::io::Result<()> {
    fs::copy(candidate, live)?;
    for suffix in ["-wal", "-shm"] {
        let sidecar = PathBuf::from(format!("{}{suffix}", live.to_string_lossy()));
        if sidecar.exists() {
            fs::remove_file(&sidecar)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use accounting_core::{apply_schema, append_event, Hlc};

    /// A unique temp directory, without adding a tempfile dependency.
    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("acct-backup-test-{tag}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn timestamp_is_utc_and_sortable() {
        // 2026-07-30T12:34:56Z = 1785414896 s
        assert_eq!(timestamp_utc(1_785_414_896_000), "20260730-123456");
        assert_eq!(timestamp_utc(0), "19700101-000000");
        assert!(timestamp_utc(1_000) < timestamp_utc(2_000_000_000_000), "must sort chronologically");
    }

    #[test]
    fn snapshot_names_carry_their_prefix() {
        assert_eq!(snapshot_name(AUTO_PREFIX, 0), "ledger-auto-19700101-000000.db");
        assert_eq!(snapshot_name(MANUAL_PREFIX, 0), "ledger-19700101-000000.db");
    }

    /// The core promise: a snapshot of a WAL database with committed-but-unmerged
    /// content is complete. A plain file copy is not.
    #[test]
    fn snapshot_of_a_wal_database_is_complete() {
        let dir = temp_dir("wal");
        let live = dir.join("ledger.db");

        let conn = Connection::open(&live).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        apply_schema(&conn).unwrap();
        let mut hlc = Hlc::new("devA");
        for i in 0..5 {
            append_event(&conn, &mut hlc, 1000 + i, "devA", "u", "ItemDefined",
                &serde_json::json!({"itemId": format!("i{i}")})).unwrap();
        }

        let dest = dir.join("snap.db");
        let bytes = snapshot_to(&conn, &dest).unwrap();
        assert!(bytes > 0, "snapshot should not be empty");

        // The snapshot must contain all 5 events even though the WAL was never
        // checkpointed, and must pass an integrity check on its own.
        let snap = Connection::open(&dest).unwrap();
        let n: i64 = snap.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 5, "WAL content must be included in the snapshot");
        let ok: String = snap.query_row("PRAGMA integrity_check", [], |r| r.get(0)).unwrap();
        assert_eq!(ok, "ok");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_refuses_to_overwrite() {
        let dir = temp_dir("overwrite");
        let conn = Connection::open(dir.join("ledger.db")).unwrap();
        apply_schema(&conn).unwrap();
        let dest = dir.join("snap.db");
        snapshot_to(&conn, &dest).unwrap();
        assert!(snapshot_to(&conn, &dest).is_err(), "VACUUM INTO must not clobber an existing file");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_keeps_the_newest_auto_backups_only() {
        let dir = temp_dir("prune");
        for stamp in ["20260101-000000", "20260102-000000", "20260103-000000", "20260104-000000"] {
            fs::write(dir.join(format!("{AUTO_PREFIX}{stamp}.db")), b"x").unwrap();
        }
        let removed = prune(&dir, AUTO_PREFIX, 3).unwrap();
        assert_eq!(removed.len(), 1);
        assert!(removed[0].to_string_lossy().contains("20260101"), "oldest must go first");
        assert!(dir.join(format!("{AUTO_PREFIX}20260104-000000.db")).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_never_touches_manual_backups() {
        let dir = temp_dir("prune-manual");
        for stamp in ["20260101-000000", "20260102-000000", "20260103-000000", "20260104-000000"] {
            fs::write(dir.join(format!("{MANUAL_PREFIX}{stamp}.db")), b"x").unwrap();
        }
        let removed = prune(&dir, AUTO_PREFIX, 3).unwrap();
        assert!(removed.is_empty(), "manual backups must never be pruned, removed {removed:?}");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 4);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_is_a_no_op_below_the_limit() {
        let dir = temp_dir("prune-few");
        fs::write(dir.join(format!("{AUTO_PREFIX}20260101-000000.db")), b"x").unwrap();
        assert!(prune(&dir, AUTO_PREFIX, 3).unwrap().is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_accepts_a_real_snapshot() {
        let dir = temp_dir("validate-ok");
        let conn = Connection::open(dir.join("ledger.db")).unwrap();
        apply_schema(&conn).unwrap();
        let mut hlc = Hlc::new("devA");
        append_event(&conn, &mut hlc, 1000, "devA", "u", "ItemDefined",
            &serde_json::json!({"itemId": "i1"})).unwrap();
        let snap = dir.join("snap.db");
        snapshot_to(&conn, &snap).unwrap();

        validate_candidate(&snap).expect("a real snapshot must validate");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_rejects_a_non_database() {
        let dir = temp_dir("validate-junk");
        let junk = dir.join("notes.db");
        fs::write(&junk, b"this is not a sqlite file").unwrap();
        assert!(validate_candidate(&junk).is_err(), "garbage must be rejected");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_rejects_a_database_with_no_events_table() {
        let dir = temp_dir("validate-noevents");
        let other = dir.join("other.db");
        let c = Connection::open(&other).unwrap();
        c.execute_batch("CREATE TABLE unrelated (x INTEGER)").unwrap();
        drop(c);
        assert!(validate_candidate(&other).is_err(), "a foreign database must be rejected");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_rejects_an_empty_event_log() {
        let dir = temp_dir("validate-empty");
        let empty = dir.join("empty.db");
        let c = Connection::open(&empty).unwrap();
        apply_schema(&c).unwrap();
        drop(c);
        assert!(validate_candidate(&empty).is_err(), "a snapshot with no events is not a ledger");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_rejects_a_missing_file() {
        let dir = temp_dir("validate-missing");
        assert!(validate_candidate(&dir.join("nope.db")).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn swap_replaces_the_file_and_removes_stale_wal_sidecars() {
        let dir = temp_dir("swap");
        let live = dir.join("ledger.db");
        let wal = dir.join("ledger.db-wal");
        let shm = dir.join("ledger.db-shm");

        // A live WAL database with 1 event, plus stale sidecars on disk.
        let conn = Connection::open(&live).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        apply_schema(&conn).unwrap();
        let mut hlc = Hlc::new("devA");
        append_event(&conn, &mut hlc, 1000, "devA", "u", "ItemDefined",
            &serde_json::json!({"itemId": "old"})).unwrap();

        // A candidate with 2 events.
        let candidate = dir.join("candidate.db");
        let c2 = Connection::open(&candidate).unwrap();
        apply_schema(&c2).unwrap();
        let mut hlc2 = Hlc::new("devB");
        for i in 0..2 {
            append_event(&c2, &mut hlc2, 1000 + i, "devB", "u", "ItemDefined",
                &serde_json::json!({"itemId": format!("new{i}")})).unwrap();
        }
        drop(c2);

        drop(conn); // release the live connection, as restore requires

        // A clean close checkpoints and removes the sidecars, so recreate them:
        // the case that matters is a crash or kill leaving a WAL behind, which
        // is exactly when SQLite would try to replay it onto the new file.
        fs::write(&wal, b"stale wal").unwrap();
        fs::write(&shm, b"stale shm").unwrap();
        assert!(wal.exists() && shm.exists(), "guard: sidecars must be present or this test proves nothing");

        swap_in_place(&candidate, &live).unwrap();

        assert!(!wal.exists(), "stale -wal must be deleted or SQLite may replay it onto the new file");
        assert!(!shm.exists(), "stale -shm must be deleted");

        let restored = Connection::open(&live).unwrap();
        let n: i64 = restored.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 2, "the live file must now be the candidate");
        let _ = fs::remove_dir_all(&dir);
    }
}
