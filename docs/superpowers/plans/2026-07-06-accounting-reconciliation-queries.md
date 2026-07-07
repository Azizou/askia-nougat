# Accounting Core — Reconciliation & Report Queries Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the read-only analytics layer on top of the projected read model — the eight §7 reconciliation/integrity checks and the §8 report queries — as pure functions over the projection tables built in Plans 2–3. Every check returns a typed pass/fail with a descriptive discrepancy; every report returns typed Rust structs. Fully tested against in-memory SQLite by seeding realistic state through the Plan 3 command handlers, with zero Tauri dependency.

**Architecture:** Event-sourced / CQRS (unchanged from Plans 1–3). This plan touches **only the read path**: it issues `SELECT`s against the denormalized projection tables (`accounts`, `inventory_lots`, `journal_lines`, `sale_lines`, `returns`, `party_balances`, `sales`, `purchases`, `payment_allocations`, …). It appends no events and mutates no projection. All SQL lives in raw string constants (never an ORM) so it ports cheaply to a TypeScript / `plugin-sql` runtime later — identical to the Plan 1 discipline.

**Tech Stack:** Rust, `rusqlite` 0.32 (with `bundled` SQLite 3.46.x, providing JSONB, generated columns, `julianday()`, `strftime()`, and the null-safe `IS NOT` operator), `serde_json`. Tests via `cargo test` against in-memory SQLite.

**Spec:** `docs/superpowers/specs/2026-07-06-accounting-schema-design.md`

---

## Plan Series Roadmap

This is **Plan 4 of 4** for the data model. Each produces working, testable software and depends on the prior:

1. **Foundations:** crate scaffold, schema DDL, HLC clock, event store append/read, genesis bootstrap.
2. **Read model + projectors + rebuild:** all §5–§6 projection-table DDL, `apply_event` per event type, the drop-and-replay `rebuild` loop, `projection_cursor`.
3. **Command handlers + guards:** the ~15 command entry points, each running its §4.5 validation guards against the read model, then appending the event and applying the projection in one transaction.
4. **Reconciliation + queries (this plan):** the §7 integrity checks and the §8 report queries — pure read-only functions over the projected state.

(Order note: this plan is last because it reads the fully-projected state that Plans 2–3 produce. It writes nothing, so it introduces no new guards or events.)

Later, outside the data-model series: Tauri IPC wiring, reactivity, UI, and (someday) the sync engine.

---

## Assumed APIs from Plans 1–3

This plan reads state produced by earlier plans. The following symbols are **assumed** to exist. Signatures marked *(assumed)* are Plan 2/3 surface that is not yet frozen; if the implementing plan named them differently, substitute the real names at the top of each test — the SQL and check logic are unaffected.

**From Plan 1 (real, quoted verbatim from the foundations plan):**
- `accounting_core::db::open_in_memory_with_schema() -> rusqlite::Result<Connection>`
- `accounting_core::hlc::Hlc::new(device_id)` and `Hlc::tick(&mut self, physical_now: u64)`
- `accounting_core::events::{append_event, read_events, LedgerEvent}`
- `accounting_core::genesis::{run_genesis, SYSTEM_USER_ID}` — `run_genesis(conn, hlc, physical_now, device_id, owner_user_id, owner_name)`. Seeds the 14-account chart, each account row keyed by `id = format!("acct_{system_role}")` (e.g. `acct_inventory`, `acct_rent`) with a unique `system_role`.

**From Plan 2 *(assumed)*:**
- Plan 2 grows `schema.sql` with the full §5–§6 projection DDL, so **`open_in_memory_with_schema()` returns a connection with every projection table present**.
- `accounting_core::projectors::apply_event(conn: &Connection, event: &LedgerEvent) -> rusqlite::Result<()>` — projects one event.
- `accounting_core::projectors::rebuild(conn: &mut Connection) -> rusqlite::Result<()>` — drops and replays all projections from the log (takes `&mut Connection` because it opens its own transaction internally).

**From Plan 3 (finalized — quoted from `2026-07-06-accounting-command-handlers.md`) — the command handlers used to seed realistic state.** All live in module `crate::commands` (per-event submodules re-exported at `commands`), are named `handle_<event_snake_case>`, and share one signature shape: **first param `ctx: &mut CommandContext`, then flat domain args, returning `Result<LedgerEvent, CommandError>`.** Each runs its §4.5 guards against the projections, then the shared `commit_event` helper wraps `conn.transaction() { append_event + apply_event }` for the atomic event+projection boundary. `CommandContext` (defined in `commands/mod.rs`) bundles the mutable connection, clock, and provenance:

```rust
pub struct CommandContext<'a> {
    pub conn: &'a mut rusqlite::Connection,
    pub hlc: &'a mut crate::hlc::Hlc,
    pub physical_now: u64,
    pub device_id: String,
    pub user_id: String,
}
pub enum CommandError { Validation(String), Db(rusqlite::Error) }
```

The handlers and their real signatures (line-input structs are Plan 3's; `LedgerEvent` re-exported from `crate::events`):

```rust
pub fn handle_item_defined(ctx: &mut CommandContext, item_id: &str, sku: &str, name: &str, unit: &str)
    -> Result<LedgerEvent, CommandError>;

pub fn handle_party_created(ctx: &mut CommandContext, party_id: &str, name: &str, kind: &str)
    -> Result<LedgerEvent, CommandError>;

pub struct PurchaseLineInput { pub item_id: String, pub qty: i64, pub unit_cost_minor: i64 }
pub fn handle_purchase_recorded(ctx: &mut CommandContext, purchase_id: &str, supplier_id: &str,
    date: &str, terms: &str /* "cash"|"credit" */, lines: Vec<PurchaseLineInput>)
    -> Result<LedgerEvent, CommandError>;

pub struct SaleLineInput { pub item_id: String, pub qty: i64, pub unit_price_minor: i64,
    /// explicit (lotId, qtyTaken) picks; None → oldest-lot-first auto-select
    pub lot_picks: Option<Vec<(String, i64)>> }
pub fn handle_sale_recorded(ctx: &mut CommandContext, sale_id: &str, customer_id: &str,
    date: &str, terms: &str, lines: Vec<SaleLineInput>) -> Result<LedgerEvent, CommandError>;

pub struct AllocInput { pub target_id: String, pub target_type: String /* "sale"|"purchase" */, pub amount_minor: i64 }
pub fn handle_payment_received(ctx: &mut CommandContext, payment_id: &str, customer_id: &str,
    amount_minor: i64, date: &str, allocations: Vec<AllocInput>) -> Result<LedgerEvent, CommandError>;

pub struct SaleReturnLineInput { pub item_id: String, pub lot_id: String, pub qty: i64 }
pub fn handle_sale_return_recorded(ctx: &mut CommandContext, return_id: &str, original_sale_id: &str,
    date: &str, lines: Vec<SaleReturnLineInput>) -> Result<LedgerEvent, CommandError>;

pub fn handle_expense_recorded(ctx: &mut CommandContext, expense_id: &str, account_id: &str,
    amount_minor: i64, date: &str, terms: &str, supplier_id: Option<&str>, memo: Option<&str>)
    -> Result<LedgerEvent, CommandError>;

// Full void of a transactional event (spec §4.5). For a sale target, clause 4 sets
// sales.reversed = 1 and nets its journal to zero; the handler runs the reversal
// legal-target / double-void / downstream guards before appending.
pub fn handle_transaction_reversed(ctx: &mut CommandContext, target_event_id: &str, reason: &str)
    -> Result<LedgerEvent, CommandError>;
```

Also available if needed: `handle_user_registered`, `handle_account_opened`, `handle_item_updated`, `handle_party_updated`, `handle_purchase_return_recorded`, `handle_payment_made`, `handle_payment_allocated`, `handle_transfer_recorded`, `handle_inventory_adjusted`, `handle_inventory_found`.

**Handler-minted lot IDs (load-bearing for the fixtures).** The caller does NOT supply lot IDs. `handle_purchase_recorded` mints each line's `lotId` deterministically as `format!("{purchase_id}#lot{i}")` (index `i` within the purchase) and freezes it into the event, so it survives rebuild (spec §4.5). A later sale/return therefore references a lot by that formula: a single-line purchase `pur_1` creates lot `pur_1#lot0`. The reference fixture's `LOT_*` constants below are set to these minted names. Other referenced IDs (`item_id`, `party_id`, `purchase_id`, `sale_id`, `payment_id`, `return_id`, `expense_id`) ARE supplied by the caller (in tests, us).

**Call-site pattern.** Construct one `CommandContext` borrowing the owned `conn`/`hlc`, pass `&mut ctx` to each handler, and scope it in an inner block so the mutable borrow ends before returning the owned connection. `physical_now` stays fixed (the HLC counter increments per append, preserving order); `.unwrap()` each handler result in fixtures since seeding must succeed. Genesis (`run_genesis`, which takes `&Connection`) runs before the `ctx` block.

### Design notes on §7/§8 (all now spec-aligned)

Every item below was a resolved ambiguity in an earlier draft; the spec has since been updated so each is now **mandated** by §7/§8. They are retained here because the exact SQL shape matters for implementers.

1. **Check #2 is returns-aware AND reversed-exclusive (spec §7.2).** The naive identity `SUM(sale_lines.revenue − cogs) == (Sales − COGS from journal_lines)` holds only with no returns and no reversals. Two corrections keep it true for all valid states: (a) **net of sale returns** — a `SaleReturnRecorded` posts `Dr Sales` / `Cr COGS`, moving the journal but leaving `sale_lines` intact (returns live in `returns`/`return_lines`, §6.8), so subtract `SUM(returns.revenue_reversed_minor − cost_restored_minor WHERE return_type='sale_return')`; (b) **exclude reversed sales** — the reversal contract's clause 4 sets `sales.reversed = 1` and nets that sale's journal to zero while leaving its `sale_lines` in place for audit, so the engine side must join `sales` and filter `WHERE reversed = 0`. (The §8.2 **report** query answers a different question — "realized gross profit from non-reversed sales" — and is kept spec-literal for its own semantics; it carries the same `reversed = 0` filter, see #5 below.)

2. **Check #4 uses the net form and reconciles the aggregate (spec §7.4).** `journal_lines` (§6.1) carries `account_id`, not `party_id`, so per-party journal reconciliation is impossible — the identity is aggregate. Critically, Plan 3 posts the **full** payment amount to A/R / A/P (not just the allocated portion), so a prepayment leaves the invoice's receivable at 0 while the GL goes negative by the unallocated remainder. The correct, spec-mandated identity is therefore the **net form**: `SUM(party_balances.receivable_minor) − SUM(unallocated_cr_minor) == A/R GL (system_role='accounts_receivable')`, and symmetrically `SUM(payable_minor) − SUM(unallocated_dr_minor) == A/P GL`. The naive `Σreceivable == A/R GL` form false-alarms on any prepayment.

3. **§8 report queries parameterize the anchor date (`'now'` → a bound `?`) — now mandated by §8.4.** The spec §8 SQL text shows `date('now', …)` / `julianday('now')`, but §8.4 explicitly states the implementation binds a parameterized anchor `date(?, …)` for deterministic tests. Every §8 query below keeps the exact spec expression but substitutes a bound parameter for the anchor, so tests pass a fixed `'2026-07-06'` and production passes `"now"`. Behaves identically to the spec when `?1 = 'now'`; preserves TS-portability (the TS runtime binds the same parameter).

4. **Check #5 is terms-aware (spec §7.5).** For a **credit** sale, `outstanding_minor = MAX(0, total − allocated − returned)`; for a **cash** sale, `outstanding_minor = 0` exactly (the invoice is stored `total = revenue, outstanding = 0` by fiat, so applying the credit identity would falsely expect `total`). The check branches on `terms`, but keeps the `outstanding < 0` guard for **all** rows (so a stray non-zero cash outstanding is still caught — the fix is NOT a `WHERE terms='credit'`). "returned" is `SUM(returns.revenue_reversed_minor)` for sales and `SUM(returns.cost_restored_minor)` for purchases (§6.8: purchase returns set `revenue_reversed_minor = 0`).

5. **Reversed sales are excluded from every `sale_lines`/`sales` report (spec §8.4).** A `sales.reversed` column (§6.2/§6.3) marks voided sales. Because a reversed sale nets the journal to zero but leaves `sale_lines` intact, any report reading `sale_lines` must join `sales` and filter `WHERE reversed = 0`: §8.1 (units sold), §8.2 gross, §8.4 gross-margin-per-item, and §8.4 best/worst sellers all carry this filter. (Balance-sheet/P&L read the journal, which the reversal already zeroed, so they need no such filter.)

6. **Return rate counts sale returns only (spec §8.4).** `return_lines` holds both sale and purchase return lines. The customer return-rate numerator must filter `return_type = 'sale_return'` (via a `returns` join) so supplier returns don't inflate it.

---

## File Structure

All paths are inside the existing crate at `crates/accounting-core/`.

- `src/reconciliation.rs` — the eight §7 integrity checks + shared `CheckOutcome`/`Check` types + `run_all_checks` aggregator. One responsibility: comparing redundant projections and reporting drift. All SQL is raw string constants at the top of the file (TS-portable).
- `src/queries.rs` — the §8 report queries: one typed function + one raw-SQL constant per report, plus the typed row structs they return. One responsibility: read-only reporting over the projection tables.
- `src/test_support.rs` — `#[cfg(test)]`-gated shared test fixture: `seed_reference_business` builds one realistic, fully-projected business by calling the Plan 3 command handlers, so both `reconciliation` and `queries` tests exercise the same known state with known expected numbers. (Compiled only under `cfg(test)`, so it ships nothing.)
- `src/lib.rs` — modified to declare/re-export the new modules.

---

## The reference business (shared test fixture)

`seed_reference_business` produces this exact state (all money in minor units; all dates business dates). Every number below is asserted by the tests, so the fixture is the single source of expected values.

| Step | Event (via Plan 3 handler) | Effect |
|---|---|---|
| genesis | `run_genesis` | 14 seeded accounts (`acct_*`), owner user, all balances 0 |
| items | `handle_item_defined` ×2 | `item_widget` (SKU `W-1`, `ea`), `item_gadget` (SKU `G-1`, `ea`) |
| parties | `handle_party_created` ×2 | `cust_acme` (customer), `supp_globex` (supplier) |
| P1 | `handle_purchase_recorded` **credit** `pur_1` 2026-06-01 | widget 100 @ 500 → minted lot `pur_1#lot0`; Dr Inventory 50000 / Cr A/P 50000 |
| P2 | `handle_purchase_recorded` **credit** `pur_2` 2026-06-15 | widget 50 @ 600 → minted lot `pur_2#lot0`; Dr Inventory 30000 / Cr A/P 30000 |
| P3 | `handle_purchase_recorded` **credit** `pur_3` 2026-06-20 | gadget 20 @ 1000 → minted lot `pur_3#lot0`; Dr Inventory 20000 / Cr A/P 20000 |
| S1 | `handle_sale_recorded` **credit** `sale_1` 2026-07-02 | widget 60 @ 900 (oldest-first → `pur_1#lot0`); Dr A/R 54000 / Cr Sales 54000; Dr COGS 30000 / Cr Inventory 30000 |
| S2 | `handle_sale_recorded` **cash** `sale_2` 2026-07-03 | gadget 5 @ 1500 (→ `pur_3#lot0`); Dr Bank 7500 / Cr Sales 7500; Dr COGS 5000 / Cr Inventory 5000 |
| EX | `handle_expense_recorded` **cash** `exp_1` 2026-07-04 | rent 3000 to `acct_rent`; Dr Rent 3000 / Cr Bank 3000 |
| PR | `handle_payment_received` `pay_1` 2026-07-05 | 40000 from `cust_acme`, allocate 40000 → `sale_1`; Dr Bank 40000 / Cr A/R 40000 |
| SR | `handle_sale_return_recorded` `ret_1` 2026-07-06 | widget 10 back to `pur_1#lot0` (price 900 frozen from sale_1); Dr Sales 9000 / Cr A/R 9000; Dr Inventory 5000 / Cr COGS 5000 |

**Resulting projected state (the numbers the tests assert):**

- **Lots (`inventory_lots`):** `pur_1#lot0` recv 100, rem 50 (100−60+10), cost 500, acquired 2026-06-01; `pur_2#lot0` recv 50, rem 50, cost 600, acquired 2026-06-15; `pur_3#lot0` recv 20, rem 15, cost 1000, acquired 2026-06-20.
- **Inventory GL balance:** 70000 = lot valuation 50·500 + 50·600 + 15·1000 = 25000+30000+15000. ✔ check #1.
- **`sale_lines`:** widget rev 54000 / cogs 30000; gadget rev 7500 / cogs 5000. Gross (engine) 26500.
- **`returns`:** `ret_1` sale_return, revenue_reversed 9000, cost_restored 5000. Engine net of returns = 26500 − 4000 = 22500 = journal (Sales 52500 − COGS 30000). ✔ check #2.
- **Account balances:** Inventory 70000, Bank 44500 (7500+40000−3000), Cash 0, A/R 5000 (54000−40000−9000), A/P 100000, Sales 52500 (income), COGS 30000 (expense), Rent 3000 (expense).
- **`party_balances`:** `cust_acme` receivable 5000, unallocated_cr 0; `supp_globex` payable 100000, unallocated_dr 0. ✔ check #4 net form (Σreceivable 5000 − Σunallocated_cr 0 = A/R GL 5000; Σpayable 100000 − Σunallocated_dr 0 = A/P GL 100000).
- **`sales.outstanding_minor`:** `sale_1` = MAX(0, 54000 − 40000 − 9000) = 5000; `sale_2` cash = 0. ✔ check #5.
- **`purchases.outstanding_minor`:** `pur_1`=50000, `pur_2`=30000, `pur_3`=20000. ✔ check #5.
- **Balance-sheet identity:** Assets 119500 (Inv 70000 + Bank 44500 + A/R 5000) = Liabilities 100000 + Equity 0 + net P&L 19500 (Income 52500 − Expense 33000). ✔

`seed_reference_business` returns `()` (all ids are stable string literals exposed as `pub const`s in `test_support`), and the anchor date is `pub const ANCHOR: &str = "2026-07-06"` (matches the plan's `currentDate`, so every relative-date window is deterministic).

---

### Task 1: Test-support fixture + reconciliation scaffold + check #1 (inventory valuation)

**Files:**
- Create: `crates/accounting-core/src/test_support.rs`
- Create: `crates/accounting-core/src/reconciliation.rs`
- Modify: `crates/accounting-core/src/lib.rs`

- [ ] **Step 1: Write the shared test fixture**

Create `crates/accounting-core/src/test_support.rs`. This is `#[cfg(test)]`-only and drives the real Plan 3 handlers (module `crate::commands`, `handle_*` names, `&mut CommandContext`) to build the reference business:

```rust
//! Shared, #[cfg(test)]-only fixtures. Compiled only for tests; ships nothing.
#![cfg(test)]

use crate::commands::{
    handle_expense_recorded, handle_item_defined, handle_party_created, handle_payment_received,
    handle_purchase_recorded, handle_sale_recorded, handle_sale_return_recorded,
    AllocInput, CommandContext, PurchaseLineInput, SaleLineInput, SaleReturnLineInput,
};
use crate::db::open_in_memory_with_schema;
use crate::genesis::run_genesis;
use crate::hlc::Hlc;
use rusqlite::Connection;

/// Fixed report anchor date. Equals the plan's currentDate so every relative
/// window (`start of month`, `-6 months`, aging buckets) is deterministic.
pub const ANCHOR: &str = "2026-07-06";

// Stable ids referenced across the fixture and assertions.
pub const WIDGET: &str = "item_widget";
pub const GADGET: &str = "item_gadget";
pub const CUST: &str = "cust_acme";
pub const SUPP: &str = "supp_globex";
// Lot ids are MINTED by handle_purchase_recorded as `{purchase_id}#lot{index}`
// (Plan 3, spec §4.5) — the caller never supplies them. Single-line purchases
// here => index 0.
pub const LOT_W1: &str = "pur_1#lot0";
pub const LOT_W2: &str = "pur_2#lot0";
pub const LOT_G1: &str = "pur_3#lot0";
pub const SALE_1: &str = "sale_1";
pub const SALE_2: &str = "sale_2";
pub const PUR_1: &str = "pur_1";

/// Open an in-memory DB with schema and populate it with the reference business
/// (see plan "The reference business"). Returns the connection ready to query
/// (the CommandContext's mutable borrow is released before returning).
pub fn open_seeded() -> (Connection, Hlc) {
    let mut conn = open_in_memory_with_schema().unwrap();
    let mut hlc = Hlc::new("deviceA");
    seed_reference_business(&mut conn, &mut hlc);
    (conn, hlc)
}

pub fn seed_reference_business(conn: &mut Connection, hlc: &mut Hlc) {
    // Genesis takes &Connection and runs before we take the &mut borrow for ctx.
    run_genesis(conn, hlc, 1000, "deviceA", "owner-1", "Jane Owner").unwrap();

    // One CommandContext borrows conn + hlc mutably for the whole seed. physical_now
    // stays fixed; the HLC counter increments per append, preserving replay order.
    let mut ctx = CommandContext {
        conn, hlc, physical_now: 1000,
        device_id: "deviceA".into(), user_id: "owner-1".into(),
    };

    handle_item_defined(&mut ctx, WIDGET, "W-1", "Widget", "ea").unwrap();
    handle_item_defined(&mut ctx, GADGET, "G-1", "Gadget", "ea").unwrap();
    handle_party_created(&mut ctx, CUST, "Acme Co", "customer").unwrap();
    handle_party_created(&mut ctx, SUPP, "Globex", "supplier").unwrap();

    // Purchases — all on credit so A/P is exercised. Lots are minted pur_N#lot0.
    handle_purchase_recorded(&mut ctx, PUR_1, SUPP, "2026-06-01", "credit",
        vec![PurchaseLineInput { item_id: WIDGET.into(), qty: 100, unit_cost_minor: 500 }]).unwrap();
    handle_purchase_recorded(&mut ctx, "pur_2", SUPP, "2026-06-15", "credit",
        vec![PurchaseLineInput { item_id: WIDGET.into(), qty: 50, unit_cost_minor: 600 }]).unwrap();
    handle_purchase_recorded(&mut ctx, "pur_3", SUPP, "2026-06-20", "credit",
        vec![PurchaseLineInput { item_id: GADGET.into(), qty: 20, unit_cost_minor: 1000 }]).unwrap();

    // Sales. Lot selection defaults to oldest-first (lot_picks: None). Widget has
    // only pur_1#lot0 available at sale time; gadget only pur_3#lot0.
    handle_sale_recorded(&mut ctx, SALE_1, CUST, "2026-07-02", "credit",
        vec![SaleLineInput { item_id: WIDGET.into(), qty: 60, unit_price_minor: 900, lot_picks: None }]).unwrap();
    handle_sale_recorded(&mut ctx, SALE_2, CUST, "2026-07-03", "cash",
        vec![SaleLineInput { item_id: GADGET.into(), qty: 5, unit_price_minor: 1500, lot_picks: None }]).unwrap();

    // Cash expense (rent) — no supplier (cash), a memo.
    handle_expense_recorded(&mut ctx, "exp_1", "acct_rent", 3000, "2026-07-04", "cash",
        None, Some("July rent")).unwrap();

    // Partial payment against the credit sale (allocates the full 40000 to sale_1).
    handle_payment_received(&mut ctx, "pay_1", CUST, 40000, "2026-07-05",
        vec![AllocInput { target_id: SALE_1.into(), target_type: "sale".into(), amount_minor: 40000 }]).unwrap();

    // Partial sale return of 10 widgets to their originating lot. The handler
    // freezes the original sale price (900) and lot cost (500); the input carries
    // only item_id + lot_id + qty.
    handle_sale_return_recorded(&mut ctx, "ret_1", SALE_1, "2026-07-06",
        vec![SaleReturnLineInput { item_id: WIDGET.into(), lot_id: LOT_W1.into(), qty: 10 }]).unwrap();
}
```

- [ ] **Step 2: Write the failing test for check #1**

Create `crates/accounting-core/src/reconciliation.rs`:

```rust
use rusqlite::Connection;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::open_seeded;

    #[test]
    fn inventory_valuation_matches_gl_on_correct_state() {
        let (conn, _hlc) = open_seeded();
        assert_eq!(check_inventory_valuation(&conn).unwrap(), CheckOutcome::Pass);
    }

    #[test]
    fn inventory_valuation_fails_when_a_lot_is_corrupted() {
        let (conn, _hlc) = open_seeded();
        // Inject drift: bump one lot's remaining qty so lot value no longer
        // equals the Inventory GL balance the projector maintained.
        conn.execute("UPDATE inventory_lots SET qty_remaining = qty_remaining + 1 WHERE id = 'pur_1#lot0'", []).unwrap();
        match check_inventory_valuation(&conn).unwrap() {
            CheckOutcome::Fail(msg) => assert!(msg.contains("70500") && msg.contains("70000"), "got {msg}"),
            CheckOutcome::Pass => panic!("check must FAIL on corrupted lot"),
        }
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p accounting-core reconciliation::tests::inventory_valuation`
Expected: FAIL — `CheckOutcome` / `check_inventory_valuation` not found (compile error).

- [ ] **Step 4: Implement the shared types and check #1**

At the top of `crates/accounting-core/src/reconciliation.rs`, above the test module:

```rust
/// Result of one integrity check. `Fail` carries a human-readable discrepancy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    Pass,
    Fail(String),
}

/// A named check outcome, for aggregate reporting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub name: &'static str,
    pub outcome: CheckOutcome,
}

// --- Check #1: inventory valuation == Inventory GL balance (spec §7.1) ---
const INVENTORY_GL_SQL: &str =
    "SELECT balance_minor FROM accounts WHERE system_role = 'inventory'";
const LOT_VALUATION_SQL: &str =
    "SELECT COALESCE(SUM(qty_remaining * unit_cost_minor), 0) FROM inventory_lots";

/// #1 — aggregate open-lot value must equal the Inventory GL account balance,
/// resolved by `system_role = 'inventory'` (never by name/id).
pub fn check_inventory_valuation(conn: &Connection) -> rusqlite::Result<CheckOutcome> {
    let gl: i64 = conn.query_row(INVENTORY_GL_SQL, [], |r| r.get(0))?;
    let lots: i64 = conn.query_row(LOT_VALUATION_SQL, [], |r| r.get(0))?;
    Ok(if gl == lots {
        CheckOutcome::Pass
    } else {
        CheckOutcome::Fail(format!(
            "inventory valuation drift: lot value {lots} != Inventory GL {gl}"
        ))
    })
}
```

- [ ] **Step 5: Wire up the modules**

In `crates/accounting-core/src/lib.rs`, add:

```rust
pub mod reconciliation;
#[cfg(test)]
mod test_support;
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p accounting-core reconciliation::tests::inventory_valuation`
Expected: PASS (both tests — correct state passes, corrupted lot fails with `70500 != 70000`).

- [ ] **Step 7: Commit**

```bash
git add crates/accounting-core/src/reconciliation.rs crates/accounting-core/src/test_support.rs crates/accounting-core/src/lib.rs
git commit -m "feat: add reconciliation check #1 (inventory valuation) + test fixture"
```

---

### Task 2: Check #2 — gross profit (returns-aware)

**Files:**
- Modify: `crates/accounting-core/src/reconciliation.rs`

See "Design notes" #1: the engine side nets against sale returns AND excludes reversed sales (`sales.reversed = 1`), because a reversed sale nets its journal to zero but leaves `sale_lines` in place for audit (spec §7.2, §4.5 reversal clause 4).

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/accounting-core/src/reconciliation.rs`:

```rust
    #[test]
    fn gross_profit_matches_journal_on_correct_state() {
        let (conn, _hlc) = open_seeded();
        assert_eq!(check_gross_profit(&conn).unwrap(), CheckOutcome::Pass);
    }

    #[test]
    fn gross_profit_fails_when_a_sale_line_cogs_is_corrupted() {
        let (conn, _hlc) = open_seeded();
        // Understate COGS on the widget sale line: engine gross rises, journal unchanged.
        conn.execute("UPDATE sale_lines SET cogs_minor = cogs_minor - 1000 WHERE sale_id = 'sale_1'", []).unwrap();
        match check_gross_profit(&conn).unwrap() {
            CheckOutcome::Fail(msg) => assert!(msg.contains("engine") && msg.contains("journal"), "got {msg}"),
            CheckOutcome::Pass => panic!("check must FAIL on corrupted sale line"),
        }
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p accounting-core reconciliation::tests::gross_profit`
Expected: FAIL — `check_gross_profit` not found.

- [ ] **Step 3: Implement check #2**

Add to `crates/accounting-core/src/reconciliation.rs`, below check #1:

```rust
// --- Check #2: gross profit engine (non-reversed, net of returns) == journal Sales−COGS (spec §7.2) ---
// Profit engine over NON-REVERSED sales only (join sales, reversed = 0), net of
// sale-return contra amounts (spec §6.8: returns move the journal but not
// sale_lines; spec §4.5 clause 4: a reversal zeroes the journal but keeps
// sale_lines for audit).
const ENGINE_GROSS_SQL: &str = "\
SELECT
  (SELECT COALESCE(SUM(sl.revenue_minor - sl.cogs_minor), 0)
     FROM sale_lines sl JOIN sales s ON s.id = sl.sale_id
     WHERE s.reversed = 0)
  -
  (SELECT COALESCE(SUM(revenue_reversed_minor - cost_restored_minor), 0)
     FROM returns WHERE return_type = 'sale_return')";
// Journal side: Sales (income, credit−debit) minus COGS (expense, debit−credit),
// both resolved by system_role.
const JOURNAL_GROSS_SQL: &str = "\
SELECT
  (SELECT COALESCE(SUM(credit_minor - debit_minor), 0) FROM journal_lines jl
     JOIN accounts a ON a.id = jl.account_id WHERE a.system_role = 'sales')
  -
  (SELECT COALESCE(SUM(debit_minor - credit_minor), 0) FROM journal_lines jl
     JOIN accounts a ON a.id = jl.account_id WHERE a.system_role = 'cogs')";

/// #2 — realized gross profit from the profit engine (non-reversed sales, net of
/// sale returns) must equal (Sales − COGS) from the journal.
pub fn check_gross_profit(conn: &Connection) -> rusqlite::Result<CheckOutcome> {
    let engine: i64 = conn.query_row(ENGINE_GROSS_SQL, [], |r| r.get(0))?;
    let journal: i64 = conn.query_row(JOURNAL_GROSS_SQL, [], |r| r.get(0))?;
    Ok(if engine == journal {
        CheckOutcome::Pass
    } else {
        CheckOutcome::Fail(format!(
            "gross profit drift: engine (net of returns) {engine} != journal Sales-COGS {journal}"
        ))
    })
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p accounting-core reconciliation::tests::gross_profit`
Expected: PASS (both).

- [ ] **Step 5: Commit**

```bash
git add crates/accounting-core/src/reconciliation.rs
git commit -m "feat: add reconciliation check #2 (returns-aware gross profit)"
```

---

### Task 3: Check #3 — double-entry (per txn debit == credit)

**Files:**
- Modify: `crates/accounting-core/src/reconciliation.rs`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module:

```rust
    #[test]
    fn double_entry_balances_on_correct_state() {
        let (conn, _hlc) = open_seeded();
        assert_eq!(check_double_entry(&conn).unwrap(), CheckOutcome::Pass);
    }

    #[test]
    fn double_entry_fails_when_a_line_is_unbalanced() {
        let (conn, _hlc) = open_seeded();
        // Corrupt one journal line so its txn no longer balances.
        conn.execute(
            "UPDATE journal_lines SET debit_minor = debit_minor + 1
             WHERE rowid = (SELECT rowid FROM journal_lines LIMIT 1)", []).unwrap();
        match check_double_entry(&conn).unwrap() {
            CheckOutcome::Fail(msg) => assert!(msg.contains("txn"), "got {msg}"),
            CheckOutcome::Pass => panic!("check must FAIL on unbalanced txn"),
        }
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p accounting-core reconciliation::tests::double_entry`
Expected: FAIL — `check_double_entry` not found.

- [ ] **Step 3: Implement check #3**

Add to `crates/accounting-core/src/reconciliation.rs`:

```rust
// --- Check #3: for every txn_id, SUM(debit) == SUM(credit) (spec §7.3) ---
const UNBALANCED_TXN_SQL: &str = "\
SELECT txn_id, SUM(debit_minor) AS d, SUM(credit_minor) AS c
FROM journal_lines
GROUP BY txn_id
HAVING SUM(debit_minor) <> SUM(credit_minor)
LIMIT 1";

/// #3 — every transaction's debits must equal its credits.
pub fn check_double_entry(conn: &Connection) -> rusqlite::Result<CheckOutcome> {
    let mut stmt = conn.prepare(UNBALANCED_TXN_SQL)?;
    let mut rows = stmt.query([])?;
    if let Some(row) = rows.next()? {
        let txn: String = row.get(0)?;
        let d: i64 = row.get(1)?;
        let c: i64 = row.get(2)?;
        return Ok(CheckOutcome::Fail(format!(
            "unbalanced txn {txn}: debit {d} != credit {c}"
        )));
    }
    Ok(CheckOutcome::Pass)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p accounting-core reconciliation::tests::double_entry`
Expected: PASS (both).

- [ ] **Step 5: Commit**

```bash
git add crates/accounting-core/src/reconciliation.rs
git commit -m "feat: add reconciliation check #3 (per-txn double-entry)"
```

---

### Task 4: Check #4 — party balances vs A/R & A/P GL

**Files:**
- Modify: `crates/accounting-core/src/reconciliation.rs`

See "Design notes" #2: the identity is the **net form** — `Σreceivable − Σunallocated_cr == A/R GL` (spec §7.4) — because Plan 3 posts the full payment to A/R, so a prepayment drives the GL negative while leaving the invoice receivable at 0.

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module. The second test injects a prepayment-shaped drift (receivable down, unallocated_cr up by the same amount) which the naive `Σreceivable == A/R GL` form would MISS but the net form catches:

```rust
    #[test]
    fn party_balances_match_gl_on_correct_state() {
        let (conn, _hlc) = open_seeded();
        assert_eq!(check_party_balances(&conn).unwrap(), CheckOutcome::Pass);
    }

    #[test]
    fn party_balances_fail_when_receivable_drifts() {
        let (conn, _hlc) = open_seeded();
        conn.execute("UPDATE party_balances SET receivable_minor = receivable_minor + 100 WHERE party_id = 'cust_acme'", []).unwrap();
        match check_party_balances(&conn).unwrap() {
            CheckOutcome::Fail(msg) => assert!(msg.contains("receivable"), "got {msg}"),
            CheckOutcome::Pass => panic!("check must FAIL on receivable drift"),
        }
    }

    #[test]
    fn party_balances_net_form_catches_credit_only_drift() {
        let (conn, _hlc) = open_seeded();
        // Move 200 from receivable into unallocated_cr without touching the A/R GL.
        // Net form (receivable-200) - (unalloc_cr+200) = A/R GL - 400 -> FAIL.
        // A naive Σreceivable-only check would ALSO catch this one; the point of the
        // net form is the extended-fixture prepayment case in Task 18, where a real
        // prepayment keeps the net identity true. Here we assert the net arithmetic.
        conn.execute("UPDATE party_balances SET receivable_minor = receivable_minor - 200, unallocated_cr_minor = unallocated_cr_minor + 200 WHERE party_id = 'cust_acme'", []).unwrap();
        assert!(matches!(check_party_balances(&conn).unwrap(), CheckOutcome::Fail(_)));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p accounting-core reconciliation::tests::party_balances`
Expected: FAIL — `check_party_balances` not found.

- [ ] **Step 3: Implement check #4 (net form)**

Add to `crates/accounting-core/src/reconciliation.rs`:

```rust
// --- Check #4: net-form party balances == A/R and A/P GL (spec §7.4) ---
// A payment posts its FULL amount to A/R/A/P, so the identity nets the held
// unallocated credits back out: Σreceivable − Σunallocated_cr == A/R GL, and
// symmetrically for payables. journal_lines has no party_id, so this reconciles
// the aggregate.
const PARTY_SUMS_SQL: &str = "\
SELECT COALESCE(SUM(receivable_minor), 0), COALESCE(SUM(unallocated_cr_minor), 0),
       COALESCE(SUM(payable_minor), 0),    COALESCE(SUM(unallocated_dr_minor), 0)
FROM party_balances";
const AR_GL_SQL: &str = "SELECT balance_minor FROM accounts WHERE system_role = 'accounts_receivable'";
const AP_GL_SQL: &str = "SELECT balance_minor FROM accounts WHERE system_role = 'accounts_payable'";

/// #4 — (Σreceivable − Σunallocated_cr) must equal the A/R GL balance, and
/// (Σpayable − Σunallocated_dr) the A/P GL balance. The net form is required
/// because a payment books its full amount to the control account; the naive
/// Σreceivable == A/R GL form false-alarms on any prepayment.
pub fn check_party_balances(conn: &Connection) -> rusqlite::Result<CheckOutcome> {
    let (sum_recv, sum_cr, sum_pay, sum_dr): (i64, i64, i64, i64) =
        conn.query_row(PARTY_SUMS_SQL, [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
    let ar_gl: i64 = conn.query_row(AR_GL_SQL, [], |r| r.get(0))?;
    let ap_gl: i64 = conn.query_row(AP_GL_SQL, [], |r| r.get(0))?;
    let net_recv = sum_recv - sum_cr;
    let net_pay = sum_pay - sum_dr;
    if net_recv != ar_gl {
        return Ok(CheckOutcome::Fail(format!(
            "receivable drift: net Σreceivable-Σunallocated_cr {net_recv} != A/R GL {ar_gl}"
        )));
    }
    if net_pay != ap_gl {
        return Ok(CheckOutcome::Fail(format!(
            "payable drift: net Σpayable-Σunallocated_dr {net_pay} != A/P GL {ap_gl}"
        )));
    }
    Ok(CheckOutcome::Pass)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p accounting-core reconciliation::tests::party_balances`
Expected: PASS (all three).

- [ ] **Step 5: Commit**

```bash
git add crates/accounting-core/src/reconciliation.rs
git commit -m "feat: add reconciliation check #4 (party balances vs A/R & A/P GL)"
```

---

### Task 5: Check #5 — invoice outstanding identity (sales & purchases)

**Files:**
- Modify: `crates/accounting-core/src/reconciliation.rs`

Identity (**terms-aware**, spec §7.5): for a **credit** invoice `outstanding_minor == MAX(0, total − allocated − returned)`; for a **cash** invoice `outstanding_minor == 0` exactly. `outstanding_minor >= 0` for **all** rows. "returned" is `revenue_reversed_minor` for sales, `cost_restored_minor` for purchases. This must NOT be a `WHERE terms='credit'` filter — the negative guard applies to cash rows too, so a stray non-zero cash outstanding is still caught. (Cash `sale_2` in the fixture is `total=7500, outstanding=0`; the credit-only identity would falsely expect 7500 — the exact bug the terms-branch avoids.)

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module:

```rust
    #[test]
    fn invoice_outstanding_matches_on_correct_state() {
        // Covers both credit invoices AND the cash sale_2 (total 7500, outstanding 0).
        let (conn, _hlc) = open_seeded();
        assert_eq!(check_invoice_outstanding(&conn).unwrap(), CheckOutcome::Pass);
    }

    #[test]
    fn invoice_outstanding_fails_when_a_sale_outstanding_drifts() {
        let (conn, _hlc) = open_seeded();
        // sale_1 (credit) should be 5000 (54000 - 40000 alloc - 9000 returned); corrupt it.
        conn.execute("UPDATE sales SET outstanding_minor = 12345 WHERE id = 'sale_1'", []).unwrap();
        match check_invoice_outstanding(&conn).unwrap() {
            CheckOutcome::Fail(msg) => assert!(msg.contains("sale sale_1") && msg.contains("5000"), "got {msg}"),
            CheckOutcome::Pass => panic!("check must FAIL on outstanding drift"),
        }
    }

    #[test]
    fn invoice_outstanding_fails_when_cash_invoice_has_nonzero_outstanding() {
        let (conn, _hlc) = open_seeded();
        // Cash sale_2 must be exactly 0; a non-zero value is drift even though the
        // credit identity is not applied to it.
        conn.execute("UPDATE sales SET outstanding_minor = 100 WHERE id = 'sale_2'", []).unwrap();
        match check_invoice_outstanding(&conn).unwrap() {
            CheckOutcome::Fail(msg) => assert!(msg.contains("sale sale_2"), "got {msg}"),
            CheckOutcome::Pass => panic!("check must FAIL on non-zero cash outstanding"),
        }
    }

    #[test]
    fn invoice_outstanding_fails_when_outstanding_is_negative() {
        let (conn, _hlc) = open_seeded();
        conn.execute("UPDATE purchases SET outstanding_minor = -1 WHERE id = 'pur_1'", []).unwrap();
        assert!(matches!(check_invoice_outstanding(&conn).unwrap(), CheckOutcome::Fail(_)));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p accounting-core reconciliation::tests::invoice_outstanding`
Expected: FAIL — `check_invoice_outstanding` not found.

- [ ] **Step 3: Implement check #5 (terms-aware)**

Add to `crates/accounting-core/src/reconciliation.rs`. `terms` is selected so the expected value branches; the `outstanding < 0` guard runs for every row regardless of terms:

```rust
// --- Check #5: terms-aware outstanding, and >= 0 for all rows (spec §7.5) ---
// Credit: outstanding == MAX(0, total − allocated − returned). Cash: == 0.
const SALES_OUTSTANDING_SQL: &str = "\
SELECT s.id, s.terms, s.total_minor, s.outstanding_minor,
  COALESCE((SELECT SUM(amount_minor) FROM payment_allocations
              WHERE target_id = s.id AND target_type = 'sale'), 0) AS allocated,
  COALESCE((SELECT SUM(revenue_reversed_minor) FROM returns
              WHERE original_id = s.id AND return_type = 'sale_return'), 0) AS returned
FROM sales s";
const PURCHASES_OUTSTANDING_SQL: &str = "\
SELECT p.id, p.terms, p.total_minor, p.outstanding_minor,
  COALESCE((SELECT SUM(amount_minor) FROM payment_allocations
              WHERE target_id = p.id AND target_type = 'purchase'), 0) AS allocated,
  COALESCE((SELECT SUM(cost_restored_minor) FROM returns
              WHERE original_id = p.id AND return_type = 'purchase_return'), 0) AS returned
FROM purchases p";

fn verify_outstanding(conn: &Connection, sql: &str, kind: &str) -> rusqlite::Result<Option<String>> {
    let mut stmt = conn.prepare(sql)?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let terms: String = row.get(1)?;
        let total: i64 = row.get(2)?;
        let outstanding: i64 = row.get(3)?;
        let allocated: i64 = row.get(4)?;
        let returned: i64 = row.get(5)?;
        // Negative guard applies to ALL rows (cash and credit).
        if outstanding < 0 {
            return Ok(Some(format!("{kind} {id}: outstanding {outstanding} is negative")));
        }
        // Cash invoices are stored total=amount, outstanding=0 by fiat; credit
        // invoices carry the re-derivable balance.
        let expected = if terms == "credit" {
            (total - allocated - returned).max(0)
        } else {
            0
        };
        if outstanding != expected {
            return Ok(Some(format!(
                "{kind} {id} ({terms}): outstanding {outstanding} != expected {expected} (total {total} - allocated {allocated} - returned {returned})"
            )));
        }
    }
    Ok(None)
}

/// #5 — every sale and purchase's stored `outstanding_minor` must equal the
/// terms-aware identity (credit: MAX(0, total−allocated−returned); cash: 0) and
/// never be negative.
pub fn check_invoice_outstanding(conn: &Connection) -> rusqlite::Result<CheckOutcome> {
    if let Some(msg) = verify_outstanding(conn, SALES_OUTSTANDING_SQL, "sale")? {
        return Ok(CheckOutcome::Fail(msg));
    }
    if let Some(msg) = verify_outstanding(conn, PURCHASES_OUTSTANDING_SQL, "purchase")? {
        return Ok(CheckOutcome::Fail(msg));
    }
    Ok(CheckOutcome::Pass)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p accounting-core reconciliation::tests::invoice_outstanding`
Expected: PASS (all four).

- [ ] **Step 5: Commit**

```bash
git add crates/accounting-core/src/reconciliation.rs
git commit -m "feat: add reconciliation check #5 (invoice outstanding identity)"
```

---

### Task 6: Check #6 — non-negative inventory

**Files:**
- Modify: `crates/accounting-core/src/reconciliation.rs`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module:

```rust
    #[test]
    fn non_negative_inventory_holds_on_correct_state() {
        let (conn, _hlc) = open_seeded();
        assert_eq!(check_non_negative_inventory(&conn).unwrap(), CheckOutcome::Pass);
    }

    #[test]
    fn non_negative_inventory_fails_on_negative_lot() {
        let (conn, _hlc) = open_seeded();
        conn.execute("UPDATE inventory_lots SET qty_remaining = -1 WHERE id = 'pur_2#lot0'", []).unwrap();
        match check_non_negative_inventory(&conn).unwrap() {
            CheckOutcome::Fail(msg) => assert!(msg.contains("pur_2#lot0"), "got {msg}"),
            CheckOutcome::Pass => panic!("check must FAIL on negative qty_remaining"),
        }
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p accounting-core reconciliation::tests::non_negative_inventory`
Expected: FAIL — `check_non_negative_inventory` not found.

- [ ] **Step 3: Implement check #6**

Add to `crates/accounting-core/src/reconciliation.rs`:

```rust
// --- Check #6: qty_remaining >= 0 for all lots (spec §7.6) ---
const NEGATIVE_LOT_SQL: &str =
    "SELECT id, qty_remaining FROM inventory_lots WHERE qty_remaining < 0 LIMIT 1";

/// #6 — no lot may have negative remaining quantity (backstops the oversell guard).
pub fn check_non_negative_inventory(conn: &Connection) -> rusqlite::Result<CheckOutcome> {
    let mut stmt = conn.prepare(NEGATIVE_LOT_SQL)?;
    let mut rows = stmt.query([])?;
    if let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let qty: i64 = row.get(1)?;
        return Ok(CheckOutcome::Fail(format!("lot {id} has negative qty_remaining {qty}")));
    }
    Ok(CheckOutcome::Pass)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p accounting-core reconciliation::tests::non_negative_inventory`
Expected: PASS (both).

- [ ] **Step 5: Commit**

```bash
git add crates/accounting-core/src/reconciliation.rs
git commit -m "feat: add reconciliation check #6 (non-negative inventory)"
```

---

### Task 7: Check #7 — lot bounds (0 <= remaining <= received)

**Files:**
- Modify: `crates/accounting-core/src/reconciliation.rs`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module:

```rust
    #[test]
    fn lot_bounds_hold_on_correct_state() {
        let (conn, _hlc) = open_seeded();
        assert_eq!(check_lot_bounds(&conn).unwrap(), CheckOutcome::Pass);
    }

    #[test]
    fn lot_bounds_fail_when_remaining_exceeds_received() {
        let (conn, _hlc) = open_seeded();
        // pur_2#lot0: received 50; push remaining above received.
        conn.execute("UPDATE inventory_lots SET qty_remaining = qty_received + 1 WHERE id = 'pur_2#lot0'", []).unwrap();
        match check_lot_bounds(&conn).unwrap() {
            CheckOutcome::Fail(msg) => assert!(msg.contains("pur_2#lot0"), "got {msg}"),
            CheckOutcome::Pass => panic!("check must FAIL when remaining > received"),
        }
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p accounting-core reconciliation::tests::lot_bounds`
Expected: FAIL — `check_lot_bounds` not found.

- [ ] **Step 3: Implement check #7**

Add to `crates/accounting-core/src/reconciliation.rs`:

```rust
// --- Check #7: 0 <= qty_remaining <= qty_received for all lots (spec §7.7) ---
const LOT_BOUNDS_SQL: &str = "\
SELECT id, qty_remaining, qty_received FROM inventory_lots
WHERE qty_remaining < 0 OR qty_remaining > qty_received LIMIT 1";

/// #7 — every lot's remaining quantity must be within [0, qty_received]
/// (backstops the sale-return over-restore guard).
pub fn check_lot_bounds(conn: &Connection) -> rusqlite::Result<CheckOutcome> {
    let mut stmt = conn.prepare(LOT_BOUNDS_SQL)?;
    let mut rows = stmt.query([])?;
    if let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let rem: i64 = row.get(1)?;
        let recv: i64 = row.get(2)?;
        return Ok(CheckOutcome::Fail(format!(
            "lot {id} out of bounds: qty_remaining {rem} not in [0, {recv}]"
        )));
    }
    Ok(CheckOutcome::Pass)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p accounting-core reconciliation::tests::lot_bounds`
Expected: PASS (both).

- [ ] **Step 5: Commit**

```bash
git add crates/accounting-core/src/reconciliation.rs
git commit -m "feat: add reconciliation check #7 (lot bounds)"
```

---

### Task 8: Check #8 — non-negative unallocated credits

**Files:**
- Modify: `crates/accounting-core/src/reconciliation.rs`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module:

```rust
    #[test]
    fn non_negative_credits_hold_on_correct_state() {
        let (conn, _hlc) = open_seeded();
        assert_eq!(check_non_negative_credits(&conn).unwrap(), CheckOutcome::Pass);
    }

    #[test]
    fn non_negative_credits_fail_on_negative_credit() {
        let (conn, _hlc) = open_seeded();
        conn.execute("UPDATE party_balances SET unallocated_cr_minor = -5 WHERE party_id = 'cust_acme'", []).unwrap();
        match check_non_negative_credits(&conn).unwrap() {
            CheckOutcome::Fail(msg) => assert!(msg.contains("cust_acme"), "got {msg}"),
            CheckOutcome::Pass => panic!("check must FAIL on negative unallocated credit"),
        }
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p accounting-core reconciliation::tests::non_negative_credits`
Expected: FAIL — `check_non_negative_credits` not found.

- [ ] **Step 3: Implement check #8**

Add to `crates/accounting-core/src/reconciliation.rs`:

```rust
// --- Check #8: unallocated_cr_minor >= 0 and unallocated_dr_minor >= 0 (spec §7.8) ---
const NEGATIVE_CREDIT_SQL: &str = "\
SELECT party_id, unallocated_cr_minor, unallocated_dr_minor FROM party_balances
WHERE unallocated_cr_minor < 0 OR unallocated_dr_minor < 0 LIMIT 1";

/// #8 — held credits (customer prepayments / supplier deposits) must never be
/// negative (backstops the credit-overdraw guard).
pub fn check_non_negative_credits(conn: &Connection) -> rusqlite::Result<CheckOutcome> {
    let mut stmt = conn.prepare(NEGATIVE_CREDIT_SQL)?;
    let mut rows = stmt.query([])?;
    if let Some(row) = rows.next()? {
        let party: String = row.get(0)?;
        let cr: i64 = row.get(1)?;
        let dr: i64 = row.get(2)?;
        return Ok(CheckOutcome::Fail(format!(
            "party {party} has negative unallocated credit: cr {cr}, dr {dr}"
        )));
    }
    Ok(CheckOutcome::Pass)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p accounting-core reconciliation::tests::non_negative_credits`
Expected: PASS (both).

- [ ] **Step 5: Commit**

```bash
git add crates/accounting-core/src/reconciliation.rs
git commit -m "feat: add reconciliation check #8 (non-negative unallocated credits)"
```

---

### Task 9: `run_all_checks` aggregator

**Files:**
- Modify: `crates/accounting-core/src/reconciliation.rs`
- Modify: `crates/accounting-core/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module:

```rust
    #[test]
    fn run_all_checks_all_pass_on_reference_business() {
        let (conn, _hlc) = open_seeded();
        let checks = run_all_checks(&conn).unwrap();
        assert_eq!(checks.len(), 8, "all eight §7 checks must run");
        for c in &checks {
            assert_eq!(c.outcome, CheckOutcome::Pass, "{} should pass: {:?}", c.name, c.outcome);
        }
        assert!(all_passed(&checks));
    }

    #[test]
    fn run_all_checks_flags_the_failing_one() {
        let (conn, _hlc) = open_seeded();
        conn.execute("UPDATE inventory_lots SET qty_remaining = -1 WHERE id = 'pur_3#lot0'", []).unwrap();
        let checks = run_all_checks(&conn).unwrap();
        assert!(!all_passed(&checks));
        // #6 (non-negative inventory) and #7 (lot bounds) both catch a negative lot.
        let failed: Vec<&str> = checks.iter()
            .filter(|c| c.outcome != CheckOutcome::Pass)
            .map(|c| c.name).collect();
        assert!(failed.contains(&"non_negative_inventory"), "got {failed:?}");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p accounting-core reconciliation::tests::run_all_checks`
Expected: FAIL — `run_all_checks` / `all_passed` not found.

- [ ] **Step 3: Implement the aggregator**

Add to `crates/accounting-core/src/reconciliation.rs`:

```rust
/// Run every §7 integrity check and return the named outcomes in spec order.
/// Read-only; safe to run periodically as a drift detector.
pub fn run_all_checks(conn: &Connection) -> rusqlite::Result<Vec<Check>> {
    Ok(vec![
        Check { name: "inventory_valuation",   outcome: check_inventory_valuation(conn)? },
        Check { name: "gross_profit",          outcome: check_gross_profit(conn)? },
        Check { name: "double_entry",          outcome: check_double_entry(conn)? },
        Check { name: "party_balances",        outcome: check_party_balances(conn)? },
        Check { name: "invoice_outstanding",   outcome: check_invoice_outstanding(conn)? },
        Check { name: "non_negative_inventory", outcome: check_non_negative_inventory(conn)? },
        Check { name: "lot_bounds",            outcome: check_lot_bounds(conn)? },
        Check { name: "non_negative_credits",  outcome: check_non_negative_credits(conn)? },
    ])
}

/// True iff every check passed.
pub fn all_passed(checks: &[Check]) -> bool {
    checks.iter().all(|c| c.outcome == CheckOutcome::Pass)
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p accounting-core reconciliation`
Expected: PASS (all reconciliation tests: 2 per check ×8 = 16, plus the 2 aggregator tests).

- [ ] **Step 5: Re-export the reconciliation API**

In `crates/accounting-core/src/lib.rs`, add:

```rust
pub use reconciliation::{all_passed, run_all_checks, Check, CheckOutcome};
```

- [ ] **Step 6: Commit**

```bash
git add crates/accounting-core/src/reconciliation.rs crates/accounting-core/src/lib.rs
git commit -m "feat: add run_all_checks aggregator over the eight §7 integrity checks"
```

---

### Task 10: Queries scaffold + §8.1 units of item X sold this / last month

**Files:**
- Create: `crates/accounting-core/src/queries.rs`
- Modify: `crates/accounting-core/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/accounting-core/src/queries.rs`:

```rust
use rusqlite::Connection;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{open_seeded, ANCHOR, WIDGET};

    #[test]
    fn units_sold_by_month_for_widget() {
        let (conn, _hlc) = open_seeded();
        let rows = units_sold_by_month(&conn, WIDGET, ANCHOR).unwrap();
        // Only sale_1 (2026-07-02, qty 60, revenue 54000) falls in [June, July].
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].month, "2026-07");
        assert_eq!(rows[0].units_sold, 60);
        assert_eq!(rows[0].revenue_minor, 54000);
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p accounting-core queries::tests::units_sold_by_month`
Expected: FAIL — `units_sold_by_month` / `MonthlyUnits` not found.

- [ ] **Step 3: Implement §8.1**

At the top of `crates/accounting-core/src/queries.rs`, above the test module. The SQL is spec §8.1 with two changes: `'now'` → bound `?2` (see "Design notes" #3), and a `sales` join with `WHERE reversed = 0` to exclude voided sales (spec §8.4, "Design notes" #5):

```rust
/// One month's sales of a single item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonthlyUnits {
    pub month: String,        // 'YYYY-MM'
    pub units_sold: i64,
    pub revenue_minor: i64,
}

// spec §8.1 — anchor 'now' parameterized as ?2; joins sales to exclude reversed.
const UNITS_SOLD_BY_MONTH_SQL: &str = "\
SELECT strftime('%Y-%m', sl.date) AS month,
       SUM(sl.qty) AS units_sold, SUM(sl.revenue_minor) AS revenue_minor
FROM sale_lines sl JOIN sales s ON s.id = sl.sale_id
WHERE sl.item_id = ?1 AND s.reversed = 0
  AND sl.date >= date(?2, 'start of month', '-1 month')
GROUP BY month ORDER BY month";

/// §8.1 — units and revenue of item `item_id` for this month and last month,
/// relative to `anchor` (pass `"now"` in production, a fixed date in tests).
pub fn units_sold_by_month(
    conn: &Connection,
    item_id: &str,
    anchor: &str,
) -> rusqlite::Result<Vec<MonthlyUnits>> {
    let mut stmt = conn.prepare(UNITS_SOLD_BY_MONTH_SQL)?;
    let rows = stmt.query_map(rusqlite::params![item_id, anchor], |r| {
        Ok(MonthlyUnits { month: r.get(0)?, units_sold: r.get(1)?, revenue_minor: r.get(2)? })
    })?;
    rows.collect()
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p accounting-core queries::tests::units_sold_by_month`
Expected: PASS.

- [ ] **Step 5: Wire up and re-export**

In `crates/accounting-core/src/lib.rs`, add:

```rust
pub mod queries;
```

- [ ] **Step 6: Commit**

```bash
git add crates/accounting-core/src/queries.rs crates/accounting-core/src/lib.rs
git commit -m "feat: add §8.1 units-sold-by-month report query"
```

---

### Task 11: §8.2 realized profit — gross AND net (null-safe `IS NOT`)

**Files:**
- Modify: `crates/accounting-core/src/queries.rs`

The net query uses the null-safe `IS NOT 'cogs'` filter (spec §8.2): user-created expense accounts have `system_role = NULL`, and `NULL <> 'cogs'` is NULL (would silently drop those rows). `NULL IS NOT 'cogs'` correctly returns true.

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/accounting-core/src/queries.rs`:

```rust
    #[test]
    fn gross_and_net_profit_over_period() {
        use crate::test_support::ANCHOR;
        let (conn, _hlc) = open_seeded();

        // Gross (non-reversed sale_lines): rev 61500, cogs 35000, gross 26500.
        let g = gross_profit(&conn, ANCHOR).unwrap();
        assert_eq!(g.revenue_minor, 61500);
        assert_eq!(g.cogs_minor, 35000);
        assert_eq!(g.gross_profit_minor, 26500);

        // Net = gross 26500 - non-COGS operating expenses (rent 3000) = 23500.
        // COGS is excluded via IS NOT 'cogs'; user-defined (NULL system_role)
        // expenses would still be included (none here).
        let net = net_profit(&conn, ANCHOR).unwrap();
        assert_eq!(net, 23500);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p accounting-core queries::tests::gross_and_net_profit`
Expected: FAIL — `gross_profit` / `net_profit` / `GrossProfit` not found.

- [ ] **Step 3: Implement §8.2**

Add to `crates/accounting-core/src/queries.rs`:

```rust
/// Gross realized profit over a period (spec §8.2), from the profit engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrossProfit {
    pub revenue_minor: i64,
    pub cogs_minor: i64,
    pub gross_profit_minor: i64,
}

// spec §8.2 gross — 'now' parameterized as ?1; joins sales to exclude reversed.
const GROSS_PROFIT_SQL: &str = "\
SELECT COALESCE(SUM(sl.revenue_minor), 0) AS revenue,
       COALESCE(SUM(sl.cogs_minor), 0) AS cogs,
       COALESCE(SUM(sl.revenue_minor - sl.cogs_minor), 0) AS gross_profit_minor
FROM sale_lines sl JOIN sales s ON s.id = sl.sale_id
WHERE s.reversed = 0 AND sl.date >= date(?1, '-6 months')";

// spec §8.2 net — 'now' parameterized as ?1. The sale_lines subquery joins sales
// and excludes reversed; the journal subquery needs no such filter (a reversal
// already zeroed those lines). Note the null-safe `IS NOT 'cogs'` so user-defined
// expense accounts (system_role NULL) are NOT silently dropped.
const NET_PROFIT_SQL: &str = "\
SELECT
  (SELECT COALESCE(SUM(sl.revenue_minor - sl.cogs_minor), 0)
     FROM sale_lines sl JOIN sales s ON s.id = sl.sale_id
     WHERE s.reversed = 0 AND sl.date >= date(?1, '-6 months'))
  -
  (SELECT COALESCE(SUM(debit_minor - credit_minor), 0) FROM journal_lines jl
     JOIN accounts a ON a.id = jl.account_id
     WHERE a.type = 'expense' AND a.system_role IS NOT 'cogs'
       AND jl.date >= date(?1, '-6 months'))
  AS net_profit_minor";

/// §8.2 gross — revenue, COGS, and gross profit over the last 6 months from `anchor`.
pub fn gross_profit(conn: &Connection, anchor: &str) -> rusqlite::Result<GrossProfit> {
    conn.query_row(GROSS_PROFIT_SQL, rusqlite::params![anchor], |r| {
        Ok(GrossProfit {
            revenue_minor: r.get(0)?,
            cogs_minor: r.get(1)?,
            gross_profit_minor: r.get(2)?,
        })
    })
}

/// §8.2 net — gross profit minus non-COGS operating expenses over the same period.
pub fn net_profit(conn: &Connection, anchor: &str) -> rusqlite::Result<i64> {
    conn.query_row(NET_PROFIT_SQL, rusqlite::params![anchor], |r| r.get(0))
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p accounting-core queries::tests::gross_and_net_profit`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/accounting-core/src/queries.rs
git commit -m "feat: add §8.2 gross & net profit queries (null-safe IS NOT for COGS)"
```

---

### Task 12: §8.3 inventory age per lot + aging buckets

**Files:**
- Modify: `crates/accounting-core/src/queries.rs`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module:

```rust
    #[test]
    fn inventory_age_per_lot_and_buckets() {
        use crate::test_support::{ANCHOR, WIDGET};
        let (conn, _hlc) = open_seeded();

        // Per-lot age for widget, ordered by acquired_at: pur_1#lot0 then pur_2#lot0.
        let lots = lot_ages(&conn, WIDGET, ANCHOR).unwrap();
        assert_eq!(lots.len(), 2);
        assert_eq!(lots[0].lot_id, "pur_1#lot0");
        assert_eq!(lots[0].qty_remaining, 50);
        assert_eq!(lots[0].age_days, 35);              // 2026-06-01 -> 2026-07-06
        assert_eq!(lots[0].value_on_hand_minor, 25000);
        assert_eq!(lots[1].lot_id, "pur_2#lot0");
        assert_eq!(lots[1].age_days, 21);              // 2026-06-15 -> 2026-07-06

        // Aging buckets across ALL items.
        let buckets = aging_buckets(&conn, ANCHOR).unwrap();
        let find = |b: &str| buckets.iter().find(|x| x.bucket == b).cloned();
        // 0-30d: pur_2#lot0 (50 units, 30000) + pur_3#lot0 (15 units, 15000) = 65 units, 45000.
        let b0 = find("0-30d").expect("0-30d bucket");
        assert_eq!(b0.units, 65);
        assert_eq!(b0.value_minor, 45000);
        // 31-90d: pur_1#lot0 (50 units, 25000).
        let b1 = find("31-90d").expect("31-90d bucket");
        assert_eq!(b1.units, 50);
        assert_eq!(b1.value_minor, 25000);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p accounting-core queries::tests::inventory_age`
Expected: FAIL — `lot_ages` / `aging_buckets` not found.

- [ ] **Step 3: Implement §8.3**

Add to `crates/accounting-core/src/queries.rs`. SQL is spec §8.3 with `julianday('now')` → `julianday(?N)`, and the aging `ORDER BY bucket` fixed to sort by age band, not lexically (`'180d+'` sorts before `'31-90d'` as text — see MINOR fix):

```rust
/// Age and on-hand value of one open lot (spec §8.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LotAge {
    pub lot_id: String,
    pub qty_remaining: i64,
    pub unit_cost_minor: i64,
    pub age_days: i64,
    pub value_on_hand_minor: i64,
}

/// One aging bucket across inventory (spec §8.3 dead-stock detector).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgingBucket {
    pub bucket: String,
    pub units: i64,
    pub value_minor: i64,
}

const LOT_AGES_SQL: &str = "\
SELECT id AS lot_id, qty_remaining, unit_cost_minor,
       CAST(julianday(?2) - julianday(acquired_at) AS INT) AS age_days,
       qty_remaining * unit_cost_minor AS value_on_hand_minor
FROM inventory_lots
WHERE item_id = ?1 AND qty_remaining > 0
ORDER BY acquired_at";

// A parallel `sort_key` (0..3) orders the buckets by age band; ordering by the
// text label would sort '180d+' before '31-90d'. GROUP BY both so the label and
// its key stay paired.
const AGING_BUCKETS_SQL: &str = "\
SELECT CASE
    WHEN julianday(?1) - julianday(acquired_at) <= 30  THEN '0-30d'
    WHEN julianday(?1) - julianday(acquired_at) <= 90  THEN '31-90d'
    WHEN julianday(?1) - julianday(acquired_at) <= 180 THEN '91-180d'
    ELSE '180d+' END AS bucket,
  CASE
    WHEN julianday(?1) - julianday(acquired_at) <= 30  THEN 0
    WHEN julianday(?1) - julianday(acquired_at) <= 90  THEN 1
    WHEN julianday(?1) - julianday(acquired_at) <= 180 THEN 2
    ELSE 3 END AS sort_key,
  SUM(qty_remaining) AS units, SUM(qty_remaining * unit_cost_minor) AS value_minor
FROM inventory_lots WHERE qty_remaining > 0
GROUP BY bucket, sort_key ORDER BY sort_key";

/// §8.3 — per-open-lot age and value for an item.
pub fn lot_ages(conn: &Connection, item_id: &str, anchor: &str) -> rusqlite::Result<Vec<LotAge>> {
    let mut stmt = conn.prepare(LOT_AGES_SQL)?;
    let rows = stmt.query_map(rusqlite::params![item_id, anchor], |r| {
        Ok(LotAge {
            lot_id: r.get(0)?,
            qty_remaining: r.get(1)?,
            unit_cost_minor: r.get(2)?,
            age_days: r.get(3)?,
            value_on_hand_minor: r.get(4)?,
        })
    })?;
    rows.collect()
}

/// §8.3 — inventory aging buckets across all items (dead-stock detector).
pub fn aging_buckets(conn: &Connection, anchor: &str) -> rusqlite::Result<Vec<AgingBucket>> {
    let mut stmt = conn.prepare(AGING_BUCKETS_SQL)?;
    let rows = stmt.query_map(rusqlite::params![anchor], |r| {
        // Columns: 0 bucket, 1 sort_key (used only for ORDER BY), 2 units, 3 value.
        Ok(AgingBucket { bucket: r.get(0)?, units: r.get(2)?, value_minor: r.get(3)? })
    })?;
    rows.collect()
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p accounting-core queries::tests::inventory_age`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/accounting-core/src/queries.rs
git commit -m "feat: add §8.3 inventory age per-lot and aging-bucket queries"
```

---

### Task 13: §8.4 stock on hand + inventory valuation

**Files:**
- Modify: `crates/accounting-core/src/queries.rs`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module:

```rust
    #[test]
    fn stock_on_hand_and_valuation() {
        use crate::test_support::{GADGET, WIDGET};
        let (conn, _hlc) = open_seeded();

        let soh = stock_on_hand(&conn).unwrap();
        let qty = |id: &str| soh.iter().find(|r| r.item_id == id).map(|r| r.qty).unwrap();
        assert_eq!(qty(WIDGET), 100);  // pur_1#lot0 50 + pur_2#lot0 50
        assert_eq!(qty(GADGET), 15);   // pur_3#lot0

        // Reconciles with the Inventory GL (check #1) and the aging buckets total.
        assert_eq!(inventory_valuation(&conn).unwrap(), 70000);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p accounting-core queries::tests::stock_on_hand_and_valuation`
Expected: FAIL — `stock_on_hand` / `inventory_valuation` not found.

- [ ] **Step 3: Implement §8.4 (stock on hand + valuation)**

Add to `crates/accounting-core/src/queries.rs`:

```rust
/// On-hand quantity for one item (spec §8.4 stock on hand).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StockOnHand {
    pub item_id: String,
    pub qty: i64,
}

const STOCK_ON_HAND_SQL: &str = "\
SELECT item_id, SUM(qty_remaining) AS qty
FROM inventory_lots WHERE qty_remaining > 0
GROUP BY item_id ORDER BY item_id";

const INVENTORY_VALUATION_SQL: &str =
    "SELECT COALESCE(SUM(qty_remaining * unit_cost_minor), 0) FROM inventory_lots";

/// §8.4 — current stock on hand per item.
pub fn stock_on_hand(conn: &Connection) -> rusqlite::Result<Vec<StockOnHand>> {
    let mut stmt = conn.prepare(STOCK_ON_HAND_SQL)?;
    let rows = stmt.query_map([], |r| Ok(StockOnHand { item_id: r.get(0)?, qty: r.get(1)? }))?;
    rows.collect()
}

/// §8.4 — total inventory valuation (reconciles with Inventory GL, check #1).
pub fn inventory_valuation(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row(INVENTORY_VALUATION_SQL, [], |r| r.get(0))
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p accounting-core queries::tests::stock_on_hand_and_valuation`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/accounting-core/src/queries.rs
git commit -m "feat: add §8.4 stock-on-hand and inventory-valuation queries"
```

---

### Task 14: §8.4 gross margin % per item + best/worst sellers

**Files:**
- Modify: `crates/accounting-core/src/queries.rs`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module:

```rust
    #[test]
    fn gross_margin_and_sellers() {
        use crate::test_support::{GADGET, WIDGET};
        let (conn, _hlc) = open_seeded();

        let margins = gross_margin_per_item(&conn).unwrap();
        let m = |id: &str| margins.iter().find(|r| r.item_id == id).cloned().unwrap();
        // widget: (54000-30000)/54000 = 44.44%
        assert_eq!(m(WIDGET).margin_pct, Some(44.44));
        // gadget: (7500-5000)/7500 = 33.33%
        assert_eq!(m(GADGET).margin_pct, Some(33.33));

        // Best sellers by units sold, descending: widget (60) then gadget (5).
        let sellers = sellers_by_units(&conn).unwrap();
        assert_eq!(sellers[0].item_id, WIDGET);
        assert_eq!(sellers[0].units, 60);
        assert_eq!(sellers[0].profit_minor, 24000);
        assert_eq!(sellers[1].item_id, GADGET);
        assert_eq!(sellers[1].units, 5);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p accounting-core queries::tests::gross_margin_and_sellers`
Expected: FAIL — `gross_margin_per_item` / `sellers_by_units` not found.

- [ ] **Step 3: Implement §8.4 (margin + sellers)**

Add to `crates/accounting-core/src/queries.rs`. `margin_pct` is `NULL` when an item has zero revenue (avoids divide-by-zero), so it maps to `Option<f64>`:

```rust
/// Gross margin for one item (spec §8.4). `margin_pct` is None when revenue is 0.
#[derive(Debug, Clone, PartialEq)]
pub struct ItemMargin {
    pub item_id: String,
    pub revenue_minor: i64,
    pub cogs_minor: i64,
    pub margin_pct: Option<f64>,
}

/// One item's sales volume and realized profit (spec §8.4 best/worst sellers).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SellerRow {
    pub item_id: String,
    pub units: i64,
    pub profit_minor: i64,
}

// Both reports join sales and filter reversed = 0 (spec §8.4, "Design notes" #5).
const GROSS_MARGIN_SQL: &str = "\
SELECT sl.item_id,
  COALESCE(SUM(sl.revenue_minor), 0) AS revenue,
  COALESCE(SUM(sl.cogs_minor), 0) AS cogs,
  CASE WHEN SUM(sl.revenue_minor) = 0 THEN NULL
       ELSE ROUND(100.0 * SUM(sl.revenue_minor - sl.cogs_minor) / SUM(sl.revenue_minor), 2)
  END AS margin_pct
FROM sale_lines sl JOIN sales s ON s.id = sl.sale_id
WHERE s.reversed = 0
GROUP BY sl.item_id ORDER BY sl.item_id";

const SELLERS_SQL: &str = "\
SELECT sl.item_id, SUM(sl.qty) AS units, SUM(sl.revenue_minor - sl.cogs_minor) AS profit_minor
FROM sale_lines sl JOIN sales s ON s.id = sl.sale_id
WHERE s.reversed = 0
GROUP BY sl.item_id ORDER BY units DESC, sl.item_id";

/// §8.4 — gross margin % per item.
pub fn gross_margin_per_item(conn: &Connection) -> rusqlite::Result<Vec<ItemMargin>> {
    let mut stmt = conn.prepare(GROSS_MARGIN_SQL)?;
    let rows = stmt.query_map([], |r| {
        Ok(ItemMargin {
            item_id: r.get(0)?,
            revenue_minor: r.get(1)?,
            cogs_minor: r.get(2)?,
            margin_pct: r.get(3)?, // NULL -> None
        })
    })?;
    rows.collect()
}

/// §8.4 — items ranked by units sold (best sellers first; reverse for worst).
pub fn sellers_by_units(conn: &Connection) -> rusqlite::Result<Vec<SellerRow>> {
    let mut stmt = conn.prepare(SELLERS_SQL)?;
    let rows = stmt.query_map([], |r| {
        Ok(SellerRow { item_id: r.get(0)?, units: r.get(1)?, profit_minor: r.get(2)? })
    })?;
    rows.collect()
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p accounting-core queries::tests::gross_margin_and_sellers`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/accounting-core/src/queries.rs
git commit -m "feat: add §8.4 gross-margin-per-item and best/worst-seller queries"
```

---

### Task 15: §8.4 party balances + A/R & A/P aging

**Files:**
- Modify: `crates/accounting-core/src/queries.rs`

A/R & A/P aging bucket credit invoices with `outstanding_minor > 0` by `date` — works because the projector maintains `outstanding_minor` from `payment_allocations` (spec §8.4, §6.7).

- [ ] **Step 1: Write the failing test**

Add to the `tests` module:

```rust
    #[test]
    fn party_balances_and_aging() {
        use crate::test_support::{ANCHOR, CUST, SUPP};
        let (conn, _hlc) = open_seeded();

        let balances = party_balances(&conn).unwrap();
        let b = |id: &str| balances.iter().find(|r| r.party_id == id).cloned().unwrap();
        assert_eq!(b(CUST).receivable_minor, 5000);   // 54000 - 40000 - 9000
        assert_eq!(b(SUPP).payable_minor, 100000);    // 50000 + 30000 + 20000

        // A/R aging: only sale_1 is credit with outstanding > 0 (5000), dated 2026-07-02.
        let ar = receivable_aging(&conn, ANCHOR).unwrap();
        assert_eq!(ar.len(), 1);
        assert_eq!(ar[0].invoice_id, "sale_1");
        assert_eq!(ar[0].outstanding_minor, 5000);
        assert_eq!(ar[0].bucket, "0-30d");            // 4 days old

        // A/P aging: three open credit purchases.
        let ap = payable_aging(&conn, ANCHOR).unwrap();
        assert_eq!(ap.len(), 3);
        assert_eq!(ap.iter().map(|r| r.outstanding_minor).sum::<i64>(), 100000);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p accounting-core queries::tests::party_balances_and_aging`
Expected: FAIL — `party_balances` / `receivable_aging` / `payable_aging` not found.

- [ ] **Step 3: Implement §8.4 (balances + aging)**

Add to `crates/accounting-core/src/queries.rs`:

```rust
/// A/R & A/P snapshot per party (spec §8.4 who owes us / whom we owe).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartyBalance {
    pub party_id: String,
    pub receivable_minor: i64,
    pub payable_minor: i64,
    pub unallocated_cr_minor: i64,
    pub unallocated_dr_minor: i64,
}

/// One open invoice with its age bucket (spec §8.4 A/R & A/P aging).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgingInvoice {
    pub invoice_id: String,
    pub date: String,
    pub outstanding_minor: i64,
    pub age_days: i64,
    pub bucket: String,
}

const PARTY_BALANCES_SQL: &str = "\
SELECT party_id, receivable_minor, payable_minor, unallocated_cr_minor, unallocated_dr_minor
FROM party_balances ORDER BY party_id";

// Shared aging shape over sales / purchases; only the source table differs.
// 'now' parameterized as ?1.
const RECEIVABLE_AGING_SQL: &str = "\
SELECT id AS invoice_id, date, outstanding_minor,
  CAST(julianday(?1) - julianday(date) AS INT) AS age_days,
  CASE
    WHEN julianday(?1) - julianday(date) <= 30 THEN '0-30d'
    WHEN julianday(?1) - julianday(date) <= 60 THEN '31-60d'
    WHEN julianday(?1) - julianday(date) <= 90 THEN '61-90d'
    ELSE '90d+' END AS bucket
FROM sales WHERE terms = 'credit' AND outstanding_minor > 0 ORDER BY date";
const PAYABLE_AGING_SQL: &str = "\
SELECT id AS invoice_id, date, outstanding_minor,
  CAST(julianday(?1) - julianday(date) AS INT) AS age_days,
  CASE
    WHEN julianday(?1) - julianday(date) <= 30 THEN '0-30d'
    WHEN julianday(?1) - julianday(date) <= 60 THEN '31-60d'
    WHEN julianday(?1) - julianday(date) <= 90 THEN '61-90d'
    ELSE '90d+' END AS bucket
FROM purchases WHERE terms = 'credit' AND outstanding_minor > 0 ORDER BY date";

/// §8.4 — per-party A/R, A/P, and held credits.
pub fn party_balances(conn: &Connection) -> rusqlite::Result<Vec<PartyBalance>> {
    let mut stmt = conn.prepare(PARTY_BALANCES_SQL)?;
    let rows = stmt.query_map([], |r| {
        Ok(PartyBalance {
            party_id: r.get(0)?,
            receivable_minor: r.get(1)?,
            payable_minor: r.get(2)?,
            unallocated_cr_minor: r.get(3)?,
            unallocated_dr_minor: r.get(4)?,
        })
    })?;
    rows.collect()
}

fn run_aging(conn: &Connection, sql: &str, anchor: &str) -> rusqlite::Result<Vec<AgingInvoice>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(rusqlite::params![anchor], |r| {
        Ok(AgingInvoice {
            invoice_id: r.get(0)?,
            date: r.get(1)?,
            outstanding_minor: r.get(2)?,
            age_days: r.get(3)?,
            bucket: r.get(4)?,
        })
    })?;
    rows.collect()
}

/// §8.4 — open customer invoices (A/R) bucketed by age.
pub fn receivable_aging(conn: &Connection, anchor: &str) -> rusqlite::Result<Vec<AgingInvoice>> {
    run_aging(conn, RECEIVABLE_AGING_SQL, anchor)
}

/// §8.4 — open supplier invoices (A/P) bucketed by age.
pub fn payable_aging(conn: &Connection, anchor: &str) -> rusqlite::Result<Vec<AgingInvoice>> {
    run_aging(conn, PAYABLE_AGING_SQL, anchor)
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p accounting-core queries::tests::party_balances_and_aging`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/accounting-core/src/queries.rs
git commit -m "feat: add §8.4 party-balance and A/R & A/P aging queries"
```

---

### Task 16: §8.4 return rate per item + age-at-sale / turnover

**Files:**
- Modify: `crates/accounting-core/src/queries.rs`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module:

```rust
    #[test]
    fn return_rate_and_age_at_sale() {
        use crate::test_support::{GADGET, WIDGET};
        let (conn, _hlc) = open_seeded();

        let rr = return_rate_per_item(&conn).unwrap();
        let r = |id: &str| rr.iter().find(|x| x.item_id == id).cloned().unwrap();
        // widget: 10 returned / 60 sold = 0.1667; gadget: 0 / 5 = 0.0
        assert_eq!(r(WIDGET).sold_qty, 60);
        assert_eq!(r(WIDGET).returned_qty, 10);
        assert_eq!(r(WIDGET).return_rate, Some(0.1667));
        assert_eq!(r(GADGET).returned_qty, 0);
        assert_eq!(r(GADGET).return_rate, Some(0.0));

        // Age at sale: widget from pur_1#lot0 (2026-06-01) sold 2026-07-02 = 31 days;
        // gadget from pur_3#lot0 (2026-06-20) sold 2026-07-03 = 13 days.
        let ages = age_at_sale(&conn).unwrap();
        let w = ages.iter().find(|x| x.item_id == WIDGET).unwrap();
        assert_eq!(w.age_at_sale_days, 31);
        assert_eq!(w.qty_taken, 60);
        let g = ages.iter().find(|x| x.item_id == GADGET).unwrap();
        assert_eq!(g.age_at_sale_days, 13);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p accounting-core queries::tests::return_rate_and_age_at_sale`
Expected: FAIL — `return_rate_per_item` / `age_at_sale` not found.

- [ ] **Step 3: Implement §8.4 (return rate + age-at-sale)**

Add to `crates/accounting-core/src/queries.rs`:

```rust
/// Return rate for one item (spec §8.4). `return_rate` = returned / sold.
#[derive(Debug, Clone, PartialEq)]
pub struct ReturnRate {
    pub item_id: String,
    pub sold_qty: i64,
    pub returned_qty: i64,
    pub return_rate: Option<f64>, // None when nothing sold
}

/// One lot consumption's holding age at the moment of sale (spec §8.4 turnover).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgeAtSale {
    pub item_id: String,
    pub sale_date: String,
    pub acquired_at: String,
    pub age_at_sale_days: i64,
    pub qty_taken: i64,
}

// Numerator counts SALE returns only: return_lines holds both sale- and
// purchase-return rows, so the subquery joins `returns` and filters
// return_type = 'sale_return' (spec §8.4) — otherwise supplier returns would
// inflate the customer return rate.
const RETURN_RATE_SQL: &str = "\
SELECT sl.item_id,
  COALESCE(SUM(sl.qty), 0) AS sold_qty,
  COALESCE((SELECT SUM(rl.qty) FROM return_lines rl
              JOIN returns r ON r.id = rl.return_id
              WHERE rl.item_id = sl.item_id AND r.return_type = 'sale_return'), 0) AS returned_qty,
  CASE WHEN SUM(sl.qty) = 0 THEN NULL
       ELSE ROUND(1.0 * COALESCE((SELECT SUM(rl.qty) FROM return_lines rl
                                    JOIN returns r ON r.id = rl.return_id
                                    WHERE rl.item_id = sl.item_id AND r.return_type = 'sale_return'), 0)
                  / SUM(sl.qty), 4)
  END AS return_rate
FROM sale_lines sl GROUP BY sl.item_id ORDER BY sl.item_id";

// spec §8.4 age-at-sale/turnover: lot_consumptions -> inventory_lots.acquired_at vs sales.date.
const AGE_AT_SALE_SQL: &str = "\
SELECT sl.item_id, s.date AS sale_date, il.acquired_at,
  CAST(julianday(s.date) - julianday(il.acquired_at) AS INT) AS age_at_sale_days,
  lc.qty_taken
FROM lot_consumptions lc
JOIN sale_lines sl ON sl.id = lc.sale_line_id
JOIN sales s ON s.id = sl.sale_id
JOIN inventory_lots il ON il.id = lc.lot_id
ORDER BY s.date, sl.item_id";

/// §8.4 — return rate per item (returned units / sold units).
pub fn return_rate_per_item(conn: &Connection) -> rusqlite::Result<Vec<ReturnRate>> {
    let mut stmt = conn.prepare(RETURN_RATE_SQL)?;
    let rows = stmt.query_map([], |r| {
        Ok(ReturnRate {
            item_id: r.get(0)?,
            sold_qty: r.get(1)?,
            returned_qty: r.get(2)?,
            return_rate: r.get(3)?,
        })
    })?;
    rows.collect()
}

/// §8.4 — holding age at the moment of each sale's lot consumption (turnover input).
pub fn age_at_sale(conn: &Connection) -> rusqlite::Result<Vec<AgeAtSale>> {
    let mut stmt = conn.prepare(AGE_AT_SALE_SQL)?;
    let rows = stmt.query_map([], |r| {
        Ok(AgeAtSale {
            item_id: r.get(0)?,
            sale_date: r.get(1)?,
            acquired_at: r.get(2)?,
            age_at_sale_days: r.get(3)?,
            qty_taken: r.get(4)?,
        })
    })?;
    rows.collect()
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p accounting-core queries::tests::return_rate_and_age_at_sale`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/accounting-core/src/queries.rs
git commit -m "feat: add §8.4 return-rate-per-item and age-at-sale/turnover queries"
```

---

### Task 17: §8.4 P&L + balance sheet

**Files:**
- Modify: `crates/accounting-core/src/queries.rs`
- Modify: `crates/accounting-core/src/lib.rs`

P&L and balance sheet group `journal_lines` (P&L, over a date range) and `accounts.balance_minor` (balance sheet, as-of) by `accounts.type` (spec §8.4).

- [ ] **Step 1: Write the failing test**

Add to the `tests` module:

```rust
    #[test]
    fn profit_and_loss_and_balance_sheet() {
        let (conn, _hlc) = open_seeded();

        // P&L over the whole business window.
        let pl = profit_and_loss(&conn, "2026-01-01", "2026-12-31").unwrap();
        // income net credit = Sales 52500; expense net debit = COGS 30000 + Rent 3000 = 33000.
        assert_eq!(pl.income_minor, 52500);
        assert_eq!(pl.expense_minor, 33000);
        assert_eq!(pl.net_profit_minor, 19500);

        // Balance sheet as-of: assets = liabilities + equity + retained P&L.
        let bs = balance_sheet(&conn).unwrap();
        assert_eq!(bs.assets_minor, 119500);      // Inv 70000 + Bank 44500 + A/R 5000
        assert_eq!(bs.liabilities_minor, 100000); // A/P
        assert_eq!(bs.equity_minor, 0);           // owner capital / retained earnings unset
        // Accounting identity (P&L not yet closed to equity):
        assert_eq!(bs.assets_minor, bs.liabilities_minor + bs.equity_minor + pl.net_profit_minor);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p accounting-core queries::tests::profit_and_loss_and_balance_sheet`
Expected: FAIL — `profit_and_loss` / `balance_sheet` not found.

- [ ] **Step 3: Implement §8.4 (P&L + balance sheet)**

Add to `crates/accounting-core/src/queries.rs`. Income accounts are normal-credit (net = credit − debit); expense accounts are normal-debit (net = debit − credit):

```rust
/// Profit & loss over a date range (spec §8.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfitAndLoss {
    pub income_minor: i64,
    pub expense_minor: i64,
    pub net_profit_minor: i64,
}

/// Balance sheet totals as-of (spec §8.4), from maintained account balances.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BalanceSheet {
    pub assets_minor: i64,
    pub liabilities_minor: i64,
    pub equity_minor: i64,
}

// Income net = Σ(credit-debit) over income accounts; Expense net = Σ(debit-credit)
// over expense accounts, within [?1, ?2].
const PL_SQL: &str = "\
SELECT
  (SELECT COALESCE(SUM(jl.credit_minor - jl.debit_minor), 0)
     FROM journal_lines jl JOIN accounts a ON a.id = jl.account_id
     WHERE a.type = 'income' AND jl.date BETWEEN ?1 AND ?2) AS income_minor,
  (SELECT COALESCE(SUM(jl.debit_minor - jl.credit_minor), 0)
     FROM journal_lines jl JOIN accounts a ON a.id = jl.account_id
     WHERE a.type = 'expense' AND jl.date BETWEEN ?1 AND ?2) AS expense_minor";

// Balance sheet reads the projector-maintained running balances by account type.
const BS_SQL: &str = "\
SELECT
  COALESCE((SELECT SUM(balance_minor) FROM accounts WHERE type = 'asset'), 0)     AS assets_minor,
  COALESCE((SELECT SUM(balance_minor) FROM accounts WHERE type = 'liability'), 0) AS liabilities_minor,
  COALESCE((SELECT SUM(balance_minor) FROM accounts WHERE type = 'equity'), 0)    AS equity_minor";

/// §8.4 — income, expense, and net profit over [`from`, `to`].
pub fn profit_and_loss(conn: &Connection, from: &str, to: &str) -> rusqlite::Result<ProfitAndLoss> {
    let (income, expense): (i64, i64) =
        conn.query_row(PL_SQL, rusqlite::params![from, to], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(ProfitAndLoss { income_minor: income, expense_minor: expense, net_profit_minor: income - expense })
}

/// §8.4 — asset / liability / equity totals from maintained account balances.
pub fn balance_sheet(conn: &Connection) -> rusqlite::Result<BalanceSheet> {
    conn.query_row(BS_SQL, [], |r| {
        Ok(BalanceSheet { assets_minor: r.get(0)?, liabilities_minor: r.get(1)?, equity_minor: r.get(2)? })
    })
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p accounting-core queries::tests::profit_and_loss_and_balance_sheet`
Expected: PASS.

- [ ] **Step 5: Re-export the full queries API and run the whole suite**

In `crates/accounting-core/src/lib.rs`, add:

```rust
pub use queries::{
    age_at_sale, aging_buckets, balance_sheet, gross_margin_per_item, gross_profit,
    inventory_valuation, lot_ages, net_profit, party_balances, payable_aging, profit_and_loss,
    receivable_aging, return_rate_per_item, sellers_by_units, stock_on_hand, units_sold_by_month,
    AgeAtSale, AgingBucket, AgingInvoice, BalanceSheet, GrossProfit, ItemMargin, LotAge,
    MonthlyUnits, PartyBalance, ProfitAndLoss, ReturnRate, SellerRow, StockOnHand,
};
```

Run: `cargo test -p accounting-core`
Expected: PASS — the entire crate suite (Plans 1–3 tests plus all reconciliation and query tests).

- [ ] **Step 6: Commit**

```bash
git add crates/accounting-core/src/queries.rs crates/accounting-core/src/lib.rs
git commit -m "feat: add §8.4 P&L and balance-sheet queries; re-export queries API"
```

---

### Task 18: Extended-fixture invariant + rebuild oracle

**Files:**
- Modify: `crates/accounting-core/src/test_support.rs`
- Modify: `crates/accounting-core/src/reconciliation.rs`

The single reference business omits the exact states that expose the check #2/#4/#5 corrections: it has no prepayment (unallocated credit), no cash purchase, no reversed sale, and only one party per side. This task adds an **extended** fixture that includes all four, then asserts two properties: (1) **all eight checks pass** on that richer state, and (2) **every §8 report is byte-identical before vs. after `rebuild`** — a projector determinism oracle (spec §3: projections are 100% derivable from the log ordered by `hlc`). If a report drifts across rebuild, a projector is non-deterministic or reads mutable state; if a check fails, one of the §7 corrections is wrong.

- [ ] **Step 1: Add the extended fixture to `test_support.rs`**

Add to `crates/accounting-core/src/test_support.rs`. It uses the same real handlers plus `handle_transaction_reversed`; extend the existing `use crate::commands::{…}` list to add `handle_transaction_reversed`:

```rust
// Extended-fixture ids. Lots are minted `{purchase_id}#lot0` as usual.
pub const CUST_BETA: &str = "cust_beta";

/// A richer business exercising: a CASH purchase, a pure PREPAYMENT (unallocated
/// credit), a SECOND customer, and a REVERSED sale. Fresh DB (does not build on
/// the reference business). Returns (conn, hlc) with everything projected; the
/// ctx borrow is released before returning.
pub fn open_seeded_extended() -> (Connection, Hlc) {
    let mut conn = open_in_memory_with_schema().unwrap();
    let mut hlc = Hlc::new("deviceA");
    run_genesis(&conn, &mut hlc, 1000, "deviceA", "owner-1", "Jane Owner").unwrap();

    // Capture the reversed sale's event id so we can target it, then drop ctx.
    let reversible_id: String;
    {
        let mut ctx = CommandContext {
            conn: &mut conn, hlc: &mut hlc, physical_now: 1000,
            device_id: "deviceA".into(), user_id: "owner-1".into(),
        };

        handle_item_defined(&mut ctx, WIDGET, "W-1", "Widget", "ea").unwrap();
        handle_item_defined(&mut ctx, GADGET, "G-1", "Gadget", "ea").unwrap();
        handle_party_created(&mut ctx, CUST, "Acme Co", "customer").unwrap();
        handle_party_created(&mut ctx, CUST_BETA, "Beta LLC", "customer").unwrap();
        handle_party_created(&mut ctx, SUPP, "Globex", "supplier").unwrap();

        // Credit purchase stocks widgets → lot epur_1#lot0.
        handle_purchase_recorded(&mut ctx, "epur_1", SUPP, "2026-06-01", "credit",
            vec![PurchaseLineInput { item_id: WIDGET.into(), qty: 50, unit_cost_minor: 500 }]).unwrap();
        // CASH purchase stocks gadgets → lot epur_2#lot0 (draws Bank negative — no guard forbids it).
        handle_purchase_recorded(&mut ctx, "epur_2", SUPP, "2026-06-05", "cash",
            vec![PurchaseLineInput { item_id: GADGET.into(), qty: 10, unit_cost_minor: 800 }]).unwrap();

        // Credit sale to the first customer (oldest-first → epur_1#lot0).
        handle_sale_recorded(&mut ctx, "esale_1", CUST, "2026-07-02", "credit",
            vec![SaleLineInput { item_id: WIDGET.into(), qty: 20, unit_price_minor: 900, lot_picks: None }]).unwrap();

        // Pure PREPAYMENT from the second customer: amount with NO allocations -> a
        // 5000 unallocated credit. Posts Dr Bank / Cr A/R for the full amount, so the
        // A/R GL goes negative by 5000 while cust_beta's receivable stays 0 — the
        // case check #4's NET form is built for.
        handle_payment_received(&mut ctx, "epay_1", CUST_BETA, 5000, "2026-07-04", vec![]).unwrap();

        // A cash sale we will fully REVERSE (cash + no allocation + no return => a
        // legal reversal target with no blocking downstream dependency).
        let reversible = handle_sale_recorded(&mut ctx, "esale_2", CUST, "2026-07-05", "cash",
            vec![SaleLineInput { item_id: GADGET.into(), qty: 5, unit_price_minor: 1500, lot_picks: None }]).unwrap();
        reversible_id = reversible.id.clone();
        handle_transaction_reversed(&mut ctx, &reversible_id, "entered in error").unwrap();
    }

    (conn, hlc)
}
```

- [ ] **Step 2: Write the failing oracle test**

Add to the `tests` module in `crates/accounting-core/src/reconciliation.rs`. It captures every §8 report before rebuild, rebuilds, and asserts equality; it also asserts all eight checks pass:

```rust
    #[test]
    fn extended_fixture_passes_all_checks_and_survives_rebuild() {
        use crate::test_support::{open_seeded_extended, ANCHOR, GADGET, WIDGET};
        use crate::queries::*;

        let (mut conn, _hlc) = open_seeded_extended();

        // (1) Invariant: every §7 check passes on the richer state (prepayment,
        // cash purchase, reversed sale, two customers).
        let checks = run_all_checks(&conn).unwrap();
        assert!(all_passed(&checks), "checks failed: {:?}",
            checks.iter().filter(|c| c.outcome != CheckOutcome::Pass).collect::<Vec<_>>());

        // Snapshot every §8 report BEFORE rebuild.
        let before = (
            units_sold_by_month(&conn, WIDGET, ANCHOR).unwrap(),
            gross_profit(&conn, ANCHOR).unwrap(),
            net_profit(&conn, ANCHOR).unwrap(),
            lot_ages(&conn, GADGET, ANCHOR).unwrap(),
            aging_buckets(&conn, ANCHOR).unwrap(),
            stock_on_hand(&conn).unwrap(),
            inventory_valuation(&conn).unwrap(),
            gross_margin_per_item(&conn).unwrap(),
            sellers_by_units(&conn).unwrap(),
            party_balances(&conn).unwrap(),
            receivable_aging(&conn, ANCHOR).unwrap(),
            payable_aging(&conn, ANCHOR).unwrap(),
            return_rate_per_item(&conn).unwrap(),
            age_at_sale(&conn).unwrap(),
            profit_and_loss(&conn, "2026-01-01", "2026-12-31").unwrap(),
            balance_sheet(&conn).unwrap(),
        );

        // Drop and replay all projections from the event log.
        crate::projectors::rebuild(&mut conn).unwrap();

        // (2) Determinism oracle: every report is identical after rebuild.
        let after = (
            units_sold_by_month(&conn, WIDGET, ANCHOR).unwrap(),
            gross_profit(&conn, ANCHOR).unwrap(),
            net_profit(&conn, ANCHOR).unwrap(),
            lot_ages(&conn, GADGET, ANCHOR).unwrap(),
            aging_buckets(&conn, ANCHOR).unwrap(),
            stock_on_hand(&conn).unwrap(),
            inventory_valuation(&conn).unwrap(),
            gross_margin_per_item(&conn).unwrap(),
            sellers_by_units(&conn).unwrap(),
            party_balances(&conn).unwrap(),
            receivable_aging(&conn, ANCHOR).unwrap(),
            payable_aging(&conn, ANCHOR).unwrap(),
            return_rate_per_item(&conn).unwrap(),
            age_at_sale(&conn).unwrap(),
            profit_and_loss(&conn, "2026-01-01", "2026-12-31").unwrap(),
            balance_sheet(&conn).unwrap(),
        );
        assert_eq!(before, after, "a §8 report drifted across rebuild — projector is non-deterministic");

        // And the checks still pass on the rebuilt projections.
        assert!(all_passed(&run_all_checks(&conn).unwrap()));
    }
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p accounting-core reconciliation::tests::extended_fixture_passes_all_checks_and_survives_rebuild`
Expected: FAIL — `open_seeded_extended` / `CUST_BETA` not found.

- [ ] **Step 4: Confirm the fixture + oracle pass**

With the Step 1 fixture in place (and the check corrections from Tasks 2/4/5 and the report `reversed = 0` filters), run the test:

Run: `cargo test -p accounting-core reconciliation::tests::extended_fixture_passes_all_checks_and_survives_rebuild`
Expected: PASS — all eight checks pass on the extended state, and all sixteen §8 reports are identical before and after rebuild.

> **If check #4 fails here**, the net-form correction (Task 4) is not applied: the prepayment drives the A/R GL to −5000 while `cust_beta.receivable = 0`, so only `Σreceivable − Σunallocated_cr` reconciles. **If check #2 or a sale report differs**, the reversed sale `esale_2` is not being excluded via `reversed = 0`. This test is the teeth behind Design-notes #1, #2, and #5.

- [ ] **Step 5: Run the whole suite and commit**

Run: `cargo test -p accounting-core`
Expected: PASS (entire crate).

```bash
git add crates/accounting-core/src/test_support.rs crates/accounting-core/src/reconciliation.rs
git commit -m "test: add extended-fixture invariant + rebuild determinism oracle"
```

---

## Definition of Done (Plan 4)

- `cargo test -p accounting-core` passes with every task's tests green (Plans 1–3 plus all Plan 4 tests).
- **All eight §7 checks implemented and independently proven.** Each of `check_inventory_valuation`, `check_gross_profit`, `check_double_entry`, `check_party_balances`, `check_invoice_outstanding`, `check_non_negative_inventory`, `check_lot_bounds`, `check_non_negative_credits` returns `CheckOutcome::Pass` on the reference business AND `CheckOutcome::Fail(descriptive discrepancy)` when a deliberately corrupted projection row is injected — the corrupt-row test proves each check has teeth. `run_all_checks` runs all eight in spec order; `all_passed` is true for the reference business.
- **All §8 reports implemented and typed.** §8.1 (`units_sold_by_month`), §8.2 gross+net (`gross_profit`, `net_profit`), §8.3 (`lot_ages`, `aging_buckets`), and every §8.4 report — `stock_on_hand`, `inventory_valuation`, `gross_margin_per_item`, `sellers_by_units`, `party_balances`, `receivable_aging`, `payable_aging`, `return_rate_per_item`, `age_at_sale`, `profit_and_loss`, `balance_sheet` — each returns typed Rust structs and is asserted against the fixture's known numbers.
- **Net profit uses the null-safe `IS NOT`.** `NET_PROFIT_SQL` filters `a.system_role IS NOT 'cogs'` (never `<> 'cogs'`), so user-created expense accounts (`system_role = NULL`) are not silently dropped.
- **Check #4 is the net form (spec §7.4).** `Σreceivable − Σunallocated_cr == A/R GL` and the symmetric payable identity — because a payment posts its full amount to the control account, so the naive `Σreceivable == A/R GL` false-alarms on prepayments. Proven by the extended fixture's pure prepayment (A/R GL −5000, receivable 0).
- **Check #5 is terms-aware (spec §7.5).** Credit invoices reconcile to `MAX(0, total−allocated−returned)`; cash invoices must be exactly 0; the `outstanding < 0` guard runs for ALL rows (not a `WHERE terms='credit'` filter). Proven by the cash `sale_2` (total 7500, outstanding 0) passing and a corrupted cash outstanding failing.
- **Reversed sales are excluded from every `sale_lines`/`sales` report and from check #2 (spec §6.2/§6.3 `reversed` column, §8.4 note).** §8.1, §8.2 gross+net (sale_lines side), §8.4 gross-margin and sellers, and check #2's engine side all `JOIN sales ... WHERE reversed = 0`. Proven by the extended fixture's reversed `esale_2`.
- **Read-only.** This plan appends no events and mutates no projection; all functions take `&Connection` and issue only `SELECT`s. It is safe to run `run_all_checks` and any report on a live store.
- **SQL is TS-portable.** Every query and check lives in a raw string constant (no ORM), so the same SQL ports to a TypeScript / `plugin-sql` runtime. The only adaptation from spec §8 is the anchor-date parameter (`'now'` → bound `?`, mandated by §8.4), documented in "Design notes" #3 and applied uniformly.
- **Rebuild-determinism oracle.** Task 18's extended fixture (cash purchase, prepayment, second customer, reversed sale) asserts all eight checks pass AND that every §8 report is byte-identical before vs. after `projectors::rebuild(&mut conn)` — catching non-deterministic projectors (spec §3).
- **Seeding exercises the real write path.** All test state is produced by the Plan 3 command handlers via `test_support::seed_reference_business` / `open_seeded_extended`, so the checks and reports run against realistic, guard-validated, fully-projected state — not hand-built rows (except the deliberate corruptions that prove the checks fail).
