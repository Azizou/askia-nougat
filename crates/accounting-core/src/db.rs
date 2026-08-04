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

/// Bring an already-created schema up to date.
///
/// `apply_schema` alone cannot do this: every statement in `schema.sql` is
/// `CREATE ... IF NOT EXISTS`, which does nothing to a table that already
/// exists, and `rebuild` clears projections with `DELETE FROM` rather than
/// dropping them. So a column added to `schema.sql` reaches fresh installs
/// only — an upgraded install would keep the old table shape and fail every
/// query that names the new column.
fn migrate_schema(conn: &Connection) -> rusqlite::Result<()> {
    if !has_column(conn, "parties", "active")? {
        conn.execute_batch(
            "ALTER TABLE parties
               ADD COLUMN active INTEGER GENERATED ALWAYS AS (doc ->> 'active') VIRTUAL",
        )?;
    }
    Ok(())
}

/// Whether `table` has a column named `column`, counting generated columns —
/// hence `table_xinfo` rather than `table_info`.
fn has_column(conn: &Connection, table: &str, column: &str) -> rusqlite::Result<bool> {
    let n: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM pragma_table_xinfo('{table}') WHERE name = ?1"),
        [column],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Apply the schema, then migrate anything an older install already created.
/// Idempotent: the DDL is all `IF NOT EXISTS` and each migration checks first.
pub fn apply_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(SCHEMA_SQL)?;
    migrate_schema(conn)
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

    #[test]
    fn migration_adds_active_to_a_pre_existing_parties_table() {
        // Reproduces a v0.1.2 install: `parties` already exists without `active`.
        // `CREATE TABLE IF NOT EXISTS` is a no-op against it and `rebuild` only
        // DELETEs rows, so without an explicit migration the column never appears
        // and every query naming it fails with "no such column".
        let conn = open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE parties (
               id   TEXT PRIMARY KEY,
               doc  BLOB NOT NULL,
               name TEXT GENERATED ALWAYS AS (doc ->> 'name') VIRTUAL,
               kind TEXT GENERATED ALWAYS AS (doc ->> 'kind') VIRTUAL
             );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO parties (id, doc) VALUES ('p_old', jsonb('{\"name\":\"Acme\",\"kind\":\"supplier\"}'))",
            [],
        )
        .unwrap();

        apply_schema(&conn).expect("apply schema over an existing table");

        let active: Option<i64> = conn
            .query_row("SELECT active FROM parties WHERE id = 'p_old'", [], |r| {
                r.get(0)
            })
            .expect("the active column must exist after migration");
        assert_eq!(active, None, "a row predating the field reads NULL, not 0");

        let visible: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM parties WHERE COALESCE(active, 1) = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            visible, 1,
            "a NULL active must be treated as active, not hidden"
        );
    }

    #[test]
    fn migration_is_idempotent() {
        let conn = open_in_memory().unwrap();
        apply_schema(&conn).unwrap();
        apply_schema(&conn).expect("second apply must not error");
        apply_schema(&conn).expect("third apply must not error");
    }
}
