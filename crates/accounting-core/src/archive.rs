//! Portable event-log archive: newline-delimited JSON.
//!
//! Line 1 is a header (format marker, version, provenance, a copy of
//! `app_settings`); every following line is one event in HLC order. The format
//! marker and version exist so a future change is *detected and rejected*
//! rather than silently misparsed.
//!
//! The archive is the whole history, not a diff: the event log is append-only,
//! so a later export is a strict superset of an earlier one.

use crate::events::{insert_raw_event, read_events, LedgerEvent};
use crate::settings::get_settings;
use rusqlite::{Connection, OptionalExtension};
use std::collections::HashMap;
use std::io::{BufRead, Write};

/// Format marker written to, and required by, every archive header.
pub const ARCHIVE_FORMAT: &str = "accounting-eventlog";
/// Current archive version. Bump only on a breaking layout change.
pub const ARCHIVE_VERSION: u32 = 1;

#[derive(Debug)]
pub enum ArchiveError {
    /// The file is not a recognised archive, or its version is unsupported.
    Format(String),
    /// A line could not be parsed as an event.
    Parse { line: usize, message: String },
    /// An incoming event's `(device_id, seq)` is taken locally by a different
    /// event id. Unmergeable — both logs authored under the same identity.
    Collision { device_id: String, seq: i64 },
    /// The merged ledger failed reconciliation.
    ///
    /// The events are **still committed** — the check necessarily runs after the
    /// commit, because `rebuild` and `run_all_checks` can only see the merged log
    /// once it is in the table. Recovery is the caller's safety copy, not a
    /// rollback; the caller must say so when it reports this.
    Reconciliation(String),
    Db(rusqlite::Error),
    Io(String),
}

impl std::fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArchiveError::Format(m) => write!(f, "unrecognised backup file: {m}"),
            ArchiveError::Parse { line, message } => {
                write!(f, "backup file is damaged at line {line}: {message}")
            }
            ArchiveError::Collision { device_id, seq } => write!(
                f,
                "cannot merge: this backup was made by an older installation that shares \
                 an identity with yours (device {device_id}, entry {seq}). Restore a full \
                 database backup instead."
            ),
            ArchiveError::Reconciliation(m) => {
                write!(
                    f,
                    "the combined ledger did not balance after the merge ({m}). The imported \
                     entries were kept, so restore the safety copy made before the import."
                )
            }
            ArchiveError::Db(e) => write!(f, "database error: {e}"),
            ArchiveError::Io(m) => write!(f, "could not read or write the file: {m}"),
        }
    }
}

impl std::error::Error for ArchiveError {}

impl From<rusqlite::Error> for ArchiveError {
    fn from(e: rusqlite::Error) -> Self {
        ArchiveError::Db(e)
    }
}

impl From<std::io::Error> for ArchiveError {
    fn from(e: std::io::Error) -> Self {
        ArchiveError::Io(e.to_string())
    }
}

/// What an import actually did. Shown to the user verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportSummary {
    /// Events new to this database and inserted.
    pub inserted: usize,
    /// Events already present (matched by id) and skipped.
    pub skipped_duplicates: usize,
    /// Total event lines read from the archive.
    pub total_events: usize,
}

/// Write the full event log plus a provenance header to `out`.
///
/// Returns the number of event lines written (excludes the header).
pub fn export_jsonl(
    conn: &Connection,
    out: &mut impl Write,
    exported_at: i64,
    app_version: &str,
) -> Result<usize, ArchiveError> {
    let settings: HashMap<String, String> = get_settings(conn)?;
    let device_id = settings.get("device_id").cloned().unwrap_or_default();

    let header = serde_json::json!({
        "format": ARCHIVE_FORMAT,
        "version": ARCHIVE_VERSION,
        "exported_at": exported_at,
        "app_version": app_version,
        "device_id": device_id,
        "settings": settings,
    });
    writeln!(out, "{header}")?;

    // read_events returns HLC order and decodes jsonb payloads to real JSON.
    let events = read_events(conn)?;
    for ev in &events {
        let line = serde_json::json!({
            "id": ev.id,
            "hlc": ev.hlc,
            "device_id": ev.device_id,
            "user_id": ev.user_id,
            "seq": ev.seq,
            "type": ev.event_type,
            "payload": ev.payload,
            "created_at": ev.created_at,
        });
        writeln!(out, "{line}")?;
    }
    out.flush()?;
    Ok(events.len())
}

/// The first line of an archive.
#[derive(Debug, Clone)]
pub struct ArchiveHeader {
    pub version: u32,
    pub exported_at: i64,
    pub device_id: String,
    /// The exporting install's `app_settings`. Archival only — merge import
    /// never applies these (see `import_jsonl`).
    pub settings: HashMap<String, String>,
}

/// Parse and validate the header line.
///
/// Rejects anything without our format marker, and any version this build does
/// not understand — better a clear refusal than a silent misread of a newer file.
pub fn parse_header(line: &str) -> Result<ArchiveHeader, ArchiveError> {
    let v: serde_json::Value = serde_json::from_str(line)
        .map_err(|e| ArchiveError::Format(format!("the first line is not valid JSON ({e})")))?;

    match v.get("format").and_then(|f| f.as_str()) {
        Some(ARCHIVE_FORMAT) => {}
        Some(other) => {
            return Err(ArchiveError::Format(format!(
                "expected an {ARCHIVE_FORMAT} file but found '{other}'"
            )))
        }
        None => return Err(ArchiveError::Format("no format marker on the first line".into())),
    }

    let version = v.get("version").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
    if version != ARCHIVE_VERSION {
        return Err(ArchiveError::Format(format!(
            "this file is version {version} but this app understands version {ARCHIVE_VERSION}"
        )));
    }

    let settings = v
        .get("settings")
        .and_then(|s| s.as_object())
        .map(|m| {
            m.iter()
                .filter_map(|(k, val)| val.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();

    Ok(ArchiveHeader {
        version,
        exported_at: v.get("exported_at").and_then(|x| x.as_i64()).unwrap_or(0),
        device_id: v.get("device_id").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
        settings,
    })
}

/// Parse one event line. `line_no` is 1-based and only used for error reporting.
pub fn parse_event(line: &str, line_no: usize) -> Result<LedgerEvent, ArchiveError> {
    let v: serde_json::Value = serde_json::from_str(line)
        .map_err(|e| ArchiveError::Parse { line: line_no, message: e.to_string() })?;

    fn text(v: &serde_json::Value, key: &str, line_no: usize) -> Result<String, ArchiveError> {
        v.get(key)
            .and_then(|x| x.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| ArchiveError::Parse {
                line: line_no,
                message: format!("missing text field '{key}'"),
            })
    }
    fn number(v: &serde_json::Value, key: &str, line_no: usize) -> Result<i64, ArchiveError> {
        v.get(key).and_then(|x| x.as_i64()).ok_or_else(|| ArchiveError::Parse {
            line: line_no,
            message: format!("missing numeric field '{key}'"),
        })
    }

    Ok(LedgerEvent {
        id: text(&v, "id", line_no)?,
        hlc: text(&v, "hlc", line_no)?,
        device_id: text(&v, "device_id", line_no)?,
        user_id: text(&v, "user_id", line_no)?,
        seq: number(&v, "seq", line_no)?,
        event_type: text(&v, "type", line_no)?,
        payload: v.get("payload").cloned().unwrap_or(serde_json::Value::Null),
        created_at: number(&v, "created_at", line_no)?,
    })
}

/// Merge an archive into this database, keyed by event id.
///
/// Events already present (same id) are skipped; the rest are inserted verbatim.
/// All inserts happen in ONE transaction, so a damaged line or an identity
/// collision leaves the log exactly as it was.
///
/// The replay and reconciliation that follow run AFTER that transaction commits,
/// so they cannot roll it back: if `rebuild` or a balance check fails, this
/// returns `Err` with the merged events already persisted, and the database is
/// left with stale or partial projections. Callers must therefore take a
/// snapshot before calling and restore it on error — an `Err` from here does not
/// mean "nothing happened".
///
/// Deliberately does NOT touch `app_settings`. Merging someone else's log must
/// not silently change your currency or locale, and overwriting `device_id`
/// would destroy this install's identity. Whole-install recovery (settings
/// included) is what a database snapshot restore is for.
pub fn import_jsonl(
    conn: &mut Connection,
    reader: impl BufRead,
) -> Result<ImportSummary, ArchiveError> {
    let mut lines = reader.lines();

    let header_line = lines
        .next()
        .transpose()?
        .ok_or_else(|| ArchiveError::Format("the file is empty".into()))?;
    let _header = parse_header(&header_line)?;

    // Parse everything BEFORE opening the transaction: a damaged file should
    // never reach the database at all.
    let mut events: Vec<LedgerEvent> = Vec::new();
    for (idx, line) in lines.enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue; // tolerate trailing newlines
        }
        events.push(parse_event(&line, idx + 2)?); // +2: 1-based, header is line 1
    }
    let total_events = events.len();

    let tx = conn.transaction()?;
    let mut inserted = 0usize;
    let mut skipped_duplicates = 0usize;

    for ev in &events {
        let existing_id: Option<String> = tx
            .query_row("SELECT id FROM events WHERE id = ?1", [&ev.id], |r| r.get(0))
            .optional()?;
        if existing_id.is_some() {
            skipped_duplicates += 1;
            continue;
        }

        // Same (device_id, seq) under a DIFFERENT id means two installs shared an
        // identity. UNIQUE (device_id, seq) would reject it anyway; fail with an
        // explanation instead of a raw constraint error.
        let clashing: Option<String> = tx
            .query_row(
                "SELECT id FROM events WHERE device_id = ?1 AND seq = ?2",
                rusqlite::params![ev.device_id, ev.seq],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(other) = clashing {
            if other != ev.id {
                return Err(ArchiveError::Collision {
                    device_id: ev.device_id.clone(),
                    seq: ev.seq,
                });
            }
        }

        insert_raw_event(&tx, ev)?;
        inserted += 1;
    }

    tx.commit()?;

    // Projections are derived state: replay the merged log in HLC order.
    crate::projectors::rebuild(conn)?;

    // Report whether the combined ledger still balances. This cannot gate the
    // commit: the checks read the projections, which only exist once the merged
    // events are committed and replayed. So a failure here is a loud warning about
    // an already-applied merge, not a veto — which is exactly why the caller takes
    // a safety copy first.
    let checks = crate::reconciliation::run_all_checks(conn)?;
    if !crate::reconciliation::all_passed(&checks) {
        let failed: Vec<String> = checks
            .iter()
            .filter(|c| !matches!(c.outcome, crate::reconciliation::CheckOutcome::Pass))
            .map(|c| c.name.to_string())
            .collect();
        return Err(ArchiveError::Reconciliation(failed.join(", ")));
    }

    Ok(ImportSummary { inserted, skipped_duplicates, total_events })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory_with_schema;
    use crate::events::append_event;
    use crate::genesis::run_genesis;
    use crate::hlc::{rehydrate_from_log, Hlc};
    use crate::projectors::rebuild;
    use crate::reconciliation::{all_passed, run_all_checks};
    use crate::settings::{ensure_device_id, set_setting};
    use serde_json::json;

    /// A database with a known identity, one setting, and two events.
    fn seeded() -> Connection {
        let conn = open_in_memory_with_schema().unwrap();
        ensure_device_id(&conn).unwrap();
        set_setting(&conn, "currency_symbol", "€").unwrap();
        let mut hlc = Hlc::new("devA");
        append_event(&conn, &mut hlc, 1000, "devA", "u", "ItemDefined", &json!({"itemId": "i1"}))
            .unwrap();
        append_event(&conn, &mut hlc, 2000, "devA", "u", "ItemDefined", &json!({"itemId": "i2"}))
            .unwrap();
        conn
    }

    fn export_to_string(conn: &Connection) -> String {
        let mut buf: Vec<u8> = Vec::new();
        export_jsonl(conn, &mut buf, 555, "0.1.2").unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn export_writes_header_then_one_line_per_event() {
        let conn = seeded();
        let text = export_to_string(&conn);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "1 header + 2 events");

        let header: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(header["format"], ARCHIVE_FORMAT);
        assert_eq!(header["version"], 1);
        assert_eq!(header["exported_at"], 555);
        assert_eq!(header["app_version"], "0.1.2");
        assert_eq!(header["settings"]["currency_symbol"], "€");
    }

    #[test]
    fn export_emits_payloads_as_real_json_not_strings() {
        let conn = seeded();
        let text = export_to_string(&conn);
        let ev: serde_json::Value = serde_json::from_str(text.lines().nth(1).unwrap()).unwrap();
        assert!(ev["payload"].is_object(), "payload must be an object, got {:?}", ev["payload"]);
        assert_eq!(ev["payload"]["itemId"], "i1");
    }

    #[test]
    fn export_preserves_hlc_order() {
        let conn = seeded();
        let text = export_to_string(&conn);
        let first: serde_json::Value = serde_json::from_str(text.lines().nth(1).unwrap()).unwrap();
        let second: serde_json::Value = serde_json::from_str(text.lines().nth(2).unwrap()).unwrap();
        assert!(
            first["hlc"].as_str().unwrap() < second["hlc"].as_str().unwrap(),
            "events must be exported in ascending HLC order"
        );
    }

    #[test]
    fn export_of_empty_log_writes_header_only() {
        let conn = open_in_memory_with_schema().unwrap();
        let mut buf: Vec<u8> = Vec::new();
        let n = export_jsonl(&conn, &mut buf, 1, "0.1.2").unwrap();
        assert_eq!(n, 0);
        assert_eq!(String::from_utf8(buf).unwrap().lines().count(), 1);
    }

    #[test]
    fn parse_header_accepts_a_valid_header() {
        let line = r#"{"format":"accounting-eventlog","version":1,"settings":{"locale":"fr"}}"#;
        let h = parse_header(line).unwrap();
        assert_eq!(h.version, 1);
        assert_eq!(h.settings.get("locale").map(String::as_str), Some("fr"));
    }

    #[test]
    fn parse_header_rejects_a_foreign_format() {
        let err = parse_header(r#"{"format":"something-else","version":1}"#).unwrap_err();
        assert!(matches!(err, ArchiveError::Format(_)), "got {err:?}");
    }

    #[test]
    fn parse_header_rejects_a_future_version() {
        let err = parse_header(r#"{"format":"accounting-eventlog","version":99}"#).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("99"), "error should name the unsupported version: {msg}");
    }

    #[test]
    fn parse_header_rejects_non_json() {
        let err = parse_header("this is not json").unwrap_err();
        assert!(matches!(err, ArchiveError::Format(_)), "got {err:?}");
    }

    #[test]
    fn parse_event_reads_all_fields() {
        let line = r#"{"id":"h1","hlc":"h1","device_id":"d","user_id":"u","seq":3,"type":"A","payload":{"x":1},"created_at":9}"#;
        let ev = parse_event(line, 2).unwrap();
        assert_eq!(ev.id, "h1");
        assert_eq!(ev.seq, 3);
        assert_eq!(ev.event_type, "A");
        assert_eq!(ev.payload["x"], 1);
        assert_eq!(ev.created_at, 9);
    }

    #[test]
    fn parse_event_reports_the_line_number_on_damage() {
        let err = parse_event(r#"{"id":"h1","seq":"not-a-number"}"#, 7).unwrap_err();
        match err {
            ArchiveError::Parse { line, .. } => assert_eq!(line, 7),
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    // ---- import ----

    /// A founded business under `device`, plus one `ItemDefined` per entry in
    /// `items`, projections rebuilt.
    ///
    /// Genesis is essential, not decoration: `check_inventory_valuation` reads
    /// the `inventory` account with a bare `query_row`, so `run_all_checks` on a
    /// log without a chart of accounts fails with `QueryReturnedNoRows` rather
    /// than a `Fail` outcome. Every import into such a log would error.
    fn seeded_as(device: &str, items: &[&str]) -> Connection {
        let mut conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new(device);
        run_genesis(&conn, &mut hlc, 1000, device, "owner-1", "Jane Owner").unwrap();
        let mut physical = 2000;
        for it in items {
            append_event(&conn, &mut hlc, physical, device, "u", "ItemDefined",
                &json!({"itemId": it, "sku": it, "name": it, "unit": "ea"}))
                .unwrap();
            physical += 1000;
        }
        rebuild(&mut conn).unwrap();
        conn
    }

    /// Copy `source`'s log verbatim into a fresh database, then append `items`
    /// under `device` — a second install of the *same* business that has since
    /// diverged. Two independent `run_genesis` runs cannot be merged at all:
    /// replaying both would open two `inventory` accounts and violate the
    /// unique index on `accounts.system_role`.
    ///
    /// Copies with `insert_raw_event` rather than `import_jsonl` so the fixture
    /// does not depend on the function under test.
    fn branch_of(source: &Connection, device: &str, items: &[&str]) -> Connection {
        let mut conn = open_in_memory_with_schema().unwrap();
        for ev in read_events(source).unwrap() {
            insert_raw_event(&conn, &ev).unwrap();
        }
        let mut hlc = Hlc::new(device);
        rehydrate_from_log(&conn, &mut hlc, 5000).unwrap();
        let mut physical = 5000;
        for it in items {
            append_event(&conn, &mut hlc, physical, device, "u", "ItemDefined",
                &json!({"itemId": it, "sku": it, "name": it, "unit": "ea"}))
                .unwrap();
            physical += 1000;
        }
        rebuild(&mut conn).unwrap();
        conn
    }

    fn export_string(conn: &Connection) -> String {
        let mut buf: Vec<u8> = Vec::new();
        export_jsonl(conn, &mut buf, 1, "0.1.2").unwrap();
        String::from_utf8(buf).unwrap()
    }

    fn event_count(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn import_into_empty_db_reproduces_identity_exactly() {
        let source = seeded_as("devA", &["i1", "i2"]);
        let text = export_string(&source);
        let src_events = read_events(&source).unwrap();

        let mut target = open_in_memory_with_schema().unwrap();
        let summary = import_jsonl(&mut target, text.as_bytes()).unwrap();
        assert_eq!(summary.inserted, src_events.len());
        assert_eq!(summary.skipped_duplicates, 0);
        assert_eq!(summary.total_events, src_events.len());

        let got_events = read_events(&target).unwrap();
        assert_eq!(got_events.len(), src_events.len());
        for (a, b) in src_events.iter().zip(got_events.iter()) {
            assert_eq!(a.id, b.id, "event id must survive the round trip");
            assert_eq!(a.hlc, b.hlc);
            assert_eq!(a.device_id, b.device_id);
            assert_eq!(a.seq, b.seq);
            assert_eq!(a.payload, b.payload);
        }
    }

    #[test]
    fn import_rebuilds_projections() {
        let source = seeded_as("devA", &["i1", "i2"]);
        let text = export_string(&source);
        let mut target = open_in_memory_with_schema().unwrap();
        import_jsonl(&mut target, text.as_bytes()).unwrap();

        let n: i64 = target.query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 2, "projections must be rebuilt from the merged log");
        assert!(all_passed(&run_all_checks(&target).unwrap()));
    }

    #[test]
    fn reimporting_the_same_archive_is_a_no_op() {
        let source = seeded_as("devA", &["i1", "i2"]);
        let text = export_string(&source);
        let total = read_events(&source).unwrap().len();
        let mut target = open_in_memory_with_schema().unwrap();
        import_jsonl(&mut target, text.as_bytes()).unwrap();

        let second = import_jsonl(&mut target, text.as_bytes()).unwrap();
        assert_eq!(second.inserted, 0, "nothing new to insert");
        assert_eq!(second.skipped_duplicates, total);
        assert_eq!(event_count(&target), total as i64, "the log must not grow on re-import");
    }

    #[test]
    fn merging_two_disjoint_logs_keeps_both() {
        // One founded business, two installs that each recorded a different item.
        let base = seeded_as("devBase", &[]);
        let shared = read_events(&base).unwrap().len();
        let a = branch_of(&base, "devA", &["i1"]);
        let b = branch_of(&base, "devB", &["i2"]);

        let mut target = open_in_memory_with_schema().unwrap();
        import_jsonl(&mut target, export_string(&a).as_bytes()).unwrap();
        let summary = import_jsonl(&mut target, export_string(&b).as_bytes()).unwrap();

        assert_eq!(summary.inserted, 1, "only devB's own event is new");
        assert_eq!(summary.skipped_duplicates, shared, "the shared genesis is already present");
        assert_eq!(event_count(&target), shared as i64 + 2, "both branches must be present");

        let events = read_events(&target).unwrap();
        let hlcs: Vec<&str> = events.iter().map(|e| e.hlc.as_str()).collect();
        let mut sorted = hlcs.clone();
        sorted.sort_unstable();
        assert_eq!(hlcs, sorted, "merged log must read back in HLC order");

        let items: i64 =
            target.query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0)).unwrap();
        assert_eq!(items, 2, "both installs' items must survive the merge");
        assert!(all_passed(&run_all_checks(&target).unwrap()));
    }

    #[test]
    fn colliding_device_seq_aborts_and_changes_nothing() {
        let a = seeded_as("devA", &["i1"]);
        let mut target = open_in_memory_with_schema().unwrap();
        import_jsonl(&mut target, export_string(&a).as_bytes()).unwrap();
        let before = event_count(&target);

        // A rival install that also believes it is devA — the pre-UUID case. It
        // authors at a later physical time, so its stamps (and therefore its
        // event ids) differ while its `seq` values restart at 1 and collide.
        let rival = open_in_memory_with_schema().unwrap();
        let mut rival_hlc = Hlc::new("devA");
        append_event(&rival, &mut rival_hlc, 9000, "devA", "u", "ItemDefined",
            &json!({"itemId": "i9", "sku": "i9", "name": "i9", "unit": "ea"}))
            .unwrap();

        let err = import_jsonl(&mut target, export_string(&rival).as_bytes()).unwrap_err();
        assert!(matches!(err, ArchiveError::Collision { .. }), "got {err:?}");
        assert_eq!(before, event_count(&target), "a rejected import must leave the log untouched");
    }

    #[test]
    fn import_never_overwrites_local_settings() {
        let source = seeded_as("devA", &["i1"]);
        set_setting(&source, "currency_symbol", "$").unwrap();
        set_setting(&source, "locale", "en").unwrap();
        let text = export_string(&source);

        let mut target = open_in_memory_with_schema().unwrap();
        let local_id = ensure_device_id(&target).unwrap();
        set_setting(&target, "currency_symbol", "€").unwrap();
        import_jsonl(&mut target, text.as_bytes()).unwrap();

        let s = get_settings(&target).unwrap();
        assert_eq!(s.get("currency_symbol").map(String::as_str), Some("€"), "must keep local currency");
        assert_eq!(s.get("device_id"), Some(&local_id), "device identity must never be replaced");
        assert_eq!(s.get("locale"), None, "must not import the source's locale");
    }

    #[test]
    fn import_rejects_a_damaged_line_without_partial_writes() {
        let source = seeded_as("devA", &["i1", "i2"]);
        let text = export_string(&source);
        let mut lines: Vec<&str> = text.lines().collect();
        lines[2] = "{ this line is broken";
        let damaged = lines.join("\n");

        let mut target = open_in_memory_with_schema().unwrap();
        let err = import_jsonl(&mut target, damaged.as_bytes()).unwrap_err();
        assert!(matches!(err, ArchiveError::Parse { .. }), "got {err:?}");
        assert_eq!(event_count(&target), 0, "a damaged archive must insert nothing at all");
    }

    #[test]
    fn import_rejects_an_empty_file() {
        let mut target = open_in_memory_with_schema().unwrap();
        let err = import_jsonl(&mut target, &b""[..]).unwrap_err();
        assert!(matches!(err, ArchiveError::Format(_)), "got {err:?}");
    }

    #[test]
    fn import_tolerates_a_trailing_blank_line() {
        let source = seeded_as("devA", &["i1"]);
        let total = read_events(&source).unwrap().len();
        let text = format!("{}\n\n", export_string(&source).trim_end());
        let mut target = open_in_memory_with_schema().unwrap();
        let summary = import_jsonl(&mut target, text.as_bytes()).unwrap();
        assert_eq!(summary.inserted, total);
    }
}
