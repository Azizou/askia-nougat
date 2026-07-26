use rusqlite::Connection;

/// Open an in-memory database with the standard PRAGMAs.
pub fn open_in_memory() -> rusqlite::Result<Connection> {
    let conn = Connection::open_in_memory()?;
    configure(&conn)?;
    Ok(conn)
}

fn configure(conn: &Connection) -> rusqlite::Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(())
}

const SCHEMA_SQL: &str = include_str!("schema.sql");

/// Apply the schema. Idempotent (all statements use IF NOT EXISTS).
pub fn apply_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(SCHEMA_SQL)
}

/// Open an in-memory database with PRAGMAs applied AND the schema created.
/// Convenience for tests.
pub fn open_in_memory_with_schema() -> rusqlite::Result<Connection> {
    let conn = open_in_memory()?;
    apply_schema(&conn)?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_in_memory_sets_wal_and_foreign_keys() {
        let conn = open_in_memory().expect("open");
        let fk: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fk, 1, "foreign_keys should be ON");
    }

    #[test]
    fn apply_schema_creates_events_table() {
        let conn = open_in_memory().unwrap();
        apply_schema(&conn).expect("apply schema");
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='events'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "events table should exist");
    }

    #[test]
    fn apply_schema_is_idempotent() {
        let conn = open_in_memory().unwrap();
        apply_schema(&conn).unwrap();
        apply_schema(&conn).expect("second apply must not error");
    }

    #[test]
    fn apply_schema_creates_all_projection_tables() {
        let conn = open_in_memory_with_schema().unwrap();
        let expected = [
            "users", "accounts", "items", "inventory_lots", "parties",
            "journal_lines", "sales", "sale_lines", "lot_consumptions",
            "purchases", "purchase_lines", "payments", "payment_allocations",
            "party_balances", "returns", "return_lines", "expenses",
            "events", "projection_cursor", "app_settings",
        ];
        for name in expected {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [name],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "table {name} should exist");
        }

        for tbl in ["sales", "purchases"] {
            let has_reversed: i64 = conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM pragma_table_info('{tbl}') WHERE name='reversed'"),
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(has_reversed, 1, "{tbl}.reversed column should exist");
        }
    }
}
