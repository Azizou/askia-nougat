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
    /// The merged ledger failed reconciliation; the import was rolled back.
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
                write!(f, "import was cancelled because the combined ledger did not balance: {m}")
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory_with_schema;
    use crate::events::append_event;
    use crate::hlc::Hlc;
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
}
