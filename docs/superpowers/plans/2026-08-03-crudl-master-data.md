# CRUDL for Master Data Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Expose the Update and Delete operations the backend already supports for items and parties, seed an anonymous cash supplier, default Sales and Purchases to cash, and let payments settle invoices.

**Architecture:** Delete is two-tiered. *Archive* sets `active=false` through the existing `ItemUpdated`/`PartyUpdated` events — reversible, keeps history intact. *Hard delete* appends new `ItemDeleted`/`PartyDeleted` events, permitted only when nothing in the read model references the row. Guards reject illegal deletes at command time; the projector never fails on one, because a failed projection makes the app unlaunchable.

**Tech Stack:** Rust (rusqlite, serde_json), Tauri 2 commands, React + TypeScript, SQLite with JSONB docs and generated columns.

---

## Investigation Findings That Shape This Plan

These were verified by executable probes against the real schema, not inferred. They are the reason several tasks look the way they do.

**F1 — `parties` has no `active` column, and adding one to `schema.sql` will not reach existing installs.**
`apply_schema` runs `CREATE TABLE IF NOT EXISTS`, which is a no-op against an existing table, and `rebuild` clears projections with `DELETE FROM` (projectors.rs:772-774), never `DROP`. So a v0.1.2 install keeps its 4-column `parties` table forever. `PRAGMA table_xinfo(parties)` returns `['id','doc','name','kind']` and `SELECT active FROM parties` fails with `no such column: active`. An explicit migration is mandatory — otherwise every party query starts erroring after upgrade. Verified: `ALTER TABLE parties ADD COLUMN active INTEGER GENERATED ALWAYS AS (doc ->> 'active') VIRTUAL` **does** succeed (SQLite allows adding a VIRTUAL generated column).

**F2 — `doc ->> 'active'` coerces JSON booleans to integers, but NULL is the real hazard.**
`->>` returns integer 1 for JSON `true` and 0 for `false`, so `handle_item_updated` demanding a boolean while `ItemDefined` projects `1` is *not* a storage mismatch. What does bite: every party row predating this feature has no `active` key at all, so `active` is NULL, and `WHERE active = 1` silently hides all of them. Every filter must be null-safe: `COALESCE(active, 1) = 1`.

**F3 — Foreign keys are ON and enforced, so a hard delete can brick the app.**
`init_state` sets `PRAGMA foreign_keys=ON` (lib.rs:26) and `sale_lines.item_id REFERENCES items(id)` (schema.sql:126). Probe: `DELETE FROM items` with a referencing `sale_lines` row fails with `FOREIGN KEY constraint failed`; inserting a `sale_lines` row for a missing item fails the same way. Startup does `rebuild(&mut conn).expect("rebuild projections")` (lib.rs:47), so **any event that cannot be projected makes the app impossible to launch.**

Single-device replay is safe, because causal order puts `ItemDeleted` after the check that found no references. The danger is the merge path (a shipped feature: `import_event_log` unions two logs): device A deletes an unused item while device B sells it. After merge, HLC order can place `ItemDeleted` *before* `SaleRecorded`, and then either the DELETE or the later sale-line INSERT violates the constraint and the ledger becomes unreplayable.

**Resolution — guards reject, projectors degrade.** The command guard refuses an interactive delete of a referenced row. The projector re-counts references at replay time and, if any exist, **archives instead of deleting** (sets `active=false`). Projection stays total: it can never fail, so no merge can brick an install. This is the same split the codebase already uses — validation belongs pre-commit, projection must always succeed.

**F4 — Payment allocation needs no new backend work, and its payload contract is already consistent.**
`handle_payment_received` emits allocations as `{saleId, amountMinor}`; the projector reads `target_key = "saleId"` for `dir == "in"` (projectors.rs:301, :324). `handle_payment_made` emits `{purchaseId, ...}` and the projector reads `"purchaseId"`. They agree. Both handlers already accept a `Vec<AllocInput>`, and both Tauri commands already deserialize `allocations`. The only defect is `Payments.tsx:71-72`, which hardcodes `allocations: []`. This is a **UI-only fix**.

**F5 — Deletes are not reversible, by design.**
`check_reversal_legal_target` (guards.rs) only accepts event types where `is_transactional` is true, and master-data events map to `vec![]` in `categories_of`. So `ItemDeleted`/`PartyDeleted` cannot be voided. That is correct: undoing a hard delete means recreating the record. Archive is the reversible operation.

**F6 — Seeded system parties must not be archivable or deletable.**
`party_walkin` is auto-selected for cash sales (Sales.tsx:87-89) and the new `party_anon_supplier` will be for cash purchases. Archiving either breaks the default path for a non-technical user, so guards must refuse both operations on them.

## Out of Scope

The user explicitly deferred these; they are recorded here so the gap is documented rather than forgotten. **Do not build them.** Accounts, users, expenses, transfers, returns, and inventory adjustments all lack UI Update/Delete. `handle_account_updated` and `handle_user_updated` exist and are unreachable from the frontend, exactly as the item and party handlers were.

## File Structure

**Create:**
- `crates/accounting-core/src/refs.rs` — the reference tables (which columns point at items and parties) plus `count_references`. Shared by the command guard and the projector so the two can never disagree about what "referenced" means.

**Modify:**
- `crates/accounting-core/src/db.rs` — `migrate_schema`, called from `apply_schema`
- `crates/accounting-core/src/schema.sql` — `parties.active` for fresh installs
- `crates/accounting-core/src/commands/setup.rs` — delete handlers, sku guard, system-party guard
- `crates/accounting-core/src/commands/categories.rs` — register the two new event types
- `crates/accounting-core/src/projectors.rs` — project the deletes, defensively
- `crates/accounting-core/src/genesis.rs` — `ensure_anon_supplier`
- `crates/accounting-core/src/lib.rs` — re-exports
- `crates/tauri-app/src/commands.rs` — 4 new commands, `active` on list rows
- `crates/tauri-app/src/lib.rs` — register commands, call `ensure_anon_supplier`
- `ui/src/i18n/en.ts`, `ui/src/i18n/fr.ts` — new keys
- `ui/src/lib.ts` — anon supplier id + display name
- `ui/src/pages/Items.tsx`, `Parties.tsx` — edit / archive / delete
- `ui/src/pages/Sales.tsx`, `Purchases.tsx` — cash default, archived filtering, anon supplier
- `ui/src/pages/Payments.tsx` — real allocations

---

### Task 1: Migrate `parties` to carry `active`

**Files:**
- Modify: `crates/accounting-core/src/schema.sql:82-88`
- Modify: `crates/accounting-core/src/db.rs`

- [ ] **Step 1: Write the failing test**

In `crates/accounting-core/src/db.rs`, inside `mod tests`:

```rust
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
        .query_row("SELECT active FROM parties WHERE id = 'p_old'", [], |r| r.get(0))
        .expect("the active column must exist after migration");
    assert_eq!(active, None, "a row predating the field reads NULL, not 0");

    let visible: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM parties WHERE COALESCE(active, 1) = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(visible, 1, "a NULL active must be treated as active, not hidden");
}

#[test]
fn migration_is_idempotent() {
    let conn = open_in_memory().unwrap();
    apply_schema(&conn).unwrap();
    apply_schema(&conn).expect("second apply must not error");
    apply_schema(&conn).expect("third apply must not error");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p accounting-core migration_adds_active`
Expected: FAIL — `the active column must exist after migration`, cause `no such column: active`.

- [ ] **Step 3: Add the column for fresh installs**

In `crates/accounting-core/src/schema.sql`, replace the `parties` block (lines 82-88):

```sql
-- §5.5 parties
CREATE TABLE IF NOT EXISTS parties (
  id     TEXT PRIMARY KEY,
  doc    BLOB NOT NULL,
  name   TEXT GENERATED ALWAYS AS (doc ->> 'name') VIRTUAL,
  kind   TEXT GENERATED ALWAYS AS (doc ->> 'kind') VIRTUAL,
  active INTEGER GENERATED ALWAYS AS (doc ->> 'active') VIRTUAL
);
CREATE INDEX IF NOT EXISTS parties_kind ON parties (kind);
```

- [ ] **Step 4: Migrate existing installs**

In `crates/accounting-core/src/db.rs`, add above `apply_schema`:

```rust
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
```

Then change `apply_schema` to run both:

```rust
/// Apply the schema, then migrate anything an older install already created.
/// Idempotent: the DDL is all `IF NOT EXISTS` and each migration checks first.
pub fn apply_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(SCHEMA_SQL)?;
    migrate_schema(conn)
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p accounting-core`
Expected: PASS, including the pre-existing `apply_schema_*` tests.

- [ ] **Step 6: Prove the test discriminates**

Temporarily make `migrate_schema` return `Ok(())` without doing anything. Run `cargo test -p accounting-core migration_adds_active` and confirm it FAILS. Restore the body.

- [ ] **Step 7: Commit**

```bash
git add crates/accounting-core/src/db.rs crates/accounting-core/src/schema.sql
git commit -m "feat(core): migrate parties to carry an active flag"
```

---

### Task 2: Shared reference tables

**Files:**
- Create: `crates/accounting-core/src/refs.rs`
- Modify: `crates/accounting-core/src/lib.rs`

The delete guard and the delete projector must agree exactly on what counts as a reference. If they drift, the guard permits a delete the projector then handles as an archive (confusing but safe) or — worse, if the projector is the laxer one — the projector attempts a DELETE that a foreign key rejects, and the app will not launch. One table, two callers.

- [ ] **Step 1: Write the failing test**

Create `crates/accounting-core/src/refs.rs`:

```rust
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
        // Guards against a new table quietly gaining an item_id or party_id
        // column that the delete guard then fails to check.
        let conn = open_in_memory_with_schema().unwrap();
        for (refs, target, id_col) in [(ITEM_REFS, "items", "item_id"), (PARTY_REFS, "parties", "party_id")] {
            let mut stmt = conn
                .prepare(
                    "SELECT m.name, p.name FROM sqlite_master m
                     JOIN pragma_table_info(m.name) p
                     WHERE m.type = 'table' AND m.name != ?1 AND p.name LIKE ?2",
                )
                .unwrap();
            let found: Vec<(String, String)> = stmt
                .query_map(rusqlite::params![target, format!("%{}", id_col.trim_start_matches("item_").trim_start_matches("party_"))], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            for (table, column) in found {
                assert!(
                    refs.iter().any(|(t, c)| *t == table && *c == column),
                    "{table}.{column} points at {target} but is missing from the reference table"
                );
            }
        }
    }
}
```

Note on the second test: it matches any column ending in `_id` whose suffix is `id`, which over-matches. Narrow it to the exact columns instead — replace the `LIKE` pattern with an equality check against a small list built from the schema:

```rust
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
        for (refs, _) in [(ITEM_REFS, "items"), (PARTY_REFS, "parties")] {
            for (table, column) in refs {
                let n: i64 = conn
                    .query_row(
                        &format!("SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name = ?1"),
                        [column],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert_eq!(n, 1, "{table}.{column} does not exist");
            }
        }
    }
```

Use this second version. Delete the first.

- [ ] **Step 2: Register the module**

In `crates/accounting-core/src/lib.rs`, add alongside the other `mod` declarations:

```rust
mod refs;
```

- [ ] **Step 3: Run the tests**

Run: `cargo test -p accounting-core refs::`
Expected: PASS (2 tests).

- [ ] **Step 4: Commit**

```bash
git add crates/accounting-core/src/refs.rs crates/accounting-core/src/lib.rs
git commit -m "feat(core): shared master-data reference tables"
```

---

### Task 3: `ItemDeleted` and `PartyDeleted` commands

**Files:**
- Modify: `crates/accounting-core/src/commands/setup.rs`
- Modify: `crates/accounting-core/src/commands/categories.rs`
- Modify: `crates/accounting-core/src/genesis.rs` (constant only)
- Modify: `crates/accounting-core/src/lib.rs`

- [ ] **Step 1: Add the anonymous-supplier constant**

The delete guard needs to name both seeded parties, and Task 5 needs the constant anyway. In `crates/accounting-core/src/genesis.rs`, below `WALKIN_PARTY_ID`:

```rust
/// The shared, always-present supplier used to record cash purchases from an
/// unrecorded seller — the buy-side counterpart of [`WALKIN_PARTY_ID`].
pub const ANON_SUPPLIER_PARTY_ID: &str = "party_anon_supplier";
```

- [ ] **Step 2: Write the failing tests**

In `crates/accounting-core/src/commands/setup.rs`, inside `mod tests`:

```rust
    fn seed_sold_item(conn: &rusqlite::Connection) {
        conn.execute(
            "INSERT INTO sales (id, event_id, date, terms, total_minor) VALUES ('s1','e1','2026-01-01','cash',100)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sale_lines (id, sale_id, item_id, qty, unit_price_minor, revenue_minor, cogs_minor, date)
             VALUES ('sl1','s1','i1',1,100,100,50,'2026-01-01')",
            [],
        )
        .unwrap();
    }

    #[test]
    fn item_deleted_removes_an_item_nothing_references() {
        let (mut conn, mut hlc) = fixture();
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_item_defined(&mut c, "i1", "S1", "Widget", "ea").expect("define");
        }
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_item_deleted(&mut c, "i1").expect("delete an unused item");
        }
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM items WHERE id = 'i1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "the projection row must be gone");
    }

    #[test]
    fn item_deleted_is_refused_once_the_item_has_been_sold() {
        let (mut conn, mut hlc) = fixture();
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_item_defined(&mut c, "i1", "S1", "Widget", "ea").expect("define");
        }
        seed_sold_item(&conn);

        let err = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_item_deleted(&mut c, "i1").unwrap_err()
        };
        assert!(matches!(err, CommandError::Validation(_)));

        let events: i64 = conn
            .query_row("SELECT COUNT(*) FROM events WHERE type = 'ItemDeleted'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(events, 0, "a rejected delete must not append an event");
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM items WHERE id = 'i1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1, "the item must survive");
    }

    #[test]
    fn item_deleted_rejects_an_unknown_id() {
        let (mut conn, mut hlc) = fixture();
        let mut c = ctx(&mut conn, &mut hlc);
        assert!(matches!(
            handle_item_deleted(&mut c, "nope").unwrap_err(),
            CommandError::Validation(_)
        ));
    }

    #[test]
    fn party_deleted_removes_a_party_nothing_references() {
        let (mut conn, mut hlc) = fixture();
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_party_created(&mut c, "p1", "Acme", "supplier").expect("create");
        }
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_party_deleted(&mut c, "p1").expect("delete an unused party");
        }
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM parties WHERE id = 'p1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn party_deleted_is_refused_once_the_party_has_traded() {
        let (mut conn, mut hlc) = fixture();
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_party_created(&mut c, "p1", "Acme", "customer").expect("create");
        }
        conn.execute(
            "INSERT INTO sales (id, event_id, customer_id, date, terms, total_minor)
             VALUES ('s1','e1','p1','2026-01-01','credit',100)",
            [],
        )
        .unwrap();

        let err = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_party_deleted(&mut c, "p1").unwrap_err()
        };
        assert!(matches!(err, CommandError::Validation(_)));
    }

    #[test]
    fn the_seeded_parties_can_be_neither_deleted_nor_archived() {
        // Cash sales auto-select the walk-in customer and cash purchases the
        // anonymous supplier. Removing or hiding either breaks the default
        // path for the very users least able to diagnose it.
        let (mut conn, mut hlc) = fixture();
        for id in [crate::genesis::WALKIN_PARTY_ID, crate::genesis::ANON_SUPPLIER_PARTY_ID] {
            {
                let mut c = ctx(&mut conn, &mut hlc);
                handle_party_created(&mut c, id, "Seeded", "both").expect("seed");
            }
            {
                let mut c = ctx(&mut conn, &mut hlc);
                assert!(matches!(
                    handle_party_deleted(&mut c, id).unwrap_err(),
                    CommandError::Validation(_)
                ), "{id} must not be deletable");
            }
            let mut c = ctx(&mut conn, &mut hlc);
            assert!(matches!(
                handle_party_updated(&mut c, id, json!({"active": false})).unwrap_err(),
                CommandError::Validation(_)
            ), "{id} must not be archivable");
        }
    }

    #[test]
    fn party_updated_still_allows_renaming_a_seeded_party() {
        // Only archiving is blocked, not every edit.
        let (mut conn, mut hlc) = fixture();
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_party_created(&mut c, crate::genesis::WALKIN_PARTY_ID, "Walk-in", "customer").unwrap();
        }
        let mut c = ctx(&mut conn, &mut hlc);
        assert!(handle_party_updated(&mut c, crate::genesis::WALKIN_PARTY_ID, json!({"name": "Passing trade"})).is_ok());
    }

    #[test]
    fn item_updated_rejects_a_duplicate_sku() {
        // `items_sku` is a UNIQUE index, so a colliding rename would fail
        // inside the projector. That rolls the commit back safely, but the
        // user sees a database error instead of an explanation.
        let (mut conn, mut hlc) = fixture();
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_item_defined(&mut c, "i1", "S1", "Widget", "ea").unwrap();
        }
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_item_defined(&mut c, "i2", "S2", "Gadget", "ea").unwrap();
        }
        let err = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_item_updated(&mut c, "i2", json!({"sku": "S1"})).unwrap_err()
        };
        assert!(matches!(err, CommandError::Validation(_)));
        // Renaming to its own sku is not a collision.
        let mut c = ctx(&mut conn, &mut hlc);
        assert!(handle_item_updated(&mut c, "i2", json!({"sku": "S2", "name": "Gadget II"})).is_ok());
    }

    #[test]
    fn item_updated_archives_by_setting_active_false() {
        let (mut conn, mut hlc) = fixture();
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_item_defined(&mut c, "i1", "S1", "Widget", "ea").unwrap();
        }
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_item_updated(&mut c, "i1", json!({"active": false})).expect("archive");
        }
        let active: Option<i64> = conn
            .query_row("SELECT active FROM items WHERE id = 'i1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(active, Some(0), "archive must be visible through the generated column");

        // ...and it is reversible.
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_item_updated(&mut c, "i1", json!({"active": true})).expect("restore");
        }
        let active: Option<i64> = conn
            .query_row("SELECT active FROM items WHERE id = 'i1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(active, Some(1));
    }
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p accounting-core commands::setup`
Expected: FAIL to compile — `cannot find function handle_item_deleted`.

- [ ] **Step 4: Implement the handlers**

In `crates/accounting-core/src/commands/setup.rs`, add at the top:

```rust
use crate::genesis::{ANON_SUPPLIER_PARTY_ID, WALKIN_PARTY_ID};
use crate::refs::{count_references, ITEM_REFS, PARTY_REFS};
```

Add these functions:

```rust
/// The seeded parties the UI selects automatically for cash trade. Neither may
/// be archived or deleted — doing so breaks the default path on both the sales
/// and purchases forms.
fn ensure_not_seeded_party(party_id: &str) -> Result<(), CommandError> {
    if party_id == WALKIN_PARTY_ID || party_id == ANON_SUPPLIER_PARTY_ID {
        return Err(reject(format!(
            "{party_id} is a built-in party used for cash trade and cannot be archived or deleted"
        )));
    }
    Ok(())
}

/// Refuse a hard delete once anything in the read model points at the row.
///
/// Deliberately strict: the alternative is a `DELETE` the foreign keys reject
/// during replay, and startup treats a failed rebuild as fatal.
fn ensure_unreferenced(
    ctx: &CommandContext,
    refs: &[(&str, &str)],
    id: &str,
    noun: &str,
) -> Result<(), CommandError> {
    let n = count_references(ctx.conn, refs, id)?;
    if n > 0 {
        return Err(reject(format!(
            "{noun} {id} is used by {n} existing record(s); archive it instead of deleting it"
        )));
    }
    Ok(())
}

pub fn handle_item_deleted(ctx: &mut CommandContext, item_id: &str)
    -> Result<crate::events::LedgerEvent, CommandError> {
    ensure_present(ctx, "items", item_id)?;
    ensure_unreferenced(ctx, ITEM_REFS, item_id, "item")?;
    commit_event(ctx, "ItemDeleted", json!({ "itemId": item_id }))
}

pub fn handle_party_deleted(ctx: &mut CommandContext, party_id: &str)
    -> Result<crate::events::LedgerEvent, CommandError> {
    ensure_present(ctx, "parties", party_id)?;
    ensure_not_seeded_party(party_id)?;
    ensure_unreferenced(ctx, PARTY_REFS, party_id, "party")?;
    commit_event(ctx, "PartyDeleted", json!({ "partyId": party_id }))
}
```

Extend `handle_item_updated` with the sku-collision guard, keeping the existing `active` check:

```rust
pub fn handle_item_updated(ctx: &mut CommandContext, item_id: &str, changes: serde_json::Value)
    -> Result<crate::events::LedgerEvent, CommandError> {
    ensure_present(ctx, "items", item_id)?;
    if let Some(a) = changes.get("active") {
        if !a.is_boolean() { return Err(reject("item 'active' must be a boolean")); }
    }
    // `items_sku` is UNIQUE, so a colliding rename fails inside the projector.
    // The commit rolls back either way; checking here turns an opaque database
    // error into something the user can act on.
    if let Some(sku) = changes.get("sku") {
        let sku = sku.as_str().ok_or_else(|| reject("item 'sku' must be a string"))?;
        let clash: i64 = ctx.conn.query_row(
            "SELECT COUNT(*) FROM items WHERE sku = ?1 AND id != ?2",
            rusqlite::params![sku, item_id],
            |r| r.get(0),
        )?;
        if clash > 0 {
            return Err(reject(format!("another item already uses SKU '{sku}'")));
        }
    }
    commit_event(ctx, "ItemUpdated", json!({ "itemId": item_id, "changes": changes }))
}
```

Extend `handle_party_updated` to protect the seeded parties from archiving:

```rust
pub fn handle_party_updated(ctx: &mut CommandContext, party_id: &str, changes: serde_json::Value)
    -> Result<crate::events::LedgerEvent, CommandError> {
    ensure_present(ctx, "parties", party_id)?;
    if let Some(k) = changes.get("kind") {
        let ok = k.as_str().map(|s| matches!(s, "supplier"|"customer"|"both")).unwrap_or(false);
        if !ok { return Err(reject(format!("invalid party kind in changes: {k}"))); }
    }
    match changes.get("active") {
        Some(a) if !a.is_boolean() => return Err(reject("party 'active' must be a boolean")),
        // Renaming a seeded party is fine; hiding it is not.
        Some(a) if a == &json!(false) => ensure_not_seeded_party(party_id)?,
        _ => {}
    }
    commit_event(ctx, "PartyUpdated", json!({ "partyId": party_id, "changes": changes }))
}
```

- [ ] **Step 5: Register the new event types**

In `crates/accounting-core/src/commands/categories.rs`, extend the master-data arm:

```rust
        "UserRegistered" | "AccountOpened" | "ItemDefined" | "PartyCreated"
        | "UserUpdated" | "AccountUpdated" | "ItemUpdated" | "PartyUpdated"
        | "ItemDeleted" | "PartyDeleted" => vec![],
```

And add a test in that module's `mod tests`:

```rust
    #[test]
    fn master_data_deletes_are_not_transactional() {
        // Non-transactional means not a legal reversal target: undoing a hard
        // delete means recreating the record, and archive is the reversible
        // operation. See check_reversal_legal_target.
        for t in ["ItemDeleted", "PartyDeleted"] {
            assert!(categories_of(t).is_empty(), "{t}");
            assert!(!is_transactional(t), "{t}");
        }
    }
```

- [ ] **Step 6: Export the handlers**

In `crates/accounting-core/src/lib.rs`, add `handle_item_deleted` and `handle_party_deleted` to the `pub use commands::setup::{...}` list, and add `ANON_SUPPLIER_PARTY_ID` to the `pub use genesis::{...}` list.

- [ ] **Step 7: Run the tests**

Run: `cargo test -p accounting-core`
Expected: The new `setup` and `categories` tests PASS. The delete-projection tests (`item_deleted_removes_an_item_nothing_references`, `party_deleted_removes_a_party_nothing_references`) will still FAIL, because `apply_event` does not yet know these event types and returns `unknown(other)`. That is expected — Task 4 fixes it. If any *other* test regresses, stop and fix it.

- [ ] **Step 8: Commit**

```bash
git add crates/accounting-core/src/commands/setup.rs crates/accounting-core/src/commands/categories.rs crates/accounting-core/src/genesis.rs crates/accounting-core/src/lib.rs
git commit -m "feat(core): delete commands for items and parties, with reference guards"
```

---

### Task 4: Project the deletes without ever failing

**Files:**
- Modify: `crates/accounting-core/src/projectors.rs`

This is the task that keeps a merged ledger replayable. Read F3 above before starting.

- [ ] **Step 1: Write the failing test**

In `crates/accounting-core/src/projectors.rs`, inside `mod tests`:

```rust
    #[test]
    fn item_deleted_removes_the_row() {
        let mut conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("devA");
        record(&mut conn, &mut hlc, 1000, "ItemDefined",
            json!({"itemId": "i1", "sku": "S1", "name": "W", "unit": "ea"}));
        record(&mut conn, &mut hlc, 1000, "ItemDeleted", json!({"itemId": "i1"}));
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn party_deleted_removes_the_row() {
        let mut conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("devA");
        record(&mut conn, &mut hlc, 1000, "PartyCreated",
            json!({"partyId": "p1", "name": "Acme", "kind": "supplier"}));
        record(&mut conn, &mut hlc, 1000, "PartyDeleted", json!({"partyId": "p1"}));
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM parties", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn a_delete_ordered_before_the_sale_that_uses_the_item_archives_instead_of_failing() {
        // The merge case. Device A deletes an item it never used; device B
        // sells it. `import_event_log` unions both logs, and HLC order can put
        // the delete first. A literal DELETE would then violate
        // sale_lines.item_id REFERENCES items(id) — and startup does
        // `rebuild(...).expect(...)`, so that would leave the app unable to
        // launch. Projection must be total: it degrades to an archive.
        let mut conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("devA");
        seed_accounts(&mut conn, &mut hlc);
        record(&mut conn, &mut hlc, 1000, "ItemDefined",
            json!({"itemId": "i1", "sku": "S1", "name": "W", "unit": "ea"}));
        record(&mut conn, &mut hlc, 1000, "PurchaseRecorded",
            json!({"purchaseId": "po1", "supplierId": null, "date": "2026-01-01", "terms": "cash",
                   "lines": [{"itemId": "i1", "qty": 10, "unitCostMinor": 100, "lotId": "lot1"}]}));

        // The purchase already references the item, standing in for "a
        // transaction the deleting device had not seen".
        record(&mut conn, &mut hlc, 2000, "ItemDeleted", json!({"itemId": "i1"}));

        let (rows, active): (i64, Option<i64>) = conn
            .query_row(
                "SELECT COUNT(*), MAX(active) FROM items WHERE id = 'i1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(rows, 1, "the row must survive so its transactions stay valid");
        assert_eq!(active, Some(0), "and it must be archived rather than deleted");

        // The whole log must still replay from scratch.
        rebuild(&mut conn).expect("a merged log containing a contested delete must stay replayable");
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM items WHERE id = 'i1'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn a_delete_of_a_row_that_is_already_gone_is_a_no_op() {
        // Two devices can both delete the same unused item. The second event
        // must not fail; it has nothing left to do.
        let mut conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("devA");
        record(&mut conn, &mut hlc, 1000, "ItemDefined",
            json!({"itemId": "i1", "sku": "S1", "name": "W", "unit": "ea"}));
        record(&mut conn, &mut hlc, 1000, "ItemDeleted", json!({"itemId": "i1"}));
        record(&mut conn, &mut hlc, 1100, "ItemDeleted", json!({"itemId": "i1"}));
        rebuild(&mut conn).expect("a duplicated delete must stay replayable");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p accounting-core projectors::tests::item_deleted projectors::tests::party_deleted projectors::tests::a_delete`
Expected: FAIL — `unknown event type: ItemDeleted`.

- [ ] **Step 3: Implement the projector**

In `crates/accounting-core/src/projectors.rs`, add near `patch_doc`:

```rust
/// Remove a master-data row, or archive it when that would orphan a
/// transaction.
///
/// A projector must never fail. `init_state` calls
/// `rebuild(...).expect("rebuild projections")`, so an event that cannot be
/// applied does not surface as an error message — it makes the app impossible
/// to launch. Two orderings force the fallback:
///
///   * a merged log (see `import_jsonl`) can place this delete before a sale
///     or purchase from another device that uses the row, and foreign keys
///     would reject the DELETE;
///   * both devices may delete the same row, so the row can already be gone.
///
/// The command guard is the strict half of this pair: it refuses an
/// interactive delete of anything referenced. Reaching the archive branch here
/// means the log itself is ambiguous, and keeping the row — hidden from new
/// transactions but present for the ones that need it — loses the least.
fn delete_master(
    tx: &Connection,
    table: &str,
    ev: &LedgerEvent,
    id_key: &str,
    refs: &[(&str, &str)],
) -> rusqlite::Result<()> {
    let id = ps(&ev.payload, id_key);
    if crate::refs::count_references(tx, refs, id)? > 0 {
        return set_active(tx, table, id, false);
    }
    tx.execute(&format!("DELETE FROM {table} WHERE id = ?1"), [id])?;
    Ok(())
}

/// Set `active` inside a master-data row's JSON doc, leaving every other field
/// alone. A missing row is a no-op, not an error, for the reasons in
/// [`delete_master`].
fn set_active(tx: &Connection, table: &str, id: &str, active: bool) -> rusqlite::Result<()> {
    use rusqlite::OptionalExtension;
    let sel = format!("SELECT json(doc) FROM {table} WHERE id = ?1");
    let Some(doc_text) = tx.query_row(&sel, [id], |r| r.get::<_, String>(0)).optional()? else {
        return Ok(());
    };
    let mut doc: Value = serde_json::from_str(&doc_text).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })?;
    doc["active"] = json!(active);
    let upd = format!("UPDATE {table} SET doc = jsonb(?2) WHERE id = ?1");
    tx.execute(&upd, rusqlite::params![id, doc.to_string()])?;
    Ok(())
}
```

Then add two arms to `apply_event`, right after the `PartyUpdated` arm:

```rust
        // ---- master data: delete (hard, or archive when contested) ----
        "ItemDeleted" => delete_master(tx, "items", ev, "itemId", crate::refs::ITEM_REFS),
        "PartyDeleted" => delete_master(tx, "parties", ev, "partyId", crate::refs::PARTY_REFS),
```

Also make `patch_doc` tolerate a missing row, for the same merge reason — a `PartyUpdated` can arrive after another device's `PartyDeleted`:

```rust
fn patch_doc(tx: &Connection, table: &str, ev: &LedgerEvent, id_key: &str) -> rusqlite::Result<()> {
    use rusqlite::OptionalExtension;
    let id = ps(&ev.payload, id_key);
    let sel = format!("SELECT json(doc) FROM {table} WHERE id = ?1");
    // A merged log can order an update after another device's delete. The row
    // is gone and there is nothing to patch; failing here would make the
    // ledger unreplayable, and startup treats that as fatal.
    let Some(doc_text) = tx.query_row(&sel, [id], |r| r.get::<_, String>(0)).optional()? else {
        return Ok(());
    };
    let mut doc: Value = serde_json::from_str(&doc_text).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })?;
    if let Some(changes) = ev.payload.get("changes").and_then(Value::as_object) {
        for (k, v) in changes {
            doc[k] = v.clone();
        }
    }
    let upd = format!("UPDATE {table} SET doc = jsonb(?2) WHERE id = ?1");
    tx.execute(&upd, rusqlite::params![id, doc.to_string()])?;
    Ok(())
}
```

Add a test for that too:

```rust
    #[test]
    fn an_update_arriving_after_a_delete_is_a_no_op() {
        let mut conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("devA");
        record(&mut conn, &mut hlc, 1000, "PartyCreated",
            json!({"partyId": "p1", "name": "Acme", "kind": "supplier"}));
        record(&mut conn, &mut hlc, 1000, "PartyDeleted", json!({"partyId": "p1"}));
        record(&mut conn, &mut hlc, 1100, "PartyUpdated",
            json!({"partyId": "p1", "changes": {"name": "Acme Renamed"}}));
        rebuild(&mut conn).expect("an update after a delete must stay replayable");
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p accounting-core`
Expected: PASS, including the two Task 3 tests that were failing.

- [ ] **Step 5: Prove the merge test discriminates**

Temporarily replace `delete_master`'s body with an unconditional
`tx.execute(&format!("DELETE FROM {table} WHERE id = ?1"), [id])?; Ok(())`.
Run `cargo test -p accounting-core projectors::tests::a_delete_ordered_before` and confirm it FAILS with a foreign-key error. Restore the guarded body. **Do not skip this** — a test that passes under the naive implementation would not be protecting anything.

- [ ] **Step 6: Commit**

```bash
git add crates/accounting-core/src/projectors.rs
git commit -m "feat(core): project master-data deletes, degrading to archive when contested"
```

---

### Task 5: Seed the anonymous supplier

**Files:**
- Modify: `crates/accounting-core/src/genesis.rs`
- Modify: `crates/accounting-core/src/lib.rs`
- Modify: `crates/tauri-app/src/lib.rs:44`

- [ ] **Step 1: Write the failing test**

In `crates/accounting-core/src/genesis.rs`, inside `mod tests`:

```rust
    #[test]
    fn ensure_anon_supplier_seeds_once_and_is_idempotent() {
        let conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("deviceA");
        run_genesis(&conn, &mut hlc, 1000, "deviceA", "owner-1", "Jane").unwrap();

        ensure_anon_supplier(&conn, &mut hlc, 2000, "deviceA").unwrap();
        ensure_anon_supplier(&conn, &mut hlc, 3000, "deviceA").unwrap();

        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM events WHERE type='PartyCreated' \
                 AND json_extract(payload, '$.partyId') = ?1",
                [ANON_SUPPLIER_PARTY_ID],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "the anonymous supplier must be seeded exactly once");
    }

    #[test]
    fn ensure_anon_supplier_projects_a_supplier_party() {
        let mut conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("deviceA");
        run_genesis(&conn, &mut hlc, 1000, "deviceA", "owner-1", "Jane").unwrap();
        ensure_anon_supplier(&conn, &mut hlc, 2000, "deviceA").unwrap();
        crate::projectors::rebuild(&mut conn).unwrap();
        let (kind, active): (String, Option<i64>) = conn
            .query_row(
                "SELECT kind, active FROM parties WHERE id = ?1",
                [ANON_SUPPLIER_PARTY_ID],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        // `handle_purchase_recorded` calls ensure_party(.., &["supplier"]),
        // so the kind must be exactly this.
        assert_eq!(kind, "supplier");
        assert_eq!(active, Some(1), "it must be visible in supplier dropdowns");
    }

    #[test]
    fn the_two_seeded_parties_are_distinct() {
        assert_ne!(WALKIN_PARTY_ID, ANON_SUPPLIER_PARTY_ID);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p accounting-core genesis::tests::ensure_anon`
Expected: FAIL to compile — `cannot find function ensure_anon_supplier`.

- [ ] **Step 3: Implement it**

In `crates/accounting-core/src/genesis.rs`, below `ensure_walkin_party`:

```rust
/// Idempotently ensure the anonymous cash supplier exists.
///
/// The mirror of [`ensure_walkin_party`], and safe on every startup for the
/// same reason: it consults the immutable event log rather than the `parties`
/// projection, which is empty until `rebuild()` runs. Covers installs whose
/// genesis predates this party.
pub fn ensure_anon_supplier(
    conn: &Connection,
    hlc: &mut Hlc,
    physical_now: u64,
    device_id: &str,
) -> rusqlite::Result<()> {
    let exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM events WHERE type = 'PartyCreated' \
         AND json_extract(payload, '$.partyId') = ?1",
        [ANON_SUPPLIER_PARTY_ID],
        |r| r.get(0),
    )?;
    if exists > 0 {
        return Ok(());
    }
    append_event(
        conn, hlc, physical_now, device_id, SYSTEM_USER_ID,
        "PartyCreated",
        &json!({ "partyId": ANON_SUPPLIER_PARTY_ID, "name": "Cash Supplier", "kind": "supplier", "active": true }),
    )?;
    Ok(())
}
```

- [ ] **Step 4: Export and call it**

In `crates/accounting-core/src/lib.rs`, extend the genesis re-export:

```rust
pub use genesis::{ensure_anon_supplier, ensure_walkin_party, run_genesis, ANON_SUPPLIER_PARTY_ID, SYSTEM_USER_ID, WALKIN_PARTY_ID};
```

In `crates/tauri-app/src/lib.rs`, add `ensure_anon_supplier` to the `use accounting_core::{...}` list, and call it immediately after `ensure_walkin_party` (line 44), before `rebuild`:

```rust
    ensure_walkin_party(&conn, &mut hlc, now_ms(), &device_id).expect("seed walk-in party");
    ensure_anon_supplier(&conn, &mut hlc, now_ms(), &device_id).expect("seed anonymous supplier");
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p accounting-core && cargo build -p tauri-app`
Expected: PASS / clean build.

- [ ] **Step 6: Commit**

```bash
git add crates/accounting-core/src/genesis.rs crates/accounting-core/src/lib.rs crates/tauri-app/src/lib.rs
git commit -m "feat(core): seed an anonymous cash supplier"
```

---

### Task 6: Tauri commands for update and delete

**Files:**
- Modify: `crates/tauri-app/src/commands.rs`
- Modify: `crates/tauri-app/src/lib.rs:112-134`

Note the Tauri 2 argument-case rule: `#[tauri::command]` defaults to Camel, so a Rust parameter `item_id` is `itemId` from JavaScript. The `input:`-struct style used everywhere in this file sidesteps that, because serde field names are matched verbatim inside the struct. Follow the existing style: one `input` struct per command.

- [ ] **Step 1: Add the commands**

In `crates/tauri-app/src/commands.rs`, extend the `accounting_core` import with `handle_item_deleted, handle_item_updated, handle_party_deleted, handle_party_updated`, then add after `create_party`:

```rust
#[derive(Deserialize)]
pub struct ItemUpdateInput {
    pub id: String,
    /// A partial document. Only the keys present are changed; `active: false`
    /// archives the item.
    pub changes: serde_json::Value,
}

#[tauri::command]
pub fn update_item(state: State<AppState>, input: ItemUpdateInput) -> Result<(), AppError> {
    with_ctx!(state, |ctx| {
        handle_item_updated(&mut ctx, &input.id, input.changes)?;
        Ok(())
    })
}

#[derive(Deserialize)]
pub struct PartyUpdateInput {
    pub id: String,
    pub changes: serde_json::Value,
}

#[tauri::command]
pub fn update_party(state: State<AppState>, input: PartyUpdateInput) -> Result<(), AppError> {
    with_ctx!(state, |ctx| {
        handle_party_updated(&mut ctx, &input.id, input.changes)?;
        Ok(())
    })
}

#[derive(Deserialize)]
pub struct DeleteInput {
    pub id: String,
}

/// Permanently remove an item. Rejected once anything references it — the
/// caller should archive instead, and the error says so.
#[tauri::command]
pub fn delete_item(state: State<AppState>, input: DeleteInput) -> Result<(), AppError> {
    with_ctx!(state, |ctx| {
        handle_item_deleted(&mut ctx, &input.id)?;
        Ok(())
    })
}

/// Permanently remove a party. Rejected once anything references it, and for
/// the two built-in cash-trade parties.
#[tauri::command]
pub fn delete_party(state: State<AppState>, input: DeleteInput) -> Result<(), AppError> {
    with_ctx!(state, |ctx| {
        handle_party_deleted(&mut ctx, &input.id)?;
        Ok(())
    })
}
```

- [ ] **Step 2: Return `active` from the list queries**

Replace `ItemRow`/`list_items` and `PartyRow`/`list_parties` (commands.rs:259-294):

```rust
#[derive(Serialize)]
pub struct ItemRow {
    pub id: String,
    pub name: String,
    pub sku: String,
    pub unit: String,
    pub active: bool,
}

/// Every item, archived ones included. The frontend decides what to show:
/// transaction forms offer only active items, while the items page can reveal
/// the archived ones behind a toggle.
#[tauri::command]
pub fn list_items(state: State<AppState>) -> Result<Vec<ItemRow>, AppError> {
    let db = state.db.lock().unwrap();
    // COALESCE, because rows written before `active` existed read NULL and a
    // bare `active = 1` would silently hide them.
    let mut stmt = db.conn()?.prepare(
        "SELECT id, name, sku, unit, COALESCE(active, 1) FROM items ORDER BY name")?;
    let rows = stmt.query_map([], |r| {
        Ok(ItemRow {
            id: r.get(0)?, name: r.get(1)?, sku: r.get(2)?, unit: r.get(3)?,
            active: r.get::<_, i64>(4)? != 0,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
}

#[derive(Serialize)]
pub struct PartyRow {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub active: bool,
}

/// Every party, archived ones included — see [`list_items`].
#[tauri::command]
pub fn list_parties(state: State<AppState>) -> Result<Vec<PartyRow>, AppError> {
    let db = state.db.lock().unwrap();
    let mut stmt = db.conn()?.prepare(
        "SELECT id, name, kind, COALESCE(active, 1) FROM parties ORDER BY name")?;
    let rows = stmt.query_map([], |r| {
        Ok(PartyRow {
            id: r.get(0)?, name: r.get(1)?, kind: r.get(2)?,
            active: r.get::<_, i64>(3)? != 0,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
}
```

- [ ] **Step 3: List open invoices for payment allocation**

Also in `commands.rs`, add after `list_payments`:

```rust
#[derive(Serialize)]
pub struct OpenInvoiceRow {
    pub id: String,
    pub date: String,
    pub total_minor: i64,
    pub outstanding_minor: i64,
}

#[derive(Deserialize)]
pub struct OpenInvoicesInput {
    pub party_id: String,
    /// `"in"` for money received (customer sales) or `"out"` for money paid
    /// (supplier purchases) — matching the payment direction the allocation
    /// will belong to.
    pub direction: String,
}

/// Unsettled invoices for one party, newest first.
///
/// `handle_payment_received` and `handle_payment_made` already accept
/// allocations, and their guards require each target to belong to the paying
/// party and to have enough outstanding balance. This query is what lets the
/// UI offer only targets that satisfy both.
#[tauri::command]
pub fn list_open_invoices(
    state: State<AppState>,
    input: OpenInvoicesInput,
) -> Result<Vec<OpenInvoiceRow>, AppError> {
    let (table, party_col) = match input.direction.as_str() {
        "in" => ("sales", "customer_id"),
        "out" => ("purchases", "supplier_id"),
        other => return Err(AppError { message: format!("invalid direction: {other}") }),
    };
    let db = state.db.lock().unwrap();
    let sql = format!(
        "SELECT id, date, total_minor, outstanding_minor FROM {table}
         WHERE {party_col} = ?1 AND reversed = 0 AND outstanding_minor > 0
         ORDER BY date DESC"
    );
    let mut stmt = db.conn()?.prepare(&sql)?;
    let rows = stmt.query_map([&input.party_id], |r| {
        Ok(OpenInvoiceRow {
            id: r.get(0)?, date: r.get(1)?,
            total_minor: r.get(2)?, outstanding_minor: r.get(3)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
}
```

- [ ] **Step 4: Register all five**

In `crates/tauri-app/src/lib.rs`, add to `tauri::generate_handler![...]`:

```rust
            commands::update_item,
            commands::update_party,
            commands::delete_item,
            commands::delete_party,
            commands::list_open_invoices,
```

- [ ] **Step 5: Build and check**

Run: `cargo build -p tauri-app && cargo clippy -p tauri-app -- -D warnings && cargo test`
Expected: clean.

Ignore rust-analyzer diagnostics in the editor: this workspace has a known proc-macro server version mismatch that reports phantom errors on `commands.rs`. `cargo` is the ground truth.

- [ ] **Step 6: Commit**

```bash
git add crates/tauri-app/src/commands.rs crates/tauri-app/src/lib.rs
git commit -m "feat(tauri): expose update, delete, and open-invoice queries"
```

---

### Task 7: Translation keys

**Files:**
- Modify: `ui/src/i18n/en.ts`
- Modify: `ui/src/i18n/fr.ts`

Both files are `as const` objects with identical shapes; the app reads `t.<section>.<key>`. Every key added to one must be added to the other or TypeScript will complain about the locale union.

- [ ] **Step 1: Extend `common` in `en.ts` (after `edit: "Correct",`)**

```ts
    archive: "Archive",
    restore: "Restore",
    delete: "Delete",
    deleteConfirm: "Permanently delete this? This cannot be undone.",
    deleted: "Deleted.",
    archived: "Archived.",
    restored: "Restored.",
    saved: "Changes saved.",
    saving: "Saving...",
    showArchived: "Show archived",
    archivedBadge: "Archived",
    status: "Status",
```

- [ ] **Step 2: The same keys in `fr.ts` (after `edit: "Corriger",`)**

```ts
    archive: "Archiver",
    restore: "Restaurer",
    delete: "Supprimer",
    deleteConfirm: "Supprimer définitivement ? Cette action est irréversible.",
    deleted: "Supprimé.",
    archived: "Archivé.",
    restored: "Restauré.",
    saved: "Modifications enregistrées.",
    saving: "Enregistrement...",
    showArchived: "Afficher les archivés",
    archivedBadge: "Archivé",
    status: "Statut",
```

- [ ] **Step 3: Extend `items` in `en.ts`**

```ts
    editTitle: "Edit Item",
```

and in `fr.ts`:

```ts
    editTitle: "Modifier l'article",
```

- [ ] **Step 4: Extend `parties` in `en.ts`**

```ts
    editTitle: "Edit Party",
    anonSupplier: "Cash Supplier",
```

and in `fr.ts` — note the app's own term for parties is "Tiers":

```ts
    editTitle: "Modifier le tiers",
    anonSupplier: "Fournisseur comptant",
```

- [ ] **Step 5: Extend `payments` in `en.ts`**

```ts
    allocate: "Settle invoices",
    allocateHint: "Leave blank to record the payment as an unallocated prepayment.",
    invoice: "Invoice",
    invoiceOutstanding: "Outstanding",
    allocateAmount: "Amount to apply",
    noOpenInvoices: "No unpaid invoices for this party.",
    allocationTotal: "Total applied",
    allocationExceeds: "Applied amount exceeds the payment.",
```

and in `fr.ts`:

```ts
    allocate: "Régler des factures",
    allocateHint: "Laissez vide pour enregistrer le paiement comme acompte.",
    invoice: "Facture",
    invoiceOutstanding: "Solde dû",
    allocateAmount: "Montant à appliquer",
    noOpenInvoices: "Aucune facture impayée pour ce tiers.",
    allocationTotal: "Total appliqué",
    allocationExceeds: "Le montant appliqué dépasse le paiement.",
```

- [ ] **Step 6: Verify**

Run: `cd ui && npx tsc --noEmit`
Expected: no errors.

- [ ] **Step 7: Commit**

```bash
git add ui/src/i18n/en.ts ui/src/i18n/fr.ts
git commit -m "feat(ui): translations for edit, archive, delete, and allocation"
```

---

### Task 8: Anonymous supplier in the shared frontend helpers

**Files:**
- Modify: `ui/src/lib.ts`

- [ ] **Step 1: Extend `lib.ts`**

Replace the walk-in block (lines 5-15) with:

```ts
// Stable IDs of the seeded shared parties (see genesis.rs). Their stored names
// are fixed English strings, so the UI translates them by ID at display time.
export const WALKIN_PARTY_ID = "party_walkin";
export const ANON_SUPPLIER_PARTY_ID = "party_anon_supplier";

// Display name for a party: the two seeded parties are localized via the
// supplied labels; every other party shows its stored name.
export function displayPartyName(
  id: string,
  storedName: string,
  walkinLabel: string,
  anonSupplierLabel?: string,
): string {
  if (id === WALKIN_PARTY_ID) return walkinLabel;
  if (id === ANON_SUPPLIER_PARTY_ID && anonSupplierLabel) return anonSupplierLabel;
  return storedName;
}
```

The fourth parameter is optional so the four existing call sites keep compiling unchanged; each is updated in the task that touches its page.

- [ ] **Step 2: Verify**

Run: `cd ui && npx tsc --noEmit`
Expected: no errors.

- [ ] **Step 3: Commit**

```bash
git add ui/src/lib.ts
git commit -m "feat(ui): recognize the anonymous supplier in party display"
```

---

### Task 9: Items page — edit, archive, delete

**Files:**
- Modify: `ui/src/pages/Items.tsx`

Keep the page's existing idiom: local `useState`, `invoke` with string literals, `useToast` for feedback, `errorMessage(e)` for errors, and a `refresh()` after every mutation. The create form already collapses under a `+` button; reuse that shape for editing rather than inventing a modal.

- [ ] **Step 1: Carry `active` on the row type and add the archived toggle**

```tsx
interface Item {
  id: string;
  name: string;
  sku: string;
  unit: string;
  active: boolean;
}
```

Add state beside the existing declarations:

```tsx
  const [showArchived, setShowArchived] = useState(false);
  const [editing, setEditing] = useState<Item | null>(null);
  const [editSku, setEditSku] = useState("");
  const [editName, setEditName] = useState("");
  const [editUnit, setEditUnit] = useState("");
```

And derive the visible list:

```tsx
  const visible = showArchived ? items : items.filter((i) => i.active);
```

- [ ] **Step 2: Add the mutation handlers**

```tsx
  const beginEdit = (i: Item) => {
    setEditing(i);
    setEditSku(i.sku);
    setEditName(i.name);
    setEditUnit(i.unit);
    setError("");
  };

  const saveEdit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!editing) return;
    setSubmitting(true);
    setError("");
    try {
      // Send only what changed: `changes` is merged into the stored document,
      // so an unchanged key is a no-op but a wrong one is a silent overwrite.
      const changes: Record<string, string> = {};
      if (editSku !== editing.sku) changes.sku = editSku;
      if (editName !== editing.name) changes.name = editName;
      if (editUnit !== editing.unit) changes.unit = editUnit;
      if (Object.keys(changes).length > 0) {
        await invoke("update_item", { input: { id: editing.id, changes } });
        toast.push(t.common.saved);
      }
      setEditing(null);
      await refresh();
    } catch (e: unknown) {
      setError(errorMessage(e));
      toast.push(errorMessage(e), "error");
    } finally {
      setSubmitting(false);
    }
  };

  const setArchived = async (i: Item, archived: boolean) => {
    try {
      await invoke("update_item", { input: { id: i.id, changes: { active: !archived } } });
      toast.push(archived ? t.common.archived : t.common.restored);
      await refresh();
    } catch (e: unknown) {
      toast.push(errorMessage(e), "error");
    }
  };

  const remove = async (i: Item) => {
    if (!window.confirm(t.common.deleteConfirm)) return;
    try {
      await invoke("delete_item", { input: { id: i.id } });
      toast.push(t.common.deleted);
      await refresh();
    } catch (e: unknown) {
      // The backend refuses to delete anything already referenced and says so;
      // surfacing its message is what tells the user to archive instead.
      toast.push(errorMessage(e), "error");
    }
  };
```

- [ ] **Step 3: Add the edit form**

Directly after the existing create `<section className="panel">`, add:

```tsx
      {editing && (
        <section className="panel">
          <h2 style={{ marginTop: 0 }}>{t.items.editTitle}</h2>
          <form onSubmit={saveEdit} className="form">
            <div className="form-row">
              <label>
                {t.items.sku}
                <input value={editSku} onChange={(e) => setEditSku(e.target.value)} required />
              </label>
              <label>
                {t.items.name}
                <input value={editName} onChange={(e) => setEditName(e.target.value)} required />
              </label>
              <label>
                {t.items.unit}
                <input value={editUnit} onChange={(e) => setEditUnit(e.target.value)} required />
              </label>
            </div>
            <div className="form-actions">
              <button type="button" className="secondary" onClick={() => setEditing(null)} disabled={submitting}>
                {t.common.cancel}
              </button>
              <button type="submit" className="primary" disabled={submitting}>
                {submitting ? t.common.saving : t.common.save}
              </button>
            </div>
            {error && <p className="error">{error}</p>}
          </form>
        </section>
      )}
```

- [ ] **Step 4: Rework the table**

Replace the table block (the `{items.length === 0 ? ... }` expression) with:

```tsx
      <label className="inline-toggle">
        <input
          type="checkbox"
          checked={showArchived}
          onChange={(e) => setShowArchived(e.target.checked)}
        />
        {t.common.showArchived}
      </label>

      {visible.length === 0 ? (
        <div className="table-wrap">
          <div className="empty">{t.items.empty}</div>
        </div>
      ) : (
        <div className="table-wrap">
          <table>
            <thead>
              <tr>
                <th>{t.items.sku}</th>
                <th>{t.items.name}</th>
                <th>{t.items.unit}</th>
                <th>{t.common.id}</th>
                <th>{t.common.actions}</th>
              </tr>
            </thead>
            <tbody>
              {visible.map((i) => (
                <tr key={i.id} className={i.active ? undefined : "row-archived"}>
                  <td>{i.sku}</td>
                  <td>
                    {i.name}
                    {!i.active && <span className="badge"> {t.common.archivedBadge}</span>}
                  </td>
                  <td>{i.unit}</td>
                  <td className="mono">{i.id.slice(0, 8)}...</td>
                  <td>
                    <button className="ghost" onClick={() => beginEdit(i)}>{t.common.edit}</button>
                    <button className="ghost" onClick={() => setArchived(i, i.active)}>
                      {i.active ? t.common.archive : t.common.restore}
                    </button>
                    <button className="ghost" onClick={() => remove(i)}>{t.common.delete}</button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
```

- [ ] **Step 5: Style the archived row**

In `ui/src/styles.css` (or whichever stylesheet the pages already use — check with `grep -rn "table-wrap" ui/src --include=*.css`), add:

```css
.row-archived { opacity: 0.55; }
.inline-toggle { display: inline-flex; align-items: center; gap: 0.4rem; margin: 0.5rem 0; }
```

- [ ] **Step 6: Verify**

Run: `cd ui && npx tsc --noEmit && npm run build`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add ui/src/pages/Items.tsx ui/src/styles.css
git commit -m "feat(ui): edit, archive, and delete items"
```

---

### Task 10: Parties page — edit, archive, delete

**Files:**
- Modify: `ui/src/pages/Parties.tsx`

The same shape as Task 9, with two differences: the editable fields are name and kind (kind is a `<select>`), and the two seeded parties must not offer archive or delete, because the backend refuses both and offering a button that always errors is worse than hiding it.

- [ ] **Step 1: Extend the row type and imports**

```tsx
import { newId, errorMessage, displayPartyName, WALKIN_PARTY_ID, ANON_SUPPLIER_PARTY_ID } from "../lib";
```

```tsx
interface Party {
  id: string;
  name: string;
  kind: PartyKind;
  active: boolean;
}
```

- [ ] **Step 2: Add state and helpers**

```tsx
  const [showArchived, setShowArchived] = useState(false);
  const [editing, setEditing] = useState<Party | null>(null);
  const [editName, setEditName] = useState("");
  const [editKind, setEditKind] = useState<PartyKind>("supplier");

  const visible = showArchived ? parties : parties.filter((p) => p.active);

  // The seeded cash-trade parties are auto-selected by the sales and purchases
  // forms, so the backend refuses to archive or delete them.
  const isSeeded = (id: string) => id === WALKIN_PARTY_ID || id === ANON_SUPPLIER_PARTY_ID;

  const partyLabel = (p: Party) =>
    displayPartyName(p.id, p.name, t.parties.walkinCustomer, t.parties.anonSupplier);
```

- [ ] **Step 3: Add the mutation handlers**

```tsx
  const beginEdit = (p: Party) => {
    setEditing(p);
    setEditName(p.name);
    setEditKind(p.kind);
    setError("");
  };

  const saveEdit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!editing) return;
    setSubmitting(true);
    setError("");
    try {
      const changes: Record<string, string> = {};
      if (editName !== editing.name) changes.name = editName;
      if (editKind !== editing.kind) changes.kind = editKind;
      if (Object.keys(changes).length > 0) {
        await invoke("update_party", { input: { id: editing.id, changes } });
        toast.push(t.common.saved);
      }
      setEditing(null);
      await refresh();
    } catch (e: unknown) {
      setError(errorMessage(e));
      toast.push(errorMessage(e), "error");
    } finally {
      setSubmitting(false);
    }
  };

  const setArchived = async (p: Party, archived: boolean) => {
    try {
      await invoke("update_party", { input: { id: p.id, changes: { active: !archived } } });
      toast.push(archived ? t.common.archived : t.common.restored);
      await refresh();
    } catch (e: unknown) {
      toast.push(errorMessage(e), "error");
    }
  };

  const remove = async (p: Party) => {
    if (!window.confirm(t.common.deleteConfirm)) return;
    try {
      await invoke("delete_party", { input: { id: p.id } });
      toast.push(t.common.deleted);
      await refresh();
    } catch (e: unknown) {
      // The backend explains when a party has traded and must be archived
      // rather than removed.
      toast.push(errorMessage(e), "error");
    }
  };
```

- [ ] **Step 4: Add the edit form after the create panel**

```tsx
      {editing && (
        <section className="panel">
          <h2 style={{ marginTop: 0 }}>{t.parties.editTitle}</h2>
          <form onSubmit={saveEdit} className="form">
            <div className="form-row">
              <label>
                {t.parties.name}
                <input value={editName} onChange={(e) => setEditName(e.target.value)} required />
              </label>
              <label>
                {t.parties.kind}
                <select value={editKind} onChange={(e) => setEditKind(e.target.value as PartyKind)}>
                  <option value="supplier">{t.parties.supplier}</option>
                  <option value="customer">{t.parties.customer}</option>
                  <option value="both">{t.parties.both}</option>
                </select>
              </label>
            </div>
            <div className="form-actions">
              <button type="button" className="secondary" onClick={() => setEditing(null)} disabled={submitting}>
                {t.common.cancel}
              </button>
              <button type="submit" className="primary" disabled={submitting}>
                {submitting ? t.common.saving : t.common.save}
              </button>
            </div>
            {error && <p className="error">{error}</p>}
          </form>
        </section>
      )}
```

- [ ] **Step 5: Rework the table**

```tsx
      <label className="inline-toggle">
        <input
          type="checkbox"
          checked={showArchived}
          onChange={(e) => setShowArchived(e.target.checked)}
        />
        {t.common.showArchived}
      </label>

      {visible.length === 0 ? (
        <div className="table-wrap">
          <div className="empty">{t.parties.empty}</div>
        </div>
      ) : (
        <div className="table-wrap">
          <table>
            <thead>
              <tr>
                <th>{t.parties.name}</th>
                <th>{t.parties.kind}</th>
                <th>{t.common.id}</th>
                <th>{t.common.actions}</th>
              </tr>
            </thead>
            <tbody>
              {visible.map((p) => (
                <tr key={p.id} className={p.active ? undefined : "row-archived"}>
                  <td>
                    {partyLabel(p)}
                    {!p.active && <span className="badge"> {t.common.archivedBadge}</span>}
                  </td>
                  <td>
                    <span className={`badge ${kindClass(p.kind)}`}>{kindLabel(p.kind)}</span>
                  </td>
                  <td className="mono">{p.id.slice(0, 8)}...</td>
                  <td>
                    <button className="ghost" onClick={() => beginEdit(p)}>{t.common.edit}</button>
                    {!isSeeded(p.id) && (
                      <>
                        <button className="ghost" onClick={() => setArchived(p, p.active)}>
                          {p.active ? t.common.archive : t.common.restore}
                        </button>
                        <button className="ghost" onClick={() => remove(p)}>{t.common.delete}</button>
                      </>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
```

- [ ] **Step 6: Verify**

Run: `cd ui && npx tsc --noEmit && npm run build`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add ui/src/pages/Parties.tsx
git commit -m "feat(ui): edit, archive, and delete parties"
```

---

### Task 11: Cash by default, and the anonymous supplier on purchases

**Files:**
- Modify: `ui/src/pages/Sales.tsx:51,121`
- Modify: `ui/src/pages/Purchases.tsx:51,117`

Most trade in this business is cash, so credit-by-default made the common case cost an extra click and, worse, silently created receivables nobody was tracking.

- [ ] **Step 1: Sales — default to cash**

In `ui/src/pages/Sales.tsx`, line 51:

```tsx
  const [terms, setTerms] = useState<Terms>("cash");
```

and in the post-submit reset (line 121):

```tsx
      setTerms("cash");
```

- [ ] **Step 2: Sales — hide archived items and parties**

The existing walk-in auto-select effect (lines 87-89) already covers the customer side. Update the two derived lists:

```tsx
  // Archived master data stays visible in history but must not be offered for
  // new transactions.
  const customers = parties.filter((p) => p.active && (p.kind === "customer" || p.kind === "both"));
  const activeItems = items.filter((i) => i.active);
```

Add `active: boolean` to the local `Party` and `Item` interfaces, and use `activeItems` in the line-item `<select>` in place of `items`.

- [ ] **Step 3: Purchases — default to cash and auto-select the anonymous supplier**

In `ui/src/pages/Purchases.tsx`, line 51:

```tsx
  const [terms, setTerms] = useState<Terms>("cash");
```

Line 117:

```tsx
      setTerms("cash");
```

Add the mirror of the Sales walk-in effect, after the keyboard-shortcut effect:

```tsx
  // Mirrors the walk-in customer on the sales form: a cash purchase from an
  // unrecorded seller needs no named supplier, so default to the seeded one.
  useEffect(() => {
    if (terms === "cash" && !supplierId) setSupplierId(ANON_SUPPLIER_PARTY_ID);
  }, [terms, supplierId]);
```

Extend the import:

```tsx
import { majorToMinor, newId, today, errorMessage, displayPartyName, ANON_SUPPLIER_PARTY_ID } from "../lib";
```

- [ ] **Step 4: Purchases — hide archived, and localize the supplier name**

```tsx
  const suppliers = parties.filter((p) => p.active && (p.kind === "supplier" || p.kind === "both"));
  const activeItems = items.filter((i) => i.active);

  const supplierName = (id: string) =>
    displayPartyName(
      id,
      parties.find((p) => p.id === id)?.name ?? id,
      t.parties.walkinCustomer,
      t.parties.anonSupplier,
    );
```

Add `active: boolean` to the local `Party` and `Item` interfaces. Use `activeItems` in the line-item `<select>`, and `supplierName(p.id)` in the supplier `<option>` label (currently the raw `p.name` at line 181) so the seeded supplier reads as "Cash Supplier" / "Fournisseur comptant".

- [ ] **Step 5: Verify**

Run: `cd ui && npx tsc --noEmit && npm run build`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add ui/src/pages/Sales.tsx ui/src/pages/Purchases.tsx
git commit -m "feat(ui): default sales and purchases to cash, seed the anonymous supplier"
```

---

### Task 12: Let payments settle invoices

**Files:**
- Modify: `ui/src/pages/Payments.tsx`

`allocations: []` at lines 71-72 is why an invoice could never be marked paid: the payment posted to the party's balance as an unallocated prepayment and the invoice's `outstanding_minor` never moved. The backend has always accepted allocations (F4); this is purely the missing UI.

- [ ] **Step 1: Add the open-invoice state**

```tsx
interface OpenInvoice {
  id: string;
  date: string;
  total_minor: number;
  outstanding_minor: number;
}
```

```tsx
  const [openInvoices, setOpenInvoices] = useState<OpenInvoice[]>([]);
  // Invoice id -> major-unit string the user typed. Absent or blank means
  // "apply nothing to this invoice".
  const [applied, setApplied] = useState<Record<string, string>>({});
```

Also add `active: boolean` to the local `Party` interface and filter the dropdown:

```tsx
  const eligible =
    direction === "in"
      ? parties.filter((p) => p.active && (p.kind === "customer" || p.kind === "both"))
      : parties.filter((p) => p.active && (p.kind === "supplier" || p.kind === "both"));
```

- [ ] **Step 2: Load open invoices when the party or direction changes**

```tsx
  useEffect(() => {
    setApplied({});
    if (!partyId) {
      setOpenInvoices([]);
      return;
    }
    let cancelled = false;
    invoke<OpenInvoice[]>("list_open_invoices", { input: { party_id: partyId, direction } })
      .then((rows) => {
        // Guard against a stale response landing after the user has moved on.
        if (!cancelled) setOpenInvoices(rows);
      })
      .catch((e: unknown) => {
        if (!cancelled) setError(errorMessage(e));
      });
    return () => {
      cancelled = true;
    };
  }, [partyId, direction]);
```

- [ ] **Step 3: Build the allocations and validate before submitting**

```tsx
  const allocations = () =>
    Object.entries(applied)
      .filter(([, major]) => major.trim() !== "" && Number(major) > 0)
      .map(([target_id, major]) => ({
        target_id,
        // `check_allocation_party_ownership` requires the target type to match
        // the direction: money in settles sales, money out settles purchases.
        target_type: direction === "in" ? "sale" : "purchase",
        amount_minor: majorToMinor(major),
      }));

  const allocatedMinor = allocations().reduce((sum, a) => sum + a.amount_minor, 0);
```

Rewrite `submit`:

```tsx
  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setError("");
    const amount_minor = majorToMinor(amountMajor);
    const allocs = allocations();
    const total = allocs.reduce((sum, a) => sum + a.amount_minor, 0);
    if (total > amount_minor) {
      // The backend rejects this too; catching it here avoids a round trip and
      // gives the message in the user's language.
      setError(t.payments.allocationExceeds);
      return;
    }
    setSubmitting(true);
    try {
      const command = direction === "in" ? "record_payment" : "record_payment_made";
      const base = { id: newId(), amount_minor, date, allocations: allocs };
      const input =
        direction === "in"
          ? { ...base, customer_id: partyId }
          : { ...base, supplier_id: partyId };
      await invoke(command, { input });
      toast.push(direction === "in" ? t.payments.added : t.payments.paidMade);
      setPartyId("");
      setAmountMajor("");
      setApplied({});
      setDate(today());
      await refresh();
    } catch (e: unknown) {
      setError(errorMessage(e));
      toast.push(errorMessage(e), "error");
    } finally {
      setSubmitting(false);
    }
  };
```

- [ ] **Step 4: Render the allocation section**

Inside the form, after the `form-row` and before `form-actions`:

```tsx
          {partyId && (
            <div className="lines">
              <div className="lines-header">
                <strong>{t.payments.allocate}</strong>
                <span className="shortcut-hint">{t.payments.allocateHint}</span>
              </div>
              {openInvoices.length === 0 ? (
                <div className="empty">{t.payments.noOpenInvoices}</div>
              ) : (
                <>
                  <table>
                    <thead>
                      <tr>
                        <th>{t.payments.invoice}</th>
                        <th>{t.common.date}</th>
                        <th className="num">{t.payments.invoiceOutstanding}</th>
                        <th className="num">{t.payments.allocateAmount}</th>
                      </tr>
                    </thead>
                    <tbody>
                      {openInvoices.map((inv) => (
                        <tr key={inv.id}>
                          <td className="mono">{inv.id.slice(0, 8)}...</td>
                          <td>{inv.date}</td>
                          <td className="num">{format(inv.outstanding_minor)}</td>
                          <td className="num">
                            <input
                              type="number"
                              step="0.01"
                              min="0"
                              // The backend refuses to over-allocate an
                              // invoice; capping here says so before submitting.
                              max={inv.outstanding_minor / 100}
                              placeholder="0.00"
                              value={applied[inv.id] ?? ""}
                              onChange={(e) =>
                                setApplied((prev) => ({ ...prev, [inv.id]: e.target.value }))
                              }
                            />
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                  <p>
                    {t.payments.allocationTotal}: {format(allocatedMinor)}
                  </p>
                </>
              )}
            </div>
          )}
```

- [ ] **Step 5: Localize the anonymous supplier here too**

Update both `displayPartyName` calls in this file (lines 61 and 123) to pass `t.parties.anonSupplier` as the fourth argument.

- [ ] **Step 6: Verify**

Run: `cd ui && npx tsc --noEmit && npm run build`
Expected: clean.

- [ ] **Step 7: Manual check**

Run `npm run tauri dev` from the repo root (or however the project starts — check `package.json`). Then: record a credit sale to a named customer; record a payment received from that customer and apply the full amount to the invoice; confirm the Sales page shows `outstanding` at zero. This is the behaviour that was impossible before, so it is worth seeing once rather than only in a type check.

- [ ] **Step 8: Commit**

```bash
git add ui/src/pages/Payments.tsx
git commit -m "fix(ui): pass real allocations so payments settle invoices"
```

---

### Task 13: Record the decisions

**Files:**
- Create: `docs/superpowers/plans/2026-08-03-crudl-master-data-decisions.md`

- [ ] **Step 1: Write the decision log**

Record, with enough detail that a reviewer can disagree productively:

- **D1** Delete is two-tiered — archive via `ItemUpdated`/`PartyUpdated` with `active=false`, hard delete via new events only when unreferenced. Why: a small business needs to retire a discontinued product without erasing the sales that mention it, and needs to remove a typo'd record cleanly.
- **D2** `parties.active` required an explicit `ALTER TABLE` migration. Why: F1 — `CREATE TABLE IF NOT EXISTS` is a no-op on an existing table and `rebuild` uses `DELETE FROM`, not `DROP`, so a v0.1.2 install would otherwise fail every query naming the column.
- **D3** All `active` filters are null-safe (`COALESCE(active, 1) = 1`). Why: F2 — party rows predating the field read NULL, and a bare `active = 1` would silently hide every existing party.
- **D4** The delete projector degrades to archive when references exist, and both delete and update projectors treat a missing row as a no-op. Why: F3 — foreign keys are enforced and `init_state` calls `rebuild(...).expect(...)`, so an unprojectable event does not produce an error message, it makes the app unlaunchable. A merged log can legitimately order a delete before a transaction that uses the row. Guards are strict pre-commit; projection must be total.
- **D5** `ItemDeleted`/`PartyDeleted` are non-transactional, hence not reversible. Why: F5 — undoing a hard delete means recreating the record, and archive already covers the reversible case.
- **D6** The two seeded parties can be renamed but neither archived nor deleted, enforced in the command layer and hidden in the UI. Why: F6 — both are auto-selected by the cash paths.
- **D7** Payment allocation needed no backend change. Why: F4 — both handlers already accept allocations and the payload contract with the projector already agrees; only `Payments.tsx` was passing `[]`. Note that the initial suspicion of a producer/consumer field-name mismatch here was checked and found to be wrong.
- **D8** A sku-collision guard was added to `handle_item_updated`. Why: `items_sku` is UNIQUE, so a colliding rename failed inside the projector — safe, since the commit rolls back, but the user saw a raw database error instead of an explanation.
- **D9** Out of scope, documented deliberately: accounts, users, expenses, transfers, returns, inventory adjustments. `handle_account_updated` and `handle_user_updated` exist and remain unreachable from the UI — the same gap this work closed for items and parties.

- [ ] **Step 2: Commit**

```bash
git add docs/superpowers/plans/2026-08-03-crudl-master-data-decisions.md
git commit -m "docs: decisions for the master-data CRUDL work"
```

---

## Final Verification

- [ ] `cargo test` — the whole workspace passes
- [ ] `cargo clippy --all-targets -- -D warnings` — clean
- [ ] `cd ui && npx tsc --noEmit && npm run build` — clean
- [ ] Launch the app against an **existing** ledger (not a fresh one) and confirm the parties page loads — this is the migration path from F1, and it is the one thing no unit test fully covers
- [ ] Archive an item, confirm it disappears from the new-sale dropdown and still appears on past sales
- [ ] Delete an unused item; try to delete a sold one and confirm the refusal explains itself
- [ ] Record a cash purchase without touching the supplier field and confirm it books against the Cash Supplier
