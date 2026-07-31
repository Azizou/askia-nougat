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
}
