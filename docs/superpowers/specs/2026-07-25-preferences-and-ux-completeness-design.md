# Preferences page + UX completeness — design

**Date:** 2026-07-25
**Status:** Draft (pending review)
**Branch target:** feature branch off `main`

## Problem

Several usability gaps surfaced while testing the desktop + Android app:

1. **Fonts too small** on mobile; no way for the user to adjust.
2. **No preferences surface.** Theme and language live as cramped buttons in the
   sidebar footer. There is nowhere to configure app-wide options.
3. **No currency.** Money is rendered as a bare number with no symbol or code.
4. **Walk-in / unknown buyers** (someone who buys one item, pays cash) cannot be
   recorded — the sale handler requires a known customer party.
5. **Paying a creditor is not exposed.** The Payments page only records receipts
   *from customers*. The backend already supports paying suppliers, but there is
   no UI or IPC command for it.
6. **No way to correct a mistaken entry.** List views cannot edit or void an
   entry, even though the backend has a full reversal (compensating-entry) model.

## Decisions (from brainstorming)

| Topic | Decision |
|---|---|
| Walk-in buyers | One shared, seeded **"Walk-in Customer"** party (`party_walkin`). No change to the sale handler. |
| Currency | **Single app-wide currency** (symbol + ISO code + decimal places), applied everywhere. |
| Preferences storage | **In the SQLite database** (survives reinstalls; currency is business config, not a per-device UI whim). |
| Font size | **Presets: Small / Medium / Large**, via a root font-size multiplier. |
| Edit/correct flow | **Edit = void + prefilled new entry.** Reverse the original, then open the create form pre-filled with old values to resubmit. |
| Reversal visibility | **Row action, keep hiding reversed rows.** Add a Void/Correct action; voided rows stay hidden from lists (still in the event log for reconciliation). |

## Architecture

Five coordinated pieces. The accounting core's event-sourced ledger is
**not** changed in its semantics — we expose existing handlers and add a
non-ledger settings store.

### 1. Backend: settings store

- New table `app_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL)`.
- **Not** part of the event log; **not** touched by `rebuild()`. It holds
  current-state configuration, not historical ledger facts. Event-sourcing a
  theme toggle would be semantically wrong.
- Added to `schema.sql` (idempotent `CREATE TABLE IF NOT EXISTS`).
- Two Tauri commands:
  - `get_settings() -> HashMap<String, String>`
  - `set_setting(key: String, value: String)`
- Keys used: `currency_symbol`, `currency_code`, `currency_decimals`,
  `theme`, `locale`, `font_size`.
- Defaults when a key is absent: symbol `""`, code `""`, decimals `0`,
  theme `light`, locale `fr`, font size `medium`. (Currency starts blank so the
  user sets it explicitly in Preferences; formatting falls back to no symbol.)

### 2. Backend: Walk-in Customer party

- Seeded party: id `party_walkin`, name `"Walk-in Customer"`, kind `customer`,
  emitted as a normal `PartyCreated` event so it flows through the existing
  projector.
- Added to `genesis.rs` for fresh installs.
- Idempotently ensured on **startup** in `lib.rs`: after rehydrate, if no
  `PartyCreated` event for `party_walkin` exists **in the events table**, append
  it. The check queries the event log directly
  (`SELECT 1 FROM events WHERE type='PartyCreated' AND json_extract(payload,'$.partyId')='party_walkin'`),
  **not** the `parties` projection — the projection is empty/stale before
  `rebuild()` runs, and mirrors how genesis checks existence. This covers
  existing installs whose genesis predates this change. If appended, the
  subsequent `rebuild()` projects it (or, if seeding after rebuild, re-run
  rebuild / re-project the single new event).
- No change to `handle_sale_recorded`. The walk-in party is just a customer that
  always exists; cash sales to unknown buyers select it in the dropdown.
- The Sales form defaults the customer to `party_walkin` when terms = cash
  (user can override). The party is labelled/translated in the dropdown.

### 3. Backend: expose existing handlers via IPC

The core already implements and tests these; they are missing only from the
Tauri command layer:

- `record_payment_made(input)` → `handle_payment_made` — pay a supplier/creditor.
- `reverse_transaction(source_event_id, reason)` → `handle_transaction_reversed`.
- `list_payments()` — new query returning payments (id, party, direction,
  amount, date) for display and as reversal targets.
- Reversal targets an **event id**, not a projection row id. All three
  projection tables (`sales`, `purchases`, `payments`) already carry the
  creating event id in an `event_id TEXT NOT NULL` column (confirmed in
  `schema.sql`). No schema change or projector change is needed — the
  `list_*` queries simply add `event_id` to their SELECT and to the row struct
  returned over IPC. The frontend passes that `event_id` straight to
  `reverse_transaction`.

### 4. Frontend: Preferences page

- New nav entry `⚙️ Preferences` (translated). Added to `NAV_ORDER` / icons.
- Sections:
  - **Appearance** — theme selector (moved out of sidebar footer), font-size
    presets (Small/Medium/Large).
  - **Language** — locale selector (moved out of sidebar footer).
  - **Currency** — symbol, ISO code, decimal places (0/2). Live preview of a
    sample amount.
- Sidebar footer keeps only the collapse toggle. Theme/language quick-cycle
  buttons are removed (now in Preferences).
- On load, `get_settings()` hydrates the providers; on change, `set_setting()`
  persists to DB **and** updates the in-memory provider. localStorage is
  dropped in favour of DB as the source of truth, with a one-time migration:
  if DB has no value but localStorage does, seed DB from it.

### 5. Frontend: currency + font wiring

- `formatMoney(minor, { symbol, decimals })` — currency-aware. A `useCurrency()`
  hook/context reads settings and exposes a `format()` used by all pages
  (Dashboard, Sales, Purchases, Payments). Replaces the bare `formatMoney`
  call sites.
- Font size sets a CSS custom property (e.g. `--font-scale`) on `:root`;
  `body` font-size becomes `calc(16px * var(--font-scale))`. Presets map to
  `0.9 / 1.0 / 1.15`. Existing `rem`/`px` mobile rules are audited so scaling
  is proportional.

### 6. Frontend: pay-a-creditor + void/correct

- **Payments page** gains a direction toggle: *Received from customer* /
  *Paid to supplier*. The party dropdown filters by kind accordingly and calls
  `record_payment` or `record_payment_made`. A `list_payments` table shows
  history.
- **Void/Correct** — Sales, Purchases, and Payments list rows get an action:
  - *Void*: prompts for a reason, calls `reverse_transaction(event_id, reason)`.
  - *Edit* (= void + prefilled): reverses, then opens the create form
    pre-filled with the row's values for resubmission.
  - The backend rejects illegal reversals (e.g. a sale with an allocated
    payment, a purchase whose stock was partly sold). The UI surfaces the
    rejection as a toast and leaves the original intact.
  - **Voided rows disappear from lists**, but the mechanism differs by table:
    - `sales` and `purchases` carry a `reversed` column; the projector sets it
      and lists filter `WHERE reversed = 0`.
    - `payments` has **no** `reversed` column — the projector *deletes* the
      reversed payment row outright (confirmed in `projectors.rs`), so a voided
      payment simply vanishes from `list_payments`. This is acceptable: the
      reversing event stays in the log for reconciliation, and we do not need to
      show "voided" payment rows. We do **not** add a `reversed` column to
      `payments` (YAGNI — no UI requirement to display voided payments).

## Data flow

```
Preferences change → set_setting (DB) → provider state → CSS var / format()
Cash sale to unknown buyer → customer = party_walkin → handle_sale_recorded
Pay creditor → record_payment_made → handle_payment_made → PaymentMade event
Void/Edit → reverse_transaction(event_id) → TransactionReversed (compensating)
          → (Edit) prefilled create form → new corrected event
```

## Error handling

- `set_setting` validates the key against an allowlist; unknown keys rejected.
- `reverse_transaction` errors (illegal target, already reversed, downstream
  dependency) return the core's `CommandError::Validation` message, surfaced as
  an error toast. No partial writes (core guarantees atomicity).
- Currency decimals restricted to {0, 2}. `majorToMinor` respects the configured
  decimals so entry and display are consistent.

## Testing

- **Core:** unit test the settings store (get/set/overwrite, unknown-key
  rejection); test that startup idempotently seeds `party_walkin` and does not
  duplicate it on a second run.
- **Core:** the reversal + payment-made handlers already have coverage; add an
  event-id-lookup test.
- **Integration:** a Tauri-level round trip — set currency, read it back;
  record a supplier payment; void a sale and confirm it disappears from
  `list_sales` while the reversing event exists.
- **Manual:** desktop + Android — font presets scale text; currency symbol shows
  everywhere; walk-in cash sale; pay a creditor; void + edit a sale.

## Out of scope (YAGNI)

- Per-transaction / multi-currency with exchange rates.
- True in-place edit of ledger entries (correction is void + re-enter by design).
- Continuous font-size slider.
- Per-device preference sync across installs.

## Reviewer verification (yolo-reviewer, 2026-07-25 — APPROVED)

All open items resolved against the codebase:

- ✅ **Event id present.** `sales`, `purchases`, and `payments` each have
  `event_id TEXT NOT NULL` in `schema.sql`. No schema/projector change; just add
  it to the `list_*` SELECTs and row structs.
- ✅ **`app_settings` invisible to invariants.** All 8 reconciliation checks in
  `reconciliation.rs` read only ledger projection tables; none reference any
  settings table.
- ✅ **IPC gaps confirmed.** `record_payment` wires only `handle_payment_received`;
  no wrappers exist for `handle_payment_made`, `handle_transaction_reversed`, or
  `list_payments`.
- ✅ **`handle_payment_made` guard unaffected.** It requires party kind
  `supplier`; the walk-in party is a `customer`, so the two flows never collide.
- ⚠️ **Resolved — payments have no `reversed` column.** The projector deletes
  reversed payments; voided payments vanish from `list_payments` naturally (see §6).
- ⚠️ **Resolved — walk-in startup check queries the events table**, not the
  stale `parties` projection (see §2).
