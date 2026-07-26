# Preferences + UX Completeness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a Preferences page (theme, language, currency, font size), a seeded Walk-in Customer party for cash sales to unknown buyers, and expose the already-implemented supplier-payment and transaction-reversal handlers over IPC so the UI can pay creditors and void/correct entries.

**Architecture:** The event-sourced ledger semantics do NOT change. We add (1) a non-event-sourced `app_settings` KV table that `rebuild()` ignores, (2) a seeded `party_walkin` party emitted as a normal `PartyCreated` event, (3) thin Tauri command wrappers over existing core handlers (`handle_payment_made`, `handle_transaction_reversed`) plus a `list_payments` query and `event_id` exposed on existing list queries, and (4) a React Preferences page backed by a DB-first settings store with localStorage migration, currency-aware money formatting, and CSS-variable font scaling.

**Tech Stack:** Rust (`accounting-core`, `tauri-app`), rusqlite/SQLite, Tauri 2 IPC, React + TypeScript + Vite.

**Design reference:** `docs/superpowers/specs/2026-07-25-preferences-and-ux-completeness-design.md`

---

## File Structure

**Backend — `crates/accounting-core/`:**
- Modify `src/schema.sql` — add `app_settings` KV table (no `event_id`, ignored by rebuild).
- Create `src/settings.rs` — `get_settings`, `set_setting`, allowlist validation (pure DB, no events).
- Modify `src/lib.rs` — export the settings API.
- Modify `src/genesis.rs` — seed the walk-in party on fresh install; add a public `WALKIN_PARTY_ID` const and a reusable `ensure_walkin_party` helper the app calls on startup.

**Backend — `crates/tauri-app/`:**
- Modify `src/commands.rs` — add `get_settings`, `set_setting`, `record_payment_made`, `reverse_transaction`, `list_payments` commands; add `event_id` to `SaleRow`/`PurchaseRow`.
- Modify `src/lib.rs` — register the new commands; call `ensure_walkin_party` on startup before `rebuild`.

**Frontend — `ui/src/`:**
- Create `src/settings.tsx` — `SettingsProvider` + `useSettings`/`useCurrency` hooks (DB-first, localStorage migration).
- Modify `src/lib.ts` — currency-aware `formatMoney(minor, currency)`.
- Create `src/pages/Preferences.tsx` — Appearance / Language / Currency sections.
- Modify `src/App.tsx` — add Preferences nav entry; remove theme/language quick-buttons from sidebar footer.
- Modify `src/theme.tsx`, `src/i18n/index.tsx` — accept externally-driven initial values from settings.
- Modify `src/pages/Payments.tsx` — direction toggle (received/paid) + payments history + void action.
- Modify `src/pages/Sales.tsx`, `src/pages/Purchases.tsx` — void/edit row actions; default cash sale customer to walk-in.
- Modify `src/i18n/fr.ts`, `src/i18n/en.ts` — add `nav.preferences`, `preferences.*`, and void/direction strings.
- Modify `src/styles.css` — `--font-scale` variable + `.pref-*` styles.

---

## Task 1: Add `app_settings` table to schema

**Files:**
- Modify: `crates/accounting-core/src/schema.sql` (after the `events`/`projection_cursor` block, before master-data section — around line 20)
- Test: `crates/accounting-core/src/db.rs` (extend `apply_schema_creates_all_projection_tables`)

- [ ] **Step 1: Add a failing assertion for the new table**

In `crates/accounting-core/src/db.rs`, inside `apply_schema_creates_all_projection_tables`, add `"app_settings"` to the `expected` array:

```rust
        let expected = [
            "users", "accounts", "items", "inventory_lots", "parties",
            "journal_lines", "sales", "sale_lines", "lot_consumptions",
            "purchases", "purchase_lines", "payments", "payment_allocations",
            "party_balances", "returns", "return_lines", "expenses",
            "events", "projection_cursor", "app_settings",
        ];
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p accounting-core apply_schema_creates_all_projection_tables`
Expected: FAIL — assertion `table app_settings should exist` (count 0).

- [ ] **Step 3: Add the table to the schema**

In `crates/accounting-core/src/schema.sql`, after the `projection_cursor` table block (after line 19), add:

```sql
-- ============================================================
-- Application settings (NOT event-sourced; ignored by rebuild).
-- Holds current-state UI/business configuration, not ledger facts.
-- ============================================================
CREATE TABLE IF NOT EXISTS app_settings (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p accounting-core apply_schema_creates_all_projection_tables`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/accounting-core/src/schema.sql crates/accounting-core/src/db.rs
git commit -m "feat(core): add app_settings KV table (non-event-sourced)"
```

---

## Task 2: Settings store module (`get_settings`, `set_setting`, allowlist)

**Files:**
- Create: `crates/accounting-core/src/settings.rs`
- Modify: `crates/accounting-core/src/lib.rs:1-10` (module decl + re-export)
- Test: inline `#[cfg(test)]` in `settings.rs`

- [ ] **Step 1: Write the module with failing tests**

Create `crates/accounting-core/src/settings.rs`:

```rust
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
```

- [ ] **Step 2: Wire the module into the crate**

In `crates/accounting-core/src/lib.rs`, add the module declaration after `pub mod queries;` (line 8):

```rust
pub mod settings;
```

And add a re-export after the `pub use queries::{...};` block (after line 35):

```rust
pub use settings::{get_settings, set_setting, SETTING_KEYS};
```

- [ ] **Step 3: Run the tests to verify they pass**

Run: `cargo test -p accounting-core settings::`
Expected: PASS (4 tests).

- [ ] **Step 4: Commit**

```bash
git add crates/accounting-core/src/settings.rs crates/accounting-core/src/lib.rs
git commit -m "feat(core): add settings store with key allowlist"
```

---

## Task 3: Walk-in party constant + idempotent seed helper

**Files:**
- Modify: `crates/accounting-core/src/genesis.rs`
- Modify: `crates/accounting-core/src/lib.rs` (re-export `WALKIN_PARTY_ID`, `ensure_walkin_party`)
- Test: inline `#[cfg(test)]` in `genesis.rs`

The walk-in party must be seeded via a normal `PartyCreated` event so it flows through the existing projector. The existence check queries the **events table** (not the `parties` projection, which is stale before `rebuild()`), matching how genesis checks for prior events. `handle_party_created` already emits `PartyCreated` with payload key `partyId` (see `commands/setup.rs:53`).

- [ ] **Step 1: Write failing tests for the seed helper**

In `crates/accounting-core/src/genesis.rs`, add to the `#[cfg(test)] mod tests` block:

```rust
    #[test]
    fn ensure_walkin_seeds_once_and_is_idempotent() {
        let conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("deviceA");
        run_genesis(&conn, &mut hlc, 1000, "deviceA", "owner-1", "Jane").unwrap();

        ensure_walkin_party(&conn, &mut hlc, 2000, "deviceA").unwrap();
        ensure_walkin_party(&conn, &mut hlc, 3000, "deviceA").unwrap();

        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM events WHERE type='PartyCreated' \
                 AND json_extract(payload, '$.partyId') = ?1",
                [WALKIN_PARTY_ID],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "walk-in party must be seeded exactly once");
    }

    #[test]
    fn ensure_walkin_projects_a_customer_party() {
        let mut conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("deviceA");
        run_genesis(&conn, &mut hlc, 1000, "deviceA", "owner-1", "Jane").unwrap();
        ensure_walkin_party(&conn, &mut hlc, 2000, "deviceA").unwrap();
        crate::projectors::rebuild(&mut conn).unwrap();
        let kind: String = conn
            .query_row("SELECT kind FROM parties WHERE id = ?1", [WALKIN_PARTY_ID], |r| r.get(0))
            .unwrap();
        assert_eq!(kind, "customer");
    }
```

Note: `rebuild` takes `&mut Connection`, so bind `conn` as `mut` (not shadowed).

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p accounting-core genesis::tests::ensure_walkin`
Expected: FAIL — `WALKIN_PARTY_ID` and `ensure_walkin_party` are not defined.

- [ ] **Step 3: Implement the const and helper**

In `crates/accounting-core/src/genesis.rs`, after the `SYSTEM_USER_ID` const (line 7), add:

```rust
/// The shared, always-present customer used to record cash sales to unknown
/// walk-in buyers.
pub const WALKIN_PARTY_ID: &str = "party_walkin";
```

Then add a new public function after `run_genesis` (after line 64):

```rust
/// Idempotently ensure the walk-in customer party exists.
///
/// Safe to call on every startup: it checks the immutable event log (not the
/// `parties` projection, which is empty until `rebuild()` runs) and only
/// appends a `PartyCreated` event when none exists yet. Covers installs whose
/// genesis predates the walk-in party.
pub fn ensure_walkin_party(
    conn: &Connection,
    hlc: &mut Hlc,
    physical_now: u64,
    device_id: &str,
) -> rusqlite::Result<()> {
    let exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM events WHERE type = 'PartyCreated' \
         AND json_extract(payload, '$.partyId') = ?1",
        [WALKIN_PARTY_ID],
        |r| r.get(0),
    )?;
    if exists > 0 {
        return Ok(());
    }
    append_event(
        conn, hlc, physical_now, device_id, SYSTEM_USER_ID,
        "PartyCreated",
        &json!({ "partyId": WALKIN_PARTY_ID, "name": "Walk-in Customer", "kind": "customer" }),
    )?;
    Ok(())
}
```

- [ ] **Step 4: Re-export from the crate root**

In `crates/accounting-core/src/lib.rs`, change the genesis re-export (line 15) from:

```rust
pub use genesis::{run_genesis, SYSTEM_USER_ID};
```

to:

```rust
pub use genesis::{ensure_walkin_party, run_genesis, SYSTEM_USER_ID, WALKIN_PARTY_ID};
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p accounting-core genesis::`
Expected: PASS (all genesis tests, including the two new ones).

- [ ] **Step 6: Commit**

```bash
git add crates/accounting-core/src/genesis.rs crates/accounting-core/src/lib.rs
git commit -m "feat(core): add walk-in customer party + idempotent seed helper"
```

---

## Task 4: Call `ensure_walkin_party` on app startup

**Files:**
- Modify: `crates/tauri-app/src/lib.rs:5,30-37`

The walk-in seed runs after genesis (or on existing installs) and **before** `rebuild`, so the new `PartyCreated` event is projected in the same startup pass.

- [ ] **Step 1: Import the helper**

In `crates/tauri-app/src/lib.rs`, change line 5 from:

```rust
use accounting_core::{apply_schema, rebuild, Hlc, rehydrate_from_log, run_genesis};
```

to:

```rust
use accounting_core::{apply_schema, ensure_walkin_party, rebuild, Hlc, rehydrate_from_log, run_genesis};
```

- [ ] **Step 2: Seed the walk-in party before rebuild**

In `init_state`, between the genesis block and the `rebuild` call (between lines 33 and 35), add:

```rust
    // Idempotently ensure the shared walk-in customer exists (covers both
    // fresh installs and installs whose genesis predates this party). Must run
    // before rebuild so the event is projected this startup.
    ensure_walkin_party(&conn, &mut hlc, now_ms(), "device-1").expect("seed walk-in party");
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo build -p tauri-app`
Expected: builds without errors.

- [ ] **Step 4: Commit**

```bash
git add crates/tauri-app/src/lib.rs
git commit -m "feat(app): seed walk-in party on startup before rebuild"
```

---

## Task 5: Expose settings commands over IPC

**Files:**
- Modify: `crates/tauri-app/src/commands.rs` (add commands near the query section, ~line 144)
- Modify: `crates/tauri-app/src/lib.rs:52-65` (register handlers)

`get_settings`/`set_setting` are plain DB reads/writes — they do NOT need `with_ctx!` (no event, no HLC). Note the core `set_setting` returns `rusqlite::Result`; `AppError` already has a `From<rusqlite::Error>` impl (used throughout `commands.rs` via `.map_err(AppError::from)` and `?`).

- [ ] **Step 1: Add the two commands**

In `crates/tauri-app/src/commands.rs`, add after the `record_payment` command (after line 142), before the `// ---- Query commands ----` divider:

```rust
// ---- Settings commands ----

#[tauri::command]
pub fn get_settings(state: State<AppState>) -> Result<std::collections::HashMap<String, String>, AppError> {
    let db = state.db.lock().unwrap();
    accounting_core::get_settings(&db.conn).map_err(AppError::from)
}

#[tauri::command]
pub fn set_setting(state: State<AppState>, key: String, value: String) -> Result<(), AppError> {
    let db = state.db.lock().unwrap();
    accounting_core::set_setting(&db.conn, &key, &value).map_err(AppError::from)
}
```

- [ ] **Step 2: Register the commands**

In `crates/tauri-app/src/lib.rs`, add to the `generate_handler!` list (after `commands::list_purchases,` on line 64):

```rust
            commands::get_settings,
            commands::set_setting,
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo build -p tauri-app`
Expected: builds without errors.

- [ ] **Step 4: Commit**

```bash
git add crates/tauri-app/src/commands.rs crates/tauri-app/src/lib.rs
git commit -m "feat(app): expose get_settings/set_setting IPC commands"
```

---

## Task 6: Expose supplier payment + reversal + list_payments; add event_id to lists

**Files:**
- Modify: `crates/tauri-app/src/commands.rs` (imports line 3-7; new commands; `SaleRow`/`PurchaseRow` + queries)
- Modify: `crates/tauri-app/src/lib.rs:52-65` (register handlers)

- [ ] **Step 1: Extend the core imports**

In `crates/tauri-app/src/commands.rs`, change the import block (lines 3-7) from:

```rust
use accounting_core::{
    handle_item_defined, handle_party_created, handle_purchase_recorded,
    handle_sale_recorded, handle_payment_received, run_all_checks, all_passed,
    CommandContext, PurchaseLineInput, SaleLineInput, AllocInput,
};
```

to:

```rust
use accounting_core::{
    handle_item_defined, handle_party_created, handle_purchase_recorded,
    handle_sale_recorded, handle_payment_received, handle_payment_made,
    handle_transaction_reversed, run_all_checks, all_passed,
    CommandContext, PurchaseLineInput, SaleLineInput, AllocInput,
};
```

- [ ] **Step 2: Add supplier-payment and reversal commands**

In `crates/tauri-app/src/commands.rs`, add after the `record_payment` command (after line 142):

```rust
#[derive(Deserialize)]
pub struct PaymentMadeInput {
    pub id: String,
    pub supplier_id: String,
    pub amount_minor: i64,
    pub date: String,
    pub allocations: Vec<AllocationDto>,
}

#[tauri::command]
pub fn record_payment_made(state: State<AppState>, input: PaymentMadeInput) -> Result<(), AppError> {
    with_ctx!(state, |ctx| {
        let allocs: Vec<AllocInput> = input.allocations.into_iter().map(|a| AllocInput {
            target_id: a.target_id, target_type: a.target_type, amount_minor: a.amount_minor,
        }).collect();
        handle_payment_made(&mut ctx, &input.id, &input.supplier_id, input.amount_minor, &input.date, allocs)?;
        Ok(())
    })
}

#[derive(Deserialize)]
pub struct ReversalInput {
    pub target_event_id: String,
    pub reason: String,
}

#[tauri::command]
pub fn reverse_transaction(state: State<AppState>, input: ReversalInput) -> Result<(), AppError> {
    with_ctx!(state, |ctx| {
        handle_transaction_reversed(&mut ctx, &input.target_event_id, &input.reason)?;
        Ok(())
    })
}
```

- [ ] **Step 3: Add the `list_payments` query**

In `crates/tauri-app/src/commands.rs`, add after `list_purchases` (after line 281):

```rust
#[derive(Serialize)]
pub struct PaymentRow {
    pub id: String,
    pub event_id: String,
    pub party_id: String,
    pub direction: String,
    pub amount_minor: i64,
    pub date: String,
}

#[tauri::command]
pub fn list_payments(state: State<AppState>) -> Result<Vec<PaymentRow>, AppError> {
    let db = state.db.lock().unwrap();
    let mut stmt = db.conn.prepare(
        "SELECT id, event_id, party_id, direction, amount_minor, date FROM payments ORDER BY date DESC")?;
    let rows = stmt.query_map([], |r| {
        Ok(PaymentRow {
            id: r.get(0)?, event_id: r.get(1)?, party_id: r.get(2)?,
            direction: r.get(3)?, amount_minor: r.get(4)?, date: r.get(5)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
}
```

Note: `payments` has no `reversed` column; reversed payments are DELETEd by the projector (`projectors.rs:717`), so they naturally drop out of this list.

- [ ] **Step 4: Add `event_id` to `SaleRow` and `PurchaseRow`**

In `crates/tauri-app/src/commands.rs`, replace the `SaleRow` struct + `list_sales` (lines 241-260) with:

```rust
#[derive(Serialize)]
pub struct SaleRow {
    pub id: String,
    pub event_id: String,
    pub customer_id: Option<String>,
    pub date: String,
    pub terms: String,
    pub total_minor: i64,
    pub outstanding_minor: i64,
}

#[tauri::command]
pub fn list_sales(state: State<AppState>) -> Result<Vec<SaleRow>, AppError> {
    let db = state.db.lock().unwrap();
    let mut stmt = db.conn.prepare(
        "SELECT id, event_id, customer_id, date, terms, total_minor, outstanding_minor FROM sales WHERE reversed = 0 ORDER BY date DESC")?;
    let rows = stmt.query_map([], |r| {
        Ok(SaleRow { id: r.get(0)?, event_id: r.get(1)?, customer_id: r.get(2)?, date: r.get(3)?, terms: r.get(4)?, total_minor: r.get(5)?, outstanding_minor: r.get(6)? })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
}
```

Then replace the `PurchaseRow` struct + `list_purchases` (lines 262-281) with:

```rust
#[derive(Serialize)]
pub struct PurchaseRow {
    pub id: String,
    pub event_id: String,
    pub supplier_id: Option<String>,
    pub date: String,
    pub terms: String,
    pub total_minor: i64,
    pub outstanding_minor: i64,
}

#[tauri::command]
pub fn list_purchases(state: State<AppState>) -> Result<Vec<PurchaseRow>, AppError> {
    let db = state.db.lock().unwrap();
    let mut stmt = db.conn.prepare(
        "SELECT id, event_id, supplier_id, date, terms, total_minor, outstanding_minor FROM purchases WHERE reversed = 0 ORDER BY date DESC")?;
    let rows = stmt.query_map([], |r| {
        Ok(PurchaseRow { id: r.get(0)?, event_id: r.get(1)?, supplier_id: r.get(2)?, date: r.get(3)?, terms: r.get(4)?, total_minor: r.get(5)?, outstanding_minor: r.get(6)? })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
}
```

- [ ] **Step 5: Register the new commands**

In `crates/tauri-app/src/lib.rs`, add to the `generate_handler!` list (after the settings commands from Task 5):

```rust
            commands::record_payment_made,
            commands::reverse_transaction,
            commands::list_payments,
```

- [ ] **Step 6: Verify it compiles**

Run: `cargo build -p tauri-app`
Expected: builds without errors.

- [ ] **Step 7: Commit**

```bash
git add crates/tauri-app/src/commands.rs crates/tauri-app/src/lib.rs
git commit -m "feat(app): expose supplier payment, reversal, list_payments; add event_id to lists"
```

---

## Task 7: Currency-aware `formatMoney`

**Files:**
- Modify: `ui/src/lib.ts:1-6`

The current `formatMoney` divides minor by 100 (hardcoded 2-decimal minor units) and takes an optional locale. The new signature keeps division by 100 (minor units are always cents in the ledger) but adds an optional currency descriptor controlling the symbol and displayed decimal places.

- [ ] **Step 1: Replace `formatMoney`**

In `ui/src/lib.ts`, replace lines 1-6 with:

```ts
export interface Currency {
  symbol: string;
  decimals: number;
}

export function formatMoney(minor: number, currency?: Currency, locale?: string): string {
  const decimals = currency?.decimals ?? 0;
  const num = (minor / 100).toLocaleString(locale ?? undefined, {
    minimumFractionDigits: decimals,
    maximumFractionDigits: decimals,
  });
  const symbol = currency?.symbol ?? "";
  return symbol ? `${symbol} ${num}` : num;
}
```

- [ ] **Step 2: Verify the UI type-checks**

Run: `cd ui && npx tsc --noEmit`
Expected: FAIL only where callers pass a `locale: string` as the 2nd arg (Sales/Purchases currently call `formatMoney(x)` with one arg, so those still compile). If any call site breaks, it will be fixed in Task 11 when pages adopt `useCurrency`. If no call site passes a 2nd positional arg today, this passes clean.

- [ ] **Step 3: Commit**

```bash
git add ui/src/lib.ts
git commit -m "feat(ui): currency-aware formatMoney"
```

---

## Task 8: Settings provider + currency hook (DB-first, localStorage migration)

**Files:**
- Create: `ui/src/settings.tsx`
- Modify: `ui/src/main.tsx` (wrap the app in `SettingsProvider`)

This provider loads settings from the DB once on mount, migrates any legacy localStorage values (`accounting.theme`, `accounting.locale`) into the DB if the DB has no value, and exposes `useSettings()` (raw map + setter) and `useCurrency()` (typed `{symbol, decimals}` + `format`).

- [ ] **Step 1: Read `main.tsx` to see the current provider nesting**

Run: `cat ui/src/main.tsx`
Expected: shows `ThemeProvider`, `I18nProvider`, `ToastProvider` wrapping `<App />`. Note the exact nesting order for Step 3.

- [ ] **Step 2: Create the settings provider**

Create `ui/src/settings.tsx`:

```tsx
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import { formatMoney, type Currency } from "./lib";

type SettingsMap = Record<string, string>;

const DEFAULTS: SettingsMap = {
  currency_symbol: "",
  currency_code: "",
  currency_decimals: "0",
  theme: "light",
  locale: "fr",
  font_size: "medium",
};

// Legacy localStorage keys migrated into the DB on first load.
const LEGACY_KEYS: Record<string, string> = {
  theme: "accounting.theme",
  locale: "accounting.locale",
};

interface SettingsCtx {
  settings: SettingsMap;
  ready: boolean;
  set: (key: string, value: string) => Promise<void>;
}

const SettingsContext = createContext<SettingsCtx | null>(null);

export function SettingsProvider({ children }: { children: ReactNode }) {
  const [settings, setSettings] = useState<SettingsMap>(DEFAULTS);
  const [ready, setReady] = useState(false);

  useEffect(() => {
    (async () => {
      try {
        const stored = await invoke<SettingsMap>("get_settings");
        const merged: SettingsMap = { ...DEFAULTS, ...stored };
        // One-time migration: if DB lacks a value but localStorage has one, seed DB.
        for (const [key, lsKey] of Object.entries(LEGACY_KEYS)) {
          if (stored[key] === undefined) {
            const legacy = window.localStorage.getItem(lsKey);
            if (legacy) {
              merged[key] = legacy;
              await invoke("set_setting", { key, value: legacy });
            }
          }
        }
        setSettings(merged);
      } catch {
        setSettings(DEFAULTS);
      } finally {
        setReady(true);
      }
    })();
  }, []);

  const set = useCallback(async (key: string, value: string) => {
    await invoke("set_setting", { key, value });
    setSettings((s) => ({ ...s, [key]: value }));
  }, []);

  const value = useMemo<SettingsCtx>(() => ({ settings, ready, set }), [settings, ready, set]);

  return <SettingsContext.Provider value={value}>{children}</SettingsContext.Provider>;
}

export function useSettings(): SettingsCtx {
  const ctx = useContext(SettingsContext);
  if (!ctx) throw new Error("useSettings must be used within SettingsProvider");
  return ctx;
}

export function useCurrency() {
  const { settings } = useSettings();
  const currency: Currency = {
    symbol: settings.currency_symbol ?? "",
    decimals: Number(settings.currency_decimals ?? "0"),
  };
  return {
    currency,
    format: (minor: number) => formatMoney(minor, currency),
  };
}
```

- [ ] **Step 3: Wrap the app in `SettingsProvider`**

In `ui/src/main.tsx`, import the provider and wrap it as the **outermost** app provider (so theme/i18n can read initial values from it in Task 10). Add the import:

```tsx
import { SettingsProvider } from "./settings";
```

Wrap the existing provider tree so `<SettingsProvider>` is the outermost wrapper. The current tree in `main.tsx` is:

```tsx
<React.StrictMode>
  <I18nProvider>
    <ThemeProvider>
      <ToastProvider>
        <App />
      </ToastProvider>
    </ThemeProvider>
  </I18nProvider>
</React.StrictMode>
```

change it to:

```tsx
<React.StrictMode>
  <SettingsProvider>
    <I18nProvider>
      <ThemeProvider>
        <ToastProvider>
          <App />
        </ToastProvider>
      </ThemeProvider>
    </I18nProvider>
  </SettingsProvider>
</React.StrictMode>
```

- [ ] **Step 4: Verify the UI builds**

Run: `cd ui && npm run build`
Expected: build succeeds.

- [ ] **Step 5: Commit**

```bash
git add ui/src/settings.tsx ui/src/main.tsx
git commit -m "feat(ui): DB-first settings provider + useCurrency hook"
```

---

## Task 9: i18n strings for Preferences, direction, and void/edit

**Files:**
- Modify: `ui/src/i18n/fr.ts`
- Modify: `ui/src/i18n/en.ts`

`fr` is the source of truth for the `Translations` type; `en` must match its shape exactly or `tsc` fails.

- [ ] **Step 1: Add French strings**

In `ui/src/i18n/fr.ts`, add `preferences: "Préférences"` to the `nav` object (after line 15's `faq`):

```ts
    faq: "Aide",
    preferences: "Préférences",
```

Add a `preferences` section after the `payments` block (after line 130's closing `},`):

```ts
  preferences: {
    title: "Préférences",
    appearance: "Apparence",
    theme: "Thème",
    themeLight: "Clair",
    themeDark: "Sombre",
    themeMidnight: "Minuit",
    fontSize: "Taille du texte",
    fontSmall: "Petit",
    fontMedium: "Moyen",
    fontLarge: "Grand",
    language: "Langue",
    currency: "Devise",
    currencySymbol: "Symbole",
    currencyCode: "Code ISO",
    currencyDecimals: "Décimales",
    currencyPreview: "Aperçu",
    saved: "Préférence enregistrée.",
  },
```

Add void/direction strings to the `payments` block (before its closing `},` on line 130):

```ts
    directionReceived: "Reçu d'un client",
    directionPaid: "Payé à un fournisseur",
    supplier: "Fournisseur",
    selectSupplier: "Sélectionner un fournisseur...",
    history: "Historique des paiements",
    noPayments: "Aucun paiement enregistré.",
    paidMade: "Paiement fournisseur enregistré.",
```

Add void/edit strings to the `common` block (before its closing `},` on line 180):

```ts
    void: "Annuler",
    voidConfirm: "Motif de l'annulation ?",
    voided: "Écriture annulée.",
    edit: "Corriger",
```

- [ ] **Step 2: Mirror the same keys in English**

In `ui/src/i18n/en.ts`, add the identical keys with English values:
- `nav.preferences: "Preferences"`
- `preferences` block: `title: "Preferences"`, `appearance: "Appearance"`, `theme: "Theme"`, `themeLight: "Light"`, `themeDark: "Dark"`, `themeMidnight: "Midnight"`, `fontSize: "Text size"`, `fontSmall: "Small"`, `fontMedium: "Medium"`, `fontLarge: "Large"`, `language: "Language"`, `currency: "Currency"`, `currencySymbol: "Symbol"`, `currencyCode: "ISO code"`, `currencyDecimals: "Decimals"`, `currencyPreview: "Preview"`, `saved: "Preference saved."`
- `payments` additions: `directionReceived: "Received from customer"`, `directionPaid: "Paid to supplier"`, `supplier: "Supplier"`, `selectSupplier: "Select a supplier..."`, `history: "Payment history"`, `noPayments: "No payments recorded."`, `paidMade: "Supplier payment recorded."`
- `common` additions: `void: "Void"`, `voidConfirm: "Reason for voiding?"`, `voided: "Entry voided."`, `edit: "Correct"`

- [ ] **Step 3: Verify translations type-check**

Run: `cd ui && npx tsc --noEmit`
Expected: PASS — `en` matches the `fr`-derived `Translations` shape. If it fails, a key is missing or misspelled in `en.ts`.

- [ ] **Step 4: Commit**

```bash
git add ui/src/i18n/fr.ts ui/src/i18n/en.ts
git commit -m "feat(ui): i18n strings for preferences, payment direction, void/edit"
```

---

## Task 10: Font-scale CSS variable + theme/i18n read initial value from settings

**Files:**
- Modify: `ui/src/styles.css` (`:root` + body font-size)
- Modify: `ui/src/theme.tsx` (apply font scale; accept settings-driven theme)
- Modify: `ui/src/i18n/index.tsx` (no change needed if App drives locale; see note)

The font-size preset maps to a `--font-scale` CSS variable on `:root`. Rather than restructure the theme/i18n providers, we apply theme, font scale, and locale from the settings provider inside `App` via effects (simplest, keeps providers untouched). This task does the CSS + a small effect hook.

- [ ] **Step 1: Add the font-scale variable to CSS**

In `ui/src/styles.css`, find the `:root` selector block. Add a default variable and make the body font-size scale. Add inside `:root { ... }`:

```css
  --font-scale: 1;
```

Then find the `body` rule and set its font-size to scale (if `body` already sets `font-size`, replace that declaration; otherwise add the rule):

```css
body {
  font-size: calc(16px * var(--font-scale));
}
```

- [ ] **Step 2: Apply theme, locale, and font scale from settings in App**

In `ui/src/App.tsx`, add the settings import at the top (`useTheme`/`useI18n` are already imported):

```tsx
import { useSettings } from "./settings";
```

Extend the **existing** `useTheme`/`useI18n` destructures on lines 33-34 to also pull the setters (keep a single call to each hook — the footer buttons still use `label, icon, cycle, localeLabel, cycleLocale` until Task 11 removes them):

```tsx
  const { label, icon, cycle, set: setTheme } = useTheme();
  const { t, localeLabel, cycleLocale, setLocale } = useI18n();
  const { settings, ready } = useSettings();
```

Add the settings-driven effect after the existing effects (after the `SIDEBAR_KEY` effect, ~line 38):

```tsx
  useEffect(() => {
    if (!ready) return;
    const scale = settings.font_size === "small" ? 0.9 : settings.font_size === "large" ? 1.15 : 1.0;
    document.documentElement.style.setProperty("--font-scale", String(scale));
    if (settings.theme) setTheme(settings.theme as "light" | "dark" | "midnight");
    if (settings.locale) setLocale(settings.locale as "fr" | "en");
  }, [ready, settings.font_size, settings.theme, settings.locale, setTheme, setLocale]);
```

- [ ] **Step 3: Verify the UI builds**

Run: `cd ui && npm run build`
Expected: build succeeds.

- [ ] **Step 4: Commit**

```bash
git add ui/src/styles.css ui/src/App.tsx
git commit -m "feat(ui): font-scale CSS var + apply theme/locale/font from settings"
```

---

## Task 11: Preferences page + nav entry; remove footer quick-buttons

**Files:**
- Create: `ui/src/pages/Preferences.tsx`
- Modify: `ui/src/App.tsx` (nav + route; remove theme/locale footer buttons)

- [ ] **Step 1: Create the Preferences page**

Create `ui/src/pages/Preferences.tsx`:

```tsx
import { useToast } from "../theme";
import { useI18n } from "../i18n";
import { useSettings } from "../settings";
import { formatMoney } from "../lib";

const DECIMALS = ["0", "2"] as const;

export function Preferences() {
  const { t } = useI18n();
  const { settings, set } = useSettings();
  const toast = useToast();

  const update = async (key: string, value: string) => {
    await set(key, value);
    toast.push(t.preferences.saved);
  };

  const previewCurrency = {
    symbol: settings.currency_symbol ?? "",
    decimals: Number(settings.currency_decimals ?? "0"),
  };

  return (
    <div>
      <div className="page-header">
        <h1>{t.preferences.title}</h1>
      </div>

      <section className="panel">
        <h2 style={{ marginTop: 0 }}>{t.preferences.appearance}</h2>
        <div className="form-row">
          <label>
            {t.preferences.theme}
            <select value={settings.theme} onChange={(e) => update("theme", e.target.value)}>
              <option value="light">{t.preferences.themeLight}</option>
              <option value="dark">{t.preferences.themeDark}</option>
              <option value="midnight">{t.preferences.themeMidnight}</option>
            </select>
          </label>
          <label>
            {t.preferences.fontSize}
            <select value={settings.font_size} onChange={(e) => update("font_size", e.target.value)}>
              <option value="small">{t.preferences.fontSmall}</option>
              <option value="medium">{t.preferences.fontMedium}</option>
              <option value="large">{t.preferences.fontLarge}</option>
            </select>
          </label>
        </div>
      </section>

      <section className="panel">
        <h2 style={{ marginTop: 0 }}>{t.preferences.language}</h2>
        <div className="form-row">
          <label>
            {t.preferences.language}
            <select value={settings.locale} onChange={(e) => update("locale", e.target.value)}>
              <option value="fr">Français</option>
              <option value="en">English</option>
            </select>
          </label>
        </div>
      </section>

      <section className="panel">
        <h2 style={{ marginTop: 0 }}>{t.preferences.currency}</h2>
        <div className="form-row">
          <label>
            {t.preferences.currencySymbol}
            <input
              type="text"
              value={settings.currency_symbol}
              placeholder="€"
              onChange={(e) => update("currency_symbol", e.target.value)}
            />
          </label>
          <label>
            {t.preferences.currencyCode}
            <input
              type="text"
              value={settings.currency_code}
              placeholder="EUR"
              onChange={(e) => update("currency_code", e.target.value)}
            />
          </label>
          <label>
            {t.preferences.currencyDecimals}
            <select value={settings.currency_decimals} onChange={(e) => update("currency_decimals", e.target.value)}>
              {DECIMALS.map((d) => (
                <option key={d} value={d}>{d}</option>
              ))}
            </select>
          </label>
        </div>
        <p className="muted">
          {t.preferences.currencyPreview}: {formatMoney(123456, previewCurrency)}
        </p>
      </section>
    </div>
  );
}
```


- [ ] **Step 2: Add Preferences to the nav and route**

In `ui/src/App.tsx`:

Change the `Page` type (line 12) to include `preferences`:

```tsx
type Page = "dashboard" | "items" | "parties" | "purchases" | "sales" | "payments" | "preferences" | "faq";
```

Add its icon to `NAV_ICONS` (in the object, lines 14-22):

```tsx
  preferences: "⚙️",
```

Add it to `NAV_ORDER` (line 24), before `faq`:

```tsx
const NAV_ORDER: Page[] = ["dashboard", "items", "parties", "purchases", "sales", "payments", "preferences", "faq"];
```

Import the page (after line 8's Faq import):

```tsx
import { Preferences } from "./pages/Preferences";
```

Add the route in `<main>` (after the payments route, line 97):

```tsx
        {page === "preferences" && <Preferences />}
```

- [ ] **Step 3: Remove theme/locale quick-buttons from the sidebar footer**

In `ui/src/App.tsx`, in the `.sidebar-footer` div (lines 63-88), remove the locale button (lines 64-71) and the theme button (lines 72-79), keeping only the collapse toggle. The footer becomes:

```tsx
        <div className="sidebar-footer">
          <button
            className="sidebar-footer-btn"
            onClick={() => setCollapsed((c) => !c)}
            title={t.app.toggleSidebar}
          >
            <span className="nav-icon">{collapsed ? "▶" : "◀"}</span>
            <span className="nav-label">{t.app.toggleSidebar}</span>
          </button>
        </div>
```

Then drop the footer-only destructured values (`label, icon, cycle` from `useTheme`; `localeLabel, cycleLocale` from `useI18n`) that the removed buttons used. After Task 10 the destructures read:

```tsx
  const { label, icon, cycle, set: setTheme } = useTheme();
  const { t, localeLabel, cycleLocale, setLocale } = useI18n();
  const { settings, ready } = useSettings();
```

Trim them to only what remains in use (the Task 10 effect still needs `setTheme`/`setLocale`/`settings`/`ready`):

```tsx
  const { set: setTheme } = useTheme();
  const { t, setLocale } = useI18n();
  const { settings, ready } = useSettings();
```

Keep each hook called exactly once.

- [ ] **Step 4: Verify the UI builds**

Run: `cd ui && npm run build`
Expected: build succeeds with no unused-variable errors. Fix any unused const/import flagged by the build.

- [ ] **Step 5: Commit**

```bash
git add ui/src/pages/Preferences.tsx ui/src/App.tsx
git commit -m "feat(ui): Preferences page; move theme/language out of sidebar footer"
```

---

## Task 12: Payments page — direction toggle, history, void

**Files:**
- Modify: `ui/src/pages/Payments.tsx`

Adds a Received/Paid direction toggle (filtering the party dropdown by kind and calling `record_payment` or `record_payment_made`), a payments-history table, and a Void action per row.

- [ ] **Step 1: Rewrite Payments.tsx**

Replace the entire contents of `ui/src/pages/Payments.tsx` with:

```tsx
import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { majorToMinor, newId, today, errorMessage } from "../lib";
import { useToast } from "../theme";
import { useI18n } from "../i18n";
import { useCurrency } from "../settings";

type Direction = "in" | "out";

interface Party {
  id: string;
  name: string;
  kind: string;
}

interface Payment {
  id: string;
  event_id: string;
  party_id: string;
  direction: string;
  amount_minor: number;
  date: string;
}

export function Payments() {
  const { t } = useI18n();
  const { format } = useCurrency();
  const [parties, setParties] = useState<Party[]>([]);
  const [payments, setPayments] = useState<Payment[]>([]);
  const [direction, setDirection] = useState<Direction>("in");
  const [partyId, setPartyId] = useState("");
  const [amountMajor, setAmountMajor] = useState("");
  const [date, setDate] = useState(today());
  const [error, setError] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const toast = useToast();

  const refresh = async () => {
    try {
      const [pt, pay] = await Promise.all([
        invoke<Party[]>("list_parties"),
        invoke<Payment[]>("list_payments"),
      ]);
      setParties(pt);
      setPayments(pay);
    } catch (e: unknown) {
      setError(errorMessage(e));
    }
  };

  useEffect(() => {
    refresh();
  }, []);

  const eligible =
    direction === "in"
      ? parties.filter((p) => p.kind === "customer" || p.kind === "both")
      : parties.filter((p) => p.kind === "supplier" || p.kind === "both");

  const partyName = (id: string) => parties.find((p) => p.id === id)?.name ?? id;

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setError("");
    setSubmitting(true);
    try {
      const command = direction === "in" ? "record_payment" : "record_payment_made";
      const input =
        direction === "in"
          ? { id: newId(), customer_id: partyId, amount_minor: majorToMinor(amountMajor), date, allocations: [] }
          : { id: newId(), supplier_id: partyId, amount_minor: majorToMinor(amountMajor), date, allocations: [] };
      await invoke(command, { input });
      toast.push(direction === "in" ? t.payments.added : t.payments.paidMade);
      setPartyId("");
      setAmountMajor("");
      setDate(today());
      await refresh();
    } catch (e: unknown) {
      setError(errorMessage(e));
      toast.push(errorMessage(e), "error");
    } finally {
      setSubmitting(false);
    }
  };

  const voidPayment = async (p: Payment) => {
    const reason = window.prompt(t.common.voidConfirm);
    if (!reason) return;
    try {
      await invoke("reverse_transaction", { input: { target_event_id: p.event_id, reason } });
      toast.push(t.common.voided);
      await refresh();
    } catch (e: unknown) {
      toast.push(errorMessage(e), "error");
    }
  };

  return (
    <div>
      <div className="page-header">
        <h1>{t.payments.title}</h1>
      </div>

      <section className="panel">
        <h2 style={{ marginTop: 0 }}>{t.payments.recordTitle}</h2>
        <form onSubmit={submit} className="form">
          <div className="form-row">
            <label>
              {t.payments.recordTitle}
              <select value={direction} onChange={(e) => { setDirection(e.target.value as Direction); setPartyId(""); }}>
                <option value="in">{t.payments.directionReceived}</option>
                <option value="out">{t.payments.directionPaid}</option>
              </select>
            </label>
            <label>
              {direction === "in" ? t.payments.customer : t.payments.supplier}
              <select value={partyId} onChange={(e) => setPartyId(e.target.value)} required>
                <option value="">
                  {direction === "in" ? t.payments.selectCustomer : t.payments.selectSupplier}
                </option>
                {eligible.map((p) => (
                  <option key={p.id} value={p.id}>{p.name}</option>
                ))}
              </select>
            </label>
            <label>
              {t.payments.amount}
              <input
                type="number"
                step="0.01"
                placeholder="0.00"
                value={amountMajor}
                onChange={(e) => setAmountMajor(e.target.value)}
                required
              />
            </label>
            <label>
              {t.payments.date}
              <input type="date" value={date} onChange={(e) => setDate(e.target.value)} required />
            </label>
          </div>
          <div className="form-actions">
            <button type="submit" className="primary" disabled={submitting}>
              {submitting ? t.common.recording : t.payments.submit}
            </button>
          </div>
          {error && <p className="error">{error}</p>}
        </form>
      </section>

      <div className="table-wrap">
        {payments.length === 0 ? (
          <div className="empty">{t.payments.noPayments}</div>
        ) : (
          <table>
            <thead>
              <tr>
                <th>{t.payments.date}</th>
                <th>{t.parties.title}</th>
                <th>{t.payments.recordTitle}</th>
                <th className="num">{t.payments.amount}</th>
                <th>{t.common.actions}</th>
              </tr>
            </thead>
            <tbody>
              {payments.map((p) => (
                <tr key={p.id}>
                  <td>{p.date}</td>
                  <td>{partyName(p.party_id)}</td>
                  <td>
                    <span className="badge">
                      {p.direction === "in" ? t.payments.directionReceived : t.payments.directionPaid}
                    </span>
                  </td>
                  <td className="num">{format(p.amount_minor)}</td>
                  <td>
                    <button className="ghost" onClick={() => voidPayment(p)}>
                      {t.common.void}
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
    </div>
  );
}
```

- [ ] **Step 2: Verify the UI builds**

Run: `cd ui && npm run build`
Expected: build succeeds.

- [ ] **Step 3: Commit**

```bash
git add ui/src/pages/Payments.tsx
git commit -m "feat(ui): payments direction toggle + history + void action"
```

---

## Task 13: Sales page — currency, walk-in default, void action

**Files:**
- Modify: `ui/src/pages/Sales.tsx`

Adds currency-aware formatting, defaults the customer to the walk-in party when terms are cash, and adds a Void row action using the sale's `event_id`.

- [ ] **Step 1: Import currency + walk-in id; add event_id to the Sale type**

In `ui/src/pages/Sales.tsx`:

Add imports after line 5:

```tsx
import { useCurrency } from "../settings";
```

Change the `Sale` interface (lines 22-29) to include `event_id`:

```tsx
interface Sale {
  id: string;
  event_id: string;
  customer_id: string;
  date: string;
  terms: Terms;
  total_minor: number;
  outstanding_minor: number;
}
```

Add the walk-in id constant near the top of the file (after the imports):

```tsx
const WALKIN_PARTY_ID = "party_walkin";
```

- [ ] **Step 2: Use the currency hook and default cash customer to walk-in**

In the `Sales` component, add after `const { t } = useI18n();` (line 40):

```tsx
  const { format } = useCurrency();
```

Add an effect that defaults the customer to walk-in when terms switch to cash and no customer is selected. Add after the existing effects (after line 82):

```tsx
  useEffect(() => {
    if (terms === "cash" && !customerId) setCustomerId(WALKIN_PARTY_ID);
  }, [terms, customerId]);
```

- [ ] **Step 3: Replace `formatMoney` calls with `format`**

In `ui/src/pages/Sales.tsx`, remove `formatMoney` from the import on line 3 (keep `majorToMinor, newId, today, errorMessage`), and replace the two `formatMoney(...)` calls in the table (lines 285 and 288) with `format(...)`:

```tsx
                  <td className="num">{format(s.total_minor)}</td>
                  <td className="num">
                    <span className={s.outstanding_minor > 0 ? "warn" : "ok"}>
                      {format(s.outstanding_minor)}
                    </span>
                  </td>
```

- [ ] **Step 4: Add a Void action column to the sales table**

In `ui/src/pages/Sales.tsx`, add a void handler inside the component (after the `submit` function, before `customerName`):

```tsx
  const voidSale = async (s: Sale) => {
    const reason = window.prompt(t.common.voidConfirm);
    if (!reason) return;
    try {
      await invoke("reverse_transaction", { input: { target_event_id: s.event_id, reason } });
      toast.push(t.common.voided);
      await refresh();
    } catch (e: unknown) {
      toast.push(errorMessage(e), "error");
    }
  };
```

Add an Actions header to the table `<thead>` (after the outstanding `<th>`, line 272):

```tsx
                <th>{t.common.actions}</th>
```

Add an actions cell to each row (after the outstanding `<td>`, line 290):

```tsx
                  <td>
                    <button className="ghost" onClick={() => voidSale(s)}>
                      {t.common.void}
                    </button>
                  </td>
```

- [ ] **Step 5: Verify the UI builds**

Run: `cd ui && npm run build`
Expected: build succeeds.

- [ ] **Step 6: Commit**

```bash
git add ui/src/pages/Sales.tsx
git commit -m "feat(ui): sales currency formatting, walk-in default, void action"
```

---

## Task 14: Purchases page — currency + void action

**Files:**
- Modify: `ui/src/pages/Purchases.tsx`

Mirrors the Sales changes: currency formatting + void row action. (No walk-in default — purchases require a real supplier.)

- [ ] **Step 1: Read the current Purchases page**

Run: `cat ui/src/pages/Purchases.tsx`
Expected: structure mirrors `Sales.tsx` — a `Purchase` interface, a table rendering `formatMoney(p.total_minor)` / `formatMoney(p.outstanding_minor)`, a `refresh` function, and `useToast`.

- [ ] **Step 2: Add event_id to the Purchase type and import currency**

In `ui/src/pages/Purchases.tsx`:

Add the import alongside the others:

```tsx
import { useCurrency } from "../settings";
```

Add `event_id: string;` as the second field of the `Purchase` interface (after `id`).

In the component body, add after the `useI18n` destructure:

```tsx
  const { format } = useCurrency();
```

- [ ] **Step 3: Replace `formatMoney` with `format` and drop the unused import**

Remove `formatMoney` from the `../lib` import (keep the other named imports). Replace each `formatMoney(...)` call in the purchases table with `format(...)`.

- [ ] **Step 4: Add the void handler and Actions column**

Add the handler inside the component (after the submit function):

```tsx
  const voidPurchase = async (p: Purchase) => {
    const reason = window.prompt(t.common.voidConfirm);
    if (!reason) return;
    try {
      await invoke("reverse_transaction", { input: { target_event_id: p.event_id, reason } });
      toast.push(t.common.voided);
      await refresh();
    } catch (e: unknown) {
      toast.push(errorMessage(e), "error");
    }
  };
```

Add `<th>{t.common.actions}</th>` as the last header in the purchases table `<thead>`, and add the matching cell as the last `<td>` in each row:

```tsx
                  <td>
                    <button className="ghost" onClick={() => voidPurchase(p)}>
                      {t.common.void}
                    </button>
                  </td>
```

- [ ] **Step 5: Verify the UI builds**

Run: `cd ui && npm run build`
Expected: build succeeds.

- [ ] **Step 6: Commit**

```bash
git add ui/src/pages/Purchases.tsx
git commit -m "feat(ui): purchases currency formatting + void action"
```

---

## Task 15: Dashboard currency formatting

**Files:**
- Modify: `ui/src/pages/Dashboard.tsx`

The Dashboard renders money KPIs (inventory value, receivable, payable, profit figures) via `formatMoney`. Switch it to the currency-aware `format` so the configured symbol/decimals appear everywhere.

- [ ] **Step 1: Read the Dashboard page**

Run: `cat ui/src/pages/Dashboard.tsx`
Expected: uses `formatMoney(...)` for KPI and profit values, imports from `../lib`.

- [ ] **Step 2: Swap to the currency hook**

In `ui/src/pages/Dashboard.tsx`:
- Add `import { useCurrency } from "../settings";`.
- Remove `formatMoney` from the `../lib` import (keep any other named imports it uses).
- In the component, add `const { format } = useCurrency();` after the existing hook calls.
- Replace every `formatMoney(x)` call with `format(x)`.

- [ ] **Step 3: Verify the UI builds**

Run: `cd ui && npm run build`
Expected: build succeeds.

- [ ] **Step 4: Commit**

```bash
git add ui/src/pages/Dashboard.tsx
git commit -m "feat(ui): dashboard currency formatting"
```

---

## Task 16: Full-workspace verification

**Files:** none (verification only)

- [ ] **Step 1: Run the full Rust test suite**

Run: `cargo test --workspace`
Expected: all tests pass, including the new settings and walk-in tests.

- [ ] **Step 2: Build the Rust app crate**

Run: `cargo build -p tauri-app`
Expected: builds without errors or warnings about unused imports/handlers.

- [ ] **Step 3: Type-check and build the UI**

Run: `cd ui && npx tsc --noEmit && npm run build`
Expected: no type errors; production build succeeds.

- [ ] **Step 4: Manual smoke test (desktop dev)**

Run: `cd crates/tauri-app && cargo tauri dev`
Verify:
- Preferences (⚙️) tab shows Appearance / Language / Currency; sidebar footer no longer has theme/language buttons.
- Changing font size resizes text; changing theme/language applies and persists across restart.
- Setting a currency symbol (e.g. `€`) + decimals shows on Dashboard, Sales, Purchases, Payments.
- Sales: choosing "cash" defaults the customer to "Walk-in Customer"; a cash sale to walk-in records successfully.
- Payments: "Paid to supplier" lists suppliers and records a supplier payment; it appears in history.
- Void a sale → it disappears from the sales list; the integrity checks on the Dashboard still pass.
- Voiding a sale that has an allocated payment shows an error toast and leaves the row intact.

- [ ] **Step 5: Final commit if any smoke-test fixes were needed**

```bash
git add -A
git commit -m "fix: address smoke-test findings for preferences + UX completeness"
```

(Skip if no fixes were required.)

---

## Notes for the implementer

- **Minor units are always cents** (integer, /100). Currency `decimals` only controls *display*, not storage. `majorToMinor` stays `Math.round(n * 100)`.
- **Reversal targets an event id**, never a projection row id. All list queries now return `event_id`; pass that straight to `reverse_transaction`.
- **Reversed payments vanish** from `list_payments` (projector deletes them); reversed sales/purchases are hidden via `WHERE reversed = 0`. Both are intended.
- **Illegal reversals are backend-guarded** (allocated payments, consumed lots, etc.); the UI just surfaces the error toast.
- **`app_settings` is invisible to the ledger** — `rebuild()` never touches it, and no reconciliation check reads it.
- Keep each hook (`useTheme`, `useI18n`, `useSettings`) called exactly once per component; Tasks 10 and 11 both touch `App.tsx` — merge their destructures.
