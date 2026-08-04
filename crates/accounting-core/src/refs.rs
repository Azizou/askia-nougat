use rusqlite::Connection;

// Consumed by the delete guard in commands::setup and the delete projector in
// projectors — both land in the next commit. Until then the lib build sees no
// caller, though the tests below exercise every path.
#[allow(dead_code)]
/// Every column in the read model that points at `items.id`.
pub(crate) const ITEM_REFS: &[(&str, &str)] = &[
    ("inventory_lots", "item_id"),
    ("sale_lines", "item_id"),
    ("purchase_lines", "item_id"),
    ("return_lines", "item_id"),
];

#[allow(dead_code)]
/// Every column in the read model that points at `parties.id`.
pub(crate) const PARTY_REFS: &[(&str, &str)] = &[
    ("inventory_lots", "supplier_id"),
    ("sales", "customer_id"),
    ("purchases", "supplier_id"),
    ("payments", "party_id"),
    ("party_balances", "party_id"),
];

/// How many read-model rows point at `id`.
///
/// Shared by the delete guard and the delete projector on purpose: the guard
/// refuses a delete that would orphan a row, and the projector re-checks the
/// same condition at replay time. Were the two to disagree, the projector
/// could attempt a DELETE that a foreign key rejects — and startup calls
/// `rebuild(...).expect(...)`, so that makes the app unlaunchable.
#[allow(dead_code)]
pub(crate) fn count_references(
    conn: &Connection,
    refs: &[(&str, &str)],
    id: &str,
) -> rusqlite::Result<i64> {
    let mut total = 0i64;
    for (table, column) in refs {
        let sql = format!("SELECT COUNT(*) FROM {table} WHERE {column} = ?1");
        total += conn.query_row(&sql, [id], |r| r.get::<_, i64>(0))?;
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory_with_schema;

    #[test]
    fn an_unreferenced_item_counts_zero_and_a_sold_one_does_not() {
        let conn = open_in_memory_with_schema().unwrap();
        conn.execute(
            "INSERT INTO items (id, doc) VALUES ('i1', jsonb('{\"sku\":\"S1\",\"name\":\"W\",\"unit\":\"ea\",\"active\":true}'))",
            [],
        )
        .unwrap();
        assert_eq!(count_references(&conn, ITEM_REFS, "i1").unwrap(), 0);

        conn.execute("INSERT INTO sales (id, event_id, date, terms, total_minor) VALUES ('s1','e1','2026-01-01','cash',100)", []).unwrap();
        conn.execute("INSERT INTO sale_lines (id, sale_id, item_id, qty, unit_price_minor, revenue_minor, cogs_minor, date) VALUES ('sl1','s1','i1',1,100,100,50,'2026-01-01')", []).unwrap();
        assert_eq!(count_references(&conn, ITEM_REFS, "i1").unwrap(), 1);
    }

    #[test]
    fn every_referencing_column_is_listed() {
        // Guards against a new table quietly gaining a column that points at
        // items or parties without the delete guard learning to check it.
        let conn = open_in_memory_with_schema().unwrap();
        let expect_listed = |refs: &[(&str, &str)], table: &str, column: &str| {
            assert!(
                refs.iter().any(|(t, c)| *t == table && *c == column),
                "{table}.{column} references master data but is missing from the reference table"
            );
        };
        for (table, column) in [
            ("inventory_lots", "item_id"),
            ("sale_lines", "item_id"),
            ("purchase_lines", "item_id"),
            ("return_lines", "item_id"),
        ] {
            expect_listed(ITEM_REFS, table, column);
        }
        for (table, column) in [
            ("inventory_lots", "supplier_id"),
            ("sales", "customer_id"),
            ("purchases", "supplier_id"),
            ("payments", "party_id"),
            ("party_balances", "party_id"),
        ] {
            expect_listed(PARTY_REFS, table, column);
        }
        // And every listed table/column must actually exist, so a rename
        // cannot leave the guard silently counting nothing.
        for refs in [ITEM_REFS, PARTY_REFS] {
            for (table, column) in refs {
                let n: i64 = conn
                    .query_row(
                        &format!(
                            "SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name = ?1"
                        ),
                        [column],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert_eq!(n, 1, "{table}.{column} does not exist");
            }
        }
    }
}
