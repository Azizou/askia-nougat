# Accounting Core — Command Handlers & Validation Guards Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **Naming note:** this is **Plan 3 of 4** by execution order (it depends on Plans 1 and 2). The filename slug says `accounting-command-handlers` and the roadmap's *content* numbering historically listed command-handlers as item 2 in an earlier draft; wherever the two disagree, the **execution order (Plan 3 of 4)** is authoritative. Read/write model + projectors is Plan 2 and MUST already be implemented before this plan runs.

**Goal:** Build the write path for the local-first accounting app — the command entry points (one family per event type) and the FULL spec §4.5 validation-guard set. Each handler validates against the read-model projection tables, computes a balanced double-entry posting, **freezes** every derived value (COGS, revenue, reversal journal lines, return unit price, assigned `lotId`) into the event payload, then — in ONE SQLite transaction — appends the immutable event and applies it to the projection. This is the correctness heart of the system: no event may ever be written that would drive a lot negative, over-allocate an invoice, overdraw a credit, or dangle a frozen reference across a rebuild.

**Architecture:** Event-sourced / CQRS (spec §2, §3). The command handler is the ONLY write path. Guards read projected state (Plan 2's tables), so guards run *before* the event exists. On success the handler opens `conn.transaction()`, calls `append_event(&tx, …)` (Plan 1) then `apply_event(&tx, &event)` (Plan 2), and commits — event and projection move together or not at all. A rejected command writes NOTHING: no event row, no projection change.

**Tech Stack:** Rust, `rusqlite` 0.32 (`bundled` SQLite ≥ 3.46, JSONB + generated columns), `serde_json`. Tests via `cargo test` against in-memory SQLite. Raw SQL only (no ORM). Null-safe comparisons use `IS` / `IS NOT`, never `=` / `<>`.

**Spec:** `docs/superpowers/specs/2026-07-06-accounting-schema-design.md`

---

## Plan Series Roadmap

This is **Plan 3 of 4** for the data model. Each produces working, testable software and depends on the prior:

1. **Foundations:** crate scaffold, schema DDL, HLC clock, event store `append_event`/`read_events`, genesis bootstrap. *(done)*
2. **Read model + projectors + rebuild:** all §5–§6 projection-table DDL, `apply_event` per event type, the drop-and-replay `rebuild` loop, `projection_cursor`. *(done)*
3. **Command handlers + guards (this plan):** the command entry points (one per event type) and the full §4.5 guard set. Each handler runs its guards against the read model, then appends the event and applies the projection in one transaction. Depends on Plan 2 because guards read projected state (e.g. a lot's `qty_remaining`, a sale's `outstanding_minor`, a party's `unallocated_cr_minor`).
4. **Reconciliation + queries:** the §7 integrity checks (as an independent backstop over the guards written here) and the §8 report queries.

Later, outside the data-model series: Tauri IPC wiring, reactivity, UI, and (someday) the sync engine.

### Assumptions carried in from Plans 1–2

- `append_event(conn, hlc, physical_now, device_id, user_id, event_type, &payload) -> rusqlite::Result<LedgerEvent>` exists (Plan 1) and takes `&Connection`; because `rusqlite::Transaction` derefs to `Connection`, it is callable as `append_event(&tx, …)` inside a transaction with no signature change.
- `apply_event(conn, &LedgerEvent) -> rusqlite::Result<()>` exists (Plan 2): the projector dispatcher that mutates the projection tables for one event. Also callable as `apply_event(&tx, &event)`.
- `LedgerEvent` fields: `id`, `hlc`, `device_id`, `user_id`, `seq`, `event_type`, `payload` (`serde_json::Value`), `created_at`.
- All §5–§6 projection tables exist and are populated by `apply_event`: `users`, `accounts`, `items`, `parties`, `inventory_lots`, `journal_lines`, `sales`, `sale_lines`, `lot_consumptions`, `purchases`, `purchase_lines`, `payments`, `payment_allocations`, `party_balances`, `returns`, `return_lines`, `expenses`, `projection_cursor`.
- Accounts are resolved by `system_role` (spec §5.2), never by name or id.
- `rebuild` (Plan 2) drops and replays projections in `ORDER BY hlc`. This is why every ID a *later* event references must be assigned by the handler and frozen into the creating event's payload (spec §4.5 deterministic-referenced-ID rule), NOT minted by the projector.

---

## Design Overview — how guards, freezing, and the atomic commit fit together

Read this once before Task 1; every task instantiates part of it.

### The one write-path shape (every handler)

```text
handle_X(ctx, input):
  1. VALIDATE   — run the guards that apply to X's event categories, reading the
                  projection tables. Any violation → return Err(CommandError), write nothing.
  2. FREEZE     — compute the balanced double-entry posting AND every derived value
                  (COGS, revenue, reversal journal lines, return unit price), assign any
                  referenced IDs (lotId, …), and build the final event payload.
  3. COMMIT     — conn.transaction():
                     let ev = append_event(&tx, hlc, now, device, user, "X", &payload)?;
                     apply_event(&tx, &ev)?;
                   tx.commit()   // event + projection atomic
```

Steps 1–2 happen *outside* the transaction (they only read). Step 3 is the sole mutation and is atomic. If `append_event` or `apply_event` errors, the transaction rolls back and the log is unchanged — the same property the guards guarantee for rejected commands, now enforced by the DB for mid-flight failures.

### Event categories drive guards (spec §4.5)

Guards are phrased over **categories**, not event names, so a new event type inherits guards by joining a category (the recurring v4–v6 failure mode the spec calls out). We encode the categories once (`categories.rs`) and each guard takes the category-relevant slice of the payload. Categories:

| Category | Events | Guards that fire |
|---|---|---|
| **lot-creating** | `PurchaseRecorded`, `OpeningBalancesRecorded`, `InventoryFound` | lot/item-match; (as reversal target) lot-source void |
| **lot-consuming** | `SaleRecorded`, `PurchaseReturnRecorded`, `InventoryAdjusted` | oversell; lot/item-match |
| **lot-restoring** | `SaleReturnRecorded` | sale-return over-restore; lot/item-match |
| **allocation-bearing** | `PaymentMade`, `PaymentReceived`, `PaymentAllocated` | invoice over-allocation; allocation party-ownership; (+payment-overallocation for the two payment events; +credit-overdraw for `PaymentAllocated`) |
| **transactional** | everything with a journal/inventory/settlement effect except master-data & `OpeningBalancesRecorded` | value validation; legal `TransactionReversed` targets |

`OpeningBalancesRecorded` is genesis-only and is NOT built as a runtime command handler in this plan (Plan 1 genesis emits it if migrating); it is listed for category completeness because guards reference its category membership (e.g. it is lot-creating but is *not* a legal reversal target).

### Frozen / handler-assigned values (spec §2 decision 3, §4.5)

The handler — never the projector — computes and freezes into the payload:
- **`lotId`** for every lot-creating line (`PurchaseRecorded`, `InventoryFound`), derived deterministically so it survives rebuild.
- **`lotConsumption[]`** for a sale (which lots, `qtyTaken`, `unitCostMinor` copied from the lot) and the resulting **`cogsMinor`** and **`revenueMinor`** per line.
- **reversal journal lines** for `TransactionReversed` (the negation of the target's postings), computed at command time like COGS.
- **`unitPriceMinor`** on sale-return lines, frozen from the original sale so revenue reversal needs no cross-event lookup.

### CommandContext

Every handler takes a small context bundling the mutable clock + provenance so call sites stay short and the transaction wrapper is written once:

```rust
pub struct CommandContext<'a> {
    pub conn: &'a mut rusqlite::Connection,
    pub hlc: &'a mut crate::hlc::Hlc,
    pub physical_now: u64,
    pub device_id: String,
    pub user_id: String,
}
```

---

## File Structure

All paths are inside the existing crate at `crates/accounting-core/`. New module tree under `src/commands/`, one responsibility per file.

- `src/commands/mod.rs` — module root. Defines `CommandContext`, the `CommandError` enum, and the single `commit_event` helper that wraps `conn.transaction() { append_event + apply_event }`. Re-exports each handler. One responsibility: the shared command scaffold and the atomic commit boundary.
- `src/commands/categories.rs` — the event-category classification (`EventCategory`, `categories_of(event_type)`) and the small typed structs the guards read (`LotLine`, `AllocationLine`). One responsibility: naming the categories the spec's guards are phrased over.
- `src/commands/guards.rs` — the shared, category-level guards that more than one handler calls: value validation, lot/item-match, oversell, invoice over-allocation, payment-overallocation, allocation party-ownership, credit-overdraw, sale-return over-restore, lot-source void, reversal legal-target + double-void, reversal downstream. One responsibility: reusable validation predicates over projection state. Each returns `Result<(), CommandError>`.
- `src/commands/setup.rs` — master-data creation + mutation handlers: `UserRegistered`, `AccountOpened`, `ItemDefined`, `PartyCreated`, `UserUpdated`, `AccountUpdated`, `ItemUpdated`, `PartyUpdated`. One responsibility: master-data commands (no journal posting).
- `src/commands/purchase.rs` — `PurchaseRecorded`, `PurchaseReturnRecorded`. One responsibility: purchase-side transactional commands (lot creation + lot-consuming return).
- `src/commands/sale.rs` — `SaleRecorded` (default oldest-lot-first selection, user-overridable; COGS/revenue freeze) and `SaleReturnRecorded` (lot-restoring; frozen return unit price; return→invoice/party-balance contract). One responsibility: sale-side commands, the profit engine.
- `src/commands/payment.rs` — `PaymentMade`, `PaymentReceived`, `PaymentAllocated`. One responsibility: settlement / allocation commands.
- `src/commands/movement.rs` — `ExpenseRecorded`, `TransferRecorded`, `InventoryAdjusted`, `InventoryFound`. One responsibility: single-purpose ledger/inventory movements (expense posting, account transfer, write-down, found stock).
- `src/commands/reversal.rs` — `TransactionReversed`: the legal-target/double-void/downstream guards' call site and clause 1 of the FOUR-part reversal contract (freeze the negated journal lines). Clauses 2–4 (inventory inverse, settlement unwind, and the `reversed = 1` void marker) are the projector's, applied from this frozen payload. One responsibility: full voiding.

`src/lib.rs` gains `pub mod commands;` and re-exports the handler functions and `CommandError`.

---

### Task 1: Command scaffold — `CommandError`, `CommandContext`, atomic `commit_event`

**Files:**
- Create: `crates/accounting-core/src/commands/mod.rs`
- Modify: `crates/accounting-core/src/lib.rs`

This task builds the shared write-path scaffold every later handler uses: the error type, the context, and the ONE place that opens a transaction and runs `append_event` then `apply_event`. We prove the atomic boundary directly.

- [ ] **Step 1: Write the failing test for the atomic commit boundary**

Create `crates/accounting-core/src/commands/mod.rs`:

```rust
use crate::events::{append_event, LedgerEvent};
use crate::projectors::apply_event; // Plan 2 projector dispatcher
use rusqlite::Connection;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory_with_schema; // Plan 2 helper
    use crate::hlc::Hlc;
    use serde_json::json;

    // A minimal genesis-free fixture: schema + projection tables, an item + a party
    // so later handlers have something to reference. Defined here, reused by sibling
    // command test modules via `super::tests::fixture`.
    pub(crate) fn fixture() -> (Connection, Hlc) {
        let conn = open_in_memory_with_schema().unwrap();
        let hlc = Hlc::new("deviceA");
        (conn, hlc)
    }

    #[test]
    fn commit_event_appends_and_projects_atomically() {
        let (mut conn, mut hlc) = fixture();
        let mut ctx = CommandContext {
            conn: &mut conn, hlc: &mut hlc, physical_now: 1000,
            device_id: "deviceA".into(), user_id: "owner-1".into(),
        };
        let ev = commit_event(&mut ctx, "ItemDefined",
            json!({"itemId": "i1", "sku": "SKU-1", "name": "Widget", "unit": "ea", "active": true}))
            .expect("commit");
        assert_eq!(ev.event_type, "ItemDefined");
        // Event row written…
        let n: i64 = ctx_count(&mut conn, "SELECT COUNT(*) FROM events");
        assert_eq!(n, 1);
        // …AND projection applied in the SAME transaction.
        let items: i64 = ctx_count(&mut conn, "SELECT COUNT(*) FROM items WHERE id='i1'");
        assert_eq!(items, 1, "projection must be applied atomically with append");
    }

    #[test]
    fn commit_event_rolls_back_both_on_projection_failure() {
        let (mut conn, mut hlc) = fixture();
        let mut ctx = CommandContext {
            conn: &mut conn, hlc: &mut hlc, physical_now: 1000,
            device_id: "deviceA".into(), user_id: "owner-1".into(),
        };
        // An event whose projector will fail (unknown type → apply_event errors,
        // or a payload the projector rejects). Assert NEITHER the event nor any
        // projection row survives — the transaction rolled back.
        let res = commit_event(&mut ctx, "TotallyUnknownEventType", json!({}));
        assert!(res.is_err(), "unknown event must fail in projector");
        let n: i64 = ctx_count(&mut conn, "SELECT COUNT(*) FROM events");
        assert_eq!(n, 0, "append must roll back when projection fails");
    }

    fn ctx_count(conn: &mut Connection, sql: &str) -> i64 {
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }
}
```

> Note: `open_in_memory_with_schema` and `apply_event`/`projector` are Plan 2 artifacts. If Plan 2 named them differently, adjust the two imports here and in every sibling test module (single source of truth is this `fixture`). The atomic-rollback test assumes Plan 2's `apply_event` returns `Err` for an unknown event type; if instead it is a no-op, replace `"TotallyUnknownEventType"` with an event whose payload the projector rejects (e.g. an `AccountOpened` with a duplicate `system_role`), keeping the assertion that the log is unchanged.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p accounting-core commands::tests::commit_event`
Expected: FAIL — `CommandContext` / `commit_event` / `CommandError` not found (compile error).

- [ ] **Step 3: Implement the scaffold**

At the top of `crates/accounting-core/src/commands/mod.rs`, above the test module:

```rust
pub mod categories;
pub mod guards;
pub mod setup;
pub mod purchase;
pub mod sale;
pub mod payment;
pub mod movement;
pub mod reversal;

/// Everything a command needs: the connection, the clock, and audit provenance.
/// `conn` is `&mut` because `commit_event` opens a transaction on it.
pub struct CommandContext<'a> {
    pub conn: &'a mut Connection,
    pub hlc: &'a mut crate::hlc::Hlc,
    pub physical_now: u64,
    pub device_id: String,
    pub user_id: String,
}

/// Every way a command can be rejected BEFORE anything is written. `Validation`
/// carries a human-readable reason; `Db` wraps a rusqlite error surfaced during
/// the transaction (which rolls back). Guards return `Validation`.
#[derive(Debug)]
pub enum CommandError {
    Validation(String),
    Db(rusqlite::Error),
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CommandError::Validation(m) => write!(f, "validation error: {m}"),
            CommandError::Db(e) => write!(f, "db error: {e}"),
        }
    }
}
impl std::error::Error for CommandError {}
impl From<rusqlite::Error> for CommandError {
    fn from(e: rusqlite::Error) -> Self { CommandError::Db(e) }
}

/// A guard rejection constructor, kept terse for guard call sites.
pub(crate) fn reject(msg: impl Into<String>) -> CommandError {
    CommandError::Validation(msg.into())
}

/// THE atomic write boundary (spec §3): append the event and apply it to the
/// projection inside a single SQLite transaction. Callers MUST have already run
/// all applicable guards (which only read) and frozen the payload. If append or
/// projection fails, the transaction rolls back — no event, no projection change.
pub fn commit_event(
    ctx: &mut CommandContext,
    event_type: &str,
    payload: serde_json::Value,
) -> Result<LedgerEvent, CommandError> {
    let tx = ctx.conn.transaction()?;
    let ev = append_event(
        &tx, ctx.hlc, ctx.physical_now, &ctx.device_id, &ctx.user_id, event_type, &payload,
    )?;
    apply_event(&tx, &ev)?;
    tx.commit()?;
    Ok(ev)
}
```

> The clock ticks *inside* `append_event`, which happens inside the transaction. If the transaction rolls back, the in-memory `Hlc` has still advanced its counter — that is harmless (it only ever moves the next stamp *forward*, preserving global ordering and uniqueness; a burnt counter value is never reused, never sorts backward).

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p accounting-core commands::tests::commit_event`
Expected: PASS (both tests).

- [ ] **Step 5: Wire up the module and re-exports**

In `crates/accounting-core/src/lib.rs`, add:

```rust
pub mod commands;
pub use commands::{CommandContext, CommandError};
```

(Later tasks add `pub use commands::<handler>` lines as each handler lands.)

- [ ] **Step 6: Commit**

```bash
git add crates/accounting-core/src/commands/mod.rs crates/accounting-core/src/lib.rs
git commit -m "feat: command scaffold with atomic append+project commit boundary"
```

---

### Task 2: Event categories (`categories.rs`)

**Files:**
- Create: `crates/accounting-core/src/commands/categories.rs`

Encode the spec §4.5 categories ONCE so guards dispatch by category, not by enumerated event name. A new event type joins a category here and automatically inherits that category's guards.

- [ ] **Step 1: Write the failing test for category membership**

Create `crates/accounting-core/src/commands/categories.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lot_creating_membership_matches_spec() {
        for t in ["PurchaseRecorded", "OpeningBalancesRecorded", "InventoryFound"] {
            assert!(categories_of(t).contains(&EventCategory::LotCreating), "{t}");
        }
        assert!(!categories_of("SaleRecorded").contains(&EventCategory::LotCreating));
    }

    #[test]
    fn lot_consuming_and_restoring_membership() {
        for t in ["SaleRecorded", "PurchaseReturnRecorded", "InventoryAdjusted"] {
            assert!(categories_of(t).contains(&EventCategory::LotConsuming), "{t}");
        }
        assert!(categories_of("SaleReturnRecorded").contains(&EventCategory::LotRestoring));
    }

    #[test]
    fn allocation_bearing_and_transactional_membership() {
        for t in ["PaymentMade", "PaymentReceived", "PaymentAllocated"] {
            assert!(categories_of(t).contains(&EventCategory::AllocationBearing), "{t}");
        }
        // Transactional = has journal/inventory/settlement effect, excludes master-data
        // and OpeningBalancesRecorded (spec §4.5).
        assert!(categories_of("SaleRecorded").contains(&EventCategory::Transactional));
        assert!(!categories_of("ItemDefined").contains(&EventCategory::Transactional));
        assert!(!categories_of("OpeningBalancesRecorded").contains(&EventCategory::Transactional));
        // PaymentAllocated is transactional (settlement effect) even though it posts no journal.
        assert!(categories_of("PaymentAllocated").contains(&EventCategory::Transactional));
    }
}
```

- [ ] **Step 2: Run to fail**

Run: `cargo test -p accounting-core categories`
Expected: FAIL — `EventCategory` / `categories_of` not found.

- [ ] **Step 3: Implement the classification**

Above the test module in `crates/accounting-core/src/commands/categories.rs`:

```rust
/// The spec §4.5 event categories. Guards and the reversal contract are phrased
/// over these, never over enumerated event names — so a new event type inherits
/// the right guards by joining a category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventCategory {
    LotCreating,
    LotConsuming,
    LotRestoring,
    AllocationBearing,
    /// Has a journal, inventory, or settlement effect. The only legal
    /// TransactionReversed targets. Excludes master-data and OpeningBalancesRecorded.
    Transactional,
}

/// The single source of truth for which categories an event type belongs to.
pub fn categories_of(event_type: &str) -> Vec<EventCategory> {
    use EventCategory::*;
    match event_type {
        // ---- master data: no category (no guards, not reversible) ----
        "UserRegistered" | "AccountOpened" | "ItemDefined" | "PartyCreated"
        | "UserUpdated" | "AccountUpdated" | "ItemUpdated" | "PartyUpdated" => vec![],

        // ---- lot-creating ----
        "PurchaseRecorded" => vec![LotCreating, Transactional],
        "InventoryFound"   => vec![LotCreating, Transactional],
        // Genesis-only; lot-creating but NOT a legal reversal target → not Transactional.
        "OpeningBalancesRecorded" => vec![LotCreating],

        // ---- lot-consuming ----
        "SaleRecorded"           => vec![LotConsuming, Transactional],
        "PurchaseReturnRecorded" => vec![LotConsuming, Transactional],
        "InventoryAdjusted"      => vec![LotConsuming, Transactional],

        // ---- lot-restoring ----
        "SaleReturnRecorded" => vec![LotRestoring, Transactional],

        // ---- allocation-bearing ----
        "PaymentMade"     => vec![AllocationBearing, Transactional],
        "PaymentReceived" => vec![AllocationBearing, Transactional],
        "PaymentAllocated"=> vec![AllocationBearing, Transactional],

        // ---- purely-transactional (journal effect, no lot/alloc) ----
        "ExpenseRecorded"  => vec![Transactional],
        "TransferRecorded" => vec![Transactional],

        // ---- TransactionReversed: itself NOT a legal target (double-void guard) ----
        "TransactionReversed" => vec![],

        _ => vec![],
    }
}

/// Convenience: is this event type a legal TransactionReversed target?
pub fn is_transactional(event_type: &str) -> bool {
    categories_of(event_type).contains(&EventCategory::Transactional)
}
```

> `SaleReturnRecorded` and `PurchaseReturnRecorded` are Transactional (journal + inventory effect) — they are themselves reversible, and the reversal downstream guard treats a return against `T` as a blocking dependency of `T` (spec §4.5 downstream edge 2).

- [ ] **Step 4: Run to pass**

Run: `cargo test -p accounting-core categories`
Expected: PASS (all three tests).

- [ ] **Step 5: Commit**

```bash
git add crates/accounting-core/src/commands/categories.rs
git commit -m "feat: encode spec 4.5 event categories for guard dispatch"
```

---

### Task 3: Value-validation guard (qty>0, amounts>=0, >=1 line)

**Files:**
- Create: `crates/accounting-core/src/commands/guards.rs`

The first and most broadly-applied guard (spec §4.5 "Value validation, all transactional events"): every line `qty > 0`; monetary amounts `>= 0`, and `> 0` where zero is meaningless (payment/expense/transfer amounts); every transactional event has `>= 1` line. Written first because every transactional handler calls it.

- [ ] **Step 1: Write the failing test**

Create `crates/accounting-core/src/commands/guards.rs`:

```rust
use crate::commands::{reject, CommandError};
use rusqlite::Connection;
use serde_json::Value;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rejects_zero_and_negative_quantities() {
        // qty must be > 0
        assert!(check_qty_positive(0).is_err());
        assert!(check_qty_positive(-3).is_err());
        assert!(check_qty_positive(1).is_ok());
    }

    #[test]
    fn rejects_negative_amounts_and_optionally_zero() {
        // amount >= 0 (inventory qtyDelta magnitudes, revenue, etc.)
        assert!(check_amount_non_negative(-1).is_err());
        assert!(check_amount_non_negative(0).is_ok());
        // amount > 0 where zero is meaningless (payments/expenses/transfers)
        assert!(check_amount_positive(0).is_err());
        assert!(check_amount_positive(50).is_ok());
    }

    #[test]
    fn rejects_empty_line_set() {
        let empty: Vec<Value> = vec![];
        assert!(check_at_least_one_line(&empty).is_err());
        assert!(check_at_least_one_line(&[json!({"x":1})]).is_ok());
    }
}
```

- [ ] **Step 2: Run to fail**

Run: `cargo test -p accounting-core guards::tests`
Expected: FAIL — functions not found.

- [ ] **Step 3: Implement the value-validation predicates**

Above the test module in `crates/accounting-core/src/commands/guards.rs`:

```rust
/// qty on every transactional line must be strictly positive (spec §4.5).
pub(crate) fn check_qty_positive(qty: i64) -> Result<(), CommandError> {
    if qty > 0 { Ok(()) } else { Err(reject(format!("qty must be > 0, got {qty}"))) }
}

/// Monetary amounts that may legitimately be zero (e.g. a revenue line at price 0)
/// must still be non-negative (spec §4.5).
pub(crate) fn check_amount_non_negative(amount: i64) -> Result<(), CommandError> {
    if amount >= 0 { Ok(()) } else { Err(reject(format!("amount must be >= 0, got {amount}"))) }
}

/// Monetary amounts where zero is meaningless: payment, expense, transfer amounts
/// (spec §4.5 "> 0 where zero is meaningless").
pub(crate) fn check_amount_positive(amount: i64) -> Result<(), CommandError> {
    if amount > 0 { Ok(()) } else { Err(reject(format!("amount must be > 0, got {amount}"))) }
}

/// Every transactional event must carry at least one line (spec §4.5).
pub(crate) fn check_at_least_one_line<T>(lines: &[T]) -> Result<(), CommandError> {
    if lines.is_empty() { Err(reject("event must have >= 1 line")) } else { Ok(()) }
}
```

> `Connection` and `Value` imports are used by later guards added to this file (oversell, allocation, etc.); keep them.

- [ ] **Step 4: Run to pass**

Run: `cargo test -p accounting-core guards::tests`
Expected: PASS (all three tests).

- [ ] **Step 5: Commit**

```bash
git add crates/accounting-core/src/commands/guards.rs
git commit -m "feat: value-validation guard (qty>0, amounts>=0/>0, >=1 line)"
```

---

### Task 4: Master-data handlers (`setup.rs`) — create + mutation

**Files:**
- Create: `crates/accounting-core/src/commands/setup.rs`
- Modify: `crates/accounting-core/src/lib.rs`

The eight master-data commands (spec §4.4 setup + mutation tables): `UserRegistered`, `AccountOpened`, `ItemDefined`, `PartyCreated`, `UserUpdated`, `AccountUpdated`, `ItemUpdated`, `PartyUpdated`. No journal posting, no lot/allocation category → no §4.5 transactional guards. Their handler-level rules: creates reject duplicate ids; `AccountUpdated` rejects any attempt to change `type`/`normalSide`/`system_role` (immutable, spec §5.2); updates reject unknown ids. These are the reference-existence prerequisites later transactional handlers depend on (a party must exist before a sale references it — spec §5.5).

- [ ] **Step 1: Write the failing tests (reject bad, accept good)**

Create `crates/accounting-core/src/commands/setup.rs`:

```rust
use crate::commands::{commit_event, reject, CommandContext, CommandError};
use serde_json::json;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::tests::fixture;

    fn ctx<'a>(conn: &'a mut rusqlite::Connection, hlc: &'a mut crate::hlc::Hlc) -> CommandContext<'a> {
        CommandContext { conn, hlc, physical_now: 1000, device_id: "deviceA".into(), user_id: "owner-1".into() }
    }

    #[test]
    fn party_created_then_duplicate_rejected_and_not_written() {
        let (mut conn, mut hlc) = fixture();
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_party_created(&mut c, "p1", "Acme", "supplier").expect("first ok");
        }
        // Duplicate id must be rejected…
        let err = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_party_created(&mut c, "p1", "Acme Dup", "supplier").unwrap_err()
        };
        assert!(matches!(err, CommandError::Validation(_)));
        // …and NOT written: still exactly one party event + one party row.
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM events WHERE type='PartyCreated'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1, "duplicate must not append a second event");
    }

    #[test]
    fn account_updated_rejects_immutable_type_change() {
        let (mut conn, mut hlc) = fixture();
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_account_opened(&mut c, "a1", "Misc", "expense", "debit", None).expect("open");
        }
        // Changing name is allowed…
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_account_updated(&mut c, "a1", json!({"name": "Miscellaneous"})).expect("rename ok");
        }
        // …changing type is immutable → reject, nothing written.
        let err = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_account_updated(&mut c, "a1", json!({"type": "asset"})).unwrap_err()
        };
        assert!(matches!(err, CommandError::Validation(_)));
    }

    #[test]
    fn item_updated_rejects_unknown_id() {
        let (mut conn, mut hlc) = fixture();
        let mut c = ctx(&mut conn, &mut hlc);
        let err = handle_item_updated(&mut c, "nope", json!({"name": "X"})).unwrap_err();
        assert!(matches!(err, CommandError::Validation(_)));
    }

    #[test]
    fn party_updated_rejects_invalid_kind_in_changes() {
        let (mut conn, mut hlc) = fixture();
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_party_created(&mut c, "p1", "Acme", "supplier").expect("create");
        }
        let mut c = ctx(&mut conn, &mut hlc);
        // A rename is fine…
        assert!(handle_party_updated(&mut c, "p1", json!({"name": "Acme Inc"})).is_ok());
        // …but an invalid `kind` in changes is rejected (mirrors create-time enum check).
        let err = handle_party_updated(&mut c, "p1", json!({"kind": "vendor"})).unwrap_err();
        assert!(matches!(err, CommandError::Validation(_)));
    }
}
```

- [ ] **Step 2: Run to fail**

Run: `cargo test -p accounting-core commands::setup`
Expected: FAIL — handlers not found.

- [ ] **Step 3: Implement the master-data handlers**

Above the test module in `crates/accounting-core/src/commands/setup.rs`:

```rust
/// Reject if a row with `id` already exists in `table` (create-time uniqueness).
fn ensure_absent(ctx: &CommandContext, table: &str, id: &str) -> Result<(), CommandError> {
    let sql = format!("SELECT COUNT(*) FROM {table} WHERE id = ?1");
    let n: i64 = ctx.conn.query_row(&sql, [id], |r| r.get(0))?;
    if n > 0 { Err(reject(format!("{table} id already exists: {id}"))) } else { Ok(()) }
}

/// Reject if a row with `id` does NOT exist (update-time existence).
fn ensure_present(ctx: &CommandContext, table: &str, id: &str) -> Result<(), CommandError> {
    let sql = format!("SELECT COUNT(*) FROM {table} WHERE id = ?1");
    let n: i64 = ctx.conn.query_row(&sql, [id], |r| r.get(0))?;
    if n == 0 { Err(reject(format!("{table} id not found: {id}"))) } else { Ok(()) }
}

pub fn handle_user_registered(ctx: &mut CommandContext, user_id: &str, name: &str, role: Option<&str>)
    -> Result<crate::events::LedgerEvent, CommandError> {
    ensure_absent(ctx, "users", user_id)?;
    let mut p = json!({ "userId": user_id, "name": name });
    if let Some(r) = role { p["role"] = json!(r); }
    commit_event(ctx, "UserRegistered", p)
}

pub fn handle_account_opened(
    ctx: &mut CommandContext, account_id: &str, name: &str, acct_type: &str,
    normal_side: &str, system_role: Option<&str>,
) -> Result<crate::events::LedgerEvent, CommandError> {
    ensure_absent(ctx, "accounts", account_id)?;
    if !matches!(acct_type, "asset"|"liability"|"equity"|"income"|"expense") {
        return Err(reject(format!("invalid account type: {acct_type}")));
    }
    if !matches!(normal_side, "debit"|"credit") {
        return Err(reject(format!("invalid normal side: {normal_side}")));
    }
    let mut p = json!({ "accountId": account_id, "name": name, "type": acct_type, "normal": normal_side });
    if let Some(r) = system_role { p["system_role"] = json!(r); }
    commit_event(ctx, "AccountOpened", p)
}

pub fn handle_item_defined(ctx: &mut CommandContext, item_id: &str, sku: &str, name: &str, unit: &str)
    -> Result<crate::events::LedgerEvent, CommandError> {
    ensure_absent(ctx, "items", item_id)?;
    commit_event(ctx, "ItemDefined",
        json!({ "itemId": item_id, "sku": sku, "name": name, "unit": unit, "active": true }))
}

pub fn handle_party_created(ctx: &mut CommandContext, party_id: &str, name: &str, kind: &str)
    -> Result<crate::events::LedgerEvent, CommandError> {
    ensure_absent(ctx, "parties", party_id)?;
    if !matches!(kind, "supplier"|"customer"|"both") {
        return Err(reject(format!("invalid party kind: {kind}")));
    }
    commit_event(ctx, "PartyCreated", json!({ "partyId": party_id, "name": name, "kind": kind }))
}

// ---- mutation handlers (patch semantics, spec §4.5) ----

pub fn handle_user_updated(ctx: &mut CommandContext, user_id: &str, changes: serde_json::Value)
    -> Result<crate::events::LedgerEvent, CommandError> {
    ensure_present(ctx, "users", user_id)?;
    commit_event(ctx, "UserUpdated", json!({ "userId": user_id, "changes": changes }))
}

pub fn handle_account_updated(ctx: &mut CommandContext, account_id: &str, changes: serde_json::Value)
    -> Result<crate::events::LedgerEvent, CommandError> {
    ensure_present(ctx, "accounts", account_id)?;
    // type / normalSide / system_role are immutable once opened (spec §5.2): a change
    // would silently corrupt historical balances and projector system_role lookups.
    for immutable in ["type", "normal", "normalSide", "system_role"] {
        if changes.get(immutable).is_some() {
            return Err(reject(format!("account field '{immutable}' is immutable")));
        }
    }
    commit_event(ctx, "AccountUpdated", json!({ "accountId": account_id, "changes": changes }))
}

pub fn handle_item_updated(ctx: &mut CommandContext, item_id: &str, changes: serde_json::Value)
    -> Result<crate::events::LedgerEvent, CommandError> {
    ensure_present(ctx, "items", item_id)?;
    // Validate changed fields mirror create-time rules: `active` must be a bool.
    if let Some(a) = changes.get("active") {
        if !a.is_boolean() { return Err(reject("item 'active' must be a boolean")); }
    }
    commit_event(ctx, "ItemUpdated", json!({ "itemId": item_id, "changes": changes }))
}

pub fn handle_party_updated(ctx: &mut CommandContext, party_id: &str, changes: serde_json::Value)
    -> Result<crate::events::LedgerEvent, CommandError> {
    ensure_present(ctx, "parties", party_id)?;
    // Mirror the create-time enum check: a changed `kind` must be valid.
    if let Some(k) = changes.get("kind") {
        let ok = k.as_str().map(|s| matches!(s, "supplier"|"customer"|"both")).unwrap_or(false);
        if !ok { return Err(reject(format!("invalid party kind in changes: {k}"))); }
    }
    commit_event(ctx, "PartyUpdated", json!({ "partyId": party_id, "changes": changes }))
}
```

- [ ] **Step 4: Run to pass**

Run: `cargo test -p accounting-core commands::setup`
Expected: PASS (all four tests).

- [ ] **Step 5: Re-export the handlers**

In `crates/accounting-core/src/lib.rs`, add:

```rust
pub use commands::setup::{
    handle_account_opened, handle_account_updated, handle_item_defined, handle_item_updated,
    handle_party_created, handle_party_updated, handle_user_registered, handle_user_updated,
};
```

- [ ] **Step 6: Commit**

```bash
git add crates/accounting-core/src/commands/setup.rs crates/accounting-core/src/lib.rs
git commit -m "feat: master-data command handlers with create/update guards"
```

---

### Task 5: Lot/item-match guard + oversell guard (shared, `guards.rs`)

**Files:**
- Modify: `crates/accounting-core/src/commands/guards.rs`

Two category-level guards read from `inventory_lots`:
- **Lot/item-match guard** (spec §4.5): in any event referencing a lot per line (lot-consuming, lot-restoring, and lot-creating events), each `lotId` must belong to the same `itemId` as its line. Checks item identity, not quantity.
- **Oversell guard** (spec §4.5, unified over the lot-consuming category): reject any lot-consuming demand whose per-lot quantity (`SaleRecorded.lotConsumption[].qtyTaken`, `PurchaseReturnRecorded.lines[].qty`, `|InventoryAdjusted.lines[].qtyDelta|`) exceeds the referenced lot's current `qty_remaining`. No event may drive a lot negative.

Both are keyed by `(lot_id, expected_item_id, qty)` tuples the handler extracts from its payload, so they are event-name-agnostic.

**Intra-command aggregation is mandatory (do not skip).** Guards read only *pre-command committed* state. If two lines (or two picks within one line) of the *same* command each draw the same lot, checking each in isolation lets both pass while their SUM drives the lot negative (two takes of 6 from a 10-unit lot: `6≤10` twice → projector decrements 12 → `qty_remaining = −2`). So the oversell guard is exposed as a small `LotDemand` accumulator that carries a **running per-`lot_id` tally across every pick/line of the command** and re-checks the *cumulative* take against the one committed `qty_remaining`. Its `available()` method also lets sale lot-selection see stock already claimed by earlier lines of the SAME command, so line 2 never re-picks line 1's units. Every lot-consuming handler (`SaleRecorded`, `PurchaseReturnRecorded`, `InventoryAdjusted`) threads ONE `LotDemand` through all its lines.

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `crates/accounting-core/src/commands/guards.rs`:

```rust
    // Helper: seed one lot directly into the projection table for guard unit tests.
    fn seed_lot(conn: &Connection, lot_id: &str, item_id: &str, qty_remaining: i64) {
        conn.execute(
            "INSERT INTO inventory_lots
               (id, item_id, source_event_id, purchase_id, unit_cost_minor,
                qty_received, qty_remaining, acquired_at, supplier_id)
             VALUES (?1, ?2, 'evt', NULL, 100, ?3, ?3, '2026-01-01', NULL)",
            rusqlite::params![lot_id, item_id, qty_remaining],
        ).unwrap();
    }

    #[test]
    fn oversell_guard_rejects_taking_more_than_remaining() {
        let conn = crate::db::open_in_memory_with_schema().unwrap();
        seed_lot(&conn, "lot1", "itemA", 10);
        // Sell 15 from a lot of 10 → reject.
        assert!(check_oversell(&conn, "lot1", 15).is_err(), "15 > 10 must reject");
        // Sell exactly 10 → ok (drives to zero, not negative).
        assert!(check_oversell(&conn, "lot1", 10).is_ok());
        // Sell 6 → ok.
        assert!(check_oversell(&conn, "lot1", 6).is_ok());
    }

    #[test]
    fn oversell_guard_rejects_unknown_lot() {
        let conn = crate::db::open_in_memory_with_schema().unwrap();
        assert!(check_oversell(&conn, "ghost", 1).is_err(), "unknown lot must reject");
    }

    #[test]
    fn lot_demand_rejects_cumulative_overdraw_within_one_command() {
        let conn = crate::db::open_in_memory_with_schema().unwrap();
        seed_lot(&conn, "lot1", "itemA", 10);
        let mut demand = LotDemand::new();
        // First take of 6 from lot1 → ok (6 <= 10).
        assert!(demand.take(&conn, "lot1", 6).is_ok());
        // Second take of 6 from the SAME lot in the SAME command → cumulative 12 > 10 → reject.
        // (Isolated per-take checks would wrongly pass both.)
        assert!(demand.take(&conn, "lot1", 6).is_err(), "cumulative 12 > 10 must reject");
        // available() reflects the committed remaining minus what THIS command already claimed.
        assert_eq!(demand.available(&conn, "lot1").unwrap(), 4, "10 - 6 already claimed");
    }

    #[test]
    fn lot_demand_allows_cumulative_within_stock() {
        let conn = crate::db::open_in_memory_with_schema().unwrap();
        seed_lot(&conn, "lot1", "itemA", 10);
        let mut demand = LotDemand::new();
        assert!(demand.take(&conn, "lot1", 6).is_ok());
        assert!(demand.take(&conn, "lot1", 4).is_ok(), "6 + 4 == 10 exactly");
        assert!(demand.take(&conn, "lot1", 1).is_err(), "one more overdraws");
    }

    #[test]
    fn lot_item_match_guard_rejects_wrong_item() {
        let conn = crate::db::open_in_memory_with_schema().unwrap();
        seed_lot(&conn, "lot1", "itemA", 10);
        // Line claims itemB but lot1 belongs to itemA → reject.
        assert!(check_lot_item_match(&conn, "lot1", "itemB").is_err());
        assert!(check_lot_item_match(&conn, "lot1", "itemA").is_ok());
    }
```

- [ ] **Step 2: Run to fail**

Run: `cargo test -p accounting-core guards::tests::oversell guards::tests::lot_demand guards::tests::lot_item`
Expected: FAIL — `check_oversell` / `LotDemand` / `check_lot_item_match` not found.

- [ ] **Step 3: Implement the lot guards**

Add to `crates/accounting-core/src/commands/guards.rs` (above the test module). Add `use std::collections::HashMap;` to the imports.

```rust
/// Oversell guard (spec §4.5, unified over lot-consuming): `qty` taken from `lot_id`
/// must not exceed the lot's current `qty_remaining`. Rejects unknown lots. `qty` is
/// the positive magnitude (callers pass |qtyDelta| for InventoryAdjusted).
///
/// This one-shot form is a convenience for single-draw callers. Handlers that draw
/// a lot from MULTIPLE lines/picks of the same command MUST use `LotDemand` instead,
/// which aggregates cumulative demand (see below) — otherwise two draws each within
/// stock can jointly drive the lot negative.
pub(crate) fn check_oversell(conn: &Connection, lot_id: &str, qty: i64) -> Result<(), CommandError> {
    let mut d = LotDemand::new();
    d.take(conn, lot_id, qty)
}

/// Running per-lot demand accumulator for ONE command. Threading a single instance
/// through all of a command's lot draws makes the oversell guard sum intra-command
/// demand against the committed `qty_remaining`, closing the two-lines-one-lot hole.
pub(crate) struct LotDemand {
    claimed: HashMap<String, i64>, // lot_id → qty already claimed by this command
}

impl LotDemand {
    pub(crate) fn new() -> Self { Self { claimed: HashMap::new() } }

    /// Committed remaining for a lot (rejects unknown lots).
    fn committed_remaining(conn: &Connection, lot_id: &str) -> Result<i64, CommandError> {
        conn.query_row("SELECT qty_remaining FROM inventory_lots WHERE id = ?1", [lot_id], |r| r.get(0))
            .optional()?
            .ok_or_else(|| reject(format!("unknown lot: {lot_id}")))
    }

    /// Stock still available for THIS command: committed remaining minus what earlier
    /// lines/picks of the same command already claimed. Used by sale lot-selection so
    /// line 2 never re-picks line 1's units.
    pub(crate) fn available(&self, conn: &Connection, lot_id: &str) -> Result<i64, CommandError> {
        let rem = Self::committed_remaining(conn, lot_id)?;
        Ok(rem - self.claimed.get(lot_id).copied().unwrap_or(0))
    }

    /// Claim `qty` from `lot_id`. Rejects if cumulative claim (this + prior claims in
    /// the command) exceeds committed `qty_remaining`. Records the claim on success.
    pub(crate) fn take(&mut self, conn: &Connection, lot_id: &str, qty: i64) -> Result<(), CommandError> {
        let rem = Self::committed_remaining(conn, lot_id)?;
        let prior = self.claimed.get(lot_id).copied().unwrap_or(0);
        let cumulative = prior + qty;
        if cumulative > rem {
            return Err(reject(format!(
                "oversell: lot {lot_id} has {rem} remaining, command already claims {prior}, cannot take {qty} more")));
        }
        self.claimed.insert(lot_id.to_string(), cumulative);
        Ok(())
    }

    /// Mirror of `take` for lot-RESTORING commands (SaleReturnRecorded): cap cumulative
    /// restore of a lot within one command against its headroom
    /// (`qty_received − qty_remaining`), so multiple lotReturns entries for the same lot
    /// cannot jointly push `qty_remaining` past `qty_received`. Records the restore.
    pub(crate) fn restore(&mut self, conn: &Connection, lot_id: &str, qty: i64) -> Result<(), CommandError> {
        let (remaining, received): (i64, i64) = conn.query_row(
            "SELECT qty_remaining, qty_received FROM inventory_lots WHERE id = ?1",
            [lot_id], |r| Ok((r.get(0)?, r.get(1)?)),
        ).optional()?.ok_or_else(|| reject(format!("unknown lot: {lot_id}")))?;
        let headroom = received - remaining;
        let prior = self.claimed.get(lot_id).copied().unwrap_or(0);
        let cumulative = prior + qty;
        if cumulative > headroom {
            return Err(reject(format!(
                "over-restore: lot {lot_id} headroom {headroom}, command already restores {prior}, cannot restore {qty} more")));
        }
        self.claimed.insert(lot_id.to_string(), cumulative);
        Ok(())
    }
}

/// Lot/item-match guard (spec §4.5): the referenced lot must belong to the line's item.
pub(crate) fn check_lot_item_match(conn: &Connection, lot_id: &str, expected_item_id: &str)
    -> Result<(), CommandError> {
    let item: Option<String> = conn.query_row(
        "SELECT item_id FROM inventory_lots WHERE id = ?1", [lot_id], |r| r.get(0),
    ).optional()?;
    match item {
        None => Err(reject(format!("unknown lot: {lot_id}"))),
        Some(it) if it != expected_item_id =>
            Err(reject(format!("lot {lot_id} belongs to item {it}, not {expected_item_id}"))),
        Some(_) => Ok(()),
    }
}
```

Add `use rusqlite::OptionalExtension;` to the imports at the top of `guards.rs` (for `.optional()`).

- [ ] **Step 4: Run to pass**

Run: `cargo test -p accounting-core guards::tests::oversell guards::tests::lot_demand guards::tests::lot_item`
Expected: PASS (all five tests).

- [ ] **Step 5: Commit**

```bash
git add crates/accounting-core/src/commands/guards.rs
git commit -m "feat: oversell (with intra-command LotDemand aggregation) and lot/item-match guards"
```

---

### Task 6: `PurchaseRecorded` handler (`purchase.rs`) — lot creation + deterministic lotId

**Files:**
- Create: `crates/accounting-core/src/commands/purchase.rs`
- Modify: `crates/accounting-core/src/lib.rs`

`PurchaseRecorded` (spec §4.4): the birth of cost layers. It is **lot-creating** and **transactional**. The handler:
1. Runs value validation (`qty>0`, `unitCostMinor>=0`, `>=1 line`) and existence checks (supplier party exists, each item exists).
2. **Assigns a deterministic `lotId` per line** and freezes it into the payload (spec §4.5 deterministic-referenced-ID rule) — `lotId = ${eventId}#${lineIndex}` is impossible before the event id exists, so we derive it from a handler-side deterministic key instead (see below).
3. Freezes the balanced posting intent (Dr Inventory / Cr Bank-or-A/P by `system_role`) — the projector posts the journal lines; the handler carries `terms` so the projector resolves the credit account.
4. Runs lot/item-match is N/A at creation for existing lots, but item existence is checked.

**Deterministic lotId without the event id:** the spec allows any deterministic function of the source event. The event id (the HLC stamp) is deterministic across rebuild *for the same log*, but the handler needs the id at creation time. Resolution: the handler mints `lotId` from a caller-supplied stable business key it also uses as the payload's `purchaseId` — `lotId = format!("{purchaseId}#lot{lineIndex}")`. Because `purchaseId` is carried in the event payload (frozen) and the projector reads `lotId` from the payload (never re-mints), a rebuild reproduces identical lot ids. This is the "deterministic-referenced-ID" contract: the *handler* assigns and freezes; the *projector* reads.

- [ ] **Step 1: Write the failing tests**

Create `crates/accounting-core/src/commands/purchase.rs`:

```rust
use crate::commands::guards::{check_amount_non_negative, check_at_least_one_line, check_qty_positive};
use crate::commands::{commit_event, reject, CommandContext, CommandError};
use serde_json::json;

/// One purchase line as supplied by the caller. `lotId` is NOT supplied — the
/// handler assigns it deterministically and freezes it into the event.
pub struct PurchaseLineInput { pub item_id: String, pub qty: i64, pub unit_cost_minor: i64 }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::tests::fixture;

    fn ctx<'a>(c: &'a mut rusqlite::Connection, h: &'a mut crate::hlc::Hlc) -> CommandContext<'a> {
        CommandContext { conn: c, hlc: h, physical_now: 1000, device_id: "deviceA".into(), user_id: "owner-1".into() }
    }
    fn seed_master(conn: &mut rusqlite::Connection, hlc: &mut crate::hlc::Hlc) {
        let mut c = ctx(conn, hlc);
        crate::commands::setup::handle_party_created(&mut c, "sup1", "Supplier", "supplier").unwrap();
        crate::commands::setup::handle_item_defined(&mut c, "itemA", "SKU-A", "A", "ea").unwrap();
    }

    #[test]
    fn purchase_freezes_deterministic_lot_id_and_creates_lot() {
        let (mut conn, mut hlc) = fixture();
        seed_master(&mut conn, &mut hlc);
        let lines = vec![PurchaseLineInput { item_id: "itemA".into(), qty: 10, unit_cost_minor: 500 }];
        let ev = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_purchase_recorded(&mut c, "pur1", "sup1", "2026-02-01", "credit", lines).expect("ok")
        };
        // lotId frozen in payload as pur1#lot0.
        assert_eq!(ev.payload["lines"][0]["lotId"], "pur1#lot0");
        // Projector created the lot with that exact id and qty_remaining=10.
        let (item, rem): (String, i64) = conn.query_row(
            "SELECT item_id, qty_remaining FROM inventory_lots WHERE id='pur1#lot0'",
            [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(item, "itemA");
        assert_eq!(rem, 10);
    }

    #[test]
    fn purchase_rejects_unknown_supplier_and_writes_nothing() {
        let (mut conn, mut hlc) = fixture();
        seed_master(&mut conn, &mut hlc);
        let lines = vec![PurchaseLineInput { item_id: "itemA".into(), qty: 1, unit_cost_minor: 1 }];
        let err = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_purchase_recorded(&mut c, "pur1", "ghost", "2026-02-01", "credit", lines).unwrap_err()
        };
        assert!(matches!(err, CommandError::Validation(_)));
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM events WHERE type='PurchaseRecorded'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn purchase_rejects_zero_qty() {
        let (mut conn, mut hlc) = fixture();
        seed_master(&mut conn, &mut hlc);
        let lines = vec![PurchaseLineInput { item_id: "itemA".into(), qty: 0, unit_cost_minor: 500 }];
        let mut c = ctx(&mut conn, &mut hlc);
        assert!(handle_purchase_recorded(&mut c, "pur1", "sup1", "2026-02-01", "credit", lines).is_err());
    }
}
```

- [ ] **Step 2: Run to fail**

Run: `cargo test -p accounting-core commands::purchase`
Expected: FAIL — `handle_purchase_recorded` not found.

- [ ] **Step 3: Implement the handler**

Above the test module in `crates/accounting-core/src/commands/purchase.rs`:

```rust
fn ensure_party(ctx: &CommandContext, party_id: &str, want_kind_in: &[&str]) -> Result<(), CommandError> {
    let kind: Option<String> = {
        use rusqlite::OptionalExtension;
        ctx.conn.query_row("SELECT kind FROM parties WHERE id = ?1", [party_id], |r| r.get(0)).optional()?
    };
    match kind {
        None => Err(reject(format!("unknown party: {party_id}"))),
        Some(k) if k != "both" && !want_kind_in.contains(&k.as_str()) =>
            Err(reject(format!("party {party_id} kind '{k}' not in {want_kind_in:?}"))),
        Some(_) => Ok(()),
    }
}

fn ensure_item(ctx: &CommandContext, item_id: &str) -> Result<(), CommandError> {
    use rusqlite::OptionalExtension;
    let found: Option<String> = ctx.conn.query_row(
        "SELECT id FROM items WHERE id = ?1", [item_id], |r| r.get(0)).optional()?;
    if found.is_none() { Err(reject(format!("unknown item: {item_id}"))) } else { Ok(()) }
}

/// `PurchaseRecorded` (spec §4.4). Lot-creating + transactional. Assigns a
/// deterministic `lotId` per line (frozen into the payload) so a rebuild
/// reproduces identical lot ids (spec §4.5 deterministic-referenced-ID rule).
pub fn handle_purchase_recorded(
    ctx: &mut CommandContext,
    purchase_id: &str,
    supplier_id: &str,
    date: &str,
    terms: &str, // "cash" | "credit"
    lines: Vec<PurchaseLineInput>,
) -> Result<crate::events::LedgerEvent, CommandError> {
    // ---- VALIDATE ----
    check_at_least_one_line(&lines)?;
    if !matches!(terms, "cash"|"credit") { return Err(reject(format!("invalid terms: {terms}"))); }
    ensure_party(ctx, supplier_id, &["supplier"])?;
    for l in &lines {
        check_qty_positive(l.qty)?;
        check_amount_non_negative(l.unit_cost_minor)?;
        ensure_item(ctx, &l.item_id)?;
    }
    // ---- FREEZE: assign deterministic lotId, compute total ----
    let mut json_lines = Vec::with_capacity(lines.len());
    let mut total_minor: i64 = 0;
    for (i, l) in lines.iter().enumerate() {
        total_minor += l.qty * l.unit_cost_minor;
        json_lines.push(json!({
            "itemId": l.item_id, "qty": l.qty, "unitCostMinor": l.unit_cost_minor,
            "lotId": format!("{purchase_id}#lot{i}"),
        }));
    }
    let payload = json!({
        "purchaseId": purchase_id, "supplierId": supplier_id, "date": date,
        "terms": terms, "totalMinor": total_minor, "lines": json_lines,
    });
    // ---- COMMIT (guards → transaction{append + project}) ----
    commit_event(ctx, "PurchaseRecorded", payload)
}
```

> **This is the concrete "guards → transaction{append + project}" flow** the plan is required to show: the handler validates against projections, freezes derived values (`lotId`, `totalMinor`), and `commit_event` performs the atomic append+project. The projector (Plan 2) reads `lines[].lotId` verbatim to create `inventory_lots` rows and posts Dr Inventory / Cr (Bank if `terms='cash'` else A/P) via `system_role`.

- [ ] **Step 4: Run to pass**

Run: `cargo test -p accounting-core commands::purchase`
Expected: PASS (all three tests).

- [ ] **Step 5: Re-export**

In `crates/accounting-core/src/lib.rs`:

```rust
pub use commands::purchase::{handle_purchase_recorded, PurchaseLineInput};
```

- [ ] **Step 6: Commit**

```bash
git add crates/accounting-core/src/commands/purchase.rs crates/accounting-core/src/lib.rs
git commit -m "feat: PurchaseRecorded handler with deterministic frozen lotId"
```

---

### Task 7: `SaleRecorded` handler (`sale.rs`) — lot selection, COGS/revenue freeze, oversell

**Files:**
- Create: `crates/accounting-core/src/commands/sale.rs`
- Modify: `crates/accounting-core/src/lib.rs`

`SaleRecorded` (spec §4.4, §4.5, load-bearing decision §2) is the profit engine. It is **lot-consuming** + **transactional**. The handler:
1. Value-validates (`qty>0`, `unitPriceMinor>=0`, `>=1 line`); checks customer + items exist.
2. **Selects lots** per line: default **oldest-lot-first** (`ORDER BY acquired_at ASC`, spec §4.5), **user-overridable** — the caller may pass an explicit `lotConsumption[]` and the handler validates it instead of auto-selecting.
3. Threads ONE `LotDemand` (Task 5) through every line/pick so cumulative per-lot demand — not just each pick in isolation — is bounded by the committed `qty_remaining`. Auto-selection reads `demand.available(lot)` so a second line for the same item never re-picks stock a first line already claimed. Also runs the **lot/item-match guard** per pick.
4. **Freezes** into the payload: the chosen `lotConsumption[]` (lotId, qtyTaken, `unitCostMinor` copied from the lot at sale time), the per-line **`revenueMinor` = qty × unitPriceMinor**, and the per-line **`cogsMinor` = Σ qtyTaken × unitCostMinor**. COGS is computed now, never at replay.
5. `commit_event`. The projector posts BOTH the revenue (Dr Bank/AR, Cr Sales) and cost (Dr COGS, Cr Inventory) legs atomically in the one event, and decrements each lot's `qty_remaining`.

If there is insufficient total stock across all lots for a line (counting stock two lines of the same command already claimed), selection/`LotDemand` fails → the whole command is rejected, nothing written.

- [ ] **Step 1: Write the failing tests**

Create `crates/accounting-core/src/commands/sale.rs`:

```rust
use crate::commands::guards::{check_amount_non_negative, check_at_least_one_line,
    check_lot_item_match, check_qty_positive, LotDemand};
use crate::commands::{commit_event, reject, CommandContext, CommandError};
use rusqlite::OptionalExtension;
use serde_json::json;

pub struct SaleLineInput {
    pub item_id: String,
    pub qty: i64,
    pub unit_price_minor: i64,
    /// Optional explicit lot picks (lotId, qtyTaken). None → default oldest-first.
    pub lot_picks: Option<Vec<(String, i64)>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::tests::fixture;
    use crate::commands::purchase::{handle_purchase_recorded, PurchaseLineInput};

    fn ctx<'a>(c: &'a mut rusqlite::Connection, h: &'a mut crate::hlc::Hlc) -> CommandContext<'a> {
        CommandContext { conn: c, hlc: h, physical_now: 1000, device_id: "deviceA".into(), user_id: "owner-1".into() }
    }
    // Seed: customer, item, and TWO lots of itemA (older cost 500, newer cost 700).
    fn seed(conn: &mut rusqlite::Connection, hlc: &mut crate::hlc::Hlc) {
        let mut c = ctx(conn, hlc);
        crate::commands::setup::handle_party_created(&mut c, "cust1", "Cust", "customer").unwrap();
        crate::commands::setup::handle_party_created(&mut c, "sup1", "Sup", "supplier").unwrap();
        crate::commands::setup::handle_item_defined(&mut c, "itemA", "SKU-A", "A", "ea").unwrap();
        // Older lot: acquired 2026-01-01, cost 500, qty 10.
        handle_purchase_recorded(&mut c, "purOld", "sup1", "2026-01-01", "cash",
            vec![PurchaseLineInput{ item_id: "itemA".into(), qty: 10, unit_cost_minor: 500 }]).unwrap();
        // Newer lot: acquired 2026-02-01, cost 700, qty 10.
        handle_purchase_recorded(&mut c, "purNew", "sup1", "2026-02-01", "cash",
            vec![PurchaseLineInput{ item_id: "itemA".into(), qty: 10, unit_cost_minor: 700 }]).unwrap();
    }

    #[test]
    fn sale_defaults_oldest_lot_first_and_freezes_cogs() {
        let (mut conn, mut hlc) = fixture();
        seed(&mut conn, &mut hlc);
        // Sell 12 @ price 1000. Oldest-first: 10 from purOld@500 + 2 from purNew@700.
        let ev = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_sale_recorded(&mut c, "sale1", "cust1", "2026-03-01", "cash",
                vec![SaleLineInput{ item_id:"itemA".into(), qty:12, unit_price_minor:1000, lot_picks: None }]).expect("ok")
        };
        // revenue = 12*1000 = 12000; cogs = 10*500 + 2*700 = 6400 (frozen).
        assert_eq!(ev.payload["lines"][0]["revenueMinor"], 12000);
        assert_eq!(ev.payload["lines"][0]["cogsMinor"], 6400);
        let cons = ev.payload["lines"][0]["lotConsumption"].as_array().unwrap();
        assert_eq!(cons[0]["lotId"], "purOld#lot0");
        assert_eq!(cons[0]["qtyTaken"], 10);
        assert_eq!(cons[0]["unitCostMinor"], 500);
        assert_eq!(cons[1]["lotId"], "purNew#lot0");
        assert_eq!(cons[1]["qtyTaken"], 2);
        // Lots drawn down by projector.
        let old_rem: i64 = conn.query_row("SELECT qty_remaining FROM inventory_lots WHERE id='purOld#lot0'", [], |r| r.get(0)).unwrap();
        let new_rem: i64 = conn.query_row("SELECT qty_remaining FROM inventory_lots WHERE id='purNew#lot0'", [], |r| r.get(0)).unwrap();
        assert_eq!(old_rem, 0);
        assert_eq!(new_rem, 8);
    }

    #[test]
    fn sale_oversell_across_all_lots_rejected_and_writes_nothing() {
        let (mut conn, mut hlc) = fixture();
        seed(&mut conn, &mut hlc); // only 20 total in stock
        let err = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_sale_recorded(&mut c, "sale1", "cust1", "2026-03-01", "cash",
                vec![SaleLineInput{ item_id:"itemA".into(), qty:25, unit_price_minor:1000, lot_picks: None }]).unwrap_err()
        };
        assert!(matches!(err, CommandError::Validation(_)));
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM events WHERE type='SaleRecorded'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0, "oversell must write no event");
        // Lots untouched.
        let old_rem: i64 = conn.query_row("SELECT qty_remaining FROM inventory_lots WHERE id='purOld#lot0'", [], |r| r.get(0)).unwrap();
        assert_eq!(old_rem, 10);
    }

    #[test]
    fn sale_two_same_item_lines_exceeding_one_lot_rejected_and_writes_nothing() {
        let (mut conn, mut hlc) = fixture();
        // ONLY the older lot exists (10 units); drop the newer one by seeding manually.
        {
            let mut c = ctx(&mut conn, &mut hlc);
            crate::commands::setup::handle_party_created(&mut c, "cust1", "Cust", "customer").unwrap();
            crate::commands::setup::handle_party_created(&mut c, "sup1", "Sup", "supplier").unwrap();
            crate::commands::setup::handle_item_defined(&mut c, "itemA", "SKU-A", "A", "ea").unwrap();
            handle_purchase_recorded(&mut c, "purOld", "sup1", "2026-01-01", "cash",
                vec![PurchaseLineInput{ item_id: "itemA".into(), qty: 10, unit_cost_minor: 500 }]).unwrap();
        }
        // Two lines of the same item, each 6. Isolated per-line auto-selection would
        // pick 6 from the 10-unit lot for BOTH (12 total) → lot goes to -2. The shared
        // LotDemand must make line 2 see only 4 available → reject, nothing written.
        let err = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_sale_recorded(&mut c, "sale1", "cust1", "2026-03-01", "cash", vec![
                SaleLineInput{ item_id:"itemA".into(), qty:6, unit_price_minor:1000, lot_picks: None },
                SaleLineInput{ item_id:"itemA".into(), qty:6, unit_price_minor:1000, lot_picks: None },
            ]).unwrap_err()
        };
        assert!(matches!(err, CommandError::Validation(_)));
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM events WHERE type='SaleRecorded'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
        let rem: i64 = conn.query_row("SELECT qty_remaining FROM inventory_lots WHERE id='purOld#lot0'", [], |r| r.get(0)).unwrap();
        assert_eq!(rem, 10, "lot untouched after rejected multi-line command");
    }

    #[test]
    fn sale_two_picks_same_lot_in_one_line_exceeding_stock_rejected() {
        let (mut conn, mut hlc) = fixture();
        seed(&mut conn, &mut hlc);
        // A single line whose user picks are [(purOld,6),(purOld,6)] → cumulative 12 > 10.
        let mut c = ctx(&mut conn, &mut hlc);
        let err = handle_sale_recorded(&mut c, "sale1", "cust1", "2026-03-01", "cash",
            vec![SaleLineInput{ item_id:"itemA".into(), qty:12, unit_price_minor:1000,
                lot_picks: Some(vec![("purOld#lot0".into(), 6), ("purOld#lot0".into(), 6)]) }]).unwrap_err();
        assert!(matches!(err, CommandError::Validation(_)));
    }

    #[test]
    fn sale_user_override_lot_pick_honored_and_oversell_still_enforced() {
        let (mut conn, mut hlc) = fixture();
        seed(&mut conn, &mut hlc);
        // User overrides: take all 12 from the NEWER lot — but it only has 10 → reject.
        let err = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_sale_recorded(&mut c, "sale1", "cust1", "2026-03-01", "cash",
                vec![SaleLineInput{ item_id:"itemA".into(), qty:12, unit_price_minor:1000,
                    lot_picks: Some(vec![("purNew#lot0".into(), 12)]) }]).unwrap_err()
        };
        assert!(matches!(err, CommandError::Validation(_)));
        // Valid override: 5 from new lot, price ok.
        let ev = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_sale_recorded(&mut c, "sale1", "cust1", "2026-03-01", "cash",
                vec![SaleLineInput{ item_id:"itemA".into(), qty:5, unit_price_minor:1000,
                    lot_picks: Some(vec![("purNew#lot0".into(), 5)]) }]).expect("ok")
        };
        assert_eq!(ev.payload["lines"][0]["cogsMinor"], 3500); // 5*700
    }

    #[test]
    fn sale_override_qty_mismatch_rejected() {
        let (mut conn, mut hlc) = fixture();
        seed(&mut conn, &mut hlc);
        // picks sum (4) != line qty (5) → reject.
        let mut c = ctx(&mut conn, &mut hlc);
        let err = handle_sale_recorded(&mut c, "sale1", "cust1", "2026-03-01", "cash",
            vec![SaleLineInput{ item_id:"itemA".into(), qty:5, unit_price_minor:1000,
                lot_picks: Some(vec![("purOld#lot0".into(), 4)]) }]).unwrap_err();
        assert!(matches!(err, CommandError::Validation(_)));
    }
}
```

- [ ] **Step 2: Run to fail**

Run: `cargo test -p accounting-core commands::sale::tests`
Expected: FAIL — `handle_sale_recorded` not found.

- [ ] **Step 3: Implement the handler**

Above the test module in `crates/accounting-core/src/commands/sale.rs`:

```rust
/// Default oldest-lot-first selection (spec §4.5): fill `qty` of `item_id` from
/// open lots ordered by `acquired_at ASC`, tie-broken by lot id for determinism.
/// Returns (lotId, qtyTaken, unitCostMinor) picks or an error if stock is short.
/// `demand` reflects stock already claimed by earlier lines of the SAME command, so
/// a lot's usable quantity is `demand.available(lot)`, not its raw `qty_remaining`.
fn select_oldest_first(conn: &rusqlite::Connection, demand: &LotDemand, item_id: &str, qty: i64)
    -> Result<Vec<(String, i64, i64)>, CommandError> {
    let mut stmt = conn.prepare(
        "SELECT id, unit_cost_minor FROM inventory_lots
         WHERE item_id = ?1 AND qty_remaining > 0
         ORDER BY acquired_at ASC, id ASC",
    )?;
    let rows = stmt.query_map([item_id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    })?;
    let mut need = qty;
    let mut picks = Vec::new();
    for row in rows {
        let (lot_id, cost) = row?;
        if need == 0 { break; }
        // Usable = committed remaining minus what this command already claimed.
        let usable = demand.available(conn, &lot_id)?;
        if usable <= 0 { continue; }
        let take = need.min(usable);
        picks.push((lot_id, take, cost));
        need -= take;
    }
    if need > 0 {
        return Err(reject(format!("insufficient stock for item {item_id}: short by {need}")));
    }
    Ok(picks)
}

/// Resolve the unit cost of a specific lot (for validating user-supplied picks).
fn lot_cost(conn: &rusqlite::Connection, lot_id: &str) -> Result<i64, CommandError> {
    conn.query_row("SELECT unit_cost_minor FROM inventory_lots WHERE id = ?1", [lot_id], |r| r.get(0))
        .optional()?
        .ok_or_else(|| reject(format!("unknown lot: {lot_id}")))
}

/// `SaleRecorded` (spec §4.4/§4.5). Lot-consuming + transactional. Selects lots
/// (oldest-first default, user-overridable), runs the oversell (via a single
/// command-wide `LotDemand`) + lot/item-match guards, and FREEZES lotConsumption
/// + cogsMinor + revenueMinor into the payload.
pub fn handle_sale_recorded(
    ctx: &mut CommandContext,
    sale_id: &str,
    customer_id: &str,
    date: &str,
    terms: &str,
    lines: Vec<SaleLineInput>,
) -> Result<crate::events::LedgerEvent, CommandError> {
    // ---- VALIDATE (existence + value) ----
    check_at_least_one_line(&lines)?;
    if !matches!(terms, "cash"|"credit") { return Err(reject(format!("invalid terms: {terms}"))); }
    // customer must be a customer party
    {
        let kind: Option<String> = ctx.conn.query_row(
            "SELECT kind FROM parties WHERE id = ?1", [customer_id], |r| r.get(0)).optional()?;
        match kind {
            None => return Err(reject(format!("unknown party: {customer_id}"))),
            Some(k) if k != "customer" && k != "both" =>
                return Err(reject(format!("party {customer_id} is not a customer"))),
            _ => {}
        }
    }

    // ---- SELECT + GUARD + FREEZE per line ----
    // ONE demand accumulator for the WHOLE command: cumulative per-lot draws are
    // bounded against committed qty_remaining, and auto-selection sees stock already
    // claimed by earlier lines of this same command.
    let mut demand = LotDemand::new();
    let mut json_lines = Vec::with_capacity(lines.len());
    let mut sale_total: i64 = 0;
    for l in &lines {
        check_qty_positive(l.qty)?;
        check_amount_non_negative(l.unit_price_minor)?;

        // Determine the consumption picks: user override or oldest-first default.
        let picks: Vec<(String, i64, i64)> = match &l.lot_picks {
            Some(user_picks) => {
                let sum: i64 = user_picks.iter().map(|(_, q)| *q).sum();
                if sum != l.qty {
                    return Err(reject(format!(
                        "lot picks for item {} sum to {sum}, expected qty {}", l.item_id, l.qty)));
                }
                let mut out = Vec::with_capacity(user_picks.len());
                for (lot_id, take) in user_picks {
                    check_qty_positive(*take)?;
                    out.push((lot_id.clone(), *take, lot_cost(ctx.conn, lot_id)?));
                }
                out
            }
            // Auto-select respecting stock already claimed by earlier lines of THIS command.
            None => select_oldest_first(ctx.conn, &demand, &l.item_id, l.qty)?,
        };

        // Guards on every consumption: lot/item-match, then cumulative oversell via
        // LotDemand.take (which also records the claim so later lines/picks see it).
        for (lot_id, take, _cost) in &picks {
            check_lot_item_match(ctx.conn, lot_id, &l.item_id)?;
            demand.take(ctx.conn, lot_id, *take)?;
        }

        // FREEZE: revenue, cogs, and the consumption records.
        let revenue_minor = l.qty * l.unit_price_minor;
        let cogs_minor: i64 = picks.iter().map(|(_, q, c)| q * c).sum();
        sale_total += revenue_minor;
        let consumption: Vec<_> = picks.iter().map(|(lot_id, q, c)| json!({
            "lotId": lot_id, "qtyTaken": q, "unitCostMinor": c,
        })).collect();
        json_lines.push(json!({
            "itemId": l.item_id, "qty": l.qty, "unitPriceMinor": l.unit_price_minor,
            "revenueMinor": revenue_minor, "cogsMinor": cogs_minor,
            "lotConsumption": consumption,
        }));
    }

    let payload = json!({
        "saleId": sale_id, "customerId": customer_id, "date": date, "terms": terms,
        "totalMinor": sale_total, "lines": json_lines,
    });
    // ---- COMMIT: atomic append + project (posts revenue AND cost legs) ----
    commit_event(ctx, "SaleRecorded", payload)
}
```

> **Oversell across auto-selection AND across lines:** `select_oldest_first` fails when stock is short *counting the command's prior claims* (it reads `demand.available`); `demand.take` is the single enforcement path for both auto-selected and user-supplied picks, and its running tally is what stops two lines/picks that each fit the raw `qty_remaining` from jointly overdrawing. The multi-line and two-picks-one-lot tests exercise exactly this.

- [ ] **Step 4: Run to pass**

Run: `cargo test -p accounting-core commands::sale::tests`
Expected: PASS (all six tests).

- [ ] **Step 5: Re-export**

In `crates/accounting-core/src/lib.rs`:

```rust
pub use commands::sale::{handle_sale_recorded, SaleLineInput};
```

- [ ] **Step 6: Commit**

```bash
git add crates/accounting-core/src/commands/sale.rs crates/accounting-core/src/lib.rs
git commit -m "feat: SaleRecorded handler with oldest-first lot selection and frozen COGS"
```

---

### Task 8: `PurchaseReturnRecorded` handler (`purchase.rs`) — lot-consuming return

**Files:**
- Modify: `crates/accounting-core/src/commands/purchase.rs`
- Modify: `crates/accounting-core/src/lib.rs`

`PurchaseReturnRecorded` (spec §4.4) — returning purchased goods to the supplier. It is **lot-consuming** (each returned unit is drawn back OUT of the lot it entered) + **transactional**. Handler:
1. Value-validates (`qty>0`, `unitCostMinor>=0`, `>=1 line`); the original purchase exists.
2. Runs **oversell guard** (can't return more than the lot still has) and **lot/item-match guard** per line.
3. Freezes the posting intent (Dr A/P-or-Bank / Cr Inventory) — projector reduces `qty_remaining` and the supplier's `payable_minor`/`purchases.outstanding_minor` per the spec §4.5 return→party-balance contract.

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `crates/accounting-core/src/commands/purchase.rs`:

```rust
    #[test]
    fn purchase_return_over_lot_remaining_rejected() {
        let (mut conn, mut hlc) = fixture();
        seed_master(&mut conn, &mut hlc);
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_purchase_recorded(&mut c, "pur1", "sup1", "2026-02-01", "credit",
                vec![PurchaseLineInput{ item_id:"itemA".into(), qty:10, unit_cost_minor:500 }]).unwrap();
        }
        // Return 15 against a lot that has 10 → reject, nothing written.
        let err = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_purchase_return_recorded(&mut c, "pret1", "pur1", "2026-03-01",
                vec![PurchaseReturnLineInput{ item_id:"itemA".into(), lot_id:"pur1#lot0".into(), qty:15, unit_cost_minor:500 }]).unwrap_err()
        };
        assert!(matches!(err, CommandError::Validation(_)));
        let rem: i64 = conn.query_row("SELECT qty_remaining FROM inventory_lots WHERE id='pur1#lot0'", [], |r| r.get(0)).unwrap();
        assert_eq!(rem, 10, "lot must be untouched after rejected return");
    }

    #[test]
    fn purchase_return_valid_draws_down_lot() {
        let (mut conn, mut hlc) = fixture();
        seed_master(&mut conn, &mut hlc);
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_purchase_recorded(&mut c, "pur1", "sup1", "2026-02-01", "credit",
                vec![PurchaseLineInput{ item_id:"itemA".into(), qty:10, unit_cost_minor:500 }]).unwrap();
        }
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_purchase_return_recorded(&mut c, "pret1", "pur1", "2026-03-01",
                vec![PurchaseReturnLineInput{ item_id:"itemA".into(), lot_id:"pur1#lot0".into(), qty:4, unit_cost_minor:500 }]).expect("ok");
        }
        let rem: i64 = conn.query_row("SELECT qty_remaining FROM inventory_lots WHERE id='pur1#lot0'", [], |r| r.get(0)).unwrap();
        assert_eq!(rem, 6);
    }

    #[test]
    fn purchase_return_lot_item_mismatch_rejected() {
        let (mut conn, mut hlc) = fixture();
        seed_master(&mut conn, &mut hlc);
        {
            let mut c = ctx(&mut conn, &mut hlc);
            crate::commands::setup::handle_item_defined(&mut c, "itemB", "SKU-B", "B", "ea").unwrap();
            handle_purchase_recorded(&mut c, "pur1", "sup1", "2026-02-01", "credit",
                vec![PurchaseLineInput{ item_id:"itemA".into(), qty:10, unit_cost_minor:500 }]).unwrap();
        }
        // Line claims itemB but lot belongs to itemA → reject.
        let mut c = ctx(&mut conn, &mut hlc);
        let err = handle_purchase_return_recorded(&mut c, "pret1", "pur1", "2026-03-01",
            vec![PurchaseReturnLineInput{ item_id:"itemB".into(), lot_id:"pur1#lot0".into(), qty:1, unit_cost_minor:500 }]).unwrap_err();
        assert!(matches!(err, CommandError::Validation(_)));
    }
```

- [ ] **Step 2: Run to fail**

Run: `cargo test -p accounting-core commands::purchase::tests::purchase_return`
Expected: FAIL — `handle_purchase_return_recorded` / `PurchaseReturnLineInput` not found.

- [ ] **Step 3: Implement**

Add to `crates/accounting-core/src/commands/purchase.rs` (above the test module).

```rust
pub struct PurchaseReturnLineInput {
    pub item_id: String, pub lot_id: String, pub qty: i64, pub unit_cost_minor: i64,
}

fn ensure_purchase_exists(ctx: &CommandContext, purchase_id: &str) -> Result<(), CommandError> {
    use rusqlite::OptionalExtension;
    let found: Option<String> = ctx.conn.query_row(
        "SELECT id FROM purchases WHERE id = ?1", [purchase_id], |r| r.get(0)).optional()?;
    if found.is_none() { Err(reject(format!("unknown purchase: {purchase_id}"))) } else { Ok(()) }
}

/// `PurchaseReturnRecorded` (spec §4.4). Lot-consuming + transactional: returned
/// units are drawn back out of their lot, so the oversell guard applies. A single
/// command-wide LotDemand bounds cumulative draws (two lines returning the same lot
/// cannot jointly overdraw it).
pub fn handle_purchase_return_recorded(
    ctx: &mut CommandContext,
    return_id: &str,
    original_purchase_id: &str,
    date: &str,
    lines: Vec<PurchaseReturnLineInput>,
) -> Result<crate::events::LedgerEvent, CommandError> {
    check_at_least_one_line(&lines)?;
    ensure_purchase_exists(ctx, original_purchase_id)?;
    let mut demand = LotDemand::new();
    for l in &lines {
        check_qty_positive(l.qty)?;
        check_amount_non_negative(l.unit_cost_minor)?;
        check_lot_item_match(ctx.conn, &l.lot_id, &l.item_id)?; // item identity
        demand.take(ctx.conn, &l.lot_id, l.qty)?;               // cumulative can't exceed lot
    }
    let json_lines: Vec<_> = lines.iter().map(|l| json!({
        "itemId": l.item_id, "lotId": l.lot_id, "qty": l.qty, "unitCostMinor": l.unit_cost_minor,
    })).collect();
    let total: i64 = lines.iter().map(|l| l.qty * l.unit_cost_minor).sum();
    let payload = json!({
        "returnId": return_id, "originalPurchaseId": original_purchase_id,
        "date": date, "totalMinor": total, "lines": json_lines,
    });
    commit_event(ctx, "PurchaseReturnRecorded", payload)
}
```

Update the top-of-file guard import to:
```rust
use crate::commands::guards::{check_amount_non_negative, check_at_least_one_line,
    check_lot_item_match, check_qty_positive, LotDemand};
```

- [ ] **Step 4: Run to pass**

Run: `cargo test -p accounting-core commands::purchase::tests::purchase_return`
Expected: PASS (all three tests).

- [ ] **Step 5: Re-export**

In `crates/accounting-core/src/lib.rs`:
```rust
pub use commands::purchase::{handle_purchase_return_recorded, PurchaseReturnLineInput};
```

- [ ] **Step 6: Commit**

```bash
git add crates/accounting-core/src/commands/purchase.rs crates/accounting-core/src/lib.rs
git commit -m "feat: PurchaseReturnRecorded handler with oversell + lot/item-match guards"
```

---

### Task 9: `SaleReturnRecorded` handler + sale-return over-restore guard (`sale.rs`, `guards.rs`)

**Files:**
- Modify: `crates/accounting-core/src/commands/guards.rs`
- Modify: `crates/accounting-core/src/commands/sale.rs`
- Modify: `crates/accounting-core/src/lib.rs`

`SaleReturnRecorded` (spec §4.4/§4.5) — a customer returns sold units. It is **lot-restoring** + **transactional**. Rules that materialise here:
- **Sale-return over-restore guard:** for any lot, the returned qty must not exceed the quantity the *original sale consumed from that lot* (per the original sale's `lot_consumptions`), AND must not raise the lot's `qty_remaining` above `qty_received`. Each referenced `lotId` must be one the original sale actually consumed.
- **Return-against-reversed guard** (spec §4.5, new): reject if the original sale's `sales.reversed = 1` — a voided sale no longer exists economically, so restoring its inventory / reversing its revenue is invalid.
- **Frozen return unit price** (spec §6.8): `return_lines.unit_price_minor` is frozen from the original sale at command time, so revenue reversal needs no cross-event lookup.

**PINNED PAYLOAD SHAPE (spec §4.4/§4.5 — projector reads this exactly).** `SaleReturnRecorded.lines` is **NESTED**, one entry per item, with a nested `lotReturns` list (because one returned item may come back across several lots):
```json
{ "itemId": "...", "qty": <total returned>, "unitPriceMinor": <frozen>,
  "lotReturns": [ { "lotId": "...", "qtyReturned": <n>, "unitCostMinor": <lot cost> }, ... ] }
```
The projector iterates `lotReturns` to restore `qty_remaining` and write one `return_lines` row per lot return. **`revenue_reversed` / `cost_restored` are DERIVED by the projector from the lines — the handler must NOT freeze them at the payload top level** (they would be dead, ignored data). So the input struct `SaleReturnItemInput` groups by item with a nested `Vec` of `(lotId, qtyReturned)` — NOT the old one-lot-per-line flat struct. (`PurchaseReturnRecorded` stays FLAT — `{itemId, qty, lotId, unitCostMinor}`, one lot per line — per spec, so Task 8 is unchanged.)

The handler reads the original sale's `lot_consumptions` (joined through `sale_lines`) to (a) validate each referenced lot was actually consumed, (b) bound each lot's return qty (both the static-consumed and the robust remaining-vs-received clauses), and (c) look up the frozen original `unit_price_minor` keyed by the specific lot. It also enforces the lot/item-match and return-against-reversed guards.

**Repeated returns must be bounded (do not skip).** The "consumed from that lot" figure is a STATIC `SUM(lot_consumptions.qty_taken)` — it never subtracts qty already restored by earlier returns. Checking `return_qty ≤ consumed` alone lets the SAME 6 units be returned twice (`qty_remaining` climbs past `qty_received`, breaking reconciliation check #7). So the guard enforces the second clause **directly and robustly** against live lot state: reject if `qty_remaining + return_qty > qty_received`. Because `qty_remaining` already reflects every prior restore, this bounds cumulative returns without a separate "already returned" query, and it composes with the intra-command `LotDemand` for multi-lotReturn commands.

- [ ] **Step 1: Write the failing tests**

First, the guard test — add to the `tests` module in `crates/accounting-core/src/commands/guards.rs`:

```rust
    #[test]
    fn sale_return_over_restore_rejects_more_than_consumed() {
        let conn = crate::db::open_in_memory_with_schema().unwrap();
        // Lot received 10, currently 4 remaining → 6 were consumed by a sale.
        conn.execute(
            "INSERT INTO inventory_lots (id,item_id,source_event_id,purchase_id,unit_cost_minor,qty_received,qty_remaining,acquired_at,supplier_id)
             VALUES ('lot1','itemA','e',NULL,500,10,4,'2026-01-01',NULL)", []).unwrap();
        // Original sale consumed 6 from lot1.
        conn.execute("INSERT INTO sales (id,event_id,customer_id,date,terms,total_minor,outstanding_minor) VALUES ('s1','e','c','2026-02-01','cash',0,0)", []).unwrap();
        conn.execute("INSERT INTO sale_lines (id,sale_id,item_id,qty,unit_price_minor,revenue_minor,cogs_minor,date) VALUES ('sl1','s1','itemA',6,1000,6000,3000,'2026-02-01')", []).unwrap();
        conn.execute("INSERT INTO lot_consumptions (id,sale_line_id,lot_id,qty_taken,unit_cost_minor) VALUES ('lc1','sl1','lot1',6,500)", []).unwrap();

        // Return 7 from lot1 but only 6 were consumed → reject.
        assert!(check_sale_return_over_restore(&conn, "s1", "lot1", 7).is_err());
        // Return 6 → ok (exactly what was consumed; restores lot to 10 = qty_received).
        assert!(check_sale_return_over_restore(&conn, "s1", "lot1", 6).is_ok());
    }

    #[test]
    fn sale_return_rejects_lot_original_sale_never_consumed() {
        let conn = crate::db::open_in_memory_with_schema().unwrap();
        conn.execute("INSERT INTO sales (id,event_id,customer_id,date,terms,total_minor,outstanding_minor) VALUES ('s1','e','c','2026-02-01','cash',0,0)", []).unwrap();
        // No lot_consumptions for s1 referencing 'lotX'.
        assert!(check_sale_return_over_restore(&conn, "s1", "lotX", 1).is_err());
    }

    #[test]
    fn sale_return_second_return_bounded_by_qty_received() {
        let conn = crate::db::open_in_memory_with_schema().unwrap();
        // Lot received 10; a sale consumed 6; then 6 were ALREADY returned, so the lot
        // is back to qty_remaining=10 (== qty_received). The static "consumed" figure is
        // still 6, so a naive `return_qty <= consumed` check would allow another 6.
        conn.execute(
            "INSERT INTO inventory_lots (id,item_id,source_event_id,purchase_id,unit_cost_minor,qty_received,qty_remaining,acquired_at,supplier_id)
             VALUES ('lot1','itemA','e',NULL,500,10,10,'2026-01-01',NULL)", []).unwrap();
        conn.execute("INSERT INTO sales (id,event_id,customer_id,date,terms,total_minor,outstanding_minor) VALUES ('s1','e','c','2026-02-01','cash',0,0)", []).unwrap();
        conn.execute("INSERT INTO sale_lines (id,sale_id,item_id,qty,unit_price_minor,revenue_minor,cogs_minor,date) VALUES ('sl1','s1','itemA',6,1000,6000,3000,'2026-02-01')", []).unwrap();
        conn.execute("INSERT INTO lot_consumptions (id,sale_line_id,lot_id,qty_taken,unit_cost_minor) VALUES ('lc1','sl1','lot1',6,500)", []).unwrap();
        // A further return of 6 would push qty_remaining to 16 > qty_received 10 → reject.
        assert!(check_sale_return_over_restore(&conn, "s1", "lot1", 6).is_err(),
            "second return must be bounded by qty_received via live qty_remaining");
        // Returning 0-more is trivially allowed only for positive qty in the handler;
        // here a return of exactly what's still returnable (0) is out of scope — the
        // point is the > qty_received rejection above.
    }

    #[test]
    fn return_against_reversed_invoice_rejected() {
        let conn = crate::db::open_in_memory_with_schema().unwrap();
        // A reversed sale and a reversed purchase (reversed = 1).
        conn.execute("INSERT INTO sales (id,event_id,customer_id,date,terms,total_minor,outstanding_minor,reversed) VALUES ('s1','e','c','2026-02-01','cash',0,0,1)", []).unwrap();
        conn.execute("INSERT INTO purchases (id,event_id,supplier_id,date,terms,total_minor,outstanding_minor,reversed) VALUES ('p1','e','sup','2026-02-01','cash',0,0,1)", []).unwrap();
        // A live (not reversed) sale.
        conn.execute("INSERT INTO sales (id,event_id,customer_id,date,terms,total_minor,outstanding_minor,reversed) VALUES ('s2','e','c','2026-02-01','cash',0,0,0)", []).unwrap();
        assert!(check_invoice_not_reversed(&conn, "sales", "s1").is_err(), "reversed sale blocks return");
        assert!(check_invoice_not_reversed(&conn, "purchases", "p1").is_err(), "reversed purchase blocks return");
        assert!(check_invoice_not_reversed(&conn, "sales", "s2").is_ok(), "live sale allows return");
        // Unknown invoice rejected too.
        assert!(check_invoice_not_reversed(&conn, "sales", "ghost").is_err());
    }
```

> This test assumes Plan 2's `sales`/`purchases` tables carry a `reversed INTEGER NOT NULL DEFAULT 0` column (set by the `TransactionReversed` projector, contract clause 4). If Plan 2 named it differently, adjust the column name in `check_invoice_not_reversed` and this test to match.

- [ ] **Step 2: Run to fail**

Run: `cargo test -p accounting-core guards::tests::sale_return guards::tests::return_against_reversed`
Expected: FAIL — `check_sale_return_over_restore` / `check_invoice_not_reversed` not found.

- [ ] **Step 3: Implement the over-restore guard**

Add to `crates/accounting-core/src/commands/guards.rs` (above test module):

```rust
/// Sale-return over-restore guard (spec §4.5). Enforces BOTH clauses:
///  1. the referenced lot must be one the original sale actually consumed, and the
///     returned qty must not exceed what that sale consumed from it (static bound);
///  2. the return must not raise the lot's live `qty_remaining` above `qty_received`
///     (robust bound). Clause 2 is checked against the LIVE lot row, which already
///     reflects every prior restore — so repeated returns of the same units are
///     rejected once the lot is back to `qty_received` (keeps reconciliation check #7).
pub(crate) fn check_sale_return_over_restore(
    conn: &Connection, original_sale_id: &str, lot_id: &str, return_qty: i64,
) -> Result<(), CommandError> {
    // Clause 1: the sale must have consumed this lot, and not less than we're returning.
    let consumed: Option<i64> = conn.query_row(
        "SELECT SUM(lc.qty_taken)
         FROM lot_consumptions lc
         JOIN sale_lines sl ON sl.id = lc.sale_line_id
         WHERE sl.sale_id = ?1 AND lc.lot_id = ?2",
        rusqlite::params![original_sale_id, lot_id],
        |r| r.get(0),
    ).optional()?.flatten();
    match consumed {
        None | Some(0) =>
            return Err(reject(format!("sale {original_sale_id} did not consume lot {lot_id}"))),
        Some(c) if return_qty > c =>
            return Err(reject(format!("over-restore: sale consumed {c} from lot {lot_id}, cannot return {return_qty}"))),
        Some(_) => {}
    }
    // Clause 2 (robust): restoring must not exceed the lot's original capacity. Reads
    // LIVE qty_remaining/qty_received, so prior returns are already accounted for.
    let (remaining, received): (i64, i64) = conn.query_row(
        "SELECT qty_remaining, qty_received FROM inventory_lots WHERE id = ?1",
        [lot_id], |r| Ok((r.get(0)?, r.get(1)?)),
    ).optional()?.ok_or_else(|| reject(format!("unknown lot: {lot_id}")))?;
    if remaining + return_qty > received {
        return Err(reject(format!(
            "over-restore: lot {lot_id} has {remaining}/{received}, returning {return_qty} would exceed qty_received")));
    }
    Ok(())
}

/// Return-against-reversed guard (spec §4.5): a return may not target an invoice that
/// has been voided (`reversed = 1`). `table` is "sales" or "purchases". Also rejects
/// an unknown invoice. Uses `IS NOT 1` semantics via an explicit reversed read (the
/// column is NOT NULL DEFAULT 0 in Plan 2's schema).
pub(crate) fn check_invoice_not_reversed(conn: &Connection, table: &str, invoice_id: &str)
    -> Result<(), CommandError> {
    let sql = format!("SELECT reversed FROM {table} WHERE id = ?1");
    let reversed: Option<i64> = conn.query_row(&sql, [invoice_id], |r| r.get(0)).optional()?;
    match reversed {
        None => Err(reject(format!("unknown {table} invoice: {invoice_id}"))),
        Some(1) => Err(reject(format!("cannot return against reversed (voided) {table} invoice {invoice_id}"))),
        Some(_) => Ok(()),
    }
}
```

> `query_row` with `SUM(...)` returns one row whose value is `NULL` when no rows match; `.optional()?.flatten()` collapses both "no row" and "SUM is NULL" to `None`. Kept explicit so the "never consumed" case is unambiguous. Clause 2's `qty_remaining + return_qty > qty_received` is the spec's exact second clause and the defence against repeated returns — it does not rely on the static consumed figure.

- [ ] **Step 4: Write the handler tests**

Add to the `tests` module in `crates/accounting-core/src/commands/sale.rs`:

```rust
    #[test]
    fn sale_return_emits_nested_lot_returns_and_restores_lot() {
        let (mut conn, mut hlc) = fixture();
        seed(&mut conn, &mut hlc);
        // Sale of 10 @ 1000 from oldest lot (purOld@500), credit terms.
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_sale_recorded(&mut c, "sale1", "cust1", "2026-03-01", "credit",
                vec![SaleLineInput{ item_id:"itemA".into(), qty:10, unit_price_minor:1000, lot_picks: None }]).unwrap();
        }
        // Return 3 units of itemA to lot purOld#lot0 — NESTED lotReturns shape.
        let ev = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_sale_return_recorded(&mut c, "sret1", "sale1", "2026-04-01",
                vec![SaleReturnItemInput{ item_id:"itemA".into(),
                    lot_returns: vec![("purOld#lot0".into(), 3)] }]).expect("ok")
        };
        // PINNED SHAPE: line carries total qty + unitPriceMinor + nested lotReturns[].
        assert_eq!(ev.payload["lines"][0]["itemId"], "itemA");
        assert_eq!(ev.payload["lines"][0]["qty"], 3);
        assert_eq!(ev.payload["lines"][0]["unitPriceMinor"], 1000); // frozen from original sale
        let lr = ev.payload["lines"][0]["lotReturns"].as_array().unwrap();
        assert_eq!(lr[0]["lotId"], "purOld#lot0");
        assert_eq!(lr[0]["qtyReturned"], 3);
        assert_eq!(lr[0]["unitCostMinor"], 500); // lot cost restored
        // Projector-derived totals must NOT be frozen at the payload top level (MODERATE 7).
        assert!(ev.payload.get("revenueReversedMinor").is_none(), "totals derived by projector");
        assert!(ev.payload.get("costRestoredMinor").is_none(), "totals derived by projector");
        // Lot restored 0 -> 3.
        let rem: i64 = conn.query_row("SELECT qty_remaining FROM inventory_lots WHERE id='purOld#lot0'", [], |r| r.get(0)).unwrap();
        assert_eq!(rem, 3);
    }

    #[test]
    fn sale_return_over_consumed_rejected_and_writes_nothing() {
        let (mut conn, mut hlc) = fixture();
        seed(&mut conn, &mut hlc);
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_sale_recorded(&mut c, "sale1", "cust1", "2026-03-01", "credit",
                vec![SaleLineInput{ item_id:"itemA".into(), qty:5, unit_price_minor:1000, lot_picks: None }]).unwrap();
        }
        // Sale consumed only 5 from purOld#lot0; return 8 → reject.
        let err = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_sale_return_recorded(&mut c, "sret1", "sale1", "2026-04-01",
                vec![SaleReturnItemInput{ item_id:"itemA".into(),
                    lot_returns: vec![("purOld#lot0".into(), 8)] }]).unwrap_err()
        };
        assert!(matches!(err, CommandError::Validation(_)));
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM events WHERE type='SaleReturnRecorded'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn sale_return_twice_second_exceeds_returnable_rejected() {
        let (mut conn, mut hlc) = fixture();
        seed(&mut conn, &mut hlc);
        // Sale consumes 6 from purOld#lot0 (qty_received 10 → qty_remaining 4).
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_sale_recorded(&mut c, "sale1", "cust1", "2026-03-01", "credit",
                vec![SaleLineInput{ item_id:"itemA".into(), qty:6, unit_price_minor:1000, lot_picks: None }]).unwrap();
        }
        // First return of 6 → ok, restores lot to qty_remaining 10 == qty_received.
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_sale_return_recorded(&mut c, "sret1", "sale1", "2026-04-01",
                vec![SaleReturnItemInput{ item_id:"itemA".into(),
                    lot_returns: vec![("purOld#lot0".into(), 6)] }]).expect("first ok");
        }
        // Second return of 6 (static consumed still says 6, but lot is full) → reject.
        let err = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_sale_return_recorded(&mut c, "sret2", "sale1", "2026-05-01",
                vec![SaleReturnItemInput{ item_id:"itemA".into(),
                    lot_returns: vec![("purOld#lot0".into(), 6)] }]).unwrap_err()
        };
        assert!(matches!(err, CommandError::Validation(_)));
        let rem: i64 = conn.query_row("SELECT qty_remaining FROM inventory_lots WHERE id='purOld#lot0'", [], |r| r.get(0)).unwrap();
        assert_eq!(rem, 10, "lot stays at qty_received; second return did not inflate it");
    }

    #[test]
    fn sale_return_against_reversed_sale_rejected() {
        let (mut conn, mut hlc) = fixture();
        seed(&mut conn, &mut hlc);
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_sale_recorded(&mut c, "sale1", "cust1", "2026-03-01", "credit",
                vec![SaleLineInput{ item_id:"itemA".into(), qty:5, unit_price_minor:1000, lot_picks: None }]).unwrap();
        }
        // Simulate the sale having been voided (projector clause 4 sets reversed=1).
        conn.execute("UPDATE sales SET reversed = 1 WHERE id='sale1'", []).unwrap();
        let err = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_sale_return_recorded(&mut c, "sret1", "sale1", "2026-04-01",
                vec![SaleReturnItemInput{ item_id:"itemA".into(),
                    lot_returns: vec![("purOld#lot0".into(), 2)] }]).unwrap_err()
        };
        assert!(matches!(err, CommandError::Validation(_)));
    }
```

- [ ] **Step 5: Run to fail**

Run: `cargo test -p accounting-core commands::sale::tests::sale_return`
Expected: FAIL — `handle_sale_return_recorded` / `SaleReturnItemInput` not found (all four handler tests).

- [ ] **Step 6: Implement the handler**

Add to `crates/accounting-core/src/commands/sale.rs` (above test module). Extend the file's guard import with `check_sale_return_over_restore, check_invoice_not_reversed, LotDemand`.

```rust
/// One returned ITEM (spec §4.4/§4.5 pinned NESTED shape). `lot_returns` is one or
/// more (lotId, qtyReturned) the item comes back across; the line's total qty is
/// derived as their sum. `unitPriceMinor` is frozen by the handler from the original
/// sale, not supplied.
pub struct SaleReturnItemInput { pub item_id: String, pub lot_returns: Vec<(String, i64)> }

/// Look up the frozen original unit price for a returned line, keyed by the SPECIFIC
/// lot being returned (via lot_consumptions → sale_lines), not by item. Keying by
/// item and taking `LIMIT 1` would pick an arbitrary price when the sale had two
/// same-item lines at different prices. If the referenced lot was filled by lines at
/// more than one distinct price (ambiguous), reject rather than guess.
fn original_sale_line_price(conn: &rusqlite::Connection, sale_id: &str, lot_id: &str)
    -> Result<i64, CommandError> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT sl.unit_price_minor
         FROM lot_consumptions lc
         JOIN sale_lines sl ON sl.id = lc.sale_line_id
         WHERE sl.sale_id = ?1 AND lc.lot_id = ?2",
    )?;
    let prices: Vec<i64> = stmt
        .query_map(rusqlite::params![sale_id, lot_id], |r| r.get::<_, i64>(0))?
        .collect::<Result<_, _>>()?;
    match prices.as_slice() {
        [] => Err(reject(format!("sale {sale_id} has no line consuming lot {lot_id}"))),
        [p] => Ok(*p),
        _ => Err(reject(format!(
            "sale {sale_id} filled lot {lot_id} at multiple prices {prices:?}; return each price line separately"))),
    }
}

/// `SaleReturnRecorded` (spec §4.4/§4.5). Lot-restoring + transactional. Enforces the
/// return-against-reversed, over-restore, and lot/item-match guards, and emits the
/// PINNED NESTED payload: one line per item `{itemId, qty, unitPriceMinor,
/// lotReturns[]:{lotId, qtyReturned, unitCostMinor}}`. Per-line `qty` is the sum of
/// its lotReturns. `revenue_reversed` / `cost_restored` are DERIVED by the projector
/// from these lines — NOT frozen here (MODERATE 7 / spec pinned shape).
pub fn handle_sale_return_recorded(
    ctx: &mut CommandContext,
    return_id: &str,
    original_sale_id: &str,
    date: &str,
    lines: Vec<SaleReturnItemInput>,
) -> Result<crate::events::LedgerEvent, CommandError> {
    check_at_least_one_line(&lines)?;
    // original sale must exist AND must not be voided (return-against-reversed guard).
    check_invoice_not_reversed(ctx.conn, "sales", original_sale_id)?;
    // ONE LotDemand across the whole command bounds cumulative restore per lot when the
    // same lot appears in multiple lotReturns entries (belt-and-braces with the guard).
    let mut demand = LotDemand::new();
    let mut json_lines = Vec::with_capacity(lines.len());
    for item in &lines {
        if item.lot_returns.is_empty() {
            return Err(reject(format!("return line for item {} has no lotReturns", item.item_id)));
        }
        let mut line_qty: i64 = 0;
        // Freeze the item's unit price from the FIRST lot's original sale line; all
        // lotReturns of one item must share one price (over-restore guard keys per lot,
        // and original_sale_line_price rejects ambiguity per lot).
        let mut unit_price: Option<i64> = None;
        let mut lot_returns_json = Vec::with_capacity(item.lot_returns.len());
        for (lot_id, qty_returned) in &item.lot_returns {
            check_qty_positive(*qty_returned)?;
            check_lot_item_match(ctx.conn, lot_id, &item.item_id)?;
            check_sale_return_over_restore(ctx.conn, original_sale_id, lot_id, *qty_returned)?;
            demand.restore(ctx.conn, lot_id, *qty_returned)?; // cumulative restore bound
            let price = original_sale_line_price(ctx.conn, original_sale_id, lot_id)?;
            match unit_price {
                None => unit_price = Some(price),
                Some(p) if p != price =>
                    return Err(reject(format!(
                        "item {} returned across lots at differing prices ({p} vs {price}); split into separate lines",
                        item.item_id))),
                _ => {}
            }
            let unit_cost = lot_cost(ctx.conn, lot_id)?;
            line_qty += *qty_returned;
            lot_returns_json.push(json!({
                "lotId": lot_id, "qtyReturned": qty_returned, "unitCostMinor": unit_cost,
            }));
        }
        json_lines.push(json!({
            "itemId": item.item_id, "qty": line_qty,
            "unitPriceMinor": unit_price.expect("non-empty lotReturns"),
            "lotReturns": lot_returns_json,
        }));
    }
    let payload = json!({
        "returnId": return_id, "originalSaleId": original_sale_id, "date": date,
        "lines": json_lines,
    });
    commit_event(ctx, "SaleReturnRecorded", payload)
}
```

Update the top-of-file guard import in `sale.rs` to include `check_sale_return_over_restore, check_invoice_not_reversed`.

> **`LotDemand.restore`** is the mirror of `take` for lot-restoring: it caps cumulative restore of a lot within one command against `qty_received − qty_remaining` (headroom). Add it alongside `take` in `guards.rs` (Task 5): it reads `qty_received` and `qty_remaining`, and rejects if `already_restored_this_cmd + qty > headroom`. If Task 5's `LotDemand` was not extended with `restore`, add it there in the same commit as this task.

- [ ] **Step 7: Run to pass**

Run: `cargo test -p accounting-core commands::sale::tests guards::tests::sale_return guards::tests::return_against_reversed`
Expected: PASS.

- [ ] **Step 8: Re-export**

In `crates/accounting-core/src/lib.rs`:
```rust
pub use commands::sale::{handle_sale_return_recorded, SaleReturnItemInput};
```

- [ ] **Step 9: Commit**

```bash
git add crates/accounting-core/src/commands/sale.rs crates/accounting-core/src/commands/guards.rs crates/accounting-core/src/lib.rs
git commit -m "feat: SaleReturnRecorded handler (nested lotReturns) + over-restore & return-against-reversed guards"
```

---

### Task 10: Allocation guards (`guards.rs`) — invoice over-allocation, payment-overallocation, party-ownership

**Files:**
- Modify: `crates/accounting-core/src/commands/guards.rs`

Three **allocation-bearing** guards from spec §4.5, all reading invoices (`sales`/`purchases`) and `parties`:
- **Invoice over-allocation guard:** no allocation line's `amountMinor` may exceed the target invoice's current `outstanding_minor` (prevents driving it negative).
- **Payment-overallocation guard:** the sum of `allocations[].amountMinor` must not exceed the payment's own `amountMinor` (`PaymentMade`/`PaymentReceived`); the remainder becomes unallocated credit. Without it, two lines each pass the per-invoice guard yet jointly book more than was paid.
- **Allocation party-ownership guard:** every allocation target must (a) belong to the payment's party and (b) match direction — a `sale` for inflows (`PaymentReceived`), a `purchase` for outflows (`PaymentMade`).

Each guard operates on generic `(target_id, target_type, amount)` allocation tuples, so all three allocation-bearing events share them.

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `crates/accounting-core/src/commands/guards.rs`:

```rust
    fn seed_credit_sale(conn: &Connection, sale_id: &str, cust: &str, outstanding: i64) {
        conn.execute(
            "INSERT INTO sales (id,event_id,customer_id,date,terms,total_minor,outstanding_minor)
             VALUES (?1,'e',?2,'2026-01-01','credit',?3,?3)",
            rusqlite::params![sale_id, cust, outstanding]).unwrap();
    }

    #[test]
    fn invoice_over_allocation_rejected() {
        let conn = crate::db::open_in_memory_with_schema().unwrap();
        seed_credit_sale(&conn, "s1", "cust1", 5000);
        // Allocate 6000 to a 5000-outstanding sale → reject.
        assert!(check_invoice_over_allocation(&conn, "s1", "sale", 6000).is_err());
        assert!(check_invoice_over_allocation(&conn, "s1", "sale", 5000).is_ok());
        assert!(check_invoice_over_allocation(&conn, "s1", "sale", 4000).is_ok());
    }

    #[test]
    fn invoice_over_allocation_aggregates_per_target() {
        let conn = crate::db::open_in_memory_with_schema().unwrap();
        seed_credit_sale(&conn, "s1", "cust1", 5000);
        // Two allocation lines BOTH targeting s1: 5000 + 3000. Each passes the per-line
        // check (5000<=5000, 3000<=5000) but their SUM 8000 > outstanding 5000 → must
        // reject once aggregated by target_id (else outstanding goes to -3000).
        let allocs = vec![("s1".to_string(), "sale".to_string(), 5000i64),
                          ("s1".to_string(), "sale".to_string(), 3000i64)];
        assert!(check_invoice_over_allocation_aggregated(&conn, &allocs).is_err(),
            "summed 8000 > outstanding 5000 must reject");
        // Two lines to the same target summing to exactly outstanding → ok.
        let ok = vec![("s1".to_string(), "sale".to_string(), 2000i64),
                      ("s1".to_string(), "sale".to_string(), 3000i64)];
        assert!(check_invoice_over_allocation_aggregated(&conn, &ok).is_ok());
    }

    #[test]
    fn payment_over_allocation_rejects_sum_exceeding_payment() {
        // Two lines summing to 9000 against a payment of only 8000 → reject.
        assert!(check_payment_over_allocation(8000, &[3000, 6000]).is_err());
        // Sum <= payment (remainder is unallocated credit) → ok.
        assert!(check_payment_over_allocation(8000, &[3000, 5000]).is_ok());
        assert!(check_payment_over_allocation(8000, &[3000]).is_ok());
    }

    #[test]
    fn party_ownership_rejects_other_party_and_wrong_direction() {
        let conn = crate::db::open_in_memory_with_schema().unwrap();
        seed_credit_sale(&conn, "s1", "cust1", 5000);
        // Correct: PaymentReceived (inflow) → target is a sale owned by cust1.
        assert!(check_allocation_party_ownership(&conn, "cust1", "in", "s1", "sale").is_ok());
        // Wrong party: sale belongs to cust1, payment claims cust2.
        assert!(check_allocation_party_ownership(&conn, "cust2", "in", "s1", "sale").is_err());
        // Wrong direction: an outflow (PaymentMade) cannot target a sale.
        assert!(check_allocation_party_ownership(&conn, "cust1", "out", "s1", "sale").is_err());
    }
```

- [ ] **Step 2: Run to fail**

Run: `cargo test -p accounting-core guards::tests::invoice_over guards::tests::payment_over guards::tests::party_ownership`
Expected: FAIL — functions not found.

- [ ] **Step 3: Implement the three allocation guards**

Add to `crates/accounting-core/src/commands/guards.rs` (above test module):

```rust
/// Resolve an invoice's current outstanding and owning party. `target_type` is
/// "sale" or "purchase". Returns (outstanding_minor, party_id).
fn invoice_row(conn: &Connection, target_id: &str, target_type: &str)
    -> Result<Option<(i64, String)>, CommandError> {
    let (table, party_col) = match target_type {
        "sale" => ("sales", "customer_id"),
        "purchase" => ("purchases", "supplier_id"),
        other => return Err(reject(format!("invalid target_type: {other}"))),
    };
    let sql = format!("SELECT outstanding_minor, {party_col} FROM {table} WHERE id = ?1");
    Ok(conn.query_row(&sql, [target_id], |r| Ok((r.get(0)?, r.get(1)?))).optional()?)
}

/// Invoice over-allocation guard, single-line form (spec §4.5): amount must not
/// exceed the target's current outstanding_minor. Callers with multiple allocation
/// lines MUST use the aggregated form below — two lines to the same target can each
/// pass this yet jointly overdraw the invoice.
pub(crate) fn check_invoice_over_allocation(
    conn: &Connection, target_id: &str, target_type: &str, amount: i64,
) -> Result<(), CommandError> {
    match invoice_row(conn, target_id, target_type)? {
        None => Err(reject(format!("unknown {target_type}: {target_id}"))),
        Some((outstanding, _)) if amount > outstanding =>
            Err(reject(format!("over-allocation: {target_type} {target_id} outstanding {outstanding}, cannot allocate {amount}"))),
        Some(_) => Ok(()),
    }
}

/// Invoice over-allocation guard, aggregated form (spec §4.5). Sums allocation
/// amounts per (target_id, target_type) across the WHOLE command and checks each
/// target's total against its outstanding_minor — closing the two-lines-one-invoice
/// hole. Allocation-bearing handlers call THIS, not the single-line form.
pub(crate) fn check_invoice_over_allocation_aggregated(
    conn: &Connection, allocs: &[(String, String, i64)], // (target_id, target_type, amount)
) -> Result<(), CommandError> {
    use std::collections::HashMap;
    let mut per_target: HashMap<(String, String), i64> = HashMap::new();
    for (target_id, target_type, amount) in allocs {
        *per_target.entry((target_id.clone(), target_type.clone())).or_insert(0) += *amount;
    }
    for ((target_id, target_type), total) in per_target {
        check_invoice_over_allocation(conn, &target_id, &target_type, total)?;
    }
    Ok(())
}

/// Payment-overallocation guard (spec §4.5): allocations must not sum to more than
/// the payment amount. Remainder is unallocated credit (allowed).
pub(crate) fn check_payment_over_allocation(payment_amount: i64, alloc_amounts: &[i64])
    -> Result<(), CommandError> {
    let sum: i64 = alloc_amounts.iter().sum();
    if sum > payment_amount {
        Err(reject(format!("payment over-allocation: allocations sum {sum} exceed payment {payment_amount}")))
    } else { Ok(()) }
}

/// Allocation party-ownership guard (spec §4.5): the target invoice must belong to
/// `party_id` and match direction — inflow ("in", PaymentReceived) → sale; outflow
/// ("out", PaymentMade) → purchase.
pub(crate) fn check_allocation_party_ownership(
    conn: &Connection, party_id: &str, direction: &str, target_id: &str, target_type: &str,
) -> Result<(), CommandError> {
    // direction/target_type must agree
    let expected_type = match direction { "in" => "sale", "out" => "purchase", d =>
        return Err(reject(format!("invalid direction: {d}"))) };
    if target_type != expected_type {
        return Err(reject(format!("direction '{direction}' cannot target a {target_type}")));
    }
    match invoice_row(conn, target_id, target_type)? {
        None => Err(reject(format!("unknown {target_type}: {target_id}"))),
        Some((_, owner)) if owner != party_id =>
            Err(reject(format!("{target_type} {target_id} belongs to {owner}, not paying party {party_id}"))),
        Some(_) => Ok(()),
    }
}
```

- [ ] **Step 4: Run to pass**

Run: `cargo test -p accounting-core guards::tests`
Expected: PASS (all guard tests so far).

- [ ] **Step 5: Commit**

```bash
git add crates/accounting-core/src/commands/guards.rs
git commit -m "feat: invoice over-allocation, payment-overallocation, party-ownership guards"
```

---

### Task 11: Credit-overdraw guard (`guards.rs`)

**Files:**
- Modify: `crates/accounting-core/src/commands/guards.rs`

**Credit-overdraw guard** (spec §4.5): `PaymentAllocated` must reject if the sum of its allocations exceeds the party's currently available unallocated credit — `unallocated_cr_minor` for a customer (inflow direction), `unallocated_dr_minor` for a supplier (outflow direction). Prevents allocating credit the party does not hold. (This is the mirror of payment-overallocation: that bounds against the incoming payment; this bounds against *held* credit.)

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/accounting-core/src/commands/guards.rs`:

```rust
    #[test]
    fn credit_overdraw_rejects_more_than_held() {
        let conn = crate::db::open_in_memory_with_schema().unwrap();
        // Customer holds 3000 of unallocated credit (a prepayment).
        conn.execute(
            "INSERT INTO party_balances (party_id,receivable_minor,payable_minor,unallocated_cr_minor,unallocated_dr_minor)
             VALUES ('cust1',0,0,3000,0)", []).unwrap();
        // Applying 4000 of held credit → reject; 3000 → ok; 2000 → ok.
        assert!(check_credit_overdraw(&conn, "cust1", "in", 4000).is_err());
        assert!(check_credit_overdraw(&conn, "cust1", "in", 3000).is_ok());
        assert!(check_credit_overdraw(&conn, "cust1", "in", 2000).is_ok());
    }

    #[test]
    fn credit_overdraw_supplier_uses_dr_column() {
        let conn = crate::db::open_in_memory_with_schema().unwrap();
        conn.execute(
            "INSERT INTO party_balances (party_id,receivable_minor,payable_minor,unallocated_cr_minor,unallocated_dr_minor)
             VALUES ('sup1',0,0,0,1500)", []).unwrap();
        assert!(check_credit_overdraw(&conn, "sup1", "out", 2000).is_err());
        assert!(check_credit_overdraw(&conn, "sup1", "out", 1500).is_ok());
    }
```

- [ ] **Step 2: Run to fail**

Run: `cargo test -p accounting-core guards::tests::credit_overdraw`
Expected: FAIL — `check_credit_overdraw` not found.

- [ ] **Step 3: Implement**

Add to `crates/accounting-core/src/commands/guards.rs` (above test module):

```rust
/// Credit-overdraw guard (spec §4.5): a PaymentAllocated may not apply more credit
/// than the party holds. Direction "in" (customer) draws unallocated_cr_minor;
/// "out" (supplier) draws unallocated_dr_minor. Missing party_balances row = 0 held.
pub(crate) fn check_credit_overdraw(
    conn: &Connection, party_id: &str, direction: &str, total_alloc: i64,
) -> Result<(), CommandError> {
    let col = match direction {
        "in" => "unallocated_cr_minor",
        "out" => "unallocated_dr_minor",
        d => return Err(reject(format!("invalid direction: {d}"))),
    };
    let sql = format!("SELECT {col} FROM party_balances WHERE party_id = ?1");
    let held: i64 = conn.query_row(&sql, [party_id], |r| r.get(0)).optional()?.unwrap_or(0);
    if total_alloc > held {
        Err(reject(format!("credit overdraw: party {party_id} holds {held}, cannot allocate {total_alloc}")))
    } else { Ok(()) }
}
```

- [ ] **Step 4: Run to pass**

Run: `cargo test -p accounting-core guards::tests::credit_overdraw`
Expected: PASS (both tests).

- [ ] **Step 5: Commit**

```bash
git add crates/accounting-core/src/commands/guards.rs
git commit -m "feat: credit-overdraw guard for PaymentAllocated"
```

---

### Task 12: Payment handlers (`payment.rs`) — `PaymentMade`, `PaymentReceived`, `PaymentAllocated`

**Files:**
- Create: `crates/accounting-core/src/commands/payment.rs`
- Modify: `crates/accounting-core/src/lib.rs`

The three **allocation-bearing** commands (spec §4.4/§4.5). Each wires together the Task 10–11 guards:

- **`PaymentMade`** (outflow, supplier): amount `>0`; supplier exists; for each allocation → invoice over-allocation + party-ownership (`out`/`purchase`); across all → payment-overallocation. Posts Dr A/P / Cr Bank; remainder → `unallocated_dr_minor`.
- **`PaymentReceived`** (inflow, customer): symmetric; targets are `sale`s; `in`/`sale`. Posts Dr Bank / Cr A/R; remainder → `unallocated_cr_minor`.
- **`PaymentAllocated`** (no journal posting — moves held credit): amount `>0` per line; each allocation → invoice over-allocation + party-ownership; across all → **credit-overdraw** (not payment-overallocation, since no new money enters). Writes only `payment_allocations` rows + adjusts derived balances.

All three freeze the `direction` and the allocation set into the payload; the projector applies settlement per spec §4.5. Note `PaymentAllocated` carries `paymentId` referencing the *source* payment whose credit it draws (spec §6.5 dual reference) — the handler validates that payment exists and belongs to the party.

- [ ] **Step 1: Write the failing tests**

Create `crates/accounting-core/src/commands/payment.rs`:

```rust
use crate::commands::guards::{check_allocation_party_ownership, check_amount_positive,
    check_credit_overdraw, check_invoice_over_allocation_aggregated, check_payment_over_allocation};
use crate::commands::{commit_event, reject, CommandContext, CommandError};
use rusqlite::OptionalExtension;
use serde_json::json;

/// One allocation line: which invoice, its type, and how much.
pub struct AllocInput { pub target_id: String, pub target_type: String, pub amount_minor: i64 }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::tests::fixture;
    use crate::commands::purchase::{handle_purchase_recorded, PurchaseLineInput};
    use crate::commands::sale::{handle_sale_recorded, SaleLineInput};

    fn ctx<'a>(c: &'a mut rusqlite::Connection, h: &'a mut crate::hlc::Hlc) -> CommandContext<'a> {
        CommandContext { conn: c, hlc: h, physical_now: 1000, device_id: "deviceA".into(), user_id: "owner-1".into() }
    }
    // Seed a customer + a credit sale of 5000 outstanding.
    fn seed_credit_sale(conn: &mut rusqlite::Connection, hlc: &mut crate::hlc::Hlc) {
        let mut c = ctx(conn, hlc);
        crate::commands::setup::handle_party_created(&mut c, "cust1", "Cust", "customer").unwrap();
        crate::commands::setup::handle_party_created(&mut c, "cust2", "Cust2", "customer").unwrap();
        crate::commands::setup::handle_party_created(&mut c, "sup1", "Sup", "supplier").unwrap();
        crate::commands::setup::handle_item_defined(&mut c, "itemA", "A", "A", "ea").unwrap();
        handle_purchase_recorded(&mut c, "pur1", "sup1", "2026-01-01", "cash",
            vec![PurchaseLineInput{ item_id:"itemA".into(), qty:10, unit_cost_minor:100 }]).unwrap();
        handle_sale_recorded(&mut c, "sale1", "cust1", "2026-02-01", "credit",
            vec![SaleLineInput{ item_id:"itemA".into(), qty:5, unit_price_minor:1000, lot_picks: None }]).unwrap();
        // sale1 total 5000, outstanding 5000.
    }

    #[test]
    fn payment_received_partial_allocation_leaves_credit() {
        let (mut conn, mut hlc) = fixture();
        seed_credit_sale(&mut conn, &mut hlc);
        // Receive 6000, allocate 5000 to sale1 → 1000 becomes unallocated credit.
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_payment_received(&mut c, "pay1", "cust1", 6000, "2026-03-01",
                vec![AllocInput{ target_id:"sale1".into(), target_type:"sale".into(), amount_minor:5000 }]).expect("ok");
        }
        let out: i64 = conn.query_row("SELECT outstanding_minor FROM sales WHERE id='sale1'", [], |r| r.get(0)).unwrap();
        assert_eq!(out, 0, "sale fully settled");
        let cr: i64 = conn.query_row("SELECT unallocated_cr_minor FROM party_balances WHERE party_id='cust1'", [], |r| r.get(0)).unwrap();
        assert_eq!(cr, 1000, "remainder held as credit");
    }

    #[test]
    fn payment_received_over_invoice_rejected_and_writes_nothing() {
        let (mut conn, mut hlc) = fixture();
        seed_credit_sale(&mut conn, &mut hlc);
        let err = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_payment_received(&mut c, "pay1", "cust1", 9000, "2026-03-01",
                vec![AllocInput{ target_id:"sale1".into(), target_type:"sale".into(), amount_minor:6000 }]).unwrap_err()
        };
        assert!(matches!(err, CommandError::Validation(_)));
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM events WHERE type='PaymentReceived'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn payment_over_allocation_sum_rejected() {
        let (mut conn, mut hlc) = fixture();
        seed_credit_sale(&mut conn, &mut hlc);
        // Payment of 4000, single allocation of 5000 → invoice guard would allow (<=5000 outstanding)
        // but payment-overallocation (5000 > 4000) rejects.
        let mut c = ctx(&mut conn, &mut hlc);
        let err = handle_payment_received(&mut c, "pay1", "cust1", 4000, "2026-03-01",
            vec![AllocInput{ target_id:"sale1".into(), target_type:"sale".into(), amount_minor:5000 }]).unwrap_err();
        assert!(matches!(err, CommandError::Validation(_)));
    }

    #[test]
    fn payment_received_wrong_party_invoice_rejected() {
        let (mut conn, mut hlc) = fixture();
        seed_credit_sale(&mut conn, &mut hlc);
        // cust2 pays but sale1 belongs to cust1 → party-ownership reject.
        let mut c = ctx(&mut conn, &mut hlc);
        let err = handle_payment_received(&mut c, "pay1", "cust2", 5000, "2026-03-01",
            vec![AllocInput{ target_id:"sale1".into(), target_type:"sale".into(), amount_minor:5000 }]).unwrap_err();
        assert!(matches!(err, CommandError::Validation(_)));
    }

    #[test]
    fn payment_two_lines_same_invoice_exceeding_outstanding_rejected() {
        let (mut conn, mut hlc) = fixture();
        seed_credit_sale(&mut conn, &mut hlc); // sale1 outstanding 5000
        // Payment of 8000 split into two lines BOTH targeting sale1: 5000 + 3000. Each
        // per-line check passes (both <= 5000), payment-overallocation passes (8000<=8000),
        // but aggregated per-target 8000 > outstanding 5000 → reject, nothing written.
        let err = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_payment_received(&mut c, "pay1", "cust1", 8000, "2026-03-01", vec![
                AllocInput{ target_id:"sale1".into(), target_type:"sale".into(), amount_minor:5000 },
                AllocInput{ target_id:"sale1".into(), target_type:"sale".into(), amount_minor:3000 },
            ]).unwrap_err()
        };
        assert!(matches!(err, CommandError::Validation(_)));
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM events WHERE type='PaymentReceived'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
        let out: i64 = conn.query_row("SELECT outstanding_minor FROM sales WHERE id='sale1'", [], |r| r.get(0)).unwrap();
        assert_eq!(out, 5000, "invoice untouched");
    }

    #[test]
    fn payment_allocated_credit_overdraw_rejected() {
        let (mut conn, mut hlc) = fixture();
        seed_credit_sale(&mut conn, &mut hlc);
        // cust1 makes a pure prepayment of 1000 (no allocations) → holds 1000 credit.
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_payment_received(&mut c, "prepay", "cust1", 1000, "2026-02-15", vec![]).expect("prepay ok");
        }
        // Later, allocate 2000 of held credit → overdraw reject (only 1000 held).
        let err = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_payment_allocated(&mut c, "alloc1", "prepay", "cust1", "2026-03-01",
                vec![AllocInput{ target_id:"sale1".into(), target_type:"sale".into(), amount_minor:2000 }]).unwrap_err()
        };
        assert!(matches!(err, CommandError::Validation(_)));
        // Allocating exactly 1000 → ok, no journal posting.
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_payment_allocated(&mut c, "alloc1", "prepay", "cust1", "2026-03-01",
                vec![AllocInput{ target_id:"sale1".into(), target_type:"sale".into(), amount_minor:1000 }]).expect("ok");
        }
        let cr: i64 = conn.query_row("SELECT unallocated_cr_minor FROM party_balances WHERE party_id='cust1'", [], |r| r.get(0)).unwrap();
        assert_eq!(cr, 0, "held credit fully applied");
    }
}
```

- [ ] **Step 2: Run to fail**

Run: `cargo test -p accounting-core commands::payment::tests`
Expected: FAIL — handlers not found.

- [ ] **Step 3: Implement the three handlers**

Above the test module in `crates/accounting-core/src/commands/payment.rs`:

```rust
fn ensure_party_kind(ctx: &CommandContext, party_id: &str, allowed: &[&str]) -> Result<(), CommandError> {
    let kind: Option<String> = ctx.conn.query_row(
        "SELECT kind FROM parties WHERE id = ?1", [party_id], |r| r.get(0)).optional()?;
    match kind {
        None => Err(reject(format!("unknown party: {party_id}"))),
        Some(k) if k != "both" && !allowed.contains(&k.as_str()) =>
            Err(reject(format!("party {party_id} kind '{k}' not allowed here"))),
        Some(_) => Ok(()),
    }
}

/// Shared allocation validation for a payment-time event (PaymentMade/Received):
/// per-line party-ownership + amount, then invoice over-allocation AGGREGATED per
/// target_id (two lines to one invoice can't jointly overdraw it), plus
/// payment-overallocation across the whole set. `direction`: "in" | "out".
fn validate_payment_allocations(
    ctx: &CommandContext, party_id: &str, direction: &str,
    payment_amount: i64, allocs: &[AllocInput],
) -> Result<(), CommandError> {
    let mut amounts = Vec::with_capacity(allocs.len());
    let mut agg = Vec::with_capacity(allocs.len());
    for a in allocs {
        check_amount_positive(a.amount_minor)?;
        check_allocation_party_ownership(ctx.conn, party_id, direction, &a.target_id, &a.target_type)?;
        amounts.push(a.amount_minor);
        agg.push((a.target_id.clone(), a.target_type.clone(), a.amount_minor));
    }
    // Aggregate per target BEFORE checking outstanding — closes the two-lines-one-invoice hole.
    check_invoice_over_allocation_aggregated(ctx.conn, &agg)?;
    check_payment_over_allocation(payment_amount, &amounts)?;
    Ok(())
}

fn alloc_json(allocs: &[AllocInput]) -> Vec<serde_json::Value> {
    allocs.iter().map(|a| json!({
        "targetId": a.target_id, "targetType": a.target_type, "amountMinor": a.amount_minor,
    })).collect()
}

/// `PaymentMade` (spec §4.4): outflow to a supplier. Dr A/P / Cr Bank.
pub fn handle_payment_made(
    ctx: &mut CommandContext, payment_id: &str, supplier_id: &str,
    amount_minor: i64, date: &str, allocations: Vec<AllocInput>,
) -> Result<crate::events::LedgerEvent, CommandError> {
    check_amount_positive(amount_minor)?;
    ensure_party_kind(ctx, supplier_id, &["supplier"])?;
    validate_payment_allocations(ctx, supplier_id, "out", amount_minor, &allocations)?;
    let payload = json!({
        "paymentId": payment_id, "supplierId": supplier_id, "direction": "out",
        "amountMinor": amount_minor, "date": date, "allocations": alloc_json(&allocations),
    });
    commit_event(ctx, "PaymentMade", payload)
}

/// `PaymentReceived` (spec §4.4): inflow from a customer. Dr Bank / Cr A/R.
pub fn handle_payment_received(
    ctx: &mut CommandContext, payment_id: &str, customer_id: &str,
    amount_minor: i64, date: &str, allocations: Vec<AllocInput>,
) -> Result<crate::events::LedgerEvent, CommandError> {
    check_amount_positive(amount_minor)?;
    ensure_party_kind(ctx, customer_id, &["customer"])?;
    validate_payment_allocations(ctx, customer_id, "in", amount_minor, &allocations)?;
    let payload = json!({
        "paymentId": payment_id, "customerId": customer_id, "direction": "in",
        "amountMinor": amount_minor, "date": date, "allocations": alloc_json(&allocations),
    });
    commit_event(ctx, "PaymentReceived", payload)
}

/// `PaymentAllocated` (spec §4.4/§4.5): applies an existing unallocated credit to
/// invoices. No journal posting. Bounds allocations against HELD credit
/// (credit-overdraw), not an incoming amount. `source_payment_id` is the payment
/// whose credit is drawn (spec §6.5 dual reference).
pub fn handle_payment_allocated(
    ctx: &mut CommandContext, alloc_event_id: &str, source_payment_id: &str,
    party_id: &str, date: &str, allocations: Vec<AllocInput>,
) -> Result<crate::events::LedgerEvent, CommandError> {
    if allocations.is_empty() { return Err(reject("PaymentAllocated must have >= 1 allocation")); }
    // Determine party direction from the source payment (in→customer credit, out→supplier).
    let direction: String = ctx.conn.query_row(
        "SELECT direction FROM payments WHERE id = ?1", [source_payment_id], |r| r.get(0))
        .optional()?
        .ok_or_else(|| reject(format!("unknown source payment: {source_payment_id}")))?;
    // Verify the source payment belongs to this party.
    {
        let owner: Option<String> = ctx.conn.query_row(
            "SELECT party_id FROM payments WHERE id = ?1", [source_payment_id], |r| r.get(0)).optional()?;
        if owner.as_deref() != Some(party_id) {
            return Err(reject(format!("payment {source_payment_id} does not belong to party {party_id}")));
        }
    }
    let mut total = 0i64;
    let mut agg = Vec::with_capacity(allocations.len());
    for a in &allocations {
        check_amount_positive(a.amount_minor)?;
        check_allocation_party_ownership(ctx.conn, party_id, &direction, &a.target_id, &a.target_type)?;
        total += a.amount_minor;
        agg.push((a.target_id.clone(), a.target_type.clone(), a.amount_minor));
    }
    // Aggregate per target so two lines to one invoice can't jointly overdraw it.
    check_invoice_over_allocation_aggregated(ctx.conn, &agg)?;
    // Credit-overdraw: cannot apply more than the party holds (bounds the total).
    check_credit_overdraw(ctx.conn, party_id, &direction, total)?;
    let payload = json!({
        "allocationEventId": alloc_event_id, "paymentId": source_payment_id,
        "partyId": party_id, "date": date, "allocations": alloc_json(&allocations),
    });
    commit_event(ctx, "PaymentAllocated", payload)
}
```

> The credit-overdraw guard reads `party_balances.unallocated_*`, which the projector maintained when the prepayment was recorded. Because guards run against the current projection and the commit is atomic, a sequence of `PaymentAllocated` events each see the credit left by the previous one — no double-spend of held credit.

- [ ] **Step 4: Run to pass**

Run: `cargo test -p accounting-core commands::payment::tests`
Expected: PASS (all six tests).

- [ ] **Step 5: Re-export**

In `crates/accounting-core/src/lib.rs`:
```rust
pub use commands::payment::{handle_payment_allocated, handle_payment_made, handle_payment_received, AllocInput};
```

- [ ] **Step 6: Commit**

```bash
git add crates/accounting-core/src/commands/payment.rs crates/accounting-core/src/lib.rs
git commit -m "feat: payment handlers wiring allocation, overallocation, overdraw guards"
```

---

### Task 13: Movement handlers (`movement.rs`) + self-transfer, expense-account-type & credit-expense party guards

**Files:**
- Modify: `crates/accounting-core/src/commands/guards.rs`
- Create: `crates/accounting-core/src/commands/movement.rs`
- Modify: `crates/accounting-core/src/lib.rs`

Four single-purpose transactional commands, each carrying a spec §4.5 guard:
- **`ExpenseRecorded`**: amount `>0`; **expense-account-type guard** — `accountId` must resolve to an `expense`-type account (else the P&L is silently wrong); **credit-expense party guard** (spec §4.5, new) — a `credit`-terms expense MUST carry a `supplierId` (the projector increases that supplier's `payable_minor`), and a `cash` expense must NOT carry one. Posts Dr Expense / Cr Bank-or-A/P.
- **`TransferRecorded`**: amount `>0`; **self-transfer guard** — reject `fromAccountId == toAccountId`; both accounts exist. Posts Dr toAccount / Cr fromAccount.
- **`InventoryAdjusted`**: write-down only — `qtyDelta` strictly **negative** (found stock uses `InventoryFound`); the magnitude is bounded by the **oversell guard** against `qty_remaining`; **lot/item-match** per line. Posts Dr expenseAccountId / Cr Inventory.
- **`InventoryFound`**: **lot-creating** — assigns a deterministic `lotId` per line (frozen), `qty>0`, `unitCostMinor>=0`. Posts Dr Inventory / Cr incomeAccountId. Creates a NEW lot (never inflates an existing one — spec §4.5).

The two new guards (self-transfer, expense-account-type) go in `guards.rs`; the movement handlers reuse oversell / lot-item-match / value guards already built.

- [ ] **Step 1: Write the failing guard tests**

Add to the `tests` module in `crates/accounting-core/src/commands/guards.rs`:

```rust
    #[test]
    fn self_transfer_rejected() {
        assert!(check_self_transfer("a1", "a1").is_err());
        assert!(check_self_transfer("a1", "a2").is_ok());
    }

    #[test]
    fn expense_account_type_guard() {
        let conn = crate::db::open_in_memory_with_schema().unwrap();
        // Insert an expense account and a non-expense (asset) account via doc JSONB.
        conn.execute("INSERT INTO accounts (id, doc, balance_minor) VALUES ('rent', jsonb('{\"name\":\"Rent\",\"type\":\"expense\",\"normal\":\"debit\"}'), 0)", []).unwrap();
        conn.execute("INSERT INTO accounts (id, doc, balance_minor) VALUES ('bank', jsonb('{\"name\":\"Bank\",\"type\":\"asset\",\"normal\":\"debit\"}'), 0)", []).unwrap();
        assert!(check_expense_account_type(&conn, "rent").is_ok());
        assert!(check_expense_account_type(&conn, "bank").is_err(), "asset account cannot be an expense");
        assert!(check_expense_account_type(&conn, "ghost").is_err(), "unknown account rejected");
    }
```

- [ ] **Step 2: Run to fail**

Run: `cargo test -p accounting-core guards::tests::self_transfer guards::tests::expense_account`
Expected: FAIL — functions not found.

- [ ] **Step 3: Implement the two guards**

Add to `crates/accounting-core/src/commands/guards.rs`:

```rust
/// Self-transfer guard (spec §4.5): a transfer's two accounts must differ.
pub(crate) fn check_self_transfer(from: &str, to: &str) -> Result<(), CommandError> {
    if from == to { Err(reject(format!("self-transfer: from == to ({from})"))) } else { Ok(()) }
}

/// Expense-account-type guard (spec §4.5): the account must be of type 'expense'.
pub(crate) fn check_expense_account_type(conn: &Connection, account_id: &str) -> Result<(), CommandError> {
    let acct_type: Option<String> = conn.query_row(
        "SELECT type FROM accounts WHERE id = ?1", [account_id], |r| r.get(0)).optional()?;
    match acct_type.as_deref() {
        None => Err(reject(format!("unknown account: {account_id}"))),
        Some("expense") => Ok(()),
        Some(t) => Err(reject(format!("account {account_id} is type '{t}', expected 'expense'"))),
    }
}
```

- [ ] **Step 4: Write the failing handler tests**

Create `crates/accounting-core/src/commands/movement.rs`:

```rust
use crate::commands::guards::{check_amount_non_negative, check_amount_positive,
    check_at_least_one_line, check_expense_account_type, check_lot_item_match,
    check_qty_positive, check_self_transfer, LotDemand};
use crate::commands::{commit_event, reject, CommandContext, CommandError};
use rusqlite::OptionalExtension;
use serde_json::json;

pub struct AdjustLineInput { pub item_id: String, pub lot_id: String, pub qty_delta: i64, pub reason_code: String, pub expense_account_id: String }
pub struct FoundLineInput { pub item_id: String, pub qty: i64, pub unit_cost_minor: i64, pub acquired_at: String, pub income_account_id: String }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::tests::fixture;
    use crate::commands::purchase::{handle_purchase_recorded, PurchaseLineInput};

    fn ctx<'a>(c: &'a mut rusqlite::Connection, h: &'a mut crate::hlc::Hlc) -> CommandContext<'a> {
        CommandContext { conn: c, hlc: h, physical_now: 1000, device_id: "deviceA".into(), user_id: "owner-1".into() }
    }
    fn seed_accounts_items(conn: &mut rusqlite::Connection, hlc: &mut crate::hlc::Hlc) {
        let mut c = ctx(conn, hlc);
        crate::commands::setup::handle_account_opened(&mut c, "bank", "Bank", "asset", "debit", Some("bank")).unwrap();
        crate::commands::setup::handle_account_opened(&mut c, "rent", "Rent", "expense", "debit", Some("rent")).unwrap();
        crate::commands::setup::handle_account_opened(&mut c, "shrink", "Shrinkage", "expense", "debit", Some("shrinkage")).unwrap();
        crate::commands::setup::handle_account_opened(&mut c, "invgain", "Gain", "income", "credit", Some("inventory_gain")).unwrap();
        crate::commands::setup::handle_party_created(&mut c, "sup1", "Sup", "supplier").unwrap();
        crate::commands::setup::handle_item_defined(&mut c, "itemA", "A", "A", "ea").unwrap();
        handle_purchase_recorded(&mut c, "pur1", "sup1", "2026-01-01", "cash",
            vec![PurchaseLineInput{ item_id:"itemA".into(), qty:10, unit_cost_minor:100 }]).unwrap();
    }

    #[test]
    fn expense_rejects_non_expense_account_and_zero_amount() {
        let (mut conn, mut hlc) = fixture();
        seed_accounts_items(&mut conn, &mut hlc);
        let mut c = ctx(&mut conn, &mut hlc);
        // (ctx, id, account, amount, date, terms, supplier_id, memo)
        assert!(handle_expense_recorded(&mut c, "e1", "bank", 500, "2026-02-01", "cash", None, None).is_err(), "asset acct");
        assert!(handle_expense_recorded(&mut c, "e2", "rent", 0, "2026-02-01", "cash", None, None).is_err(), "zero amount");
        assert!(handle_expense_recorded(&mut c, "e3", "rent", 500, "2026-02-01", "cash", None, None).is_ok());
    }

    #[test]
    fn expense_credit_requires_supplier_and_cash_forbids_one() {
        let (mut conn, mut hlc) = fixture();
        seed_accounts_items(&mut conn, &mut hlc);
        let mut c = ctx(&mut conn, &mut hlc);
        // Credit expense with NO supplier → reject (spec §4.5 credit-expense party guard).
        assert!(handle_expense_recorded(&mut c, "e1", "rent", 500, "2026-02-01", "credit", None, None).is_err());
        // Credit expense naming an unknown supplier → reject.
        assert!(handle_expense_recorded(&mut c, "e2", "rent", 500, "2026-02-01", "credit", Some("ghost"), None).is_err());
        // Credit expense with a valid supplier → ok.
        assert!(handle_expense_recorded(&mut c, "e3", "rent", 500, "2026-02-01", "credit", Some("sup1"), None).is_ok());
        // Cash expense that (wrongly) carries a supplier → reject.
        assert!(handle_expense_recorded(&mut c, "e4", "rent", 500, "2026-02-01", "cash", Some("sup1"), None).is_err());
    }

    #[test]
    fn transfer_rejects_self_and_zero() {
        let (mut conn, mut hlc) = fixture();
        seed_accounts_items(&mut conn, &mut hlc);
        let mut c = ctx(&mut conn, &mut hlc);
        assert!(handle_transfer_recorded(&mut c, "t1", "bank", "bank", 100, "2026-02-01", None).is_err(), "self");
        assert!(handle_transfer_recorded(&mut c, "t2", "bank", "rent", 0, "2026-02-01", None).is_err(), "zero");
        assert!(handle_transfer_recorded(&mut c, "t3", "bank", "rent", 100, "2026-02-01", None).is_ok());
    }

    #[test]
    fn inventory_adjusted_requires_negative_delta_and_bounds() {
        let (mut conn, mut hlc) = fixture();
        seed_accounts_items(&mut conn, &mut hlc);
        // Positive delta rejected (write-down only).
        {
            let mut c = ctx(&mut conn, &mut hlc);
            assert!(handle_inventory_adjusted(&mut c, "adj1", "2026-02-01",
                vec![AdjustLineInput{ item_id:"itemA".into(), lot_id:"pur1#lot0".into(), qty_delta:3, reason_code:"x".into(), expense_account_id:"shrink".into() }]).is_err());
        }
        // Over-shrink (|delta| 15 > remaining 10) rejected.
        {
            let mut c = ctx(&mut conn, &mut hlc);
            assert!(handle_inventory_adjusted(&mut c, "adj2", "2026-02-01",
                vec![AdjustLineInput{ item_id:"itemA".into(), lot_id:"pur1#lot0".into(), qty_delta:-15, reason_code:"x".into(), expense_account_id:"shrink".into() }]).is_err());
        }
        // Valid write-down of 4.
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_inventory_adjusted(&mut c, "adj3", "2026-02-01",
                vec![AdjustLineInput{ item_id:"itemA".into(), lot_id:"pur1#lot0".into(), qty_delta:-4, reason_code:"damage".into(), expense_account_id:"shrink".into() }]).expect("ok");
        }
        let rem: i64 = conn.query_row("SELECT qty_remaining FROM inventory_lots WHERE id='pur1#lot0'", [], |r| r.get(0)).unwrap();
        assert_eq!(rem, 6);
    }

    #[test]
    fn inventory_found_creates_new_lot_with_frozen_id() {
        let (mut conn, mut hlc) = fixture();
        seed_accounts_items(&mut conn, &mut hlc);
        let ev = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_inventory_found(&mut c, "found1", "2026-02-01",
                vec![FoundLineInput{ item_id:"itemA".into(), qty:5, unit_cost_minor:120, acquired_at:"2026-02-01".into(), income_account_id:"invgain".into() }]).expect("ok")
        };
        assert_eq!(ev.payload["lines"][0]["lotId"], "found1#lot0");
        let (item, rem): (String, i64) = conn.query_row(
            "SELECT item_id, qty_remaining FROM inventory_lots WHERE id='found1#lot0'", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(item, "itemA");
        assert_eq!(rem, 5);
    }
}
```

- [ ] **Step 5: Run to fail**

Run: `cargo test -p accounting-core commands::movement::tests`
Expected: FAIL — handlers not found.

- [ ] **Step 6: Implement the movement handlers**

Above the test module in `crates/accounting-core/src/commands/movement.rs`:

```rust
fn ensure_account(ctx: &CommandContext, account_id: &str) -> Result<(), CommandError> {
    let found: Option<String> = ctx.conn.query_row(
        "SELECT id FROM accounts WHERE id = ?1", [account_id], |r| r.get(0)).optional()?;
    if found.is_none() { Err(reject(format!("unknown account: {account_id}"))) } else { Ok(()) }
}

/// `ExpenseRecorded` (spec §4.4/§4.5). Dr Expense / Cr Bank-or-A/P. A credit expense
/// MUST name a supplier (projector bumps their payable_minor); a cash expense must not.
pub fn handle_expense_recorded(
    ctx: &mut CommandContext, expense_id: &str, account_id: &str, amount_minor: i64,
    date: &str, terms: &str, supplier_id: Option<&str>, memo: Option<&str>,
) -> Result<crate::events::LedgerEvent, CommandError> {
    check_amount_positive(amount_minor)?;
    if !matches!(terms, "cash"|"credit") { return Err(reject(format!("invalid terms: {terms}"))); }
    check_expense_account_type(ctx.conn, account_id)?; // must be an expense account
    // Credit-expense party guard (spec §4.5): credit ⇒ a valid supplier; cash ⇒ none.
    match (terms, supplier_id) {
        ("credit", None) =>
            return Err(reject("credit expense requires a supplierId")),
        ("credit", Some(sup)) => {
            let found: Option<String> = ctx.conn.query_row(
                "SELECT kind FROM parties WHERE id = ?1", [sup], |r| r.get(0)).optional()?;
            match found {
                None => return Err(reject(format!("unknown supplier: {sup}"))),
                Some(k) if k != "supplier" && k != "both" =>
                    return Err(reject(format!("party {sup} is not a supplier"))),
                _ => {}
            }
        }
        ("cash", Some(_)) =>
            return Err(reject("cash expense must not carry a supplierId")),
        _ => {}
    }
    let mut p = json!({ "expenseId": expense_id, "accountId": account_id,
        "amountMinor": amount_minor, "date": date, "terms": terms });
    if let Some(s) = supplier_id { p["supplierId"] = json!(s); }
    if let Some(m) = memo { p["memo"] = json!(m); }
    commit_event(ctx, "ExpenseRecorded", p)
}

/// `TransferRecorded` (spec §4.4). Dr toAccount / Cr fromAccount.
pub fn handle_transfer_recorded(
    ctx: &mut CommandContext, transfer_id: &str, from_account_id: &str, to_account_id: &str,
    amount_minor: i64, date: &str, memo: Option<&str>,
) -> Result<crate::events::LedgerEvent, CommandError> {
    check_amount_positive(amount_minor)?;
    check_self_transfer(from_account_id, to_account_id)?;
    ensure_account(ctx, from_account_id)?;
    ensure_account(ctx, to_account_id)?;
    let mut p = json!({ "transferId": transfer_id, "fromAccountId": from_account_id,
        "toAccountId": to_account_id, "amountMinor": amount_minor, "date": date });
    if let Some(m) = memo { p["memo"] = json!(m); }
    commit_event(ctx, "TransferRecorded", p)
}

/// `InventoryAdjusted` (spec §4.4/§4.5). Write-down only: qtyDelta strictly
/// negative; cumulative |qtyDelta| per lot bounded by LotDemand against
/// qty_remaining; lot/item-match per line.
pub fn handle_inventory_adjusted(
    ctx: &mut CommandContext, adjustment_id: &str, date: &str, lines: Vec<AdjustLineInput>,
) -> Result<crate::events::LedgerEvent, CommandError> {
    check_at_least_one_line(&lines)?;
    let mut demand = LotDemand::new();
    for l in &lines {
        if l.qty_delta >= 0 {
            return Err(reject(format!("InventoryAdjusted qtyDelta must be < 0 (use InventoryFound for gains), got {}", l.qty_delta)));
        }
        check_expense_account_type(ctx.conn, &l.expense_account_id)?;
        check_lot_item_match(ctx.conn, &l.lot_id, &l.item_id)?;
        demand.take(ctx.conn, &l.lot_id, -l.qty_delta)?; // cumulative magnitude vs qty_remaining
    }
    let json_lines: Vec<_> = lines.iter().map(|l| json!({
        "itemId": l.item_id, "lotId": l.lot_id, "qtyDelta": l.qty_delta,
        "reasonCode": l.reason_code, "expenseAccountId": l.expense_account_id,
    })).collect();
    commit_event(ctx, "InventoryAdjusted",
        json!({ "adjustmentId": adjustment_id, "date": date, "lines": json_lines }))
}

/// `InventoryFound` (spec §4.4/§4.5). Lot-creating: assigns deterministic lotId
/// per line (frozen), creates a NEW lot. Dr Inventory / Cr incomeAccountId.
pub fn handle_inventory_found(
    ctx: &mut CommandContext, found_id: &str, date: &str, lines: Vec<FoundLineInput>,
) -> Result<crate::events::LedgerEvent, CommandError> {
    check_at_least_one_line(&lines)?;
    for l in &lines {
        check_qty_positive(l.qty)?;
        check_amount_non_negative(l.unit_cost_minor)?;
        ensure_account(ctx, &l.income_account_id)?;
        let found: Option<String> = ctx.conn.query_row(
            "SELECT id FROM items WHERE id = ?1", [&l.item_id], |r| r.get(0)).optional()?;
        if found.is_none() { return Err(reject(format!("unknown item: {}", l.item_id))); }
    }
    let json_lines: Vec<_> = lines.iter().enumerate().map(|(i, l)| json!({
        "itemId": l.item_id, "lotId": format!("{found_id}#lot{i}"), "qty": l.qty,
        "unitCostMinor": l.unit_cost_minor, "acquiredAt": l.acquired_at,
        "incomeAccountId": l.income_account_id,
    })).collect();
    commit_event(ctx, "InventoryFound",
        json!({ "foundId": found_id, "date": date, "lines": json_lines }))
}
```

- [ ] **Step 7: Run to pass**

Run: `cargo test -p accounting-core commands::movement::tests guards::tests::self_transfer guards::tests::expense_account`
Expected: PASS.

- [ ] **Step 8: Re-export**

In `crates/accounting-core/src/lib.rs`:
```rust
pub use commands::movement::{handle_expense_recorded, handle_inventory_adjusted,
    handle_inventory_found, handle_transfer_recorded, AdjustLineInput, FoundLineInput};
```

- [ ] **Step 9: Commit**

```bash
git add crates/accounting-core/src/commands/movement.rs crates/accounting-core/src/commands/guards.rs crates/accounting-core/src/lib.rs
git commit -m "feat: movement handlers + self-transfer and expense-account-type guards"
```

---

### Task 14: Reversal guards (`guards.rs`) — legal-target, double-void, lot-source void, downstream

**Files:**
- Modify: `crates/accounting-core/src/commands/guards.rs`

`TransactionReversed` (spec §4.5) is guarded by five related predicates. All read the event log + projections:

- **Legal-target guard:** the target event must be **transactional-category** (Task 2's `is_transactional`). Reject master-data events, `OpeningBalancesRecorded`, and `TransactionReversed` itself.
- **Double-void guard:** reject if the target has already been reversed (track reversed target ids from prior `TransactionReversed` events' payloads).
- **Lot-source void guard:** reject reversing **any lot-creating** target if any lot it created has been consumed (`qty_remaining < qty_received`). The lots can't be un-created while later events drew from them.
- **Reversal downstream guard:** reject if the target `T` has any *later, not-yet-reversed* blocking dependency (spec §4.5 dependency relation, now FIVE edges):
  1. a `payment_allocations` row whose `target_id` is `T`'s sale/purchase (allocation against an invoice `T` created);
  2. a `returns` row whose `original_id` is `T` (a return against `T`);
  3. a `PaymentAllocated` whose `payment_id` is `T`'s payment, when `T` is a `PaymentMade`/`PaymentReceived` (drew on unallocated credit `T` created) — the non-obvious reverse-reference edge;
  4. edge 4 (consumed a lot `T` created) is covered by the lot-source void guard;
  5. **(spec §4.5 edge 5, new)** when `T` is **lot-restoring** (`SaleReturnRecorded`), reject if any lot it restored has since fallen below the level it restored to — i.e. later consumption drew on the restored units. Reversing `T` re-decrements `qty_remaining` by the restored amount (contract clause 2), so if those units were already re-consumed the lot would go negative. Equivalent check: reject if, for any lot `T` restored, `qty_remaining < qty_restored_by_T`. This is the mirror of edge 4 for the restoring direction. `qty_restored_by_T` is read from `T`'s own `return_lines` (`qty` per `lot_id`).

We need to resolve, from `T`'s payload, the projection ids it created/restored (saleId/purchaseId/paymentId/lotIds, and for edge 5 the return's `returnId` → its `return_lines`). The guards take those resolved ids.

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `crates/accounting-core/src/commands/guards.rs`:

```rust
    fn insert_event(conn: &Connection, id: &str, hlc: &str, etype: &str, payload_json: &str) {
        conn.execute(
            "INSERT INTO events (id,hlc,device_id,user_id,seq,type,payload,created_at)
             VALUES (?1,?2,'d','u',1,?3, jsonb(?4), 0)",
            rusqlite::params![id, hlc, etype, payload_json]).unwrap();
    }

    #[test]
    fn legal_target_and_double_void() {
        let conn = crate::db::open_in_memory_with_schema().unwrap();
        insert_event(&conn, "e_item", "h1", "ItemDefined", r#"{"itemId":"i1"}"#);
        insert_event(&conn, "e_sale", "h2", "SaleRecorded", r#"{"saleId":"s1"}"#);
        // Master-data target is illegal.
        assert!(check_reversal_legal_target(&conn, "e_item").is_err());
        // Transactional target is legal…
        assert!(check_reversal_legal_target(&conn, "e_sale").is_ok());
        // …until it's already been reversed.
        insert_event(&conn, "e_rev", "h3", "TransactionReversed", r#"{"targetEventId":"e_sale"}"#);
        assert!(check_not_already_reversed(&conn, "e_sale").is_err());
        assert!(check_not_already_reversed(&conn, "e_item").is_ok());
        // Unknown target rejected.
        assert!(check_reversal_legal_target(&conn, "ghost").is_err());
        // Reversing a TransactionReversed is illegal.
        assert!(check_reversal_legal_target(&conn, "e_rev").is_err());
    }

    #[test]
    fn lot_source_void_rejects_consumed_lot() {
        let conn = crate::db::open_in_memory_with_schema().unwrap();
        // A purchase-created lot, partially consumed (received 10, remaining 6).
        conn.execute("INSERT INTO inventory_lots (id,item_id,source_event_id,purchase_id,unit_cost_minor,qty_received,qty_remaining,acquired_at,supplier_id) VALUES ('lot1','itemA','e_pur',NULL,100,10,6,'2026-01-01',NULL)", []).unwrap();
        // Reversing the source event must reject (lot consumed).
        assert!(check_lot_source_void(&conn, "e_pur").is_err());
        // A fully-intact lot (remaining == received) can be voided.
        conn.execute("INSERT INTO inventory_lots (id,item_id,source_event_id,purchase_id,unit_cost_minor,qty_received,qty_remaining,acquired_at,supplier_id) VALUES ('lot2','itemA','e_pur2',NULL,100,10,10,'2026-01-01',NULL)", []).unwrap();
        assert!(check_lot_source_void(&conn, "e_pur2").is_ok());
    }

    #[test]
    fn downstream_guard_blocks_allocation_return_and_credit_draw() {
        let conn = crate::db::open_in_memory_with_schema().unwrap();
        // Edge 1: an allocation against a sale T created.
        assert!(check_reversal_downstream(&conn, "sale", "s1", None).is_ok());
        conn.execute("INSERT INTO payment_allocations (id,event_id,payment_id,target_id,target_type,amount_minor) VALUES ('pa1','ep','pay1','s1','sale',100)", []).unwrap();
        assert!(check_reversal_downstream(&conn, "sale", "s1", None).is_err(), "allocation blocks");

        // Edge 2: a return against T.
        conn.execute("INSERT INTO returns (id,event_id,return_type,original_id,date,revenue_reversed_minor,cost_restored_minor) VALUES ('r1','er','sale_return','s2','2026-02-01',100,50)", []).unwrap();
        assert!(check_reversal_downstream(&conn, "sale", "s2", None).is_err(), "return blocks");

        // Edge 3: a PaymentAllocated drawing on the credit T (a payment) created.
        insert_event(&conn, "e_alloc", "h9", "PaymentAllocated", r#"{"paymentId":"pay1"}"#);
        assert!(check_reversal_downstream(&conn, "payment", "invoiceX", Some("pay1")).is_err(), "credit draw blocks");
        // A payment with no downstream PaymentAllocated is fine.
        assert!(check_reversal_downstream(&conn, "payment", "invoiceY", Some("payNONE")).is_ok());
    }

    #[test]
    fn downstream_edge5_blocks_reversing_return_whose_units_were_reconsumed() {
        let conn = crate::db::open_in_memory_with_schema().unwrap();
        // A SaleReturnRecorded 'ret1' restored 6 units to lot1 (return_lines row).
        conn.execute("INSERT INTO returns (id,event_id,return_type,original_id,date,revenue_reversed_minor,cost_restored_minor) VALUES ('ret1','e_ret','sale_return','sOrig','2026-03-01',6000,3000)", []).unwrap();
        conn.execute("INSERT INTO return_lines (id,return_id,item_id,qty,unit_price_minor,unit_cost_minor,lot_id) VALUES ('rl1','ret1','itemA',6,1000,500,'lot1')", []).unwrap();
        // Case A: lot still has >= 6 remaining (units NOT re-consumed) → reversible.
        conn.execute("INSERT INTO inventory_lots (id,item_id,source_event_id,purchase_id,unit_cost_minor,qty_received,qty_remaining,acquired_at,supplier_id) VALUES ('lot1','itemA','e_pur',NULL,500,10,10,'2026-01-01',NULL)", []).unwrap();
        assert!(check_reversal_lot_restore_reconsumed(&conn, "ret1").is_ok(), "10 >= 6 restored → ok");
        // Case B: a later sale drew the restored units back down to 4 (< 6 restored) →
        // reversing the return would re-decrement 6 and drive the lot to -2 → reject.
        conn.execute("UPDATE inventory_lots SET qty_remaining = 4 WHERE id='lot1'", []).unwrap();
        assert!(check_reversal_lot_restore_reconsumed(&conn, "ret1").is_err(),
            "4 < 6 restored: restored units re-consumed → block reversal");
    }
```

- [ ] **Step 2: Run to fail**

Run: `cargo test -p accounting-core guards::tests::legal_target guards::tests::lot_source guards::tests::downstream`
Expected: FAIL — functions not found (including `check_reversal_lot_restore_reconsumed`).

- [ ] **Step 3: Implement the reversal guards**

Add to `crates/accounting-core/src/commands/guards.rs`. Add `use crate::commands::categories::is_transactional;` to the imports.

```rust
/// Legal-target guard (spec §4.5): the target must be a transactional-category
/// event. Rejects master-data, OpeningBalancesRecorded, TransactionReversed, and
/// unknown ids.
pub(crate) fn check_reversal_legal_target(conn: &Connection, target_event_id: &str)
    -> Result<(), CommandError> {
    let etype: Option<String> = conn.query_row(
        "SELECT type FROM events WHERE id = ?1", [target_event_id], |r| r.get(0)).optional()?;
    match etype {
        None => Err(reject(format!("unknown target event: {target_event_id}"))),
        Some(t) if !is_transactional(&t) =>
            Err(reject(format!("event type '{t}' is not a legal reversal target"))),
        Some(_) => Ok(()),
    }
}

/// Double-void guard (spec §4.5): reject if a prior TransactionReversed already
/// targeted this event. Reads targetEventId from each reversal's payload.
pub(crate) fn check_not_already_reversed(conn: &Connection, target_event_id: &str)
    -> Result<(), CommandError> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM events
         WHERE type = 'TransactionReversed' AND payload ->> 'targetEventId' = ?1",
        [target_event_id], |r| r.get(0))?;
    if n > 0 { Err(reject(format!("event {target_event_id} already reversed"))) } else { Ok(()) }
}

/// Lot-source void guard (spec §4.5): reject reversing a lot-creating event if any
/// lot it created has been consumed (qty_remaining < qty_received).
pub(crate) fn check_lot_source_void(conn: &Connection, source_event_id: &str)
    -> Result<(), CommandError> {
    let consumed: i64 = conn.query_row(
        "SELECT COUNT(*) FROM inventory_lots
         WHERE source_event_id = ?1 AND qty_remaining < qty_received",
        [source_event_id], |r| r.get(0))?;
    if consumed > 0 {
        Err(reject(format!("cannot void event {source_event_id}: it created lots already consumed; use a return or adjustment")))
    } else { Ok(()) }
}

/// Reversal downstream guard (spec §4.5): reject if T has a blocking downstream
/// dependency. `invoice_id` is T's sale/purchase id (edges 1, 2); `payment_id` is
/// Some(T's payment id) when T is a PaymentMade/PaymentReceived (edge 3). Edge 4 is
/// the lot-source void guard.
pub(crate) fn check_reversal_downstream(
    conn: &Connection, invoice_type: &str, invoice_id: &str, payment_id: Option<&str>,
) -> Result<(), CommandError> {
    // Edge 1: any allocation against an invoice T created.
    let allocs: i64 = conn.query_row(
        "SELECT COUNT(*) FROM payment_allocations WHERE target_id = ?1 AND target_type = ?2",
        rusqlite::params![invoice_id, invoice_type], |r| r.get(0))?;
    if allocs > 0 {
        return Err(reject(format!("cannot reverse: {invoice_type} {invoice_id} has payment allocations; reverse them first")));
    }
    // Edge 2: any return against T.
    let returns: i64 = conn.query_row(
        "SELECT COUNT(*) FROM returns WHERE original_id = ?1", [invoice_id], |r| r.get(0))?;
    if returns > 0 {
        return Err(reject(format!("cannot reverse: {invoice_id} has returns against it; reverse them first")));
    }
    // Edge 3: a PaymentAllocated drew on the credit this payment created.
    if let Some(pid) = payment_id {
        let draws: i64 = conn.query_row(
            "SELECT COUNT(*) FROM events
             WHERE type = 'PaymentAllocated' AND payload ->> 'paymentId' = ?1",
            [pid], |r| r.get(0))?;
        if draws > 0 {
            return Err(reject(format!("cannot reverse payment {pid}: a later PaymentAllocated drew its credit; reverse that first")));
        }
    }
    // Edge 4 is the lot-source void guard (lot-creating targets).
    // Edge 5 is check_reversal_lot_restore_reconsumed (lot-restoring targets), below.
    Ok(())
}

/// Reversal downstream guard, EDGE 5 (spec §4.5): reject reversing a lot-restoring
/// target (a `SaleReturnRecorded`, identified by its `return_id`) if any lot it
/// restored has since fallen below the level it restored to — later consumption drew
/// on the restored units. Reversing re-decrements `qty_remaining` by the restored
/// amount (contract clause 2), so `qty_remaining < qty_restored_by_T` means the
/// re-decrement would drive the lot negative. Reads restored quantities from the
/// return's own `return_lines`.
pub(crate) fn check_reversal_lot_restore_reconsumed(conn: &Connection, return_id: &str)
    -> Result<(), CommandError> {
    let mut stmt = conn.prepare(
        "SELECT rl.lot_id, SUM(rl.qty) AS restored, il.qty_remaining
         FROM return_lines rl
         JOIN inventory_lots il ON il.id = rl.lot_id
         WHERE rl.return_id = ?1
         GROUP BY rl.lot_id, il.qty_remaining",
    )?;
    let rows = stmt.query_map([return_id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?))
    })?;
    for row in rows {
        let (lot_id, restored, remaining) = row?;
        if remaining < restored {
            return Err(reject(format!(
                "cannot reverse return {return_id}: lot {lot_id} has {remaining} remaining but the return restored {restored}; restored units were re-consumed — reverse the later consumption first")));
        }
    }
    Ok(())
}
```

> Edge 3 is the spec's called-out non-obvious case: a pure prepayment writes zero `payment_allocations` rows on itself, so a later `PaymentAllocated` that drew its credit is discoverable ONLY by the reverse reference `PaymentAllocated.payment_id → T` — which is exactly the query above. Edge 5 is its mirror for the lot-restoring direction: a return writes no self-referencing marker when its restored units are later re-consumed, so the only safe check is `qty_remaining < qty_restored_by_T` against the return's own `return_lines`. All downstream checks ignore already-reversed dependents implicitly because a reversed dependent's own settlement/return effect is still recorded until *it* is reversed; the guard forces reversing dependents first (no implicit cascade), per spec.

- [ ] **Step 4: Run to pass**

Run: `cargo test -p accounting-core guards::tests`
Expected: PASS (all guard tests).

- [ ] **Step 5: Commit**

```bash
git add crates/accounting-core/src/commands/guards.rs
git commit -m "feat: reversal legal-target, double-void, lot-source void, downstream (5 edges) guards"
```

---

### Task 15: `TransactionReversed` handler (`reversal.rs`) — guard orchestration + frozen reversal journal lines

**Files:**
- Create: `crates/accounting-core/src/commands/reversal.rs`
- Modify: `crates/accounting-core/src/lib.rs`

The handler ties the Task 14 guards together and **freezes the reversal journal lines** into the payload (spec §4.5 FOUR-part contract, clause 1 — computed at command time, like COGS). The projector (Plan 2) applies the remaining clauses using these frozen lines plus the target's own payload: clause 2 = inventory inverse, clause 3 = allocation/settlement unwind, **clause 4 = set `reversed = 1` on the `sales`/`purchases` row (the void marker) — owned entirely by the projector**. The handler's job is guards + freezing the financial negation (clause 1).

**Resolving the target's created ids for the downstream/lot guards:** the handler loads the target event's row (type + payload), then:
- if the target created lots (lot-creating) → run `check_lot_source_void(target_event_id)`;
- if the target created an invoice (`SaleRecorded`→`saleId`/sale, `PurchaseRecorded`→`purchaseId`/purchase) → run `check_reversal_downstream` with that invoice id;
- if the target is a payment (`PaymentMade`/`PaymentReceived`) → run `check_reversal_downstream` with `payment_id = Some(target.paymentId)`;
- if the target is **lot-restoring** (`SaleReturnRecorded`) → run `check_reversal_lot_restore_reconsumed(returnId)` (edge 5) so a return whose restored units were re-consumed cannot be reversed into a negative lot;
- for `PurchaseReturnRecorded` the target's own `returns` row is not a dependency *of itself*, so no downstream invoice check; it remains reversible directly.

**Freezing the reversal journal lines:** read the target's `journal_lines` (by `event_id = target_event_id`) and negate each — swap debit/credit. For a `PaymentAllocated` target this yields zero lines (it posted none), matching the contract's "no-op financial" clause.

- [ ] **Step 1: Write the failing tests**

Create `crates/accounting-core/src/commands/reversal.rs`:

```rust
use crate::commands::guards::{check_lot_source_void, check_not_already_reversed,
    check_reversal_downstream, check_reversal_legal_target, check_reversal_lot_restore_reconsumed};
use crate::commands::{commit_event, reject, CommandContext, CommandError};
use rusqlite::OptionalExtension;
use serde_json::json;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::tests::fixture;
    use crate::commands::purchase::{handle_purchase_recorded, PurchaseLineInput};
    use crate::commands::sale::{handle_sale_recorded, handle_sale_return_recorded, SaleLineInput, SaleReturnItemInput};
    use crate::commands::payment::{handle_payment_received, AllocInput};

    fn ctx<'a>(c: &'a mut rusqlite::Connection, h: &'a mut crate::hlc::Hlc) -> CommandContext<'a> {
        CommandContext { conn: c, hlc: h, physical_now: 1000, device_id: "deviceA".into(), user_id: "owner-1".into() }
    }
    fn seed(conn: &mut rusqlite::Connection, hlc: &mut crate::hlc::Hlc) {
        let mut c = ctx(conn, hlc);
        // Accounts the projector posts to by system_role must exist for journal lines
        // (and thus for freeze_reversal_journal_lines' role lookup) to resolve.
        crate::commands::setup::handle_account_opened(&mut c, "bank", "Bank", "asset", "debit", Some("bank")).unwrap();
        crate::commands::setup::handle_account_opened(&mut c, "inv", "Inventory", "asset", "debit", Some("inventory")).unwrap();
        crate::commands::setup::handle_account_opened(&mut c, "ar", "AR", "asset", "debit", Some("accounts_receivable")).unwrap();
        crate::commands::setup::handle_account_opened(&mut c, "ap", "AP", "liability", "credit", Some("accounts_payable")).unwrap();
        crate::commands::setup::handle_account_opened(&mut c, "sales", "Sales", "income", "credit", Some("sales")).unwrap();
        crate::commands::setup::handle_account_opened(&mut c, "cogs", "COGS", "expense", "debit", Some("cogs")).unwrap();
        crate::commands::setup::handle_party_created(&mut c, "sup1", "Sup", "supplier").unwrap();
        crate::commands::setup::handle_party_created(&mut c, "cust1", "Cust", "customer").unwrap();
        crate::commands::setup::handle_item_defined(&mut c, "itemA", "A", "A", "ea").unwrap();
    }

    #[test]
    fn reverse_rejects_master_data_target() {
        let (mut conn, mut hlc) = fixture();
        let item_ev = {
            let mut c = ctx(&mut conn, &mut hlc);
            crate::commands::setup::handle_item_defined(&mut c, "itemA", "A", "A", "ea").unwrap()
        };
        let mut c = ctx(&mut conn, &mut hlc);
        let err = handle_transaction_reversed(&mut c, &item_ev.id, "oops").unwrap_err();
        assert!(matches!(err, CommandError::Validation(_)));
    }

    #[test]
    fn reverse_freezes_negated_journal_lines() {
        let (mut conn, mut hlc) = fixture();
        seed(&mut conn, &mut hlc);
        // A cash purchase posts Dr Inventory / Cr Bank (via projector).
        let pur_ev = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_purchase_recorded(&mut c, "pur1", "sup1", "2026-01-01", "cash",
                vec![PurchaseLineInput{ item_id:"itemA".into(), qty:10, unit_cost_minor:100 }]).unwrap()
        };
        // Reverse it (lot fully intact → allowed).
        let rev = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_transaction_reversed(&mut c, &pur_ev.id, "entered in error").expect("ok")
        };
        // Frozen reversal lines negate the originals: original Dr Inventory becomes Cr, etc.
        let lines = rev.payload["reversalJournalLines"].as_array().unwrap();
        assert!(!lines.is_empty(), "purchase reversal must carry negated journal lines");
        // PINNED SHAPE: each line carries accountId (stable — frozen in AccountOpened),
        // debitMinor, creditMinor — field-for-field what the projector reads.
        assert!(lines.iter().all(|l| l["accountId"].is_string()), "lines carry accountId");
        assert!(lines.iter().all(|l| l.get("accountRole").is_none()), "no accountRole (user accts have NULL system_role)");
        // A cash purchase posts Dr Inventory / Cr Bank; the reversal negates → Cr Inventory / Dr Bank.
        // The seed opened these with ids "inv" and "bank".
        let inv = lines.iter().find(|l| l["accountId"] == "inv").unwrap();
        assert_eq!(inv["creditMinor"], 1000); // 10 * 100, was a debit, now credited
        let bank = lines.iter().find(|l| l["accountId"] == "bank").unwrap();
        assert_eq!(bank["debitMinor"], 1000);
        // Every reversal line swaps debit<->credit of some original line (sum balanced).
        let dr: i64 = lines.iter().map(|l| l["debitMinor"].as_i64().unwrap()).sum();
        let cr: i64 = lines.iter().map(|l| l["creditMinor"].as_i64().unwrap()).sum();
        assert_eq!(dr, cr, "reversal posting must be balanced");
    }

    #[test]
    fn reverse_rejects_consumed_purchase_lot() {
        let (mut conn, mut hlc) = fixture();
        seed(&mut conn, &mut hlc);
        let pur_ev = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_purchase_recorded(&mut c, "pur1", "sup1", "2026-01-01", "cash",
                vec![PurchaseLineInput{ item_id:"itemA".into(), qty:10, unit_cost_minor:100 }]).unwrap()
        };
        // Consume some of the lot via a sale.
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_sale_recorded(&mut c, "sale1", "cust1", "2026-02-01", "cash",
                vec![SaleLineInput{ item_id:"itemA".into(), qty:3, unit_price_minor:200, lot_picks: None }]).unwrap();
        }
        // Now reversing the purchase must fail (lot-source void guard).
        let mut c = ctx(&mut conn, &mut hlc);
        let err = handle_transaction_reversed(&mut c, &pur_ev.id, "x").unwrap_err();
        assert!(matches!(err, CommandError::Validation(_)));
    }

    #[test]
    fn reverse_rejects_double_void() {
        let (mut conn, mut hlc) = fixture();
        seed(&mut conn, &mut hlc);
        let pur_ev = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_purchase_recorded(&mut c, "pur1", "sup1", "2026-01-01", "cash",
                vec![PurchaseLineInput{ item_id:"itemA".into(), qty:10, unit_cost_minor:100 }]).unwrap()
        };
        { let mut c = ctx(&mut conn, &mut hlc);
          handle_transaction_reversed(&mut c, &pur_ev.id, "first").expect("first ok"); }
        let mut c = ctx(&mut conn, &mut hlc);
        let err = handle_transaction_reversed(&mut c, &pur_ev.id, "second").unwrap_err();
        assert!(matches!(err, CommandError::Validation(_)));
    }

    #[test]
    fn reverse_rejects_sale_with_allocation() {
        let (mut conn, mut hlc) = fixture();
        seed(&mut conn, &mut hlc);
        {   // stock, credit sale, then a payment allocation against it
            let mut c = ctx(&mut conn, &mut hlc);
            handle_purchase_recorded(&mut c, "pur1", "sup1", "2026-01-01", "cash",
                vec![PurchaseLineInput{ item_id:"itemA".into(), qty:10, unit_cost_minor:100 }]).unwrap();
        }
        let sale_ev = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_sale_recorded(&mut c, "sale1", "cust1", "2026-02-01", "credit",
                vec![SaleLineInput{ item_id:"itemA".into(), qty:5, unit_price_minor:1000, lot_picks: None }]).unwrap()
        };
        {   let mut c = ctx(&mut conn, &mut hlc);
            handle_payment_received(&mut c, "pay1", "cust1", 5000, "2026-03-01",
                vec![AllocInput{ target_id:"sale1".into(), target_type:"sale".into(), amount_minor:5000 }]).unwrap();
        }
        // Reversing the sale is blocked by the downstream allocation.
        let mut c = ctx(&mut conn, &mut hlc);
        let err = handle_transaction_reversed(&mut c, &sale_ev.id, "x").unwrap_err();
        assert!(matches!(err, CommandError::Validation(_)));
    }

    #[test]
    fn reverse_sale_blocked_by_real_downstream_return() {
        // MINOR 4: drive a REAL SaleReturnRecorded, then assert reversing the sale is
        // blocked by downstream edge 2 (a returns row whose original_id is the sale).
        let (mut conn, mut hlc) = fixture();
        seed(&mut conn, &mut hlc);
        {   let mut c = ctx(&mut conn, &mut hlc);
            handle_purchase_recorded(&mut c, "pur1", "sup1", "2026-01-01", "cash",
                vec![PurchaseLineInput{ item_id:"itemA".into(), qty:10, unit_cost_minor:100 }]).unwrap();
        }
        let sale_ev = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_sale_recorded(&mut c, "sale1", "cust1", "2026-02-01", "credit",
                vec![SaleLineInput{ item_id:"itemA".into(), qty:5, unit_price_minor:1000, lot_picks: None }]).unwrap()
        };
        {   let mut c = ctx(&mut conn, &mut hlc);
            handle_sale_return_recorded(&mut c, "sret1", "sale1", "2026-03-01",
                vec![SaleReturnItemInput{ item_id:"itemA".into(), lot_returns: vec![("pur1#lot0".into(), 2)] }]).unwrap();
        }
        // The return against sale1 blocks reversing sale1 (edge 2).
        let mut c = ctx(&mut conn, &mut hlc);
        let err = handle_transaction_reversed(&mut c, &sale_ev.id, "x").unwrap_err();
        assert!(matches!(err, CommandError::Validation(_)));
    }

    #[test]
    fn reverse_return_blocked_when_restored_units_reconsumed() {
        // MODERATE 1 / edge 5: return restores units, a later sale re-consumes them,
        // reversing the return would drive the lot negative → blocked.
        let (mut conn, mut hlc) = fixture();
        seed(&mut conn, &mut hlc);
        {   let mut c = ctx(&mut conn, &mut hlc);
            handle_purchase_recorded(&mut c, "pur1", "sup1", "2026-01-01", "cash",
                vec![PurchaseLineInput{ item_id:"itemA".into(), qty:10, unit_cost_minor:100 }]).unwrap();
            // Sale of 6 (lot → 4 remaining).
            handle_sale_recorded(&mut c, "sale1", "cust1", "2026-02-01", "cash",
                vec![SaleLineInput{ item_id:"itemA".into(), qty:6, unit_price_minor:200, lot_picks: None }]).unwrap();
        }
        // Return 6 (lot → 10 remaining).
        let ret_ev = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_sale_return_recorded(&mut c, "sret1", "sale1", "2026-03-01",
                vec![SaleReturnItemInput{ item_id:"itemA".into(), lot_returns: vec![("pur1#lot0".into(), 6)] }]).unwrap()
        };
        // A later sale re-consumes 8 (lot → 2 remaining, below the 6 restored).
        {   let mut c = ctx(&mut conn, &mut hlc);
            handle_sale_recorded(&mut c, "sale2", "cust1", "2026-04-01", "cash",
                vec![SaleLineInput{ item_id:"itemA".into(), qty:8, unit_price_minor:200, lot_picks: None }]).unwrap();
        }
        // Reversing the return now would re-decrement 6 from a lot holding 2 → block (edge 5).
        let mut c = ctx(&mut conn, &mut hlc);
        let err = handle_transaction_reversed(&mut c, &ret_ev.id, "x").unwrap_err();
        assert!(matches!(err, CommandError::Validation(_)));
    }
}
```

- [ ] **Step 2: Run to fail**

Run: `cargo test -p accounting-core commands::reversal::tests`
Expected: FAIL — `handle_transaction_reversed` not found.

- [ ] **Step 3: Implement the handler**

Above the test module in `crates/accounting-core/src/commands/reversal.rs`:

```rust
use crate::commands::categories::{categories_of, EventCategory};

/// Load a target event's (type, payload) from the log.
fn load_target(conn: &rusqlite::Connection, target_event_id: &str)
    -> Result<(String, serde_json::Value), CommandError> {
    let row: Option<(String, String)> = conn.query_row(
        "SELECT type, json(payload) FROM events WHERE id = ?1",
        [target_event_id], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
    let (etype, payload_text) = row.ok_or_else(|| reject(format!("unknown target event: {target_event_id}")))?;
    let payload: serde_json::Value = serde_json::from_str(&payload_text)
        .map_err(|e| reject(format!("corrupt target payload: {e}")))?;
    Ok((etype, payload))
}

/// Compute the frozen reversal journal lines: negate every journal line the target
/// posted (swap debit/credit) and carry each line's `account_id` verbatim. Account ids
/// ARE stable across a rebuild — they are frozen in the `AccountOpened` event payload
/// (genesis derives `acct_{system_role}`; user-created accounts carry a caller-supplied
/// id), so replay reproduces them exactly. We deliberately do NOT resolve to
/// `system_role`: a reversal may touch a USER-created account whose `system_role` is
/// NULL (spec §5.2) and thus unnameable by role. The projector posts to `accountId`
/// directly — no role resolution for reversal lines. Empty for PaymentAllocated (it
/// posts no journal lines). Shape matches spec §4.4:
/// `reversalJournalLines[]: {accountId, debitMinor, creditMinor}`.
fn freeze_reversal_journal_lines(conn: &rusqlite::Connection, target_event_id: &str)
    -> Result<Vec<serde_json::Value>, CommandError> {
    let mut stmt = conn.prepare(
        "SELECT account_id, debit_minor, credit_minor
         FROM journal_lines WHERE event_id = ?1 ORDER BY id")?;
    let rows = stmt.query_map([target_event_id], |r| {
        Ok((r.get::<_,String>(0)?, r.get::<_,i64>(1)?, r.get::<_,i64>(2)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (account_id, debit, credit) = row?;
        // Negate: original debit becomes credit and vice versa. Shape matches the
        // projector's reader exactly: {accountId, debitMinor, creditMinor}.
        out.push(json!({
            "accountId": account_id, "debitMinor": credit, "creditMinor": debit,
        }));
    }
    Ok(out)
}

/// `TransactionReversed` (spec §4.5). Runs the legal-target, double-void,
/// lot-source void, and downstream (5-edge) guards, then freezes the negated
/// journal lines (clause 1 of the four-part contract; clauses 2–4 — inventory
/// inverse, settlement unwind, `reversed = 1` void marker — are the projector's).
pub fn handle_transaction_reversed(
    ctx: &mut CommandContext, target_event_id: &str, reason: &str,
) -> Result<crate::events::LedgerEvent, CommandError> {
    // ---- GUARDS ----
    check_reversal_legal_target(ctx.conn, target_event_id)?;
    check_not_already_reversed(ctx.conn, target_event_id)?;

    let (etype, tpayload) = load_target(ctx.conn, target_event_id)?;
    let cats = categories_of(&etype);

    // Lot-source void: any lot-creating target whose lots were consumed.
    if cats.contains(&EventCategory::LotCreating) {
        check_lot_source_void(ctx.conn, target_event_id)?;
    }
    // Downstream: invoice-creating and payment-creating targets.
    match etype.as_str() {
        "SaleRecorded" => {
            let sale_id = tpayload["saleId"].as_str().unwrap_or_default();
            check_reversal_downstream(ctx.conn, "sale", sale_id, None)?;
        }
        "PurchaseRecorded" => {
            let purchase_id = tpayload["purchaseId"].as_str().unwrap_or_default();
            check_reversal_downstream(ctx.conn, "purchase", purchase_id, None)?;
        }
        "PaymentMade" | "PaymentReceived" => {
            let payment_id = tpayload["paymentId"].as_str().unwrap_or_default();
            // no invoice created by a payment; pass its id for the credit-draw edge (3).
            check_reversal_downstream(ctx.conn, "sale", "", Some(payment_id))?;
        }
        "SaleReturnRecorded" => {
            // Edge 5: reversing re-decrements the restored units; block if they were
            // re-consumed (lot would go negative). Keyed by the return's own id.
            let return_id = tpayload["returnId"].as_str().unwrap_or_default();
            check_reversal_lot_restore_reconsumed(ctx.conn, return_id)?;
        }
        _ => {}
    }

    // ---- FREEZE: negated journal lines (clause 1 of the FOUR-part contract) ----
    let reversal_lines = freeze_reversal_journal_lines(ctx.conn, target_event_id)?;

    let payload = json!({
        "targetEventId": target_event_id, "targetType": etype,
        "reason": reason, "reversalJournalLines": reversal_lines,
    });
    // ---- COMMIT: projector applies clause 1 (these frozen lines) + clause 2 (inventory
    // inverse) + clause 3 (settlement unwind) + clause 4 (set reversed = 1 void marker). ----
    commit_event(ctx, "TransactionReversed", payload)
}
```

> `check_reversal_downstream` with `invoice_id = ""` for a payment target harmlessly finds no allocations/returns for the empty id and only evaluates edge 3 (the payment's credit draw) — the branch we care about. If Plan 2 prefers, the projector can also assert the target's payload `targetType` matches at apply time; the handler already froze it.

- [ ] **Step 4: Run to pass**

Run: `cargo test -p accounting-core commands::reversal::tests`
Expected: PASS (all seven tests).

- [ ] **Step 5: Re-export**

In `crates/accounting-core/src/lib.rs`:
```rust
pub use commands::reversal::handle_transaction_reversed;
```

- [ ] **Step 6: Commit**

```bash
git add crates/accounting-core/src/commands/reversal.rs crates/accounting-core/src/lib.rs
git commit -m "feat: TransactionReversed handler with guard orchestration and frozen reversal lines"
```

---

### Task 16: End-to-end integration + rebuild-determinism test

**Files:**
- Modify: `crates/accounting-core/src/commands/mod.rs` (add an `e2e` test module)

A single integration test that drives a realistic business day through the public handlers, then proves the two properties this plan is responsible for: (a) rejected commands leave the log untouched, and (b) because handlers freeze all referenced ids (`lotId`, COGS, revenue), a full drop-and-`rebuild` (Plan 2) reproduces byte-identical projection state — the deterministic-referenced-ID invariant (spec §4.5). This is the capstone that the freeze-at-command-time decisions actually hold end-to-end.

- [ ] **Step 1: Write the failing integration test**

Add a new test module at the bottom of `crates/accounting-core/src/commands/mod.rs`:

```rust
#[cfg(test)]
mod e2e {
    use super::*;
    use crate::db::open_in_memory_with_schema;
    use crate::hlc::Hlc;
    use crate::commands::setup::*;
    use crate::commands::purchase::*;
    use crate::commands::sale::*;
    use crate::commands::payment::*;
    use crate::projectors::rebuild; // Plan 2

    fn ctx<'a>(c: &'a mut rusqlite::Connection, h: &'a mut Hlc) -> CommandContext<'a> {
        CommandContext { conn: c, hlc: h, physical_now: 1000, device_id: "deviceA".into(), user_id: "owner-1".into() }
    }

    #[test]
    fn business_day_then_rebuild_is_identical() {
        let mut conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("deviceA");
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_account_opened(&mut c, "bank", "Bank", "asset", "debit", Some("bank")).unwrap();
            handle_account_opened(&mut c, "inv", "Inventory", "asset", "debit", Some("inventory")).unwrap();
            handle_account_opened(&mut c, "ar", "AR", "asset", "debit", Some("accounts_receivable")).unwrap();
            handle_account_opened(&mut c, "ap", "AP", "liability", "credit", Some("accounts_payable")).unwrap();
            handle_account_opened(&mut c, "sales", "Sales", "income", "credit", Some("sales")).unwrap();
            handle_account_opened(&mut c, "cogs", "COGS", "expense", "debit", Some("cogs")).unwrap();
            handle_party_created(&mut c, "sup1", "Sup", "supplier").unwrap();
            handle_party_created(&mut c, "cust1", "Cust", "customer").unwrap();
            handle_item_defined(&mut c, "itemA", "A", "A", "ea").unwrap();
            handle_purchase_recorded(&mut c, "pur1", "sup1", "2026-01-01", "credit",
                vec![PurchaseLineInput{ item_id:"itemA".into(), qty:10, unit_cost_minor:500 }]).unwrap();
            handle_sale_recorded(&mut c, "sale1", "cust1", "2026-02-01", "credit",
                vec![SaleLineInput{ item_id:"itemA".into(), qty:4, unit_price_minor:1000, lot_picks: None }]).unwrap();
            handle_payment_received(&mut c, "pay1", "cust1", 4000, "2026-03-01",
                vec![AllocInput{ target_id:"sale1".into(), target_type:"sale".into(), amount_minor:4000 }]).unwrap();
        }

        // Snapshot key projection state.
        let snap = |conn: &rusqlite::Connection| -> (i64, i64, i64, String) {
            let lot_rem: i64 = conn.query_row("SELECT qty_remaining FROM inventory_lots WHERE id='pur1#lot0'", [], |r| r.get(0)).unwrap();
            let cogs: i64 = conn.query_row("SELECT cogs_minor FROM sale_lines WHERE sale_id='sale1'", [], |r| r.get(0)).unwrap();
            let out: i64 = conn.query_row("SELECT outstanding_minor FROM sales WHERE id='sale1'", [], |r| r.get(0)).unwrap();
            let lot_id: String = conn.query_row("SELECT lot_id FROM lot_consumptions LIMIT 1", [], |r| r.get(0)).unwrap();
            (lot_rem, cogs, out, lot_id)
        };
        let before = snap(&conn);
        assert_eq!(before, (6, 2000, 0, "pur1#lot0".to_string()));

        // MODERATE 2: double-entry must balance for EVERY txn_id (reconciliation
        // check #3). A projector posting bug (unbalanced Dr/Cr) surfaces right here at
        // the command→projection seam, not later in Plan 4. Handlers freeze VALUES;
        // the projector derives the Dr/Cr legs via system_role — this asserts it did so
        // in balance for the purchase, the sale (revenue + cost legs), and the payment.
        let unbalanced: i64 = conn.query_row(
            "SELECT COUNT(*) FROM (
               SELECT txn_id FROM journal_lines
               GROUP BY txn_id
               HAVING SUM(debit_minor) <> SUM(credit_minor))",
            [], |r| r.get(0)).unwrap();
        assert_eq!(unbalanced, 0, "every txn_id must have SUM(debit)=SUM(credit)");

        // Drop & rebuild all projections from the event log (Plan 2).
        rebuild(&mut conn).unwrap();
        let after = snap(&conn);
        assert_eq!(before, after, "rebuild must reproduce identical projection state (frozen ids/values)");
        // Balance invariant must still hold after rebuild.
        let unbalanced_after: i64 = conn.query_row(
            "SELECT COUNT(*) FROM (
               SELECT txn_id FROM journal_lines
               GROUP BY txn_id
               HAVING SUM(debit_minor) <> SUM(credit_minor))",
            [], |r| r.get(0)).unwrap();
        assert_eq!(unbalanced_after, 0, "double-entry balance must survive rebuild");
    }

    #[test]
    fn rejected_command_leaves_log_untouched() {
        let mut conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("deviceA");
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_party_created(&mut c, "sup1", "Sup", "supplier").unwrap();
            handle_item_defined(&mut c, "itemA", "A", "A", "ea").unwrap();
            handle_purchase_recorded(&mut c, "pur1", "sup1", "2026-01-01", "cash",
                vec![PurchaseLineInput{ item_id:"itemA".into(), qty:10, unit_cost_minor:500 }]).unwrap();
        }
        let count_before: i64 = conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();
        // Oversell rejection.
        {
            let mut c = ctx(&mut conn, &mut hlc);
            let bad = handle_party_created(&mut c, "sup1", "dup", "supplier"); // duplicate id
            assert!(bad.is_err());
        }
        let count_after: i64 = conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();
        assert_eq!(count_before, count_after, "rejected command must not append");
    }
}
```

- [ ] **Step 2: Run to fail**

Run: `cargo test -p accounting-core commands::e2e`
Expected: FAIL — compile error until the sibling handler modules and `rebuild` are all present (they are, from Tasks 4–15 + Plan 2). If `rebuild`'s path differs in Plan 2, fix the import.

- [ ] **Step 3: Make it pass**

No new production code — this task integrates existing handlers. If the assertions reveal a real gap (e.g. a projector not maintaining `outstanding_minor`), fix in the responsible module and re-run. Adjust the `snap` values only if Plan 2's projector semantics justify it (document why in the test).

- [ ] **Step 4: Run the full crate suite**

Run: `cargo test -p accounting-core`
Expected: PASS (every task's tests across db, hlc, events, genesis, projector, commands).

- [ ] **Step 5: Commit**

```bash
git add crates/accounting-core/src/commands/mod.rs
git commit -m "test: e2e business-day drive-through with rebuild-determinism assertion"
```

---

### Task 17: Write→read payload round-trip integration test (catches emitter/projector drift)

**Files:**
- Modify: `crates/accounting-core/src/commands/mod.rs` (extend the `e2e` module)

The unit tests in Tasks 4–15 assert each handler *emits* a payload and (via `commit_event`) that the projector *accepts* it, but they lean on each plan's own fixtures. The failure mode this task defends against is **field-name drift between the emitted payload and the projector's reader** — e.g. a `SaleReturnRecorded` emitting flat lines while the projector reads nested `lotReturns`, or a `TransactionReversed` freezing `reversalJournalLines` while the projector reads `reversalLines`. Those break *silently*: `commit_event` succeeds (the projector runs, just does nothing for the mis-keyed field), so no error surfaces. This task drives the REAL handler → REAL projector round-trip and asserts the projection *content* is what the event meant — the test that fails loudly on drift. It depends on both the handlers (this plan) and the projector (Plan 2); it is deliberately last because handlers build atop projectors.

- [ ] **Step 1: Write the failing round-trip tests**

Add to the `e2e` module in `crates/accounting-core/src/commands/mod.rs`:

```rust
    use crate::commands::reversal::handle_transaction_reversed;

    #[test]
    fn sale_return_roundtrip_restores_inventory_and_writes_return_lines() {
        // REAL handler → REAL projector. Asserts the nested lotReturns payload the
        // handler emits is exactly what the projector reads: inventory restored,
        // return_lines row written, cost_restored derived (NOT read from a top-level
        // frozen field). Catches the flat-vs-nested drift.
        let mut conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("deviceA");
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_account_opened(&mut c, "bank", "Bank", "asset", "debit", Some("bank")).unwrap();
            handle_account_opened(&mut c, "inv", "Inventory", "asset", "debit", Some("inventory")).unwrap();
            handle_account_opened(&mut c, "ar", "AR", "asset", "debit", Some("accounts_receivable")).unwrap();
            handle_account_opened(&mut c, "sales", "Sales", "income", "credit", Some("sales")).unwrap();
            handle_account_opened(&mut c, "cogs", "COGS", "expense", "debit", Some("cogs")).unwrap();
            handle_party_created(&mut c, "sup1", "Sup", "supplier").unwrap();
            handle_party_created(&mut c, "cust1", "Cust", "customer").unwrap();
            handle_item_defined(&mut c, "itemA", "A", "A", "ea").unwrap();
            handle_purchase_recorded(&mut c, "pur1", "sup1", "2026-01-01", "cash",
                vec![PurchaseLineInput{ item_id:"itemA".into(), qty:10, unit_cost_minor:500 }]).unwrap();
            handle_sale_recorded(&mut c, "sale1", "cust1", "2026-02-01", "cash",
                vec![SaleLineInput{ item_id:"itemA".into(), qty:6, unit_price_minor:1000, lot_picks: None }]).unwrap();
        }
        // Lot at 4 remaining after the sale of 6.
        let before: i64 = conn.query_row("SELECT qty_remaining FROM inventory_lots WHERE id='pur1#lot0'", [], |r| r.get(0)).unwrap();
        assert_eq!(before, 4);
        // Return 3 via the REAL handler (nested lotReturns) → REAL projector.
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_sale_return_recorded(&mut c, "sret1", "sale1", "2026-03-01",
                vec![SaleReturnItemInput{ item_id:"itemA".into(), lot_returns: vec![("pur1#lot0".into(), 3)] }]).unwrap();
        }
        // Projector actually restored inventory (proves it read lotReturns, not a flat line).
        let after: i64 = conn.query_row("SELECT qty_remaining FROM inventory_lots WHERE id='pur1#lot0'", [], |r| r.get(0)).unwrap();
        assert_eq!(after, 7, "return must restore 3 units — silent drift would leave this at 4");
        // return_lines row written, one per lot return.
        let rl_count: i64 = conn.query_row("SELECT COUNT(*) FROM return_lines rl JOIN returns r ON r.id=rl.return_id WHERE r.id='sret1'", [], |r| r.get(0)).unwrap();
        assert_eq!(rl_count, 1);
        // cost_restored DERIVED by the projector from the lines (3 * 500), not a frozen top-level total.
        let cost_restored: i64 = conn.query_row("SELECT cost_restored_minor FROM returns WHERE id='sret1'", [], |r| r.get(0)).unwrap();
        assert_eq!(cost_restored, 1500);
    }

    #[test]
    fn transaction_reversed_roundtrip_flattens_the_journal() {
        // REAL handler → REAL projector. A reversed transaction's GL must net to zero:
        // clause 1 posts the frozen reversalJournalLines (keyed by accountId). If the
        // projector read the wrong key, the reversal would post ZERO lines and the
        // reversed txn's accounts would NOT return to their pre-transaction totals.
        let mut conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("deviceA");
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_account_opened(&mut c, "bank", "Bank", "asset", "debit", Some("bank")).unwrap();
            handle_account_opened(&mut c, "inv", "Inventory", "asset", "debit", Some("inventory")).unwrap();
            handle_account_opened(&mut c, "ap", "AP", "liability", "credit", Some("accounts_payable")).unwrap();
            handle_party_created(&mut c, "sup1", "Sup", "supplier").unwrap();
            handle_item_defined(&mut c, "itemA", "A", "A", "ea").unwrap();
        }
        // Inventory balance before the purchase.
        let inv_role = "inventory";
        let bal = |conn: &rusqlite::Connection, role: &str| -> i64 {
            conn.query_row(
                "SELECT COALESCE(SUM(jl.debit_minor - jl.credit_minor),0)
                 FROM journal_lines jl JOIN accounts a ON a.id = jl.account_id
                 WHERE a.system_role = ?1", [role], |r| r.get(0)).unwrap()
        };
        let inv_before = bal(&conn, inv_role);
        // A cash purchase (Dr Inventory 5000 / Cr Bank 5000).
        let pur_ev = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_purchase_recorded(&mut c, "pur1", "sup1", "2026-01-01", "cash",
                vec![PurchaseLineInput{ item_id:"itemA".into(), qty:10, unit_cost_minor:500 }]).unwrap()
        };
        assert_eq!(bal(&conn, inv_role), inv_before + 5000, "purchase raised Inventory");
        // Reverse it — REAL handler freezes accountId lines, REAL projector posts them.
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_transaction_reversed(&mut c, &pur_ev.id, "entered in error").unwrap();
        }
        // The journal must be flattened back: Inventory returns to its pre-purchase total.
        assert_eq!(bal(&conn, inv_role), inv_before,
            "reversal must net Inventory back to pre-transaction — a mis-keyed reversalJournalLines would leave it at +5000");
        // And the sale/purchase carries the void marker (clause 4, projector-owned).
        let reversed: i64 = conn.query_row("SELECT reversed FROM purchases WHERE id='pur1'", [], |r| r.get(0)).unwrap();
        assert_eq!(reversed, 1, "projector must set reversed = 1");
    }
```

- [ ] **Step 2: Run to fail**

Run: `cargo test -p accounting-core commands::e2e::sale_return_roundtrip commands::e2e::transaction_reversed_roundtrip`
Expected: FAIL initially only if the seam is broken. Because Tasks 9 and 15 already emit the pinned shapes, these should compile once those tasks land; if either assertion fails, the emitted payload and the projector reader have DRIFTED — fix the emitter (this plan) or coordinate the projector (Plan 2) until the field names match exactly. This is the intended tripwire.

- [ ] **Step 3: Make it pass**

No new production code beyond Tasks 9/15's pinned shapes. If a round-trip assertion fails, reconcile the emitted payload field names with the projector's reader field-for-field (`lotReturns`/`qtyReturned`/`unitCostMinor` for sale returns; `reversalJournalLines`/`accountId`/`debitMinor`/`creditMinor` for reversals). Do NOT "fix" the test by loosening the assertion — the content check is the point.

- [ ] **Step 4: Run the full crate suite**

Run: `cargo test -p accounting-core`
Expected: PASS (all tasks, including both round-trip tests).

- [ ] **Step 5: Commit**

```bash
git add crates/accounting-core/src/commands/mod.rs
git commit -m "test: write->read round-trip guards against SaleReturn/TransactionReversed payload drift"
```

---

## Definition of Done (Plan 3)

- `cargo test -p accounting-core` passes with every task's tests green (db, hlc, events, genesis, projector from Plans 1–2, plus all `commands::*` and `guards::*` here).
- **One handler per event type exists** and is re-exported from `lib.rs`: master data (`UserRegistered`, `AccountOpened`, `ItemDefined`, `PartyCreated`, `UserUpdated`, `AccountUpdated`, `ItemUpdated`, `PartyUpdated`), transactional (`PurchaseRecorded`, `SaleRecorded`, `PurchaseReturnRecorded`, `SaleReturnRecorded`, `PaymentMade`, `PaymentReceived`, `PaymentAllocated`, `ExpenseRecorded`, `TransferRecorded`, `InventoryAdjusted`, `InventoryFound`, `TransactionReversed`). (`OpeningBalancesRecorded` is genesis-only, per §4.3, not a runtime command here.)
- **Every spec §4.5 guard is implemented with a test that constructs a violating scenario and asserts the event is NOT written**, and a valid scenario that succeeds:
  - oversell (unified over lot-consuming), **including intra-command cumulative aggregation via `LotDemand`** — Task 5 + used in Tasks 7, 8, 13 (multi-line and two-picks-one-lot tests in Task 7).
  - lot-source void — Task 14 + Task 15.
  - sale-return over-restore, **including the repeated-return bound (`qty_remaining + return_qty > qty_received`)** — Task 9 (single + two-return tests).
  - invoice over-allocation, **aggregated per target_id across the command** — Task 10 + Task 12 (two-lines-one-invoice test).
  - credit-overdraw — Task 11 + Task 12.
  - payment-overallocation — Task 10 + Task 12.
  - allocation party-ownership — Task 10 + Task 12.
  - reversal legal-target + double-void — Task 14 + Task 15.
  - reversal downstream (**all FIVE edges**, incl. the PaymentAllocated-drew-credit reverse-reference edge 3 and the new lot-restoring-reconsumed edge 5) — Task 14 + Task 15 (edge-2 driven by a real `SaleReturnRecorded`, edge 5 by a re-consumed restore).
  - value validation (qty>0, amounts>=0/>0, ≥1 line) — Task 3 + used everywhere.
  - self-transfer — Task 13.
  - expense-account-type — Task 13.
  - credit-expense party (spec §4.5, new: credit expense ⇒ valid supplier; cash ⇒ none) — Task 13.
  - return-against-reversed (spec §4.5, new: reject a return whose target invoice has `reversed = 1`) — Task 9 (guard test + handler test).
  - lot/item-match — Task 5 + used in Tasks 7, 8, 9, 13.
- **Intra-command aggregation is closed on both dimensions:** lot draws are summed per `lot_id` (`LotDemand`, threaded through every lot-consuming handler; auto-selection reads `available()` so line 2 sees line 1's claims) and allocations are summed per `target_id` (`check_invoice_over_allocation_aggregated`), so no pair of lines that each pass in isolation can jointly drive a lot or an invoice negative.
- **Guards are phrased over event CATEGORIES** (`categories.rs`, Task 2): a new event type joins a category and inherits its guards, rather than being enumerated per guard. Handlers dispatch via `categories_of` for the category-level decisions (reversal contract, lot-source void, edge-5 lot-restoring check).
- **Freeze-at-command-time holds:** handlers (not the projector) compute and freeze into the payload: COGS + revenue (`SaleRecorded`), the negated reversal journal lines (`TransactionReversed`), the return unit price keyed by the specific returned lot (`SaleReturnRecorded`), and assign deterministic `lotId`s (`PurchaseRecorded`, `InventoryFound`). Verified by the rebuild-determinism test (Task 16).
- **Emitted payloads match the projector's reader field-for-field (write→read seam):**
  - `SaleReturnRecorded` emits the **pinned NESTED** shape — `lines[]: {itemId, qty, unitPriceMinor, lotReturns[]: {lotId, qtyReturned, unitCostMinor}}` — and does **not** freeze top-level `revenueReversedMinor`/`costRestoredMinor` (the projector derives them from the lines). `PurchaseReturnRecorded` stays FLAT (`{itemId, qty, lotId, unitCostMinor}`) per spec.
  - `TransactionReversed` freezes `reversalJournalLines[]: {accountId, debitMinor, creditMinor}` — carrying the account's `account_id` verbatim (stable across rebuild, frozen in `AccountOpened`; NOT `system_role`, which is NULL for user-created accounts and so unnameable), which the projector posts directly by id (clause 1).
  - Task 17's REAL-handler→REAL-projector round-trip test is the tripwire that fails loudly on any field-name drift (the failure mode where `commit_event` silently succeeds but the projection is wrong): it asserts a sale return actually restores inventory + writes `return_lines` + derives `cost_restored`, and a `TransactionReversed` actually flattens the reversed txn's GL back to its pre-transaction total and sets `reversed = 1`.
- **Balanced posting is deferred to the projector — intentionally (with one exception).** For ordinary transactional events, handlers freeze VALUES (COGS, revenue, prices, assigned ids) and do NOT emit journal `debit_minor`/`credit_minor` legs; the projector derives the balanced Dr/Cr by resolving accounts via `system_role` (spec §4.5 "account resolution"). The lone exception is `TransactionReversed`, whose clause-1 `reversalJournalLines` ARE frozen Dr/Cr legs (the negation of the target's already-posted lines) — these carry `accountId` (not a role) precisely because the target may have posted to a user-created account with a NULL `system_role`; the projector posts them by id verbatim. Both paths are deterministic at replay, and Task 16's per-`txn_id` `SUM(debit)=SUM(credit)` assertion (over the purchase + sale + payment it drives) catches any projector posting bug at the command→projection seam, both before and after `rebuild`.
- **Deterministic referenced ids survive rebuild:** `lotId = {purchaseId|foundId}#lot{index}`, carried in the creating event's payload and read verbatim by the projector — a drop-and-`rebuild` reproduces byte-identical projection state (Task 16).
- **Atomic write boundary proven:** `commit_event` runs `append_event` + `apply_event` in one `conn.transaction()`; a projection failure rolls back the append (Task 1). Every rejected command writes nothing (asserted per guard and in Task 16).
- **Default oldest-lot-first sale selection, user-overridable** (Task 7), with the oversell guard enforced (cumulatively) on both auto-selected and user-supplied lot picks.
- **Transactional business-key uniqueness** (`purchaseId`/`saleId`/`paymentId`/etc.): the projector's PRIMARY KEY on each projection table (`purchases.id`, `sales.id`, …) rejects a duplicate business id at apply time, which — because append+project share one transaction — rolls back the whole command. Handlers therefore rely on the PK backstop for transactional ids rather than a redundant pre-check (unlike master-data handlers, which pre-check `ensure_absent` because a friendly early rejection is cheap there); this is documented so implementers do not mistake the absence of a pre-check for a gap.
- Business dates are `TEXT 'YYYY-MM-DD'` in every payload; money is integer minor units throughout; null-safe `IS`/`IS NOT` used for nullable comparisons; raw SQL only.
- **Carried forward to Plan 4:** the §7 reconciliation checks are written there as an *independent* backstop that must agree with these guards (e.g. check #6 non-negative inventory backstops the oversell guard; check #8 non-negative credits backstops credit-overdraw). Plan 4 also builds the §8 report queries. The projector's FOUR-part `TransactionReversed` contract (clause 2 inventory inverse, clause 3 settlement unwind, clause 4 `reversed = 1` void marker) is Plan 2's responsibility applying the frozen `reversalJournalLines` (clause 1) this plan produces.

### Spec ambiguities resolved in this plan

1. **Deterministic `lotId` derivation.** Spec §4.5 says `lotId` is "a deterministic function of its source event" and gives `${eventId}#${lineIndex}` as the example for *never-referenced* ids — but the event id (HLC stamp) does not exist until `append_event` runs *inside* the transaction, while the handler must freeze `lotId` into the payload *before* that. Resolved by deriving `lotId` from the caller-supplied business key that is itself frozen in the payload: `lotId = {purchaseId}#lot{index}` (and `{foundId}#lot{index}`). This is deterministic across rebuild (the payload is immutable) and available at freeze time. The projector reads it verbatim; it never mints ids. Task 16 verifies rebuild identity.
2. **`PaymentAllocated` direction + source-payment linkage.** The spec's payload lists `paymentId, partyId, allocations[], date` but the credit-overdraw guard needs to know whether to check `unallocated_cr` (customer) or `unallocated_dr` (supplier). Resolved by deriving `direction` from the referenced source `payments` row (`direction` column, set by the original `PaymentMade`/`PaymentReceived`), and validating the source payment belongs to `partyId`. This also cleanly powers downstream-guard edge 3.
3. **Reversal downstream guard for payment targets.** `check_reversal_downstream` is invoice-centric (edges 1–2) but edge 3 is payment-centric. Resolved by calling it with an empty `invoice_id` and `payment_id = Some(...)` for payment targets, so only edge 3 evaluates. A payment creates no invoice, so edges 1–2 are correctly no-ops for it.
4. **`OpeningBalancesRecorded` scope.** Spec §4.4 lists it as a transactional-looking event but §4.3/§4.5 mark it genesis-only, "may only appear once," and explicitly *not* a legal reversal target. Resolved by NOT building a runtime command handler for it in this plan (genesis in Plan 1 emits it if migrating), and by placing it in the `LotCreating` category but **not** `Transactional` in `categories.rs` — so the reversal legal-target guard rejects it automatically.
5. **`SaleReturnRecorded` requires the original sale to have been projected.** The over-restore guard and frozen-price lookup both read the original sale's `sale_lines`/`lot_consumptions`. This assumes the original `SaleRecorded` was already committed (its projection exists) — true for any real sequence, and guaranteed within a single device's log ordering. Cross-device late arrival is a sync-time concern (spec §10, out of scope).
6. **Expense/transfer credit-account resolution deferred to the projector.** Handlers carry `terms` (`ExpenseRecorded`) and the two account ids (`TransferRecorded`) but do not themselves resolve the Bank vs A/P `system_role`; posting the balanced journal lines is the projector's job (Plan 2), consistent with §4.5 "account resolution via system_role" living in the projector. The handler's contract is guards + freezing values that must not be recomputed at replay (COGS, reversal lines, prices) — a credit-account lookup by fixed `system_role` is deterministic at replay, so it correctly stays in the projector.
7. **Intra-command over-draw / over-allocate (review CRITICAL 1).** The spec phrases oversell and invoice over-allocation per-line, but a single command with two lines hitting the same lot/invoice can pass every per-line check yet jointly go negative. Resolved by aggregating within the command: a `LotDemand` accumulator sums per-`lot_id` draws (and feeds oldest-first selection so later lines see earlier claims), and `check_invoice_over_allocation_aggregated` sums per-`target_id` allocations before comparing to `outstanding_minor`. Both are the intended reading of "no event may drive a lot/invoice negative."
8. **Repeated sale returns (review CRITICAL 2).** The static "consumed from this lot" figure (`SUM(lot_consumptions.qty_taken)`) never decreases as returns restore units, so bounding only against it lets the same units be returned twice. Resolved by enforcing the spec's exact second clause directly against LIVE lot state: `qty_remaining + return_qty > qty_received` rejects — this already reflects every prior restore, needs no "already returned" bookkeeping, and keeps reconciliation check #7 true.
9. **Reversal of a lot-restoring target (spec §4.5 edge 5, added this revision).** A `SaleReturnRecorded` is itself reversible, but reversing it re-decrements `qty_remaining` by the amount it restored; if those restored units were meanwhile re-consumed, the lot would go negative. Resolved with `check_reversal_lot_restore_reconsumed(returnId)`, which reads the return's own `return_lines` and rejects if any restored lot now has `qty_remaining < qty_restored_by_T` — the mirror of the lot-source void guard (edge 4) for the restoring direction. Wired into the reversal handler's `SaleReturnRecorded` arm.
10. **Return unit-price lookup keyed by lot, not item (review MINOR 1).** A sale with two same-item lines at different prices made a `WHERE item_id = ? LIMIT 1` price lookup arbitrary. Resolved by keying the frozen price through the specific returned lot (`lot_consumptions → sale_lines`), and rejecting when a lot was genuinely filled at multiple distinct prices (return each price line separately) rather than silently guessing.
11. **Write→read payload shapes pinned (cross-plan review CRITICAL 1 & 2).** The emitter and projector disagreed on two payloads. Resolved by matching the spec's pinned shapes exactly: `SaleReturnRecorded` now emits NESTED per-item lines with `lotReturns[]` (was flat one-lot-per-line) and drops the projector-derived `revenueReversedMinor`/`costRestoredMinor` top-level totals; `TransactionReversed` freezes `reversalJournalLines` (canonical key) with per-line `{accountId, debitMinor, creditMinor}`. The line carries `accountId`, NOT `accountRole`: account ids are stable across rebuild (frozen in `AccountOpened` — genesis derives `acct_{system_role}`, user accounts carry a caller-supplied id), whereas `system_role` is NULL for user-created accounts (spec §5.2) and cannot name them; the projector posts by `accountId` directly. Enforced by Task 17's real round-trip test. `SaleReturnItemInput` replaces the old flat `SaleReturnLineInput`.
12. **`TransactionReversed` is a FOUR-part contract, not three (review MODERATE 6).** Clause 4 sets `reversed = 1` on the `sales`/`purchases` row (void marker; `sale_lines`/`lot_consumptions` stay for audit, and every `sale_lines`-reading report must filter `WHERE reversed = 0`). This clause is entirely the projector's; the handler only freezes clause 1 (`reversalJournalLines`). Prose, comments, and the reversal file-structure line updated to say four-part and to name the projector as clause 4's owner. The handler consumes `reversed` in the new return-against-reversed guard (a return may not target a voided invoice).
