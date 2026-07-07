# Accounting Core — Read Model, Projectors & Rebuild Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the read-model layer for the local-first accounting app — every projection table from spec §5–§6, a single `apply_event` dispatcher with one handler per event type, and a drop-and-replay `rebuild` — on top of Plan 1's event store. All state is derived from the event log; projections stay 100% rebuildable. Fully tested against in-memory SQLite, zero Tauri dependency.

**Architecture:** Event-sourced / CQRS. Plan 1 built the append-only `events` table (source of truth), the HLC clock, `append_event` / `read_events`, and genesis. This plan builds the **projector**: it reads immutable events and writes denormalized read tables. Money is integer minor units throughout; quantities are integers; business dates are `TEXT 'YYYY-MM-DD'`. Well-known accounts are resolved by `system_role`, never by name or id. The write path in Plan 3 will call `append_event` **and** `apply_event` inside one SQLite transaction; this plan drives the projector directly from tests using the same transactional pattern.

**Tech Stack:** Rust, `rusqlite` 0.32 (with `bundled` SQLite 3.46.x, providing JSONB and generated columns), `serde_json`. Tests via `cargo test` against in-memory SQLite.

**Spec:** `docs/superpowers/specs/2026-07-06-accounting-schema-design.md`

---

## Plan Series Roadmap

This is **Plan 2 of 4** for the data model (the *read model / projector* plan), per the authoritative roadmap established in Plan 1. Each produces working, testable software and depends on the prior:

> **Header-numbering note (reconciling review feedback):** a review referred to this as "Plan 3 in build order." That is a numbering slip: Plan 1's roadmap fixes this as **Plan 2 — read model + projectors**, which *precedes* Plan 3 — command handlers + guards (guards read the projections this plan builds). This document keeps the "Plan 2 of 4" label to stay consistent with Plan 1. Any in-body reference to "the command-handlers plan" means **Plan 3**.


1. **Foundations (done):** crate scaffold, event-store schema DDL, HLC clock, event store append/read, genesis bootstrap.
2. **Read model + projectors + rebuild (this plan):** all §5–§6 projection-table DDL, `apply_event` per event type, the drop-and-replay `rebuild` loop, `projection_cursor`. Built and tested by appending events (via Plan 1's `append_event`) and asserting projected state + rebuild determinism.
3. **Command handlers + guards:** the ~15 command entry points, each running its §4.5 validation guards *against the read model this plan builds*, then appending the event and applying the projection in one transaction.
4. **Reconciliation + queries:** the §7 integrity checks and the §8 report queries.

(Order note: projectors precede command handlers because the write path validates against the read model — guards query projections, so projections must exist first. This plan therefore builds **projection only**; it does not enforce the §4.5 command guards — those are Plan 3. The projector trusts that events written to the log already passed their guards, exactly as replay/sync-merge will.)

---

## File Structure

All paths are inside the existing crate at `crates/accounting-core/`.

- `src/schema.sql` — **modified**: append all §5.2–§5.5 and §6.1–§6.9 projection-table DDL after the Plan 1 `events` + `projection_cursor` tables. Portable SQL asset; one physical schema for the whole crate.
- `src/projectors.rs` — **created**: the `apply_event(tx, &LedgerEvent)` dispatcher plus one private handler per event type, shared posting/balance helpers, and the `rebuild` function. One responsibility: turning events into read-model rows.
- `src/lib.rs` — **modified**: add `pub mod projectors;` and re-export `apply_event` and `rebuild`.

No other files change. `db.rs`, `hlc.rs`, `events.rs`, `genesis.rs` are consumed as built in Plan 1.

---

### Task 1: Projection-table schema DDL

**Files:**
- Modify: `crates/accounting-core/src/schema.sql`

- [ ] **Step 1: Write the failing test that every projection table exists**

Add a new test module at the bottom of `crates/accounting-core/src/db.rs`'s `tests` module (inside the existing `mod tests { ... }` block from Plan 1):

```rust
    #[test]
    fn apply_schema_creates_all_projection_tables() {
        let conn = open_in_memory_with_schema().unwrap();
        let expected = [
            "users", "accounts", "items", "inventory_lots", "parties",
            "journal_lines", "sales", "sale_lines", "lot_consumptions",
            "purchases", "purchase_lines", "payments", "payment_allocations",
            "party_balances", "returns", "return_lines", "expenses",
            "events", "projection_cursor",
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

        // The `reversed` void-marker column (spec §6.2/§6.3) must exist on both
        // sales and purchases — clause 4 of the reversal contract sets it.
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
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p accounting-core apply_schema_creates_all_projection_tables`
Expected: FAIL — assertion fails on the first missing table (e.g. `table users should exist`).

- [ ] **Step 3: Append the projection DDL to `schema.sql`**

Append the following to `crates/accounting-core/src/schema.sql`, **after** the Plan 1 `events` and `projection_cursor` statements. This is copied verbatim from spec §5.2–§5.5 and §6.1–§6.9 (generated columns, indexes, and partial indexes preserved), wrapped in `IF NOT EXISTS` so `apply_schema` stays idempotent:

```sql
-- ============================================================
-- Master data projections (spec §5)
-- ============================================================

-- §5.1 users — the who
CREATE TABLE IF NOT EXISTS users (
  id          TEXT PRIMARY KEY,
  doc         BLOB NOT NULL,                                 -- jsonb
  name        TEXT GENERATED ALWAYS AS (doc ->> 'name') VIRTUAL,
  created_at  INTEGER NOT NULL
);

-- §5.2 accounts — chart of accounts
CREATE TABLE IF NOT EXISTS accounts (
  id            TEXT PRIMARY KEY,
  doc           BLOB NOT NULL,
  name          TEXT GENERATED ALWAYS AS (doc ->> 'name')        VIRTUAL,
  type          TEXT GENERATED ALWAYS AS (doc ->> 'type')        VIRTUAL,  -- asset|liability|equity|income|expense
  normal_side   TEXT GENERATED ALWAYS AS (doc ->> 'normal')      VIRTUAL,  -- 'debit'|'credit'
  system_role   TEXT GENERATED ALWAYS AS (doc ->> 'system_role') VIRTUAL,  -- nullable; well-known account identifier
  balance_minor INTEGER NOT NULL DEFAULT 0                                 -- running, maintained by projector
);
CREATE INDEX IF NOT EXISTS accounts_type ON accounts (type);
CREATE UNIQUE INDEX IF NOT EXISTS accounts_system_role ON accounts (system_role) WHERE system_role IS NOT NULL;

-- §5.3 items — catalog
CREATE TABLE IF NOT EXISTS items (
  id     TEXT PRIMARY KEY,
  doc    BLOB NOT NULL,
  sku    TEXT GENERATED ALWAYS AS (doc ->> 'sku')    VIRTUAL,
  name   TEXT GENERATED ALWAYS AS (doc ->> 'name')   VIRTUAL,
  unit   TEXT GENERATED ALWAYS AS (doc ->> 'unit')   VIRTUAL,
  active INTEGER GENERATED ALWAYS AS (doc ->> 'active') VIRTUAL
);
CREATE UNIQUE INDEX IF NOT EXISTS items_sku ON items (sku);

-- §5.4 inventory_lots — the crux
CREATE TABLE IF NOT EXISTS inventory_lots (
  id              TEXT PRIMARY KEY,
  item_id         TEXT NOT NULL REFERENCES items(id),
  source_event_id TEXT NOT NULL,             -- event that created this lot (any lot-creating type)
  purchase_id     TEXT,                      -- nullable: null for opening-balance / found lots
  unit_cost_minor INTEGER NOT NULL,          -- exact acquisition cost per unit (immutable)
  qty_received    INTEGER NOT NULL,          -- original quantity
  qty_remaining   INTEGER NOT NULL,          -- decremented as sales consume; never negative
  acquired_at     TEXT NOT NULL,             -- 'YYYY-MM-DD' business date → drives inventory AGE
  supplier_id     TEXT
);
CREATE INDEX IF NOT EXISTS lots_item_open ON inventory_lots (item_id, acquired_at)
  WHERE qty_remaining > 0;                    -- partial index: fast "oldest open lot" / on-hand

-- §5.5 parties — suppliers & customers
CREATE TABLE IF NOT EXISTS parties (
  id   TEXT PRIMARY KEY,
  doc  BLOB NOT NULL,
  name TEXT GENERATED ALWAYS AS (doc ->> 'name') VIRTUAL,
  kind TEXT GENERATED ALWAYS AS (doc ->> 'kind') VIRTUAL   -- 'supplier'|'customer'|'both'
);
CREATE INDEX IF NOT EXISTS parties_kind ON parties (kind);

-- ============================================================
-- Transactional read model (spec §6)
-- ============================================================

-- §6.1 journal_lines — universal double-entry ledger
CREATE TABLE IF NOT EXISTS journal_lines (
  id           TEXT PRIMARY KEY,
  event_id     TEXT NOT NULL,              -- source event (→ audit: userId, deviceId, hlc)
  txn_id       TEXT NOT NULL,              -- groups the lines of one transaction
  account_id   TEXT NOT NULL REFERENCES accounts(id),
  debit_minor  INTEGER NOT NULL DEFAULT 0,
  credit_minor INTEGER NOT NULL DEFAULT 0,
  date         TEXT NOT NULL,             -- business date 'YYYY-MM-DD'
  memo         TEXT
);
CREATE INDEX IF NOT EXISTS jl_account_date ON journal_lines (account_id, date);
CREATE INDEX IF NOT EXISTS jl_txn          ON journal_lines (txn_id);
CREATE INDEX IF NOT EXISTS jl_date         ON journal_lines (date);

-- §6.2 sales + sale_lines + lot_consumptions — profit engine
CREATE TABLE IF NOT EXISTS sales (
  id                TEXT PRIMARY KEY,
  event_id          TEXT NOT NULL,
  customer_id       TEXT,
  date              TEXT NOT NULL,
  terms             TEXT NOT NULL,       -- 'cash'|'credit'
  total_minor       INTEGER NOT NULL,
  outstanding_minor INTEGER NOT NULL DEFAULT 0, -- derived: total_minor − allocated; 0 for cash
  reversed          INTEGER NOT NULL DEFAULT 0  -- set to 1 by the projector when a TransactionReversed voids this sale
);
CREATE INDEX IF NOT EXISTS sales_date ON sales (date);
CREATE INDEX IF NOT EXISTS sales_outstanding ON sales (outstanding_minor) WHERE outstanding_minor > 0;

CREATE TABLE IF NOT EXISTS sale_lines (
  id               TEXT PRIMARY KEY,
  sale_id          TEXT NOT NULL REFERENCES sales(id),
  item_id          TEXT NOT NULL REFERENCES items(id),
  qty              INTEGER NOT NULL,
  unit_price_minor INTEGER NOT NULL,      -- what customer paid per unit
  revenue_minor    INTEGER NOT NULL,      -- qty * unit_price (frozen)
  cogs_minor       INTEGER NOT NULL,      -- frozen cost of goods for this line
  date             TEXT NOT NULL          -- denormalized from sale for fast filtering
);
CREATE INDEX IF NOT EXISTS sl_item_date ON sale_lines (item_id, date);
CREATE INDEX IF NOT EXISTS sl_sale      ON sale_lines (sale_id);

CREATE TABLE IF NOT EXISTS lot_consumptions (
  id              TEXT PRIMARY KEY,
  sale_line_id    TEXT NOT NULL REFERENCES sale_lines(id),
  lot_id          TEXT NOT NULL REFERENCES inventory_lots(id),
  qty_taken       INTEGER NOT NULL,
  unit_cost_minor INTEGER NOT NULL        -- copied from the lot at sale time (frozen)
);
CREATE INDEX IF NOT EXISTS lc_lot ON lot_consumptions (lot_id);

-- §6.3 purchases + purchase_lines
CREATE TABLE IF NOT EXISTS purchases (
  id                TEXT PRIMARY KEY,
  event_id          TEXT NOT NULL,
  supplier_id       TEXT,
  date              TEXT NOT NULL,
  terms             TEXT NOT NULL,
  total_minor       INTEGER NOT NULL,
  outstanding_minor INTEGER NOT NULL DEFAULT 0, -- derived: total_minor − allocated; 0 for cash
  reversed          INTEGER NOT NULL DEFAULT 0  -- set to 1 by the projector when a TransactionReversed voids this purchase
);
CREATE INDEX IF NOT EXISTS purchases_date ON purchases (date);
CREATE INDEX IF NOT EXISTS purchases_outstanding ON purchases (outstanding_minor) WHERE outstanding_minor > 0;

CREATE TABLE IF NOT EXISTS purchase_lines (
  id              TEXT PRIMARY KEY,
  purchase_id     TEXT NOT NULL REFERENCES purchases(id),
  item_id         TEXT NOT NULL REFERENCES items(id),
  qty             INTEGER NOT NULL,
  unit_cost_minor INTEGER NOT NULL,
  lot_id          TEXT NOT NULL REFERENCES inventory_lots(id)  -- the lot this line created
);
CREATE INDEX IF NOT EXISTS pl_purchase ON purchase_lines (purchase_id);
CREATE INDEX IF NOT EXISTS pl_item     ON purchase_lines (item_id);

-- §6.4 payments
CREATE TABLE IF NOT EXISTS payments (
  id           TEXT PRIMARY KEY,
  event_id     TEXT NOT NULL,
  party_id     TEXT NOT NULL,
  direction    TEXT NOT NULL,          -- 'in' (received) | 'out' (made)
  amount_minor INTEGER NOT NULL,
  date         TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS payments_party_date ON payments (party_id, date);
CREATE INDEX IF NOT EXISTS payments_date        ON payments (date);

-- §6.5 payment_allocations
CREATE TABLE IF NOT EXISTS payment_allocations (
  id              TEXT PRIMARY KEY,
  event_id        TEXT NOT NULL,              -- event that recorded THIS row (PaymentMade/Received, or a later PaymentAllocated)
  payment_id      TEXT NOT NULL,              -- the source payment whose money/credit is applied
  target_id       TEXT NOT NULL,              -- the purchase or sale being settled
  target_type     TEXT NOT NULL,              -- 'purchase'|'sale'
  amount_minor    INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS pa_payment ON payment_allocations (payment_id);
CREATE INDEX IF NOT EXISTS pa_target  ON payment_allocations (target_id);

-- §6.6 party_balances
CREATE TABLE IF NOT EXISTS party_balances (
  party_id             TEXT PRIMARY KEY REFERENCES parties(id),
  receivable_minor     INTEGER NOT NULL DEFAULT 0,   -- customer owes us
  payable_minor        INTEGER NOT NULL DEFAULT 0,   -- we owe supplier
  unallocated_cr_minor INTEGER NOT NULL DEFAULT 0,   -- customer prepayments (credits we hold)
  unallocated_dr_minor INTEGER NOT NULL DEFAULT 0    -- supplier prepayments (deposits we've paid)
);

-- §6.8 returns + return_lines
CREATE TABLE IF NOT EXISTS returns (
  id                    TEXT PRIMARY KEY,
  event_id              TEXT NOT NULL,
  return_type           TEXT NOT NULL,      -- 'sale_return'|'purchase_return'
  original_id           TEXT NOT NULL,      -- the sale or purchase being returned against
  date                  TEXT NOT NULL,
  revenue_reversed_minor INTEGER NOT NULL DEFAULT 0,  -- sale returns: qty × original sale price
  cost_restored_minor    INTEGER NOT NULL DEFAULT 0   -- inventory value put back at lot cost
);
CREATE INDEX IF NOT EXISTS returns_original ON returns (original_id);

CREATE TABLE IF NOT EXISTS return_lines (
  id               TEXT PRIMARY KEY,
  return_id        TEXT NOT NULL REFERENCES returns(id),
  item_id          TEXT NOT NULL REFERENCES items(id),
  qty              INTEGER NOT NULL,
  unit_price_minor INTEGER NOT NULL DEFAULT 0,  -- frozen from original sale (sale returns); 0 for purchase returns
  unit_cost_minor  INTEGER NOT NULL,            -- the lot cost restored
  lot_id           TEXT NOT NULL REFERENCES inventory_lots(id)
);
CREATE INDEX IF NOT EXISTS rl_return ON return_lines (return_id);
CREATE INDEX IF NOT EXISTS rl_item   ON return_lines (item_id);

-- §6.9 expenses
CREATE TABLE IF NOT EXISTS expenses (
  id           TEXT PRIMARY KEY,
  event_id     TEXT NOT NULL,
  account_id   TEXT NOT NULL REFERENCES accounts(id),  -- which expense account (rent, wages, etc.)
  amount_minor INTEGER NOT NULL,
  date         TEXT NOT NULL,
  memo         TEXT,
  terms        TEXT NOT NULL              -- 'cash'|'credit'
);
CREATE INDEX IF NOT EXISTS expenses_date    ON expenses (date);
CREATE INDEX IF NOT EXISTS expenses_account ON expenses (account_id, date);
```

Note: `projection_cursor` (spec §6.10) already exists from Plan 1 — do not re-declare it.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p accounting-core apply_schema_creates_all_projection_tables`
Expected: PASS. Also run `cargo test -p accounting-core apply_schema` to confirm Plan 1's idempotency test still passes with the enlarged schema.

- [ ] **Step 5: Commit**

```bash
git add crates/accounting-core/src/schema.sql crates/accounting-core/src/db.rs
git commit -m "feat: add all read-model projection tables to schema"
```

---

### Task 2: Projector dispatcher + master-data creation handlers

**Files:**
- Create: `crates/accounting-core/src/projectors.rs`
- Modify: `crates/accounting-core/src/lib.rs`

Handles the four setup events (§4.4 setup/master-data): `UserRegistered`, `AccountOpened`, `ItemDefined`, `PartyCreated`. None post journal lines. Each stores a `doc` JSONB body from which the generated columns project. Row ids come from the payload (deterministic, rebuild-safe per §4.5).

- [ ] **Step 1: Write the failing test for master-data creation**

Create `crates/accounting-core/src/projectors.rs`:

```rust
use crate::events::LedgerEvent;
use rusqlite::Connection;
use serde_json::{json, Value};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory_with_schema;
    use crate::events::append_event;
    use crate::hlc::Hlc;

    /// Append one event and project it inside a single transaction — the exact
    /// atomicity boundary Plan 3's command handlers will use. Returns the event.
    fn record(conn: &mut Connection, hlc: &mut Hlc, phys: u64, ty: &str, payload: Value) -> LedgerEvent {
        let tx = conn.transaction().unwrap();
        let ev = append_event(&tx, hlc, phys, "devA", "userX", ty, &payload).unwrap();
        apply_event(&tx, &ev).unwrap();
        tx.commit().unwrap();
        ev
    }

    #[test]
    fn creates_user_account_item_party_rows() {
        let mut conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("devA");

        record(&mut conn, &mut hlc, 1000, "UserRegistered",
            json!({"userId": "u1", "name": "Jane"}));
        record(&mut conn, &mut hlc, 1000, "AccountOpened",
            json!({"accountId": "a_inv", "name": "Inventory", "type": "asset",
                   "normal": "debit", "system_role": "inventory"}));
        record(&mut conn, &mut hlc, 1000, "ItemDefined",
            json!({"itemId": "i1", "sku": "SKU1", "name": "Widget", "unit": "ea"}));
        record(&mut conn, &mut hlc, 1000, "PartyCreated",
            json!({"partyId": "p1", "name": "Acme", "kind": "supplier"}));

        // users: generated `name` column projects from doc.
        let uname: String = conn.query_row("SELECT name FROM users WHERE id='u1'", [], |r| r.get(0)).unwrap();
        assert_eq!(uname, "Jane");
        // accounts: resolvable by system_role, balance starts at 0.
        let (atype, bal): (String, i64) = conn.query_row(
            "SELECT type, balance_minor FROM accounts WHERE system_role = 'inventory'",
            [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(atype, "asset");
        assert_eq!(bal, 0);
        // items: active defaults to 1 (so user-facing selectors don't drop fresh items).
        let active: i64 = conn.query_row("SELECT active FROM items WHERE id='i1'", [], |r| r.get(0)).unwrap();
        assert_eq!(active, 1);
        // parties.
        let kind: String = conn.query_row("SELECT kind FROM parties WHERE id='p1'", [], |r| r.get(0)).unwrap();
        assert_eq!(kind, "supplier");
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p accounting-core creates_user_account_item_party_rows`
Expected: FAIL — `apply_event` not found (compile error).

- [ ] **Step 3: Implement the dispatcher, helpers, and the four create handlers**

At the top of `crates/accounting-core/src/projectors.rs`, above the test module:

```rust
// ---- small payload accessors (integer minor units; missing → sensible default) ----
fn pi(v: &Value, k: &str) -> i64 { v.get(k).and_then(Value::as_i64).unwrap_or(0) }
fn ps<'a>(v: &'a Value, k: &str) -> &'a str { v.get(k).and_then(Value::as_str).unwrap_or("") }
fn pos<'a>(v: &'a Value, k: &str) -> Option<&'a str> { v.get(k).and_then(Value::as_str) }
fn parr<'a>(v: &'a Value, k: &str) -> &'a [Value] {
    v.get(k).and_then(Value::as_array).map(|a| a.as_slice()).unwrap_or(&[])
}

/// Central dispatcher: apply one event to the read model. `tx` is any connection;
/// Plan 3 passes a `Transaction` (which derefs to `Connection`) so the event insert
/// and this projection commit atomically.
///
/// CURSOR OBLIGATION (Plan 3): `apply_event` does NOT advance `projection_cursor`
/// — only `rebuild` does (it owns the whole-log replay). On the incremental write
/// path, Plan 3's command handler MUST, inside the SAME transaction as
/// `append_event` + `apply_event`, update `projection_cursor` to `ev.hlc`
/// (`INSERT ... ON CONFLICT(projection) DO UPDATE SET last_hlc = excluded.last_hlc`).
/// Skipping this leaves the cursor stale, so incremental resume-after-restart and
/// sync-merge replay-from-cursor never advance. It is deliberately the caller's job
/// so `apply_event` stays a pure per-event projector reusable by both `rebuild` and
/// the live path.
pub fn apply_event(tx: &Connection, ev: &LedgerEvent) -> rusqlite::Result<()> {
    let p = &ev.payload;
    match ev.event_type.as_str() {
        // ---- master data: create (no journal) ----
        "UserRegistered" => {
            tx.execute(
                "INSERT INTO users (id, doc, created_at) VALUES (?1, jsonb(?2), ?3)",
                rusqlite::params![ps(p, "userId"), p.to_string(), ev.created_at],
            )?;
            Ok(())
        }
        "AccountOpened" => {
            tx.execute(
                "INSERT INTO accounts (id, doc, balance_minor) VALUES (?1, jsonb(?2), 0)",
                rusqlite::params![ps(p, "accountId"), p.to_string()],
            )?;
            Ok(())
        }
        "ItemDefined" => {
            // Default active=1 so `items.active` is never NULL for a fresh item
            // (user-facing selectors filter on active; NULL would hide the item).
            let mut doc = p.clone();
            if doc.get("active").is_none() {
                doc["active"] = json!(1);
            }
            tx.execute(
                "INSERT INTO items (id, doc) VALUES (?1, jsonb(?2))",
                rusqlite::params![ps(p, "itemId"), doc.to_string()],
            )?;
            Ok(())
        }
        "PartyCreated" => {
            tx.execute(
                "INSERT INTO parties (id, doc) VALUES (?1, jsonb(?2))",
                rusqlite::params![ps(p, "partyId"), p.to_string()],
            )?;
            Ok(())
        }
        other => Err(unknown(other)),
    }
}

fn unknown(ty: &str) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_MISMATCH),
        Some(format!("projector has no handler for event type: {ty}")),
    )
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p accounting-core creates_user_account_item_party_rows`
Expected: PASS.

- [ ] **Step 5: Wire up the module and re-export**

In `crates/accounting-core/src/lib.rs`, add:

```rust
pub mod projectors;
pub use projectors::apply_event;
```

- [ ] **Step 6: Commit**

```bash
git add crates/accounting-core/src/projectors.rs crates/accounting-core/src/lib.rs
git commit -m "feat: add projector dispatcher and master-data create handlers"
```

---

### Task 3: Master-data update handlers (patch semantics)

**Files:**
- Modify: `crates/accounting-core/src/projectors.rs`

`UserUpdated`, `AccountUpdated`, `ItemUpdated`, `PartyUpdated` (§4.4 mutation events) carry only the changed fields under a `changes` object and merge them into the existing `doc` JSONB (§4.5 patch semantics). Type/normalSide/system_role on accounts are immutable — the projector simply merges whatever `changes` contains, and the command handler (Plan 3) forbids illegal changes.

- [ ] **Step 1: Write the failing test for patch merges**

Add to the `tests` module in `crates/accounting-core/src/projectors.rs`:

```rust
    #[test]
    fn updates_patch_only_changed_fields() {
        let mut conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("devA");

        record(&mut conn, &mut hlc, 1000, "ItemDefined",
            json!({"itemId": "i1", "sku": "SKU1", "name": "Widget", "unit": "ea"}));
        // Rename and deactivate; sku/unit must survive the merge.
        record(&mut conn, &mut hlc, 1000, "ItemUpdated",
            json!({"itemId": "i1", "changes": {"name": "Widget v2", "active": 0}}));

        let (name, sku, active): (String, String, i64) = conn.query_row(
            "SELECT name, sku, active FROM items WHERE id='i1'", [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert_eq!(name, "Widget v2");
        assert_eq!(sku, "SKU1", "unchanged field must survive patch");
        assert_eq!(active, 0);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p accounting-core updates_patch_only_changed_fields`
Expected: FAIL — dispatcher returns the `unknown` error for `ItemUpdated`.

- [ ] **Step 3: Implement the patch helper and add the four update arms**

Add this helper above the `tests` module in `crates/accounting-core/src/projectors.rs`:

```rust
/// Merge an event's `changes` object into an existing row's `doc` JSONB.
/// `table` is chosen from a fixed set (never user input), so string-formatting it
/// into the SQL is safe. `id_key` names the payload field holding the row id.
fn patch_doc(tx: &Connection, table: &str, ev: &LedgerEvent, id_key: &str) -> rusqlite::Result<()> {
    let id = ps(&ev.payload, id_key);
    let sel = format!("SELECT json(doc) FROM {table} WHERE id = ?1");
    let doc_text: String = tx.query_row(&sel, [id], |r| r.get(0))?;
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

Then, in the `apply_event` `match`, add these arms immediately before the `other =>` arm:

```rust
        "UserUpdated" => patch_doc(tx, "users", ev, "userId"),
        "AccountUpdated" => patch_doc(tx, "accounts", ev, "accountId"),
        "ItemUpdated" => patch_doc(tx, "items", ev, "itemId"),
        "PartyUpdated" => patch_doc(tx, "parties", ev, "partyId"),
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p accounting-core updates_patch_only_changed_fields`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/accounting-core/src/projectors.rs
git commit -m "feat: add master-data update handlers with JSONB patch merge"
```

---

### Task 4: Journal posting + balance maintenance helpers

**Files:**
- Modify: `crates/accounting-core/src/projectors.rs`

Every transactional event posts balanced debit/credit lines and maintains `accounts.balance_minor` using `normal_side` (assets/expenses increase on debit; liabilities/equity/income on credit — spec §5.2). Two shared helpers do this so no handler re-implements it. Well-known accounts resolve by `system_role` (§4.5). `journal_lines.id` is derived deterministically as `${eventId}#${lineIndex}` (§4.5 rebuild invariant).

- [ ] **Step 1: Write the failing test for post_line + balance direction**

Add to the `tests` module in `crates/accounting-core/src/projectors.rs`:

```rust
    #[test]
    fn post_line_moves_balance_by_normal_side() {
        let mut conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("devA");
        // Seed one debit-normal (asset) and one credit-normal (liability) account.
        record(&mut conn, &mut hlc, 1000, "AccountOpened",
            json!({"accountId": "a_inv", "name": "Inventory", "type": "asset",
                   "normal": "debit", "system_role": "inventory"}));
        record(&mut conn, &mut hlc, 1000, "AccountOpened",
            json!({"accountId": "a_ap", "name": "Accounts Payable", "type": "liability",
                   "normal": "credit", "system_role": "accounts_payable"}));

        let tx = conn.transaction().unwrap();
        let inv = account_id_by_role(&tx, "inventory").unwrap();
        let ap = account_id_by_role(&tx, "accounts_payable").unwrap();
        // Balanced txn: Dr Inventory 1000 / Cr A/P 1000.
        post_line(&tx, "evX", "evX", 0, &inv, 1000, 0, "2026-01-05", None).unwrap();
        post_line(&tx, "evX", "evX", 1, &ap, 0, 1000, "2026-01-05", None).unwrap();
        tx.commit().unwrap();

        let inv_bal: i64 = conn.query_row(
            "SELECT balance_minor FROM accounts WHERE system_role='inventory'", [], |r| r.get(0)).unwrap();
        let ap_bal: i64 = conn.query_row(
            "SELECT balance_minor FROM accounts WHERE system_role='accounts_payable'", [], |r| r.get(0)).unwrap();
        assert_eq!(inv_bal, 1000, "debit-normal account rises on debit");
        assert_eq!(ap_bal, 1000, "credit-normal account rises on credit");

        // Double-entry: the txn's debits equal its credits.
        let (d, c): (i64, i64) = conn.query_row(
            "SELECT SUM(debit_minor), SUM(credit_minor) FROM journal_lines WHERE txn_id='evX'",
            [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(d, c);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p accounting-core post_line_moves_balance_by_normal_side`
Expected: FAIL — `account_id_by_role` / `post_line` not found.

- [ ] **Step 3: Implement the helpers**

Add above the `tests` module in `crates/accounting-core/src/projectors.rs`:

```rust
/// Resolve a well-known account's id by its immutable `system_role` (§4.5) —
/// never by name (user-renameable) or id (auto-generated).
fn account_id_by_role(tx: &Connection, role: &str) -> rusqlite::Result<String> {
    tx.query_row("SELECT id FROM accounts WHERE system_role = ?1", [role], |r| r.get(0))
}

/// Post one journal line and maintain the account's running balance via normal_side.
/// `line_index` makes the row id deterministic (`${event_id}#${line_index}`), so a
/// drop-and-rebuild reproduces identical ids.
#[allow(clippy::too_many_arguments)]
fn post_line(
    tx: &Connection,
    event_id: &str,
    txn_id: &str,
    line_index: usize,
    account_id: &str,
    debit: i64,
    credit: i64,
    date: &str,
    memo: Option<&str>,
) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO journal_lines
           (id, event_id, txn_id, account_id, debit_minor, credit_minor, date, memo)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            format!("{event_id}#{line_index}"),
            event_id, txn_id, account_id, debit, credit, date, memo
        ],
    )?;
    // assets/expenses (normal 'debit') rise on debit; liabilities/equity/income
    // (normal 'credit') rise on credit.
    tx.execute(
        "UPDATE accounts SET balance_minor = balance_minor +
           CASE WHEN normal_side = 'debit' THEN ?2 - ?3 ELSE ?3 - ?2 END
         WHERE id = ?1",
        rusqlite::params![account_id, debit, credit],
    )?;
    Ok(())
}

/// Upsert a party's balance row and apply signed deltas to each column.
fn adjust_party_balance(
    tx: &Connection,
    party_id: &str,
    d_receivable: i64,
    d_payable: i64,
    d_unalloc_cr: i64,
    d_unalloc_dr: i64,
) -> rusqlite::Result<()> {
    tx.execute("INSERT OR IGNORE INTO party_balances (party_id) VALUES (?1)", [party_id])?;
    tx.execute(
        "UPDATE party_balances SET
           receivable_minor     = receivable_minor + ?2,
           payable_minor        = payable_minor + ?3,
           unallocated_cr_minor = unallocated_cr_minor + ?4,
           unallocated_dr_minor = unallocated_dr_minor + ?5
         WHERE party_id = ?1",
        rusqlite::params![party_id, d_receivable, d_payable, d_unalloc_cr, d_unalloc_dr],
    )?;
    Ok(())
}

/// Add `delta` (may be negative) to a lot's remaining quantity.
fn adjust_lot_remaining(tx: &Connection, lot_id: &str, delta: i64) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE inventory_lots SET qty_remaining = qty_remaining + ?2 WHERE id = ?1",
        rusqlite::params![lot_id, delta],
    )?;
    Ok(())
}

fn lot_unit_cost(tx: &Connection, lot_id: &str) -> rusqlite::Result<i64> {
    tx.query_row("SELECT unit_cost_minor FROM inventory_lots WHERE id = ?1", [lot_id], |r| r.get(0))
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p accounting-core post_line_moves_balance_by_normal_side`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/accounting-core/src/projectors.rs
git commit -m "feat: add journal-posting, party-balance, and lot helpers"
```

---

### Task 5: PurchaseRecorded

**Files:**
- Modify: `crates/accounting-core/src/projectors.rs`

`PurchaseRecorded` (§4.4): `{purchaseId, supplierId, date, terms, lines[]:{itemId, qty, unitCostMinor, lotId}}`. Each line creates one `inventory_lot` (the birth of a cost layer, §6.3). Journal: **Dr Inventory** (total) / **Cr Bank** (cash) *or* **Cr A/P** (credit). For credit terms: set `purchases.outstanding_minor = total` and increase the supplier's `payable_minor`.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module. This includes the reconciliation assertion "lot value equals Inventory GL balance after a purchase". Add a small fixture helper that seeds the accounts this and later tasks need:

```rust
    /// Seed the well-known accounts used across transactional tests.
    fn seed_accounts(conn: &mut Connection, hlc: &mut Hlc) {
        let roles = [
            ("cash", "Cash", "asset", "debit"),
            ("bank", "Bank", "asset", "debit"),
            ("inventory", "Inventory", "asset", "debit"),
            ("accounts_receivable", "Accounts Receivable", "asset", "debit"),
            ("accounts_payable", "Accounts Payable", "liability", "credit"),
            ("owner_capital", "Owner Capital", "equity", "credit"),
            ("sales", "Sales", "income", "credit"),
            ("cogs", "Cost of Goods Sold", "expense", "debit"),
            ("shrinkage", "Inventory Shrinkage", "expense", "debit"),
            ("inventory_gain", "Inventory Gain", "income", "credit"),
            ("rent", "Rent", "expense", "debit"),
        ];
        for (role, name, ty, normal) in roles {
            record(conn, hlc, 1000, "AccountOpened",
                json!({"accountId": format!("acct_{role}"), "name": name,
                       "type": ty, "normal": normal, "system_role": role}));
        }
    }

    #[test]
    fn purchase_creates_lots_and_posts_inventory() {
        let mut conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("devA");
        seed_accounts(&mut conn, &mut hlc);
        record(&mut conn, &mut hlc, 1000, "ItemDefined",
            json!({"itemId": "i1", "sku": "S1", "name": "Widget", "unit": "ea"}));
        record(&mut conn, &mut hlc, 1000, "PartyCreated",
            json!({"partyId": "sup1", "name": "Acme", "kind": "supplier"}));

        // Credit purchase: 10 @ 100 = 1000.
        record(&mut conn, &mut hlc, 1000, "PurchaseRecorded",
            json!({"purchaseId": "po1", "supplierId": "sup1", "date": "2026-01-05",
                   "terms": "credit",
                   "lines": [{"itemId": "i1", "qty": 10, "unitCostMinor": 100, "lotId": "lot1"}]}));

        // Lot created with frozen cost.
        let (recv, rem, cost): (i64, i64, i64) = conn.query_row(
            "SELECT qty_received, qty_remaining, unit_cost_minor FROM inventory_lots WHERE id='lot1'",
            [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert_eq!((recv, rem, cost), (10, 10, 100));

        // Reconciliation #1: aggregate lot value == Inventory GL balance.
        let lot_value: i64 = conn.query_row(
            "SELECT COALESCE(SUM(qty_remaining * unit_cost_minor), 0) FROM inventory_lots", [], |r| r.get(0)).unwrap();
        let inv_bal: i64 = conn.query_row(
            "SELECT balance_minor FROM accounts WHERE system_role='inventory'", [], |r| r.get(0)).unwrap();
        assert_eq!(lot_value, 1000);
        assert_eq!(lot_value, inv_bal);

        // Credit terms → outstanding + supplier payable.
        let outstanding: i64 = conn.query_row(
            "SELECT outstanding_minor FROM purchases WHERE id='po1'", [], |r| r.get(0)).unwrap();
        let payable: i64 = conn.query_row(
            "SELECT payable_minor FROM party_balances WHERE party_id='sup1'", [], |r| r.get(0)).unwrap();
        assert_eq!(outstanding, 1000);
        assert_eq!(payable, 1000);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p accounting-core purchase_creates_lots_and_posts_inventory`
Expected: FAIL — dispatcher returns `unknown` for `PurchaseRecorded`.

- [ ] **Step 3: Implement the handler**

Add this function above the `tests` module:

```rust
fn purchase_recorded(tx: &Connection, ev: &LedgerEvent) -> rusqlite::Result<()> {
    let p = &ev.payload;
    let purchase_id = ps(p, "purchaseId");
    let date = ps(p, "date");
    let terms = ps(p, "terms");
    let supplier = pos(p, "supplierId");

    let mut total = 0i64;
    for (i, line) in parr(p, "lines").iter().enumerate() {
        let qty = pi(line, "qty");
        let unit_cost = pi(line, "unitCostMinor");
        total += qty * unit_cost;
        let lot_id = ps(line, "lotId");
        let item_id = ps(line, "itemId");
        tx.execute(
            "INSERT INTO inventory_lots
               (id, item_id, source_event_id, purchase_id, unit_cost_minor,
                qty_received, qty_remaining, acquired_at, supplier_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?7, ?8)",
            rusqlite::params![lot_id, item_id, ev.id, purchase_id, unit_cost, qty, date, supplier],
        )?;
        tx.execute(
            "INSERT INTO purchase_lines (id, purchase_id, item_id, qty, unit_cost_minor, lot_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![format!("{}#line#{i}", ev.id), purchase_id, item_id, qty, unit_cost, lot_id],
        )?;
    }

    let outstanding = if terms == "credit" { total } else { 0 };
    tx.execute(
        "INSERT INTO purchases (id, event_id, supplier_id, date, terms, total_minor, outstanding_minor)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![purchase_id, ev.id, supplier, date, terms, total, outstanding],
    )?;

    // Dr Inventory / Cr Bank (cash) or A/P (credit).
    let inventory = account_id_by_role(tx, "inventory")?;
    post_line(tx, &ev.id, purchase_id, 0, &inventory, total, 0, date, None)?;
    if terms == "credit" {
        let ap = account_id_by_role(tx, "accounts_payable")?;
        post_line(tx, &ev.id, purchase_id, 1, &ap, 0, total, date, None)?;
        if let Some(s) = supplier {
            adjust_party_balance(tx, s, 0, total, 0, 0)?;
        }
    } else {
        let bank = account_id_by_role(tx, "bank")?;
        post_line(tx, &ev.id, purchase_id, 1, &bank, 0, total, date, None)?;
    }
    Ok(())
}
```

Add the dispatch arm before `other =>`:

```rust
        "PurchaseRecorded" => purchase_recorded(tx, ev),
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p accounting-core purchase_creates_lots_and_posts_inventory`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/accounting-core/src/projectors.rs
git commit -m "feat: project PurchaseRecorded into lots, journal, and A/P"
```

---

### Task 6: SaleRecorded

**Files:**
- Modify: `crates/accounting-core/src/projectors.rs`

`SaleRecorded` (§4.4): `{saleId, customerId, date, terms, lines[]:{itemId, qty, unitPriceMinor, lotConsumption[]:{lotId, qtyTaken, unitCostMinor}}}`. Per line, `revenue = qty*unitPriceMinor` and `cogs = Σ qtyTaken*unitCostMinor` — both frozen (§6.2). Consumes lots (decrements `qty_remaining`). Journal posts **both** revenue (Dr Bank/AR, Cr Sales) **and** cost (Dr COGS, Cr Inventory) atomically (§4.5). Credit sales set `sales.outstanding_minor` and raise the customer's `receivable_minor`.

- [ ] **Step 1: Write the failing test (purchase then sale; reconciliation after both)**

Add to the `tests` module:

```rust
    #[test]
    fn sale_freezes_profit_consumes_lots_and_reconciles() {
        let mut conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("devA");
        seed_accounts(&mut conn, &mut hlc);
        record(&mut conn, &mut hlc, 1000, "ItemDefined",
            json!({"itemId": "i1", "sku": "S1", "name": "Widget", "unit": "ea"}));
        record(&mut conn, &mut hlc, 1000, "PartyCreated",
            json!({"partyId": "cust1", "name": "Bob", "kind": "customer"}));
        // Buy 10 @ 100 (cash).
        record(&mut conn, &mut hlc, 1000, "PurchaseRecorded",
            json!({"purchaseId": "po1", "supplierId": null, "date": "2026-01-01", "terms": "cash",
                   "lines": [{"itemId": "i1", "qty": 10, "unitCostMinor": 100, "lotId": "lot1"}]}));
        // Sell 4 @ 250 on credit, consuming 4 from lot1 @ 100.
        record(&mut conn, &mut hlc, 1000, "SaleRecorded",
            json!({"saleId": "so1", "customerId": "cust1", "date": "2026-01-10", "terms": "credit",
                   "lines": [{"itemId": "i1", "qty": 4, "unitPriceMinor": 250,
                              "lotConsumption": [{"lotId": "lot1", "qtyTaken": 4, "unitCostMinor": 100}]}]}));

        // Frozen line profit = revenue - cogs = 1000 - 400 = 600.
        let (rev, cogs): (i64, i64) = conn.query_row(
            "SELECT revenue_minor, cogs_minor FROM sale_lines WHERE sale_id='so1'", [],
            |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((rev, cogs), (1000, 400));

        // Lot drawn down 10 → 6.
        let rem: i64 = conn.query_row("SELECT qty_remaining FROM inventory_lots WHERE id='lot1'", [], |r| r.get(0)).unwrap();
        assert_eq!(rem, 6);

        // Reconciliation #1 after purchase+sale: lot value (600) == Inventory GL.
        let lot_value: i64 = conn.query_row(
            "SELECT SUM(qty_remaining * unit_cost_minor) FROM inventory_lots", [], |r| r.get(0)).unwrap();
        let inv_bal: i64 = conn.query_row(
            "SELECT balance_minor FROM accounts WHERE system_role='inventory'", [], |r| r.get(0)).unwrap();
        assert_eq!(lot_value, 600);
        assert_eq!(inv_bal, 600);

        // Credit sale → outstanding + receivable = revenue.
        let outstanding: i64 = conn.query_row("SELECT outstanding_minor FROM sales WHERE id='so1'", [], |r| r.get(0)).unwrap();
        let recv: i64 = conn.query_row("SELECT receivable_minor FROM party_balances WHERE party_id='cust1'", [], |r| r.get(0)).unwrap();
        assert_eq!(outstanding, 1000);
        assert_eq!(recv, 1000);

        // Double-entry holds for the sale txn.
        let (d, c): (i64, i64) = conn.query_row(
            "SELECT SUM(debit_minor), SUM(credit_minor) FROM journal_lines WHERE txn_id='so1'", [],
            |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(d, c);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p accounting-core sale_freezes_profit_consumes_lots_and_reconciles`
Expected: FAIL — dispatcher returns `unknown` for `SaleRecorded`.

- [ ] **Step 3: Implement the handler**

Add above the `tests` module:

```rust
fn sale_recorded(tx: &Connection, ev: &LedgerEvent) -> rusqlite::Result<()> {
    let p = &ev.payload;
    let sale_id = ps(p, "saleId");
    let date = ps(p, "date");
    let terms = ps(p, "terms");
    let customer = pos(p, "customerId");

    let mut total_rev = 0i64;
    let mut total_cogs = 0i64;
    for (i, line) in parr(p, "lines").iter().enumerate() {
        let qty = pi(line, "qty");
        let unit_price = pi(line, "unitPriceMinor");
        let revenue = qty * unit_price;
        let item_id = ps(line, "itemId");
        let sale_line_id = format!("{}#line#{i}", ev.id);

        let mut line_cogs = 0i64;
        for (j, lc) in parr(line, "lotConsumption").iter().enumerate() {
            let lot_id = ps(lc, "lotId");
            let qty_taken = pi(lc, "qtyTaken");
            let unit_cost = pi(lc, "unitCostMinor");
            line_cogs += qty_taken * unit_cost;
            tx.execute(
                "INSERT INTO lot_consumptions (id, sale_line_id, lot_id, qty_taken, unit_cost_minor)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![format!("{}#lc#{i}#{j}", ev.id), sale_line_id, lot_id, qty_taken, unit_cost],
            )?;
            adjust_lot_remaining(tx, lot_id, -qty_taken)?;
        }

        tx.execute(
            "INSERT INTO sale_lines
               (id, sale_id, item_id, qty, unit_price_minor, revenue_minor, cogs_minor, date)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![sale_line_id, sale_id, item_id, qty, unit_price, revenue, line_cogs, date],
        )?;
        total_rev += revenue;
        total_cogs += line_cogs;
    }

    let outstanding = if terms == "credit" { total_rev } else { 0 };
    tx.execute(
        "INSERT INTO sales (id, event_id, customer_id, date, terms, total_minor, outstanding_minor)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![sale_id, ev.id, customer, date, terms, total_rev, outstanding],
    )?;

    // Revenue posting: Dr Bank (cash) or A/R (credit) / Cr Sales.
    let sales_acct = account_id_by_role(tx, "sales")?;
    let debit_acct = if terms == "credit" {
        account_id_by_role(tx, "accounts_receivable")?
    } else {
        account_id_by_role(tx, "bank")?
    };
    post_line(tx, &ev.id, sale_id, 0, &debit_acct, total_rev, 0, date, None)?;
    post_line(tx, &ev.id, sale_id, 1, &sales_acct, 0, total_rev, date, None)?;

    // Cost posting: Dr COGS / Cr Inventory.
    let cogs_acct = account_id_by_role(tx, "cogs")?;
    let inventory = account_id_by_role(tx, "inventory")?;
    post_line(tx, &ev.id, sale_id, 2, &cogs_acct, total_cogs, 0, date, None)?;
    post_line(tx, &ev.id, sale_id, 3, &inventory, 0, total_cogs, date, None)?;

    if terms == "credit" {
        if let Some(c) = customer {
            adjust_party_balance(tx, c, total_rev, 0, 0, 0)?;
        }
    }
    Ok(())
}
```

Add the dispatch arm before `other =>`:

```rust
        "SaleRecorded" => sale_recorded(tx, ev),
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p accounting-core sale_freezes_profit_consumes_lots_and_reconciles`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/accounting-core/src/projectors.rs
git commit -m "feat: project SaleRecorded with frozen profit, lot draw-down, A/R"
```

---

### Task 7: PaymentMade & PaymentReceived

**Files:**
- Modify: `crates/accounting-core/src/projectors.rs`

`PaymentReceived` (§4.4): `{paymentId, customerId, amountMinor, date, allocations[]:{saleId, amountMinor}}` → **Dr Bank / Cr A/R** (full amount). `PaymentMade`: `{paymentId, supplierId, amountMinor, date, allocations[]:{purchaseId, amountMinor}}` → **Dr A/P / Cr Bank**. Each allocation reduces the target invoice's `outstanding_minor` and the party's `receivable`/`payable`. Any remainder (allocations may sum to less than the payment — §4.5) becomes the party's unallocated credit (`unallocated_cr` for inflows, `unallocated_dr` for outflows). Writes a `payments` row (§6.4, so pure prepayments remain visible) and one `payment_allocations` row per allocation (§6.5, `payment_id = event_id` here since these are settled at payment time).

Balance-identity note (resolved ambiguity, see final report): crediting/debiting A/R (A/P) by the **full** amount — not just the allocated portion — is deliberate. It lets the A/R GL balance equal `Σ receivable_minor − Σ unallocated_cr_minor` across parties (and A/P equal `Σ payable_minor − Σ unallocated_dr_minor`), which is the net form of reconciliation check #4 in the presence of prepayments.

- [ ] **Step 1: Write the failing test (full allocation + prepayment remainder)**

Add to the `tests` module:

```rust
    #[test]
    fn payment_received_allocates_and_holds_prepayment() {
        let mut conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("devA");
        seed_accounts(&mut conn, &mut hlc);
        record(&mut conn, &mut hlc, 1000, "ItemDefined",
            json!({"itemId": "i1", "sku": "S1", "name": "W", "unit": "ea"}));
        record(&mut conn, &mut hlc, 1000, "PartyCreated",
            json!({"partyId": "cust1", "name": "Bob", "kind": "customer"}));
        record(&mut conn, &mut hlc, 1000, "PurchaseRecorded",
            json!({"purchaseId": "po1", "supplierId": null, "date": "2026-01-01", "terms": "cash",
                   "lines": [{"itemId": "i1", "qty": 10, "unitCostMinor": 100, "lotId": "lot1"}]}));
        record(&mut conn, &mut hlc, 1000, "SaleRecorded",
            json!({"saleId": "so1", "customerId": "cust1", "date": "2026-01-10", "terms": "credit",
                   "lines": [{"itemId": "i1", "qty": 4, "unitPriceMinor": 250,
                              "lotConsumption": [{"lotId": "lot1", "qtyTaken": 4, "unitCostMinor": 100}]}]}));
        // Customer pays 1200 but only 1000 is owed → 200 becomes unallocated credit.
        record(&mut conn, &mut hlc, 1000, "PaymentReceived",
            json!({"paymentId": "pay1", "customerId": "cust1", "amountMinor": 1200, "date": "2026-01-15",
                   "allocations": [{"saleId": "so1", "amountMinor": 1000}]}));

        let outstanding: i64 = conn.query_row("SELECT outstanding_minor FROM sales WHERE id='so1'", [], |r| r.get(0)).unwrap();
        assert_eq!(outstanding, 0, "invoice fully settled");
        let (recv, ucr): (i64, i64) = conn.query_row(
            "SELECT receivable_minor, unallocated_cr_minor FROM party_balances WHERE party_id='cust1'",
            [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(recv, 0);
        assert_eq!(ucr, 200, "overpayment held as prepayment credit");

        // payments row exists (prepayment visibility).
        let (dir, amt): (String, i64) = conn.query_row(
            "SELECT direction, amount_minor FROM payments WHERE id='pay1'", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((dir.as_str(), amt), ("in", 1200));

        // A/R GL == Σreceivable − Σunallocated_cr = 0 − 200 = -200 (net form of check #4).
        let ar_bal: i64 = conn.query_row(
            "SELECT balance_minor FROM accounts WHERE system_role='accounts_receivable'", [], |r| r.get(0)).unwrap();
        assert_eq!(ar_bal, -200);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p accounting-core payment_received_allocates_and_holds_prepayment`
Expected: FAIL — dispatcher returns `unknown` for `PaymentReceived`.

- [ ] **Step 3: Implement the shared payment handler**

Add above the `tests` module:

```rust
/// Handle PaymentReceived (`dir = "in"`) and PaymentMade (`dir = "out"`).
/// Inflows settle sales (Dr Bank / Cr A/R); outflows settle purchases (Dr A/P / Cr Bank).
fn payment(tx: &Connection, ev: &LedgerEvent, dir: &str) -> rusqlite::Result<()> {
    let p = &ev.payload;
    let payment_id = ps(p, "paymentId");
    let amount = pi(p, "amountMinor");
    let date = ps(p, "date");
    let party = if dir == "in" { ps(p, "customerId") } else { ps(p, "supplierId") };
    let target_type = if dir == "in" { "sale" } else { "purchase" };
    let target_key = if dir == "in" { "saleId" } else { "purchaseId" };
    let target_table = if dir == "in" { "sales" } else { "purchases" };

    tx.execute(
        "INSERT INTO payments (id, event_id, party_id, direction, amount_minor, date)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![payment_id, ev.id, party, dir, amount, date],
    )?;

    // Full-amount GL posting (see balance-identity note).
    let bank = account_id_by_role(tx, "bank")?;
    if dir == "in" {
        let ar = account_id_by_role(tx, "accounts_receivable")?;
        post_line(tx, &ev.id, payment_id, 0, &bank, amount, 0, date, None)?;
        post_line(tx, &ev.id, payment_id, 1, &ar, 0, amount, date, None)?;
    } else {
        let ap = account_id_by_role(tx, "accounts_payable")?;
        post_line(tx, &ev.id, payment_id, 0, &ap, amount, 0, date, None)?;
        post_line(tx, &ev.id, payment_id, 1, &bank, 0, amount, date, None)?;
    }

    let mut allocated = 0i64;
    for (i, a) in parr(p, "allocations").iter().enumerate() {
        let target_id = ps(a, target_key);
        let amt = pi(a, "amountMinor");
        allocated += amt;
        tx.execute(
            "INSERT INTO payment_allocations (id, event_id, payment_id, target_id, target_type, amount_minor)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![format!("{}#alloc#{i}", ev.id), ev.id, payment_id, target_id, target_type, amt],
        )?;
        let upd = format!("UPDATE {target_table} SET outstanding_minor = outstanding_minor - ?2 WHERE id = ?1");
        tx.execute(&upd, rusqlite::params![target_id, amt])?;
        if dir == "in" {
            adjust_party_balance(tx, party, -amt, 0, 0, 0)?;   // receivable down
        } else {
            adjust_party_balance(tx, party, 0, -amt, 0, 0)?;   // payable down
        }
    }

    let remainder = amount - allocated;
    if remainder != 0 {
        if dir == "in" {
            adjust_party_balance(tx, party, 0, 0, remainder, 0)?; // held customer credit
        } else {
            adjust_party_balance(tx, party, 0, 0, 0, remainder)?; // supplier deposit
        }
    }
    Ok(())
}
```

Add the dispatch arms before `other =>`:

```rust
        "PaymentReceived" => payment(tx, ev, "in"),
        "PaymentMade" => payment(tx, ev, "out"),
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p accounting-core payment_received_allocates_and_holds_prepayment`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/accounting-core/src/projectors.rs
git commit -m "feat: project PaymentReceived/PaymentMade with allocations + prepayment"
```

---

### Task 8: PaymentAllocated

**Files:**
- Modify: `crates/accounting-core/src/projectors.rs`

`PaymentAllocated` (§4.4): `{paymentId, partyId, allocations[]:{targetId, targetType, amountMinor}, date}`. Applies an **existing** unallocated credit to invoices — **no journal posting, no `payments` row** (§4.5, §6.4): the money already moved when the original payment posted. It writes `payment_allocations` rows (with `payment_id = original payment`, `event_id = this PaymentAllocated event` — §6.5), reduces each target's `outstanding_minor`, reduces the party's `receivable`/`payable`, and reduces the held unallocated credit.

- [ ] **Step 1: Write the failing test (prepay, later invoice, then allocate held credit)**

Add to the `tests` module:

```rust
    #[test]
    fn payment_allocated_applies_held_credit_without_journal() {
        let mut conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("devA");
        seed_accounts(&mut conn, &mut hlc);
        record(&mut conn, &mut hlc, 1000, "ItemDefined",
            json!({"itemId": "i1", "sku": "S1", "name": "W", "unit": "ea"}));
        record(&mut conn, &mut hlc, 1000, "PartyCreated",
            json!({"partyId": "cust1", "name": "Bob", "kind": "customer"}));
        // Pure prepayment: 500 credit, no allocations.
        record(&mut conn, &mut hlc, 1000, "PaymentReceived",
            json!({"paymentId": "pay1", "customerId": "cust1", "amountMinor": 500, "date": "2026-01-01",
                   "allocations": []}));
        // Later a cash-free credit sale of 500 arrives.
        record(&mut conn, &mut hlc, 1000, "PurchaseRecorded",
            json!({"purchaseId": "po1", "supplierId": null, "date": "2026-01-02", "terms": "cash",
                   "lines": [{"itemId": "i1", "qty": 5, "unitCostMinor": 100, "lotId": "lot1"}]}));
        record(&mut conn, &mut hlc, 1000, "SaleRecorded",
            json!({"saleId": "so1", "customerId": "cust1", "date": "2026-01-03", "terms": "credit",
                   "lines": [{"itemId": "i1", "qty": 5, "unitPriceMinor": 100,
                              "lotConsumption": [{"lotId": "lot1", "qtyTaken": 5, "unitCostMinor": 100}]}]}));
        // Count journal lines before allocation.
        let jl_before: i64 = conn.query_row("SELECT COUNT(*) FROM journal_lines", [], |r| r.get(0)).unwrap();
        // Apply the 500 held credit to so1.
        record(&mut conn, &mut hlc, 1000, "PaymentAllocated",
            json!({"paymentId": "pay1", "partyId": "cust1", "date": "2026-01-04",
                   "allocations": [{"targetId": "so1", "targetType": "sale", "amountMinor": 500}]}));

        let jl_after: i64 = conn.query_row("SELECT COUNT(*) FROM journal_lines", [], |r| r.get(0)).unwrap();
        assert_eq!(jl_before, jl_after, "PaymentAllocated posts no journal lines");

        let outstanding: i64 = conn.query_row("SELECT outstanding_minor FROM sales WHERE id='so1'", [], |r| r.get(0)).unwrap();
        let (recv, ucr): (i64, i64) = conn.query_row(
            "SELECT receivable_minor, unallocated_cr_minor FROM party_balances WHERE party_id='cust1'",
            [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(outstanding, 0);
        assert_eq!(recv, 0);
        assert_eq!(ucr, 0, "held credit consumed");
        // No extra payments row created by PaymentAllocated.
        let pay_count: i64 = conn.query_row("SELECT COUNT(*) FROM payments", [], |r| r.get(0)).unwrap();
        assert_eq!(pay_count, 1);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p accounting-core payment_allocated_applies_held_credit_without_journal`
Expected: FAIL — dispatcher returns `unknown` for `PaymentAllocated`.

- [ ] **Step 3: Implement the handler**

Add above the `tests` module:

```rust
fn payment_allocated(tx: &Connection, ev: &LedgerEvent) -> rusqlite::Result<()> {
    let p = &ev.payload;
    let payment_id = ps(p, "paymentId");
    let party = ps(p, "partyId");
    for (i, a) in parr(p, "allocations").iter().enumerate() {
        let target_id = ps(a, "targetId");
        let target_type = ps(a, "targetType"); // 'sale' | 'purchase'
        let amt = pi(a, "amountMinor");
        tx.execute(
            "INSERT INTO payment_allocations (id, event_id, payment_id, target_id, target_type, amount_minor)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![format!("{}#alloc#{i}", ev.id), ev.id, payment_id, target_id, target_type, amt],
        )?;
        let table = if target_type == "sale" { "sales" } else { "purchases" };
        let upd = format!("UPDATE {table} SET outstanding_minor = outstanding_minor - ?2 WHERE id = ?1");
        tx.execute(&upd, rusqlite::params![target_id, amt])?;
        if target_type == "sale" {
            // Consume held customer credit; reduce receivable.
            adjust_party_balance(tx, party, -amt, 0, -amt, 0)?;
        } else {
            // Consume supplier deposit; reduce payable.
            adjust_party_balance(tx, party, 0, -amt, 0, -amt)?;
        }
    }
    Ok(())
}
```

Add the dispatch arm before `other =>`:

```rust
        "PaymentAllocated" => payment_allocated(tx, ev),
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p accounting-core payment_allocated_applies_held_credit_without_journal`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/accounting-core/src/projectors.rs
git commit -m "feat: project PaymentAllocated (apply held credit, no journal)"
```

---

### Task 9: ExpenseRecorded & TransferRecorded

**Files:**
- Modify: `crates/accounting-core/src/projectors.rs`

`ExpenseRecorded` (§4.4, updated payload): `{expenseId, accountId, amountMinor, date, terms, supplierId?, memo?}` → **Dr Expense** (the given `accountId`) / **Cr Bank** (cash) *or* **Cr A/P** (credit). Writes an `expenses` row (§6.9). `TransferRecorded`: `{transferId, fromAccountId, toAccountId, amountMinor, date, memo?}` → **Dr toAccount / Cr fromAccount** (§4.5). Both reference their expense/transfer accounts by explicit `accountId` (already GL account ids), not by system_role.

**Credit-expense party contract (spec §4.5 "Credit-expense party guard"):** a `credit`-terms `ExpenseRecorded` MUST carry a `supplierId`, and the projector MUST increase that supplier's `payable_minor` — exactly like a credit purchase. Without it, the credit expense posts to the A/P GL with no `party_balances` counterpart and reconciliation check #4's A/P net form (`Σpayable − Σunallocated_dr = A/P GL`) fails on every credit expense. Cash expenses carry no party and touch no `party_balances` row. (Plan 3's command handler enforces the *presence* of `supplierId` on credit terms; this projector applies the balance effect.)

- [ ] **Step 1: Write the failing test**

Add to the `tests` module:

```rust
    #[test]
    fn expense_and_transfer_post_correctly() {
        let mut conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("devA");
        seed_accounts(&mut conn, &mut hlc);

        // Cash rent expense of 300.
        record(&mut conn, &mut hlc, 1000, "ExpenseRecorded",
            json!({"expenseId": "ex1", "accountId": "acct_rent", "amountMinor": 300,
                   "date": "2026-02-01", "terms": "cash", "memo": "Feb rent"}));
        let rent_bal: i64 = conn.query_row("SELECT balance_minor FROM accounts WHERE system_role='rent'", [], |r| r.get(0)).unwrap();
        let bank_bal: i64 = conn.query_row("SELECT balance_minor FROM accounts WHERE system_role='bank'", [], |r| r.get(0)).unwrap();
        assert_eq!(rent_bal, 300, "expense (debit-normal) rises");
        assert_eq!(bank_bal, -300, "bank paid out");
        let (ex_amt, ex_memo): (i64, String) = conn.query_row(
            "SELECT amount_minor, memo FROM expenses WHERE id='ex1'", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((ex_amt, ex_memo.as_str()), (300, "Feb rent"));

        // Credit rent expense of 400 on terms from supplier "sup1".
        record(&mut conn, &mut hlc, 1000, "PartyCreated",
            json!({"partyId": "sup1", "name": "Landlord Co", "kind": "supplier"}));
        record(&mut conn, &mut hlc, 1000, "ExpenseRecorded",
            json!({"expenseId": "ex2", "accountId": "acct_rent", "amountMinor": 400,
                   "date": "2026-02-01", "terms": "credit", "supplierId": "sup1", "memo": "Feb rent on account"}));
        let ap_bal: i64 = conn.query_row("SELECT balance_minor FROM accounts WHERE system_role='accounts_payable'", [], |r| r.get(0)).unwrap();
        let payable: i64 = conn.query_row("SELECT payable_minor FROM party_balances WHERE party_id='sup1'", [], |r| r.get(0)).unwrap();
        assert_eq!(ap_bal, 400, "credit expense credits A/P GL");
        assert_eq!(payable, 400, "credit expense raises the supplier's payable (check #4 A/P net form)");

        // Transfer 500 cash → bank.
        record(&mut conn, &mut hlc, 1000, "TransferRecorded",
            json!({"transferId": "tr1", "fromAccountId": "acct_cash", "toAccountId": "acct_bank",
                   "amountMinor": 500, "date": "2026-02-02"}));
        let cash_bal: i64 = conn.query_row("SELECT balance_minor FROM accounts WHERE system_role='cash'", [], |r| r.get(0)).unwrap();
        let bank_bal2: i64 = conn.query_row("SELECT balance_minor FROM accounts WHERE system_role='bank'", [], |r| r.get(0)).unwrap();
        assert_eq!(cash_bal, -500, "credited (from) side");
        assert_eq!(bank_bal2, 200, "-300 + 500 debited (to) side");
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p accounting-core expense_and_transfer_post_correctly`
Expected: FAIL — dispatcher returns `unknown` for `ExpenseRecorded`.

- [ ] **Step 3: Implement the handlers**

Add above the `tests` module:

```rust
fn expense_recorded(tx: &Connection, ev: &LedgerEvent) -> rusqlite::Result<()> {
    let p = &ev.payload;
    let expense_id = ps(p, "expenseId");
    let account_id = ps(p, "accountId");
    let amount = pi(p, "amountMinor");
    let date = ps(p, "date");
    let terms = ps(p, "terms");
    let memo = pos(p, "memo");
    tx.execute(
        "INSERT INTO expenses (id, event_id, account_id, amount_minor, date, memo, terms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![expense_id, ev.id, account_id, amount, date, memo, terms],
    )?;
    post_line(tx, &ev.id, expense_id, 0, account_id, amount, 0, date, memo)?;
    let credit_acct = if terms == "credit" {
        account_id_by_role(tx, "accounts_payable")?
    } else {
        account_id_by_role(tx, "bank")?
    };
    post_line(tx, &ev.id, expense_id, 1, &credit_acct, 0, amount, date, memo)?;
    // Credit-expense party contract (§4.5): raise the supplier's payable so the
    // A/P net form of reconciliation check #4 holds. Guaranteed present on credit
    // terms by Plan 3's Credit-expense party guard.
    if terms == "credit" {
        if let Some(s) = pos(p, "supplierId") {
            adjust_party_balance(tx, s, 0, amount, 0, 0)?;
        }
    }
    Ok(())
}

fn transfer_recorded(tx: &Connection, ev: &LedgerEvent) -> rusqlite::Result<()> {
    let p = &ev.payload;
    let transfer_id = ps(p, "transferId");
    let from = ps(p, "fromAccountId");
    let to = ps(p, "toAccountId");
    let amount = pi(p, "amountMinor");
    let date = ps(p, "date");
    let memo = pos(p, "memo");
    // Dr toAccount / Cr fromAccount.
    post_line(tx, &ev.id, transfer_id, 0, to, amount, 0, date, memo)?;
    post_line(tx, &ev.id, transfer_id, 1, from, 0, amount, date, memo)?;
    Ok(())
}
```

Add the dispatch arms before `other =>`:

```rust
        "ExpenseRecorded" => expense_recorded(tx, ev),
        "TransferRecorded" => transfer_recorded(tx, ev),
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p accounting-core expense_and_transfer_post_correctly`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/accounting-core/src/projectors.rs
git commit -m "feat: project ExpenseRecorded and TransferRecorded"
```

---

### Task 10: InventoryAdjusted & InventoryFound

**Files:**
- Modify: `crates/accounting-core/src/projectors.rs`

`InventoryAdjusted` (§4.4, write-down only): `{adjustmentId, date, lines[]:{itemId, lotId, qtyDelta (negative), reasonCode, expenseAccountId}}`. Decrements an existing lot's `qty_remaining` by `|qtyDelta|` and posts **Dr expenseAccountId** (defaults to `system_role='shrinkage'`) / **Cr Inventory** at the lot's frozen unit cost (§4.5). `InventoryFound`: `{foundId, date, lines[]:{itemId, lotId, qty, unitCostMinor, acquiredAt, incomeAccountId}}` creates a **new** lot and posts **Dr Inventory / Cr incomeAccountId** (defaults to `system_role='inventory_gain'`) — never inflates an existing lot (§4.5, one-lot-one-cost-layer invariant).

- [ ] **Step 1: Write the failing test**

Add to the `tests` module:

```rust
    #[test]
    fn inventory_adjusted_and_found_move_lots_and_gl() {
        let mut conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("devA");
        seed_accounts(&mut conn, &mut hlc);
        record(&mut conn, &mut hlc, 1000, "ItemDefined",
            json!({"itemId": "i1", "sku": "S1", "name": "W", "unit": "ea"}));
        record(&mut conn, &mut hlc, 1000, "PurchaseRecorded",
            json!({"purchaseId": "po1", "supplierId": null, "date": "2026-01-01", "terms": "cash",
                   "lines": [{"itemId": "i1", "qty": 10, "unitCostMinor": 100, "lotId": "lot1"}]}));
        // Inventory begins at 1000.

        // Write down 2 units (damage) at lot cost 100 → -200.
        record(&mut conn, &mut hlc, 1000, "InventoryAdjusted",
            json!({"adjustmentId": "adj1", "date": "2026-01-05",
                   "lines": [{"itemId": "i1", "lotId": "lot1", "qtyDelta": -2,
                              "reasonCode": "damage", "expenseAccountId": "acct_shrinkage"}]}));
        let rem: i64 = conn.query_row("SELECT qty_remaining FROM inventory_lots WHERE id='lot1'", [], |r| r.get(0)).unwrap();
        assert_eq!(rem, 8);
        let shrink: i64 = conn.query_row("SELECT balance_minor FROM accounts WHERE system_role='shrinkage'", [], |r| r.get(0)).unwrap();
        assert_eq!(shrink, 200);

        // Find 3 units at cost 90 (new lot), crediting inventory_gain by default.
        record(&mut conn, &mut hlc, 1000, "InventoryFound",
            json!({"foundId": "f1", "date": "2026-01-06",
                   "lines": [{"itemId": "i1", "lotId": "lot2", "qty": 3, "unitCostMinor": 90,
                              "acquiredAt": "2026-01-06"}]}));
        let (recv2, cost2): (i64, i64) = conn.query_row(
            "SELECT qty_received, unit_cost_minor FROM inventory_lots WHERE id='lot2'", [],
            |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((recv2, cost2), (3, 90));
        let gain: i64 = conn.query_row("SELECT balance_minor FROM accounts WHERE system_role='inventory_gain'", [], |r| r.get(0)).unwrap();
        assert_eq!(gain, 270);

        // Reconciliation #1: lot value == Inventory GL. 8*100 + 3*90 = 1070.
        let lot_value: i64 = conn.query_row("SELECT SUM(qty_remaining*unit_cost_minor) FROM inventory_lots", [], |r| r.get(0)).unwrap();
        let inv_bal: i64 = conn.query_row("SELECT balance_minor FROM accounts WHERE system_role='inventory'", [], |r| r.get(0)).unwrap();
        assert_eq!(lot_value, 1070);
        assert_eq!(inv_bal, 1070);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p accounting-core inventory_adjusted_and_found_move_lots_and_gl`
Expected: FAIL — dispatcher returns `unknown` for `InventoryAdjusted`.

- [ ] **Step 3: Implement the handlers**

Add above the `tests` module:

```rust
fn inventory_adjusted(tx: &Connection, ev: &LedgerEvent) -> rusqlite::Result<()> {
    let p = &ev.payload;
    let adj_id = ps(p, "adjustmentId");
    let date = ps(p, "date");
    let inventory = account_id_by_role(tx, "inventory")?;
    let mut li = 0usize;
    for line in parr(p, "lines") {
        let lot_id = ps(line, "lotId");
        let qty = -pi(line, "qtyDelta"); // qtyDelta is negative → qty is the positive magnitude
        let unit_cost = lot_unit_cost(tx, lot_id)?;
        let value = qty * unit_cost;
        adjust_lot_remaining(tx, lot_id, -qty)?;
        // Dr expenseAccountId (default shrinkage) / Cr Inventory.
        let expense_acct = match pos(line, "expenseAccountId") {
            Some(a) => a.to_string(),
            None => account_id_by_role(tx, "shrinkage")?,
        };
        post_line(tx, &ev.id, adj_id, li, &expense_acct, value, 0, date, None)?;
        li += 1;
        post_line(tx, &ev.id, adj_id, li, &inventory, 0, value, date, None)?;
        li += 1;
    }
    Ok(())
}

fn inventory_found(tx: &Connection, ev: &LedgerEvent) -> rusqlite::Result<()> {
    let p = &ev.payload;
    let found_id = ps(p, "foundId");
    let date = ps(p, "date");
    let inventory = account_id_by_role(tx, "inventory")?;
    let mut li = 0usize;
    for line in parr(p, "lines") {
        let lot_id = ps(line, "lotId");
        let item_id = ps(line, "itemId");
        let qty = pi(line, "qty");
        let unit_cost = pi(line, "unitCostMinor");
        let acquired = ps(line, "acquiredAt");
        let value = qty * unit_cost;
        tx.execute(
            "INSERT INTO inventory_lots
               (id, item_id, source_event_id, purchase_id, unit_cost_minor,
                qty_received, qty_remaining, acquired_at, supplier_id)
             VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?5, ?6, NULL)",
            rusqlite::params![lot_id, item_id, ev.id, unit_cost, qty, acquired],
        )?;
        // Dr Inventory / Cr incomeAccountId (default inventory_gain).
        let income_acct = match pos(line, "incomeAccountId") {
            Some(a) => a.to_string(),
            None => account_id_by_role(tx, "inventory_gain")?,
        };
        post_line(tx, &ev.id, found_id, li, &inventory, value, 0, date, None)?;
        li += 1;
        post_line(tx, &ev.id, found_id, li, &income_acct, 0, value, date, None)?;
        li += 1;
    }
    Ok(())
}
```

Add the dispatch arms before `other =>`:

```rust
        "InventoryAdjusted" => inventory_adjusted(tx, ev),
        "InventoryFound" => inventory_found(tx, ev),
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p accounting-core inventory_adjusted_and_found_move_lots_and_gl`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/accounting-core/src/projectors.rs
git commit -m "feat: project InventoryAdjusted (write-down) and InventoryFound (new lot)"
```

---

### Task 11: OpeningBalancesRecorded

**Files:**
- Modify: `crates/accounting-core/src/projectors.rs`

`OpeningBalancesRecorded` (§4.4): `{date, accountBalances[]:{accountId, debitMinor, creditMinor}, lots[]:{itemId, lotId, qty, unitCostMinor, acquiredAt, supplierId?}}`. One-time genesis event for an existing business. Posts one balanced journal line per account balance (Dr `debitMinor` / Cr `creditMinor` on the given account), and creates the initial inventory lots. The command handler guarantees debits equal credits and that the Inventory account's opening debit equals the sum of lot values (so reconciliation #1 holds); the projector just applies both faithfully. `txn_id = ev.id` (the event has no separate business id).

- [ ] **Step 1: Write the failing test**

Add to the `tests` module:

```rust
    #[test]
    fn opening_balances_sets_gl_and_creates_lots() {
        let mut conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("devA");
        seed_accounts(&mut conn, &mut hlc);
        record(&mut conn, &mut hlc, 1000, "ItemDefined",
            json!({"itemId": "i1", "sku": "S1", "name": "W", "unit": "ea"}));

        // Opening: Inventory 1000 (Dr), Bank 500 (Dr), balanced by Owner Capital 1500 (Cr).
        record(&mut conn, &mut hlc, 1000, "OpeningBalancesRecorded",
            json!({"date": "2026-01-01",
                   "accountBalances": [
                       {"accountId": "acct_inventory", "debitMinor": 1000, "creditMinor": 0},
                       {"accountId": "acct_bank", "debitMinor": 500, "creditMinor": 0},
                       {"accountId": "acct_owner_capital", "debitMinor": 0, "creditMinor": 1500}],
                   "lots": [
                       {"itemId": "i1", "lotId": "lotOB", "qty": 10, "unitCostMinor": 100,
                        "acquiredAt": "2025-12-01"}]}));

        let inv_bal: i64 = conn.query_row("SELECT balance_minor FROM accounts WHERE system_role='inventory'", [], |r| r.get(0)).unwrap();
        let cap_bal: i64 = conn.query_row("SELECT balance_minor FROM accounts WHERE system_role='owner_capital'", [], |r| r.get(0)).unwrap();
        assert_eq!(inv_bal, 1000);
        assert_eq!(cap_bal, 1500);

        let (rem, cost): (i64, i64) = conn.query_row(
            "SELECT qty_remaining, unit_cost_minor FROM inventory_lots WHERE id='lotOB'", [],
            |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((rem, cost), (10, 100));
        // Lot value equals opening Inventory GL (reconciliation #1).
        let lot_value: i64 = conn.query_row("SELECT SUM(qty_remaining*unit_cost_minor) FROM inventory_lots", [], |r| r.get(0)).unwrap();
        assert_eq!(lot_value, inv_bal);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p accounting-core opening_balances_sets_gl_and_creates_lots`
Expected: FAIL — dispatcher returns `unknown` for `OpeningBalancesRecorded`.

- [ ] **Step 3: Implement the handler**

Add above the `tests` module:

```rust
fn opening_balances(tx: &Connection, ev: &LedgerEvent) -> rusqlite::Result<()> {
    let p = &ev.payload;
    let date = ps(p, "date");
    let mut li = 0usize;
    for ab in parr(p, "accountBalances") {
        let account_id = ps(ab, "accountId");
        let debit = pi(ab, "debitMinor");
        let credit = pi(ab, "creditMinor");
        post_line(tx, &ev.id, &ev.id, li, account_id, debit, credit, date, Some("opening balance"))?;
        li += 1;
    }
    for lot in parr(p, "lots") {
        let lot_id = ps(lot, "lotId");
        let item_id = ps(lot, "itemId");
        let qty = pi(lot, "qty");
        let unit_cost = pi(lot, "unitCostMinor");
        let acquired = ps(lot, "acquiredAt");
        let supplier = pos(lot, "supplierId");
        tx.execute(
            "INSERT INTO inventory_lots
               (id, item_id, source_event_id, purchase_id, unit_cost_minor,
                qty_received, qty_remaining, acquired_at, supplier_id)
             VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?5, ?6, ?7)",
            rusqlite::params![lot_id, item_id, ev.id, unit_cost, qty, acquired, supplier],
        )?;
    }
    Ok(())
}
```

Add the dispatch arm before `other =>`:

```rust
        "OpeningBalancesRecorded" => opening_balances(tx, ev),
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p accounting-core opening_balances_sets_gl_and_creates_lots`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/accounting-core/src/projectors.rs
git commit -m "feat: project OpeningBalancesRecorded (GL + initial lots)"
```

---

### Task 12: SaleReturnRecorded & PurchaseReturnRecorded

**Files:**
- Modify: `crates/accounting-core/src/projectors.rs`

`SaleReturnRecorded` (§4.4): `{returnId, originalSaleId, date, lines[]:{itemId, qty, unitPriceMinor, lotReturns[]:{lotId, qtyReturned, unitCostMinor}}}`. Restores inventory (increments `qty_remaining` on the returned lots) and reverses revenue. `revenue_reversed = Σ qty*unitPriceMinor`, `cost_restored = Σ qtyReturned*unitCostMinor` — tracked separately (§6.8). Journal: **Dr Sales** (`revenue_reversed`) / **Cr A/R _or_ Bank** (customer-refund side, see below) and **Dr Inventory** (`cost_restored`) / **Cr COGS**.

**Return → invoice/party-balance contract (§4.5):** for a **credit** sale (a customer is present) reduce the original sale's `outstanding_minor` and the customer's `receivable_minor` by the returned revenue, **capped at the remaining outstanding**; any excess (sale already paid) becomes the customer's `unallocated_cr_minor` refund credit. Never drive `outstanding_minor` below zero. `PurchaseReturnRecorded` is the mirror: `{returnId, originalPurchaseId, date, lines[]:{itemId, qty, lotId, unitCostMinor}}` consumes inventory (decrements `qty_remaining`), sets `revenue_reversed = 0`, `cost_restored = Σ qty*unitCostMinor`, posts **Dr A/P _or_ Bank / Cr Inventory**, and (credit purchase only) reduces the purchase's `outstanding_minor` + supplier `payable_minor` (capped; excess → `unallocated_dr_minor`).

Resolved ambiguity (see final report): the refund side is chosen by the original invoice's terms, not hard-wired to A/R. **Credit** sale/purchase → credit/debit **A/R / A/P** (the capped portion reduces receivable/payable, the excess raises `unallocated_cr`/`unallocated_dr`), modeling a refund as store credit per the spec's "becomes an unallocated_cr_minor refund credit" wording and keeping the net A/R/A/P identity of check #4 intact. **Cash** sale/purchase (party is NULL) → refund to **Bank**: the customer/supplier already settled in cash and there is no `party_balances` row to attribute a receivable/payable to, so crediting A/R (debiting A/P) would strand a balance with no party counterpart and break check #4. This is the party-less cash-return branch.

- [ ] **Step 1: Write the failing test (sale return on an unpaid credit sale)**

Add to the `tests` module:

```rust
    #[test]
    fn sale_return_restores_inventory_and_reduces_receivable() {
        let mut conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("devA");
        seed_accounts(&mut conn, &mut hlc);
        record(&mut conn, &mut hlc, 1000, "ItemDefined",
            json!({"itemId": "i1", "sku": "S1", "name": "W", "unit": "ea"}));
        record(&mut conn, &mut hlc, 1000, "PartyCreated",
            json!({"partyId": "cust1", "name": "Bob", "kind": "customer"}));
        record(&mut conn, &mut hlc, 1000, "PurchaseRecorded",
            json!({"purchaseId": "po1", "supplierId": null, "date": "2026-01-01", "terms": "cash",
                   "lines": [{"itemId": "i1", "qty": 10, "unitCostMinor": 100, "lotId": "lot1"}]}));
        // Credit sale of 4 @ 250 → outstanding 1000, receivable 1000, lot rem 6.
        record(&mut conn, &mut hlc, 1000, "SaleRecorded",
            json!({"saleId": "so1", "customerId": "cust1", "date": "2026-01-10", "terms": "credit",
                   "lines": [{"itemId": "i1", "qty": 4, "unitPriceMinor": 250,
                              "lotConsumption": [{"lotId": "lot1", "qtyTaken": 4, "unitCostMinor": 100}]}]}));
        // Customer returns 1 unit @ 250, restoring 1 to lot1 @ 100.
        record(&mut conn, &mut hlc, 1000, "SaleReturnRecorded",
            json!({"returnId": "ret1", "originalSaleId": "so1", "date": "2026-01-12",
                   "lines": [{"itemId": "i1", "qty": 1, "unitPriceMinor": 250,
                              "lotReturns": [{"lotId": "lot1", "qtyReturned": 1, "unitCostMinor": 100}]}]}));

        let rem: i64 = conn.query_row("SELECT qty_remaining FROM inventory_lots WHERE id='lot1'", [], |r| r.get(0)).unwrap();
        assert_eq!(rem, 7, "1 unit restored to lot");
        let (rr, cr): (i64, i64) = conn.query_row(
            "SELECT revenue_reversed_minor, cost_restored_minor FROM returns WHERE id='ret1'", [],
            |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((rr, cr), (250, 100));
        let outstanding: i64 = conn.query_row("SELECT outstanding_minor FROM sales WHERE id='so1'", [], |r| r.get(0)).unwrap();
        let recv: i64 = conn.query_row("SELECT receivable_minor FROM party_balances WHERE party_id='cust1'", [], |r| r.get(0)).unwrap();
        assert_eq!(outstanding, 750, "1000 - 250 returned");
        assert_eq!(recv, 750);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p accounting-core sale_return_restores_inventory_and_reduces_receivable`
Expected: FAIL — dispatcher returns `unknown` for `SaleReturnRecorded`.

- [ ] **Step 3: Implement both handlers**

Add above the `tests` module:

```rust
fn sale_return(tx: &Connection, ev: &LedgerEvent) -> rusqlite::Result<()> {
    let p = &ev.payload;
    let return_id = ps(p, "returnId");
    let original_id = ps(p, "originalSaleId");
    let date = ps(p, "date");

    let mut revenue_reversed = 0i64;
    let mut cost_restored = 0i64;
    let mut li = 0usize;
    for line in parr(p, "lines") {
        let item_id = ps(line, "itemId");
        let qty = pi(line, "qty");
        let unit_price = pi(line, "unitPriceMinor");
        revenue_reversed += qty * unit_price;
        for lr in parr(line, "lotReturns") {
            let lot_id = ps(lr, "lotId");
            let qty_ret = pi(lr, "qtyReturned");
            let unit_cost = pi(lr, "unitCostMinor");
            cost_restored += qty_ret * unit_cost;
            adjust_lot_remaining(tx, lot_id, qty_ret)?; // restore
            tx.execute(
                "INSERT INTO return_lines (id, return_id, item_id, qty, unit_price_minor, unit_cost_minor, lot_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![format!("{}#line#{li}", ev.id), return_id, item_id, qty_ret, unit_price, unit_cost, lot_id],
            )?;
            li += 1;
        }
    }

    tx.execute(
        "INSERT INTO returns (id, event_id, return_type, original_id, date, revenue_reversed_minor, cost_restored_minor)
         VALUES (?1, ?2, 'sale_return', ?3, ?4, ?5, ?6)",
        rusqlite::params![return_id, ev.id, original_id, date, revenue_reversed, cost_restored],
    )?;

    // The refund side depends on whether the original sale had a customer:
    //  - credit sale (customer present) → Cr A/R, and the party-balance contract
    //    reduces receivable (capped) / adds unallocated_cr for any excess;
    //  - cash sale (customer_id NULL) → the customer already paid cash, so refund
    //    to Bank (Cr Bank). There is no party row to attribute a receivable to, so
    //    crediting A/R here would strand a balance with no `party_balances`
    //    counterpart and break reconciliation check #4's A/R net form. This is the
    //    party-less cash-sale return branch.
    let customer: Option<String> = tx.query_row(
        "SELECT customer_id FROM sales WHERE id = ?1", [original_id], |r| r.get(0))?;

    // Financial: reverse revenue (Dr Sales / Cr A/R or Bank) and restore inventory (Dr Inventory / Cr COGS).
    let sales_acct = account_id_by_role(tx, "sales")?;
    let inventory = account_id_by_role(tx, "inventory")?;
    let cogs = account_id_by_role(tx, "cogs")?;
    let refund_acct = if customer.is_some() {
        account_id_by_role(tx, "accounts_receivable")?
    } else {
        account_id_by_role(tx, "bank")?
    };
    post_line(tx, &ev.id, return_id, 0, &sales_acct, revenue_reversed, 0, date, None)?;
    post_line(tx, &ev.id, return_id, 1, &refund_acct, 0, revenue_reversed, date, None)?;
    post_line(tx, &ev.id, return_id, 2, &inventory, cost_restored, 0, date, None)?;
    post_line(tx, &ev.id, return_id, 3, &cogs, 0, cost_restored, date, None)?;

    // Return → invoice/party-balance contract: cap reduction at remaining outstanding.
    // Only credit sales carry outstanding + a customer party balance; a cash sale's
    // outstanding is already 0 and it has no party row, so this block is skipped.
    if let Some(c) = customer {
        let outstanding: i64 = tx.query_row(
            "SELECT outstanding_minor FROM sales WHERE id = ?1", [original_id], |r| r.get(0))?;
        let reduce = revenue_reversed.min(outstanding.max(0));
        let excess = revenue_reversed - reduce;
        tx.execute("UPDATE sales SET outstanding_minor = outstanding_minor - ?2 WHERE id = ?1",
            rusqlite::params![original_id, reduce])?;
        adjust_party_balance(tx, &c, -reduce, 0, excess, 0)?;
    }
    Ok(())
}

fn purchase_return(tx: &Connection, ev: &LedgerEvent) -> rusqlite::Result<()> {
    let p = &ev.payload;
    let return_id = ps(p, "returnId");
    let original_id = ps(p, "originalPurchaseId");
    let date = ps(p, "date");

    let mut cost_restored = 0i64;
    let mut li = 0usize;
    for line in parr(p, "lines") {
        let item_id = ps(line, "itemId");
        let qty = pi(line, "qty");
        let unit_cost = pi(line, "unitCostMinor");
        let lot_id = ps(line, "lotId");
        cost_restored += qty * unit_cost;
        adjust_lot_remaining(tx, lot_id, -qty)?; // consume (return to supplier)
        tx.execute(
            "INSERT INTO return_lines (id, return_id, item_id, qty, unit_price_minor, unit_cost_minor, lot_id)
             VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6)",
            rusqlite::params![format!("{}#line#{li}", ev.id), return_id, item_id, qty, unit_cost, lot_id],
        )?;
        li += 1;
    }

    tx.execute(
        "INSERT INTO returns (id, event_id, return_type, original_id, date, revenue_reversed_minor, cost_restored_minor)
         VALUES (?1, ?2, 'purchase_return', ?3, ?4, 0, ?5)",
        rusqlite::params![return_id, ev.id, original_id, date, cost_restored],
    )?;

    // Mirror of the sale-return refund logic: a credit purchase (supplier present)
    // debits A/P; a cash purchase (supplier_id NULL) gets a Bank refund, since
    // debiting A/P with no `party_balances` counterpart would break check #4's A/P
    // net form. This is the party-less cash-purchase return branch.
    let supplier: Option<String> = tx.query_row(
        "SELECT supplier_id FROM purchases WHERE id = ?1", [original_id], |r| r.get(0))?;

    // Financial: Dr A/P (or Bank) / Cr Inventory.
    let inventory = account_id_by_role(tx, "inventory")?;
    let refund_acct = if supplier.is_some() {
        account_id_by_role(tx, "accounts_payable")?
    } else {
        account_id_by_role(tx, "bank")?
    };
    post_line(tx, &ev.id, return_id, 0, &refund_acct, cost_restored, 0, date, None)?;
    post_line(tx, &ev.id, return_id, 1, &inventory, 0, cost_restored, date, None)?;

    // Reduce purchase outstanding + supplier payable, capped; excess → unallocated_dr.
    // Skipped for a cash purchase (outstanding already 0, no party row).
    if let Some(s) = supplier {
        let outstanding: i64 = tx.query_row(
            "SELECT outstanding_minor FROM purchases WHERE id = ?1", [original_id], |r| r.get(0))?;
        let reduce = cost_restored.min(outstanding.max(0));
        let excess = cost_restored - reduce;
        tx.execute("UPDATE purchases SET outstanding_minor = outstanding_minor - ?2 WHERE id = ?1",
            rusqlite::params![original_id, reduce])?;
        adjust_party_balance(tx, &s, 0, -reduce, 0, excess)?;
    }
    Ok(())
}
```

Add the dispatch arms before `other =>`:

```rust
        "SaleReturnRecorded" => sale_return(tx, ev),
        "PurchaseReturnRecorded" => purchase_return(tx, ev),
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p accounting-core sale_return_restores_inventory_and_reduces_receivable`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/accounting-core/src/projectors.rs
git commit -m "feat: project sale/purchase returns with invoice + party-balance contract"
```

---

### Task 13: TransactionReversed (four-part contract)

**Files:**
- Modify: `crates/accounting-core/src/projectors.rs`

`TransactionReversed` (§4.4): `{targetEventId, targetType, reason, reversalJournalLines[]:{accountId, debitMinor, creditMinor}}`. The reversal journal lines are **frozen (pre-negated) into the payload at command time** (like COGS), so the projector posts them verbatim and never recomputes them (§4.5). **Each frozen line names its account by `accountId`, posted directly.** Account ids are stable across rebuild (frozen in the `AccountOpened` payload — genesis uses `acct_{system_role}`, user accounts get a caller-supplied id — and replay reproduces them), and unlike `system_role` (nullable for user-created accounts, §5.2) an id can address *any* account, including the user-created expense/income accounts a reversal might touch. So clause 1 posts each frozen line's `accountId` verbatim — no role resolution. The projector applies the §4.5 **four-part contract**:

1. **Financial:** post the frozen `reversalJournalLines` by their frozen `accountId` (no-op for `PaymentAllocated` targets, which carry none).
2. **Inventory** (inverse of the *target's category*, reading the **target's** payload):
   - lot-creating target (`PurchaseRecorded`, `InventoryFound`, `OpeningBalancesRecorded`) → zero each lot it created (`qty_remaining = 0`; the lot-source void guard, enforced in Plan 3, guarantees they were unconsumed, so no orphan counts toward stock-on-hand).
   - lot-consuming target (`SaleRecorded` via its `lot_consumptions` rows; `PurchaseReturnRecorded` / `InventoryAdjusted` via their payload) → restore `qty_remaining`.
   - lot-restoring target (`SaleReturnRecorded`) → re-decrement `qty_remaining`.
3. **Allocation/settlement:**
   - **allocation-bearing targets** (`PaymentMade`/`PaymentReceived`/`PaymentAllocated`): delete the target's `payment_allocations` rows, re-open the `outstanding_minor` they settled, and restore the party's `receivable`/`payable` and `unallocated_*`; also delete the `payments` row for `PaymentMade`/`PaymentReceived` targets.
   - **invoice-creating targets** (`SaleRecorded`/`PurchaseRecorded` on **credit** terms): the frozen financial lines (clause 1) flatten the A/R/A/P GL, but the *derived convenience* columns are separate and must be unwound too — otherwise a voided credit sale still shows as owed and check #4's net form fails (`Σreceivable − Σunalloc_cr = 1000 ≠ A/R GL 0`). So set `outstanding_minor := 0` and reverse the party-balance contribution (credit sale: `receivable_minor −= total_minor`; credit purchase: `payable_minor −= total_minor`). The reversal downstream guard (Plan 3, §4.5) guarantees no partial allocation or return remains at reversal time, so the full `total_minor` is exactly what was booked. Cash invoices have `outstanding_minor = 0` and no party row, so this is a no-op for them.
4. **Void marker** (§4.5 clause 4): when the target is `SaleRecorded`/`PurchaseRecorded`, set that row's `reversed = 1` in `sales`/`purchases`. The `sale_lines`/`lot_consumptions` (and purchase equivalents) are **left in place for audit**, so every `sale_lines`-reading report and reconciliation check #2 must filter `WHERE reversed = 0` (Plan 4). Otherwise the frozen profit engine (unchanged by reversal) would disagree with the journal (netted to zero by clause 1).

The projector loads the target event from the `events` table by id and dispatches on its type.

**Reversal posting date (resolved ambiguity — MINOR 3, coordinated with Plan 3):** the earlier draft read an undocumented `reversalDate` payload field. That field is **dropped**. `TransactionReversed` is narrowed to full same-day voids of erroneous entries (§4.5), so the reversal journal lines post at the **target's own business date** — the void books into the *original* period, keeping period-level reports internally consistent (a January error reversed in January nets to zero within January). Plan 3's command handler must therefore NOT add a `reversalDate` to the payload; if a later requirement needs current-period voids, that becomes an explicit payload field designed jointly with Plan 3.

- [ ] **Step 1: Write the failing test (reverse a credit sale)**

Add to the `tests` module:

```rust
    #[test]
    fn transaction_reversed_unwinds_sale() {
        let mut conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("devA");
        seed_accounts(&mut conn, &mut hlc);
        record(&mut conn, &mut hlc, 1000, "ItemDefined",
            json!({"itemId": "i1", "sku": "S1", "name": "W", "unit": "ea"}));
        record(&mut conn, &mut hlc, 1000, "PartyCreated",
            json!({"partyId": "cust1", "name": "Bob", "kind": "customer"}));
        record(&mut conn, &mut hlc, 1000, "PurchaseRecorded",
            json!({"purchaseId": "po1", "supplierId": null, "date": "2026-01-01", "terms": "cash",
                   "lines": [{"itemId": "i1", "qty": 10, "unitCostMinor": 100, "lotId": "lot1"}]}));
        let sale = record(&mut conn, &mut hlc, 1000, "SaleRecorded",
            json!({"saleId": "so1", "customerId": "cust1", "date": "2026-01-10", "terms": "credit",
                   "lines": [{"itemId": "i1", "qty": 4, "unitPriceMinor": 250,
                              "lotConsumption": [{"lotId": "lot1", "qtyTaken": 4, "unitCostMinor": 100}]}]}));

        // Reverse it: frozen, pre-negated lines fully negate the sale's postings.
        // Each line names its account by its frozen, rebuild-stable `accountId`.
        // Original: Dr AR 1000 / Cr Sales 1000 ; Dr COGS 400 / Cr Inv 400.
        // Reversal: Cr AR 1000 / Dr Sales 1000 ; Cr COGS 400 / Dr Inv 400.
        let rev = record(&mut conn, &mut hlc, 1000, "TransactionReversed",
            json!({"targetEventId": sale.id, "targetType": "SaleRecorded", "reason": "entry error",
                   "reversalJournalLines": [
                       {"accountId": "acct_sales", "debitMinor": 1000, "creditMinor": 0},
                       {"accountId": "acct_accounts_receivable", "debitMinor": 0, "creditMinor": 1000},
                       {"accountId": "acct_inventory", "debitMinor": 400, "creditMinor": 0},
                       {"accountId": "acct_cogs", "debitMinor": 0, "creditMinor": 400}]}));

        // Clause 1 posted the frozen lines (by their frozen accountId → 4 journal
        // rows on the reversal txn) and they net to zero (balanced double-entry).
        // The reversal handler posts under txn_id = the reversal event's id.
        let (rev_lines, rev_d, rev_c): (i64, i64, i64) = conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(debit_minor),0), COALESCE(SUM(credit_minor),0)
             FROM journal_lines WHERE txn_id = ?1", [&rev.id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert_eq!(rev_lines, 4, "all 4 frozen reversal lines were posted by their frozen accountId");
        assert_eq!(rev_d, rev_c, "reversal txn's journal nets to zero");
        assert_eq!(rev_d, 1400, "1000 sales + 400 inventory debits");

        // Inventory restored to 10 (lot re-credited from lot_consumptions).
        let rem: i64 = conn.query_row("SELECT qty_remaining FROM inventory_lots WHERE id='lot1'", [], |r| r.get(0)).unwrap();
        assert_eq!(rem, 10);
        // GL flat: Sales, COGS back to 0; Inventory back to 1000.
        let sales_bal: i64 = conn.query_row("SELECT balance_minor FROM accounts WHERE system_role='sales'", [], |r| r.get(0)).unwrap();
        let inv_bal: i64 = conn.query_row("SELECT balance_minor FROM accounts WHERE system_role='inventory'", [], |r| r.get(0)).unwrap();
        assert_eq!(sales_bal, 0);
        assert_eq!(inv_bal, 1000);

        // CRITICAL 1 — settlement convenience columns are also unwound.
        let outstanding: i64 = conn.query_row("SELECT outstanding_minor FROM sales WHERE id='so1'", [], |r| r.get(0)).unwrap();
        let recv: i64 = conn.query_row("SELECT receivable_minor FROM party_balances WHERE party_id='cust1'", [], |r| r.get(0)).unwrap();
        let ar_bal: i64 = conn.query_row("SELECT balance_minor FROM accounts WHERE system_role='accounts_receivable'", [], |r| r.get(0)).unwrap();
        assert_eq!(outstanding, 0, "voided sale must not still show as owed");
        assert_eq!(recv, 0, "customer receivable unwound");
        assert_eq!(ar_bal, 0, "A/R GL flat; check #4 net form: Σrecv − Σunalloc_cr = 0 = A/R GL");

        // CRITICAL 2 — the void marker is set (sale_lines left in place for audit).
        let reversed: i64 = conn.query_row("SELECT reversed FROM sales WHERE id='so1'", [], |r| r.get(0)).unwrap();
        assert_eq!(reversed, 1, "clause 4 marks the sale reversed");
        let sale_line_count: i64 = conn.query_row("SELECT COUNT(*) FROM sale_lines WHERE sale_id='so1'", [], |r| r.get(0)).unwrap();
        assert_eq!(sale_line_count, 1, "sale_lines retained for audit despite reversal");
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p accounting-core transaction_reversed_unwinds_sale`
Expected: FAIL — dispatcher returns `unknown` for `TransactionReversed`.

- [ ] **Step 3: Implement the handler**

Add above the `tests` module:

```rust
fn transaction_reversed(tx: &Connection, ev: &LedgerEvent) -> rusqlite::Result<()> {
    let p = &ev.payload;
    let target_id = ps(p, "targetEventId");

    // Load the target event.
    let (target_type, target_payload_text): (String, String) = tx.query_row(
        "SELECT type, json(payload) FROM events WHERE id = ?1", [target_id],
        |r| Ok((r.get(0)?, r.get(1)?)))?;
    let tp: Value = serde_json::from_str(&target_payload_text).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(e))
    })?;
    // Reversal posts at the TARGET's own business date (voids book into the
    // original period). No `reversalDate` payload field — see the task preamble.
    let post_date = ps(&tp, "date");

    // 1. Financial: post the frozen, pre-negated reversal lines verbatim. Each
    // line carries its account's `accountId` directly — account ids are stable
    // across rebuild (frozen in AccountOpened) and can address any account,
    // including user-created ones with a NULL system_role. Post the id as-is.
    for (i, rl) in parr(p, "reversalJournalLines").iter().enumerate() {
        let account_id = ps(rl, "accountId");
        let debit = pi(rl, "debitMinor");
        let credit = pi(rl, "creditMinor");
        post_line(tx, &ev.id, &ev.id, i, account_id, debit, credit, post_date, Some("reversal"))?;
    }

    // 2. Inventory inverse, keyed on the target's category.
    match target_type.as_str() {
        // lot-creating → zero the lots created.
        "PurchaseRecorded" | "InventoryFound" | "OpeningBalancesRecorded" => {
            tx.execute(
                "UPDATE inventory_lots SET qty_remaining = 0 WHERE source_event_id = ?1",
                [target_id],
            )?;
        }
        // lot-consuming (sale) → restore via the recorded consumptions.
        "SaleRecorded" => {
            let mut stmt = tx.prepare(
                "SELECT lc.lot_id, lc.qty_taken FROM lot_consumptions lc
                 JOIN sale_lines sl ON sl.id = lc.sale_line_id
                 WHERE sl.sale_id = ?1")?;
            let rows: Vec<(String, i64)> = stmt
                .query_map([ps(&tp, "saleId")], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            for (lot_id, qty) in rows {
                adjust_lot_remaining(tx, &lot_id, qty)?;
            }
        }
        // lot-consuming (from payload).
        "PurchaseReturnRecorded" => {
            for line in parr(&tp, "lines") {
                adjust_lot_remaining(tx, ps(line, "lotId"), pi(line, "qty"))?;
            }
        }
        "InventoryAdjusted" => {
            for line in parr(&tp, "lines") {
                // qtyDelta is negative; restoring re-adds its magnitude.
                adjust_lot_remaining(tx, ps(line, "lotId"), -pi(line, "qtyDelta"))?;
            }
        }
        // lot-restoring → re-decrement.
        "SaleReturnRecorded" => {
            for line in parr(&tp, "lines") {
                for lr in parr(line, "lotReturns") {
                    adjust_lot_remaining(tx, ps(lr, "lotId"), -pi(lr, "qtyReturned"))?;
                }
            }
        }
        _ => {} // no inventory effect (payments, transfer, expense)
    }

    // 3. Allocation/settlement for allocation-bearing targets.
    match target_type.as_str() {
        "PaymentReceived" | "PaymentMade" | "PaymentAllocated" => {
            let is_sale_dir = target_type == "PaymentReceived";
            // Re-open each settled invoice and restore party balances.
            let mut stmt = tx.prepare(
                "SELECT target_id, target_type, amount_minor FROM payment_allocations WHERE event_id = ?1")?;
            let allocs: Vec<(String, String, i64)> = stmt
                .query_map([target_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<_>>()?;
            for (tid, ttype, amt) in &allocs {
                let table = if ttype == "sale" { "sales" } else { "purchases" };
                let upd = format!("UPDATE {table} SET outstanding_minor = outstanding_minor + ?2 WHERE id = ?1");
                tx.execute(&upd, rusqlite::params![tid, amt])?;
            }
            if target_type == "PaymentAllocated" {
                // Restore the credit that was drawn down, and the receivable/payable.
                let party = ps(&tp, "partyId");
                for (_tid, ttype, amt) in &allocs {
                    if ttype == "sale" {
                        adjust_party_balance(tx, party, *amt, 0, *amt, 0)?;
                    } else {
                        adjust_party_balance(tx, party, 0, *amt, 0, *amt)?;
                    }
                }
            } else {
                // PaymentMade/Received: reverse the allocated receivable/payable and the
                // held unallocated remainder, then remove the payment record.
                let payment_id = ps(&tp, "paymentId");
                let party = if is_sale_dir { ps(&tp, "customerId") } else { ps(&tp, "supplierId") };
                let amount: i64 = tx.query_row(
                    "SELECT amount_minor FROM payments WHERE id = ?1", [payment_id], |r| r.get(0))?;
                let allocated: i64 = allocs.iter().map(|(_, _, a)| *a).sum();
                let remainder = amount - allocated;
                if is_sale_dir {
                    adjust_party_balance(tx, party, allocated, 0, -remainder, 0)?;
                } else {
                    adjust_party_balance(tx, party, 0, allocated, 0, -remainder)?;
                }
                tx.execute("DELETE FROM payments WHERE id = ?1", [payment_id])?;
            }
            tx.execute("DELETE FROM payment_allocations WHERE event_id = ?1", [target_id])?;
        }
        // 3b. Invoice-creating targets: unwind the derived settlement convenience
        // columns the frozen financial lines don't touch (CRITICAL 1). The reversal
        // downstream guard (Plan 3) guarantees no partial allocation/return remains,
        // so `total_minor` is exactly what was booked. Cash invoices → no-op
        // (outstanding already 0, customer/supplier NULL).
        "SaleRecorded" => {
            let (customer, total): (Option<String>, i64) = tx.query_row(
                "SELECT customer_id, total_minor FROM sales WHERE id = ?1",
                [ps(&tp, "saleId")], |r| Ok((r.get(0)?, r.get(1)?)))?;
            tx.execute("UPDATE sales SET outstanding_minor = 0 WHERE id = ?1", [ps(&tp, "saleId")])?;
            if let Some(c) = customer {
                adjust_party_balance(tx, &c, -total, 0, 0, 0)?; // receivable -= total
            }
        }
        "PurchaseRecorded" => {
            let (supplier, total): (Option<String>, i64) = tx.query_row(
                "SELECT supplier_id, total_minor FROM purchases WHERE id = ?1",
                [ps(&tp, "purchaseId")], |r| Ok((r.get(0)?, r.get(1)?)))?;
            tx.execute("UPDATE purchases SET outstanding_minor = 0 WHERE id = ?1", [ps(&tp, "purchaseId")])?;
            if let Some(s) = supplier {
                adjust_party_balance(tx, &s, 0, -total, 0, 0)?; // payable -= total
            }
        }
        _ => {}
    }

    // 4. Void marker (§4.5 clause 4): flag the sale/purchase reversed, leaving
    // sale_lines / lot_consumptions in place for audit. Reports and check #2 filter
    // `WHERE reversed = 0`.
    match target_type.as_str() {
        "SaleRecorded" => {
            tx.execute("UPDATE sales SET reversed = 1 WHERE id = ?1", [ps(&tp, "saleId")])?;
        }
        "PurchaseRecorded" => {
            tx.execute("UPDATE purchases SET reversed = 1 WHERE id = ?1", [ps(&tp, "purchaseId")])?;
        }
        _ => {}
    }
    Ok(())
}
```

Add the dispatch arm before `other =>`:

```rust
        "TransactionReversed" => transaction_reversed(tx, ev),
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p accounting-core transaction_reversed_unwinds_sale`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/accounting-core/src/projectors.rs
git commit -m "feat: project TransactionReversed (four-part: financial + inventory + settlement + void marker)"
```

---

### Task 14: rebuild + projection_cursor + determinism

**Files:**
- Modify: `crates/accounting-core/src/projectors.rs`
- Modify: `crates/accounting-core/src/lib.rs`

`rebuild` (spec §3 Rebuildability): DELETE every projection table, then replay `read_events` (HLC order) through `apply_event`, advancing `projection_cursor` to each event's `hlc`. Because all row ids are deterministic functions of their source event (§4.5), a rebuild reproduces byte-identical projected state. Takes `&mut Connection` so it can wrap the whole replay in one transaction (all-or-nothing rebuild).

- [ ] **Step 1: Write the failing determinism + cursor test**

Add to the `tests` module:

```rust
    /// Full ordered dump of EVERY projection table as text, so a divergence in any
    /// column of any table (journal_lines, sale_lines, lot_consumptions,
    /// payment_allocations, returns, doc-JSONB, generated columns, …) is caught —
    /// not just accounts/lots/party_balances. Each table is serialized to a stable
    /// string via `quote(...)` over all columns, ordered by primary key / natural
    /// key. `doc` BLOBs are rendered with `json(doc)` so JSONB bytes compare by
    /// canonical text rather than storage layout.
    fn full_dump(conn: &Connection) -> String {
        // (table, ordered SELECT of a single concatenated text column per row).
        let queries: &[(&str, &str)] = &[
            ("users", "SELECT quote(id)||'|'||json(doc)||'|'||quote(created_at) FROM users ORDER BY id"),
            ("accounts", "SELECT quote(id)||'|'||json(doc)||'|'||quote(balance_minor) FROM accounts ORDER BY id"),
            ("items", "SELECT quote(id)||'|'||json(doc) FROM items ORDER BY id"),
            ("parties", "SELECT quote(id)||'|'||json(doc) FROM parties ORDER BY id"),
            ("inventory_lots", "SELECT quote(id)||'|'||quote(item_id)||'|'||quote(source_event_id)||'|'||quote(purchase_id)||'|'||quote(unit_cost_minor)||'|'||quote(qty_received)||'|'||quote(qty_remaining)||'|'||quote(acquired_at)||'|'||quote(supplier_id) FROM inventory_lots ORDER BY id"),
            ("journal_lines", "SELECT quote(id)||'|'||quote(event_id)||'|'||quote(txn_id)||'|'||quote(account_id)||'|'||quote(debit_minor)||'|'||quote(credit_minor)||'|'||quote(date)||'|'||quote(memo) FROM journal_lines ORDER BY id"),
            ("sales", "SELECT quote(id)||'|'||quote(event_id)||'|'||quote(customer_id)||'|'||quote(date)||'|'||quote(terms)||'|'||quote(total_minor)||'|'||quote(outstanding_minor)||'|'||quote(reversed) FROM sales ORDER BY id"),
            ("sale_lines", "SELECT quote(id)||'|'||quote(sale_id)||'|'||quote(item_id)||'|'||quote(qty)||'|'||quote(unit_price_minor)||'|'||quote(revenue_minor)||'|'||quote(cogs_minor)||'|'||quote(date) FROM sale_lines ORDER BY id"),
            ("lot_consumptions", "SELECT quote(id)||'|'||quote(sale_line_id)||'|'||quote(lot_id)||'|'||quote(qty_taken)||'|'||quote(unit_cost_minor) FROM lot_consumptions ORDER BY id"),
            ("purchases", "SELECT quote(id)||'|'||quote(event_id)||'|'||quote(supplier_id)||'|'||quote(date)||'|'||quote(terms)||'|'||quote(total_minor)||'|'||quote(outstanding_minor)||'|'||quote(reversed) FROM purchases ORDER BY id"),
            ("purchase_lines", "SELECT quote(id)||'|'||quote(purchase_id)||'|'||quote(item_id)||'|'||quote(qty)||'|'||quote(unit_cost_minor)||'|'||quote(lot_id) FROM purchase_lines ORDER BY id"),
            ("payments", "SELECT quote(id)||'|'||quote(event_id)||'|'||quote(party_id)||'|'||quote(direction)||'|'||quote(amount_minor)||'|'||quote(date) FROM payments ORDER BY id"),
            ("payment_allocations", "SELECT quote(id)||'|'||quote(event_id)||'|'||quote(payment_id)||'|'||quote(target_id)||'|'||quote(target_type)||'|'||quote(amount_minor) FROM payment_allocations ORDER BY id"),
            ("party_balances", "SELECT quote(party_id)||'|'||quote(receivable_minor)||'|'||quote(payable_minor)||'|'||quote(unallocated_cr_minor)||'|'||quote(unallocated_dr_minor) FROM party_balances ORDER BY party_id"),
            ("returns", "SELECT quote(id)||'|'||quote(event_id)||'|'||quote(return_type)||'|'||quote(original_id)||'|'||quote(date)||'|'||quote(revenue_reversed_minor)||'|'||quote(cost_restored_minor) FROM returns ORDER BY id"),
            ("return_lines", "SELECT quote(id)||'|'||quote(return_id)||'|'||quote(item_id)||'|'||quote(qty)||'|'||quote(unit_price_minor)||'|'||quote(unit_cost_minor)||'|'||quote(lot_id) FROM return_lines ORDER BY id"),
            ("expenses", "SELECT quote(id)||'|'||quote(event_id)||'|'||quote(account_id)||'|'||quote(amount_minor)||'|'||quote(date)||'|'||quote(memo)||'|'||quote(terms) FROM expenses ORDER BY id"),
        ];
        let mut out = String::new();
        for (name, sql) in queries {
            out.push_str("== ");
            out.push_str(name);
            out.push('\n');
            let mut stmt = conn.prepare(sql).unwrap();
            let rows: Vec<String> = stmt
                .query_map([], |r| r.get::<_, String>(0)).unwrap()
                .collect::<rusqlite::Result<_>>().unwrap();
            for row in rows {
                out.push_str(&row);
                out.push('\n');
            }
        }
        out
    }

    #[test]
    fn rebuild_is_deterministic_and_sets_cursor() {
        let mut conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("devA");
        seed_accounts(&mut conn, &mut hlc);
        record(&mut conn, &mut hlc, 1000, "ItemDefined",
            json!({"itemId": "i1", "sku": "S1", "name": "W", "unit": "ea"}));
        record(&mut conn, &mut hlc, 1000, "PartyCreated",
            json!({"partyId": "cust1", "name": "Bob", "kind": "customer"}));
        record(&mut conn, &mut hlc, 1000, "PurchaseRecorded",
            json!({"purchaseId": "po1", "supplierId": null, "date": "2026-01-01", "terms": "cash",
                   "lines": [{"itemId": "i1", "qty": 10, "unitCostMinor": 100, "lotId": "lot1"}]}));
        let sale = record(&mut conn, &mut hlc, 1000, "SaleRecorded",
            json!({"saleId": "so1", "customerId": "cust1", "date": "2026-01-10", "terms": "credit",
                   "lines": [{"itemId": "i1", "qty": 4, "unitPriceMinor": 250,
                              "lotConsumption": [{"lotId": "lot1", "qtyTaken": 4, "unitCostMinor": 100}]}]}));
        // Exercise payments, allocations, returns, and a reversal so those tables
        // are non-empty and their rebuild-determinism is actually asserted.
        record(&mut conn, &mut hlc, 1000, "PaymentReceived",
            json!({"paymentId": "pay1", "customerId": "cust1", "amountMinor": 500, "date": "2026-01-11",
                   "allocations": [{"saleId": "so1", "amountMinor": 500}]}));
        record(&mut conn, &mut hlc, 1000, "SaleReturnRecorded",
            json!({"returnId": "ret1", "originalSaleId": "so1", "date": "2026-01-12",
                   "lines": [{"itemId": "i1", "qty": 1, "unitPriceMinor": 250,
                              "lotReturns": [{"lotId": "lot1", "qtyReturned": 1, "unitCostMinor": 100}]}]}));
        // A second cash sale we then fully void, to populate `reversed = 1`.
        let sale2 = record(&mut conn, &mut hlc, 1000, "SaleRecorded",
            json!({"saleId": "so2", "customerId": null, "date": "2026-01-13", "terms": "cash",
                   "lines": [{"itemId": "i1", "qty": 1, "unitPriceMinor": 250,
                              "lotConsumption": [{"lotId": "lot1", "qtyTaken": 1, "unitCostMinor": 100}]}]}));
        record(&mut conn, &mut hlc, 1000, "TransactionReversed",
            json!({"targetEventId": sale2.id, "targetType": "SaleRecorded", "reason": "oops",
                   "reversalJournalLines": [
                       {"accountId": "acct_sales", "debitMinor": 250, "creditMinor": 0},
                       {"accountId": "acct_bank", "debitMinor": 0, "creditMinor": 250},
                       {"accountId": "acct_inventory", "debitMinor": 100, "creditMinor": 0},
                       {"accountId": "acct_cogs", "debitMinor": 0, "creditMinor": 100}]}));
        let _ = sale;

        let before = full_dump(&conn);
        let last_hlc: String = conn.query_row("SELECT MAX(hlc) FROM events", [], |r| r.get(0)).unwrap();

        rebuild(&mut conn).unwrap();

        let after = full_dump(&conn);
        assert_eq!(before, after, "rebuild must reproduce byte-identical projected state across ALL tables");

        // Cursor advanced to the last replayed event.
        let cursor: String = conn.query_row(
            "SELECT last_hlc FROM projection_cursor WHERE projection='main'", [], |r| r.get(0)).unwrap();
        assert_eq!(cursor, last_hlc);

        // No duplicate rows introduced.
        let sales: i64 = conn.query_row("SELECT COUNT(*) FROM sales", [], |r| r.get(0)).unwrap();
        let lots: i64 = conn.query_row("SELECT COUNT(*) FROM inventory_lots", [], |r| r.get(0)).unwrap();
        assert_eq!((sales, lots), (2, 1));
        // The voided cash sale carries its marker after rebuild.
        let reversed: i64 = conn.query_row("SELECT reversed FROM sales WHERE id='so2'", [], |r| r.get(0)).unwrap();
        assert_eq!(reversed, 1);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p accounting-core rebuild_is_deterministic_and_sets_cursor`
Expected: FAIL — `rebuild` not found.

- [ ] **Step 3: Implement `rebuild`**

Add above the `tests` module (it needs `read_events`, so add `use crate::events::read_events;` alongside the existing `use crate::events::LedgerEvent;` at the top of the file):

```rust
/// Projection tables in child-before-parent order, so deletes don't trip
/// foreign keys. `events` and `projection_cursor` are NOT projections and are
/// never touched here.
const PROJECTION_TABLES: &[&str] = &[
    "lot_consumptions", "sale_lines", "sales",
    "purchase_lines", "purchases",
    "return_lines", "returns",
    "payment_allocations", "payments",
    "expenses", "journal_lines",
    "party_balances", "inventory_lots",
    "parties", "items", "accounts", "users",
];

/// Drop all projection state and replay the entire event log in HLC order,
/// advancing `projection_cursor`. Deterministic: identical input log → identical
/// output tables (spec §3). Runs in one transaction so a failed replay leaves the
/// prior projection intact.
pub fn rebuild(conn: &mut Connection) -> rusqlite::Result<()> {
    let events = read_events(conn)?;
    let tx = conn.transaction()?;
    for table in PROJECTION_TABLES {
        tx.execute(&format!("DELETE FROM {table}"), [])?;
    }
    tx.execute("DELETE FROM projection_cursor WHERE projection = 'main'", [])?;
    let mut last_hlc = String::new();
    for ev in &events {
        apply_event(&tx, ev)?;
        last_hlc = ev.hlc.clone();
    }
    if !last_hlc.is_empty() {
        tx.execute(
            "INSERT INTO projection_cursor (projection, last_hlc) VALUES ('main', ?1)
             ON CONFLICT(projection) DO UPDATE SET last_hlc = excluded.last_hlc",
            [last_hlc],
        )?;
    }
    tx.commit()?;
    Ok(())
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p accounting-core rebuild_is_deterministic_and_sets_cursor`
Expected: PASS.

- [ ] **Step 5: Re-export `rebuild` and run the full suite**

In `crates/accounting-core/src/lib.rs`, update the projectors re-export line:

```rust
pub use projectors::{apply_event, rebuild};
```

Run: `cargo test -p accounting-core`
Expected: PASS (all Plan 1 + Plan 2 tests across db, hlc, events, genesis, projectors).

- [ ] **Step 6: Commit**

```bash
git add crates/accounting-core/src/projectors.rs crates/accounting-core/src/lib.rs
git commit -m "feat: add deterministic projection rebuild with cursor bookmark"
```

---

## Definition of Done (Plan 2)

- `cargo test -p accounting-core` passes with every task's tests green.
- **Every §5–§6 projection table exists** in `schema.sql` with the spec's generated columns, indexes, and partial indexes; `apply_schema` remains idempotent.
- **`apply_event` has a handler for all 21 event types** in §4.4 (4 master-data create, 4 master-data update, and the transactional set: PurchaseRecorded, SaleRecorded, PaymentMade, PaymentReceived, PaymentAllocated, ExpenseRecorded, TransferRecorded, InventoryAdjusted, InventoryFound, OpeningBalancesRecorded, SaleReturnRecorded, PurchaseReturnRecorded, TransactionReversed). Unknown types return a loud error, never a silent no-op.
- **Money is integer minor units** everywhere; **business dates** are `TEXT 'YYYY-MM-DD'`; quantities are integers.
- **`accounts.balance_minor` is maintained via `normal_side`** (assets/expenses ↑ on debit; liabilities/equity/income ↑ on credit) through the single `post_line` helper.
- **Well-known accounts resolve by `system_role`**, never by name or id; the null-safe `IS NOT` operator is used wherever a nullable column is compared (Plan 3/4 queries; this plan's projector uses exact-id/exact-role equality only).
- **`party_balances`, `outstanding_minor`, `reversed`, and `qty_remaining` obey the spec contracts:** credit invoices raise receivable/payable and set outstanding; allocations (at payment time and via `PaymentAllocated`) reduce them; a credit `ExpenseRecorded` raises the supplier's payable (§4.5 credit-expense party contract); the return→invoice/party-balance contract caps reductions at remaining outstanding and routes excess to unallocated credit, refunding to Bank for party-less cash returns; and the **four-part** `TransactionReversed` contract unwinds (1) financial, (2) inventory, (3) settlement — including the derived `outstanding_minor`/`receivable`/`payable` columns of a reversed credit invoice — and (4) sets the `reversed = 1` void marker on the sale/purchase while leaving `sale_lines`/`lot_consumptions` in place for audit.
- **Reversing a credit sale/purchase leaves no owed balance:** after reversal, `outstanding_minor == 0`, the party's `receivable`/`payable` is unwound, and the A/R/A/P GL is flat — check #4's net form holds (verified by `transaction_reversed_unwinds_sale`).
- **The `reversed` void-marker column exists** on both `sales` and `purchases` (spec §6.2/§6.3), asserted by the schema-existence test; downstream reports and reconciliation check #2 must filter `WHERE reversed = 0` (Plan 4).
- **Rebuild is deterministic across ALL projection tables:** DELETE-all + replay reproduces byte-identical state for every table (a full ordered `quote(...)`/`json(doc)` dump — journal_lines, sales, sale_lines, lot_consumptions, purchases, purchase_lines, payments, payment_allocations, party_balances, returns, return_lines, expenses, and the `doc`-JSONB master tables), verified by `rebuild_is_deterministic_and_sets_cursor` over a fixture exercising payments, allocations, returns, and a reversal; and `projection_cursor` records the last-applied HLC.
- **Reconciliation-relevant assertions pass:** aggregate open-lot value equals the Inventory GL balance after a purchase, after a purchase+sale, and after adjust/found (checks #1); every posted transaction is balanced (check #3).
- **Atomicity pattern is demonstrated:** the tests' `record` helper appends the event and projects it inside one `conn.transaction()`, matching the write-path boundary Plan 3 will use (`append_event(&tx, …)` then `apply_event(&tx, …)` then `tx.commit()`).
- **Cursor-advance obligation recorded for Plan 3:** `apply_event` never advances `projection_cursor` (only `rebuild` does). Plan 3's atomic commit MUST update `projection_cursor` to `ev.hlc` inside the same transaction as `append_event` + `apply_event`, or incremental resume/sync-merge-from-cursor never advances. This is documented in the `apply_event` doc comment and is an explicit acceptance item for Plan 3.
- **All SQL is raw strings / the `.sql` asset** (no ORM), preserving TS-portability; nullable comparisons use the null-safe `IS NOT` operator (Plan 4 queries), never `<>`.
- **No command guards yet** (Plan 3) and **no report queries / periodic reconciliation checks yet** (Plan 4) — the projector trusts that logged events already passed their guards, exactly as replay and future sync-merge will.
