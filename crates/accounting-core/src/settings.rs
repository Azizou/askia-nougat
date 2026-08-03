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
    // NOTE: "device_id" is deliberately absent. It is written once by
    // ensure_device_id and must never be writable through this generic path,
    // which the frontend can drive with an arbitrary key. get_settings ignores
    // this allowlist, so the UI can still read it.
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
///
/// Write-once by construction: the insert is `DO NOTHING` and the value is read
/// back afterwards, so this can only ever *adopt* a stored id, never replace one.
/// That also makes two racing callers converge on the same id instead of one
/// returning an identity the database no longer records. Deliberately bypasses
/// `set_setting`, since `device_id` is not — and must not be — allowlisted.
///
/// One deliberate exception exists: `remint_device_id`, called after restoring a
/// snapshot taken by a *different* install, whose `app_settings` would otherwise
/// hand this install that install's identity.
pub fn ensure_device_id(conn: &Connection) -> rusqlite::Result<String> {
    conn.execute(
        "INSERT INTO app_settings (key, value) VALUES ('device_id', ?1)
         ON CONFLICT(key) DO NOTHING",
        [uuid::Uuid::new_v4().to_string()],
    )?;
    conn.query_row("SELECT value FROM app_settings WHERE key = 'device_id'", [], |r| r.get(0))
}

/// Replace this install's `device_id` with a freshly minted one.
///
/// The single legitimate exception to the write-once rule above, and it exists
/// because a snapshot restore copies the *whole* database file — `app_settings`
/// included. Restoring another install's backup would otherwise make this install
/// author under that install's UUID, so both would mint byte-identical event ids
/// for different events and collide on `(device_id, seq)`: exactly the
/// unmergeable state per-install identity was introduced to prevent.
///
/// Call this only when the restored identity is known to belong to a different
/// install. Restoring this install's own backup carries its own id back, and that
/// continuity is worth keeping — `device_id` is write-once, so any snapshot of
/// this install necessarily holds this install's id.
///
/// Events already in the log keep the device id they were authored under. That is
/// correct: their ids are historical facts, and only newly authored events need to
/// come from an identity nobody else holds.
pub fn remint_device_id(conn: &Connection) -> rusqlite::Result<String> {
    let fresh = uuid::Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO app_settings (key, value) VALUES ('device_id', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [&fresh],
    )?;
    Ok(fresh)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory_with_schema;

    #[test]
    fn remint_replaces_a_restored_foreign_identity() {
        let conn = open_in_memory_with_schema().unwrap();
        let original = ensure_device_id(&conn).unwrap();

        let fresh = remint_device_id(&conn).unwrap();
        assert_ne!(fresh, original, "reminting must produce a different id");
        assert_eq!(
            uuid::Uuid::parse_str(&fresh).unwrap().get_version_num(),
            4,
            "the replacement must be a real UUID v4, like the original"
        );

        // The stored value must be the new one, and ensure_device_id must now
        // adopt it rather than mint again.
        assert_eq!(ensure_device_id(&conn).unwrap(), fresh);
    }

    #[test]
    fn remint_still_cannot_be_reached_through_set_setting() {
        // remint_device_id is deliberately a separate function: adding device_id
        // to SETTING_KEYS would reopen the IPC hole that b0d5cae closed.
        let conn = open_in_memory_with_schema().unwrap();
        ensure_device_id(&conn).unwrap();
        assert!(
            set_setting(&conn, "device_id", "attacker-supplied").is_err(),
            "device_id must stay unwritable through the generic settings path"
        );
    }

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
        // Parse rather than check the length: a 36-char string is not necessarily
        // a UUID, and a malformed device id silently corrupts event identity.
        let parsed = uuid::Uuid::parse_str(&id).expect("device id must be a valid UUID");
        assert_eq!(parsed.get_version_num(), 4, "device id must be a UUID v4, got {id:?}");
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
    fn device_id_cannot_be_written_through_set_setting() {
        // set_setting is reachable from the frontend with an arbitrary key
        // (tauri-app/src/commands.rs), and it upserts. If device_id were
        // allowlisted, any caller could permanently overwrite this install's
        // identity, orphaning every event it has already authored.
        let conn = open_in_memory_with_schema().unwrap();
        let minted = ensure_device_id(&conn).unwrap();
        assert!(
            set_setting(&conn, "device_id", "hijacked").is_err(),
            "device_id must not be writable through the generic settings path"
        );
        assert_eq!(ensure_device_id(&conn).unwrap(), minted, "identity must be unchanged");
    }

    #[test]
    fn ensure_device_id_reuses_a_preexisting_row() {
        let conn = open_in_memory_with_schema().unwrap();
        conn.execute(
            "INSERT INTO app_settings (key, value) VALUES ('device_id', 'preexisting')",
            [],
        )
        .unwrap();
        assert_eq!(
            ensure_device_id(&conn).unwrap(),
            "preexisting",
            "must adopt the stored id, never mint over it"
        );
    }

    #[test]
    fn distinct_installs_mint_distinct_ids() {
        // The property the whole design rests on: uniqueness across installs.
        // Idempotence alone would also hold for a hardcoded constant.
        let a = open_in_memory_with_schema().unwrap();
        let b = open_in_memory_with_schema().unwrap();
        assert_ne!(
            ensure_device_id(&a).unwrap(),
            ensure_device_id(&b).unwrap(),
            "two installs must not share an identity, or their event ids collide"
        );
    }

    #[test]
    fn device_id_survives_a_projection_rebuild() {
        // Identity lives in app_settings precisely because that table is absent
        // from PROJECTION_TABLES, while rebuild() runs on every startup. That
        // coupling spans two files; this pins it. Adding "app_settings" to
        // PROJECTION_TABLES would wipe every install's identity.
        let mut conn = open_in_memory_with_schema().unwrap();
        let minted = ensure_device_id(&conn).unwrap();
        crate::projectors::rebuild(&mut conn).unwrap();
        assert_eq!(
            ensure_device_id(&conn).unwrap(),
            minted,
            "rebuild must not clear app_settings"
        );
    }
}
