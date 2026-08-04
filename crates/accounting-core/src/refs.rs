use rusqlite::Connection;

/// Every column in the read model that points at `items.id`.
pub(crate) const ITEM_REFS: &[(&str, &str)] = &[
    ("inventory_lots", "item_id"),
    ("sale_lines", "item_id"),
    ("purchase_lines", "item_id"),
    ("return_lines", "item_id"),
];

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
        // Derived from the live schema, not from a second hand-written list. The
        // previous version of this test enumerated the same nine columns the
        // constants already contain and asserted each was present, which is
        // "my list is a subset of my list" — a new table with an `item_id`
        // pointing at master data passed it without anyone touching either list,
        // which is precisely the drift it was supposed to catch.
        //
        // Ground truth is the naming convention: a column named `item_id`,
        // `party_id`, `customer_id` or `supplier_id` points at master data. That
        // is stronger than reading `PRAGMA foreign_key_list`, because only five of
        // the nine columns declare a real FK — `sales.customer_id` and
        // `payments.party_id` among those that do not — so an FK-derived check
        // would miss the majority of them.
        let conn = open_in_memory_with_schema().unwrap();
        let tables: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
                .unwrap();
            let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
            rows.collect::<rusqlite::Result<_>>().unwrap()
        };

        let mut unlisted = Vec::new();
        for table in &tables {
            // `items` and `parties` are the targets, not referrers; the events log
            // is the source of truth and is never subject to the delete guard.
            if matches!(table.as_str(), "items" | "parties" | "events") {
                continue;
            }
            let columns: Vec<String> = {
                let mut stmt = conn
                    .prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))
                    .unwrap();
                let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
                rows.collect::<rusqlite::Result<_>>().unwrap()
            };
            for column in columns {
                let expected: &[(&str, &str)] = match column.as_str() {
                    "item_id" => ITEM_REFS,
                    "party_id" | "customer_id" | "supplier_id" => PARTY_REFS,
                    _ => continue,
                };
                if !expected.iter().any(|(t, c)| t == table && *c == column) {
                    unlisted.push(format!("{table}.{column}"));
                }
            }
        }
        assert!(
            unlisted.is_empty(),
            "these columns reference master data but are missing from ITEM_REFS/PARTY_REFS, \
             so the delete guard will not count them and a delete could orphan them: {unlisted:?}"
        );

        // The converse: every listed table/column must actually exist, so a rename
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

        // And the two lists must be exhaustive in the other direction too: the
        // discovery loop above found every convention-named column, so the counts
        // must match, or a listed entry names a column the convention does not
        // recognise and the discovery loop is blind to part of the schema.
        let discovered = tables
            .iter()
            .filter(|t| !matches!(t.as_str(), "items" | "parties" | "events"))
            .flat_map(|table| {
                let mut stmt = conn
                    .prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))
                    .unwrap();
                let cols: Vec<String> = stmt
                    .query_map([], |r| r.get::<_, String>(0))
                    .unwrap()
                    .collect::<rusqlite::Result<_>>()
                    .unwrap();
                cols.into_iter()
                    .filter(|c| {
                        matches!(c.as_str(), "item_id" | "party_id" | "customer_id" | "supplier_id")
                    })
                    .collect::<Vec<_>>()
            })
            .count();
        assert_eq!(
            discovered,
            ITEM_REFS.len() + PARTY_REFS.len(),
            "the reference lists and the schema disagree on how many columns point at master data"
        );
    }
}
