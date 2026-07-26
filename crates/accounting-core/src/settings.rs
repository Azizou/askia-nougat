use rusqlite::Connection;
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
}
