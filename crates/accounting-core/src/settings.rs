use rusqlite::{Connection, OptionalExtension};
use std::collections::HashMap;

/// Allowed settings keys. `set_setting` rejects anything not listed here so a
/// buggy or malicious frontend cannot write arbitrary rows.
pub const SETTING_KEYS: &[&str] = &[
    "currency_symbol",
    "currency_code",
    "currency_decimals",
    "theme",
    "locale",
    "font_size",
    // Stable identity of this install. Authored into every event's HLC, so it
    // must never change once minted. See ensure_device_id.
    "device_id",
    // Folder remembered from the last manual backup; auto-backup on close
    // writes here. Absent means "no automatic backups yet".
    "backup_folder",
    // Unix ms of the last successful backup, shown in Preferences.
    "last_backup_at",
];

/// Read every stored setting as a key→value map. Absent keys are simply not
/// present; callers apply their own defaults.
pub fn get_settings(conn: &Connection) -> rusqlite::Result<HashMap<String, String>> {
    let mut stmt = conn.prepare("SELECT key, value FROM app_settings")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut out = HashMap::new();
    for row in rows {
        let (k, v) = row?;
        out.insert(k, v);
    }
    Ok(out)
}

/// Upsert one setting. Returns an error if `key` is not in `SETTING_KEYS`.
pub fn set_setting(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    if !SETTING_KEYS.contains(&key) {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
            Some(format!("unknown setting key: {key}")),
        ));
    }
    conn.execute(
        "INSERT INTO app_settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![key, value],
    )?;
    Ok(())
}

/// Return this install's stable device id, minting and persisting one on first
/// call. The id is embedded in every event's HLC (and therefore in every event
/// id), so it must be stable for the life of the install and unique across
/// installs — otherwise two installs mint colliding event ids and clashing
/// `(device_id, seq)` pairs, and logs cannot be merged.
pub fn ensure_device_id(conn: &Connection) -> rusqlite::Result<String> {
    let existing: Option<String> = conn
        .query_row("SELECT value FROM app_settings WHERE key = 'device_id'", [], |r| r.get(0))
        .optional()?;
    if let Some(id) = existing {
        return Ok(id);
    }
    let id = uuid::Uuid::new_v4().to_string();
    set_setting(conn, "device_id", &id)?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory_with_schema;

    #[test]
    fn set_then_get_round_trips() {
        let conn = open_in_memory_with_schema().unwrap();
        set_setting(&conn, "currency_symbol", "€").unwrap();
        let s = get_settings(&conn).unwrap();
        assert_eq!(s.get("currency_symbol").map(String::as_str), Some("€"));
    }

    #[test]
    fn set_overwrites_existing_value() {
        let conn = open_in_memory_with_schema().unwrap();
        set_setting(&conn, "theme", "light").unwrap();
        set_setting(&conn, "theme", "midnight").unwrap();
        let s = get_settings(&conn).unwrap();
        assert_eq!(s.get("theme").map(String::as_str), Some("midnight"));
    }

    #[test]
    fn unknown_key_is_rejected_and_writes_nothing() {
        let conn = open_in_memory_with_schema().unwrap();
        let err = set_setting(&conn, "evil_key", "x");
        assert!(err.is_err());
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM app_settings", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn get_on_empty_returns_empty_map() {
        let conn = open_in_memory_with_schema().unwrap();
        assert!(get_settings(&conn).unwrap().is_empty());
    }

    #[test]
    fn ensure_device_id_mints_and_persists() {
        let conn = open_in_memory_with_schema().unwrap();
        let id = ensure_device_id(&conn).unwrap();
        assert_eq!(id.len(), 36, "expected a hyphenated UUID v4, got {id:?}");
        let stored = get_settings(&conn).unwrap();
        assert_eq!(stored.get("device_id"), Some(&id));
    }

    #[test]
    fn ensure_device_id_is_idempotent() {
        let conn = open_in_memory_with_schema().unwrap();
        let first = ensure_device_id(&conn).unwrap();
        let second = ensure_device_id(&conn).unwrap();
        assert_eq!(first, second, "must not mint a second identity");
    }

    #[test]
    fn device_id_is_an_allowed_key() {
        let conn = open_in_memory_with_schema().unwrap();
        set_setting(&conn, "device_id", "abc").expect("device_id must be allowlisted");
        assert_eq!(ensure_device_id(&conn).unwrap(), "abc", "must reuse the stored value");
    }
}
