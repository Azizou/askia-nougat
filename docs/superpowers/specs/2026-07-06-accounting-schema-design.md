# Local-First Accounting App — Data Model & Schema Design

**Date:** 2026-07-06
**Status:** Design approved (schema); pending implementation plan

## 1. Purpose & Scope

A **local-first, desktop accounting application** for a small trading business (buys and
sells physical goods). Ships as a single installer for Windows (built via CI), runs fully
offline, with the on-device store as the source of truth. Multi-device sync is a planned
future addition, not a day-one feature — but the data model is designed to be sync-ready
from the start.

This document specifies the **data model and schema only**. Runtime/packaging (Tauri +
`plugin-sql`), the sync engine, and UI are separate concerns covered elsewhere or later.

### Questions the model must answer
1. How many of item X were sold this month / last month.
2. How much profit (gross and net) was realized over a period (e.g. last 6 months).
3. How long we have held an item (inventory age), and value of aged stock.
4. Derived reports that fall out for free: current stock on hand, inventory valuation,
   gross margin per item, best/worst sellers, A/R & A/P and their aging, inventory
   turnover, P&L, and balance sheet.

## 2. Load-Bearing Decisions

These three decisions, made during brainstorming, drive the entire design. Changing any of
them later is expensive.

1. **Append-only event log is the source of truth (CQRS / event-sourced).** All state is
   derived by replaying immutable events in deterministic order. Read tables are disposable
   projections that can be dropped and rebuilt.
2. **Per-lot specific-identification inventory costing.** One inventory lot per
   purchase-of-an-item, each with its own frozen unit cost and acquisition date. A sale
   consumes quantity from specific lots. This is what makes exact COGS, realized profit, and
   inventory age all directly answerable. (Chosen over FIFO-layers and weighted-average.)
3. **COGS and revenue are frozen into the sale event at command time.** Profit is
   stored-at-sale, never recomputed at report time. This keeps history immutable and makes
   the model safe under future multi-device sync (a late-arriving purchase cannot rewrite a
   past sale's profit).

Supporting decisions:
- **Full double-entry bookkeeping** (not inventory-trading-only): a real chart of accounts,
  balanced debits/credits per transaction, producing P&L and balance sheet.
- **Both cash and credit settlement:** Accounts Payable (A/P) and Accounts Receivable (A/R)
  are first-class, so purchases/sales can settle immediately or on terms.
- **Money as integer minor units** (e.g. cents) everywhere. Never floats. Quantities are
  integers. Divide by 100 only for display.
- **Audit provenance in the event envelope:** every event carries `userId` (who) and
  `deviceId` (where), in addition to `hlc`/`seq`/`createdAt`. These are for audit and sync
  provenance, **not** an authorization mechanism.

## 3. Architecture Layering

```
Command  ──▶  Event log (append-only, source of truth)  ──▶  Projector  ──▶  Read model
Query    ◀──────────────────────────────────────────────────────────────────────┘
```

- **Write path:** a command handler validates, computes a balanced double-entry posting
  (and, for sales, freezes lot consumption + COGS), appends one immutable event, and applies
  it to the projection — atomically, in one SQLite transaction.
- **Read path:** queries hit denormalized projection tables only.
- **Reactivity:** after a successful commit, the handler emits a change signal to the
  frontend (Tauri event) so affected queries re-run.
- **Rebuildability:** every projection table is 100% derivable from `events` ordered by
  `hlc`. A `projection_cursor` records the last-applied `hlc` so projections can resume or be
  fully rebuilt.

### Ordering & sync-readiness
- Each event is stamped with a **Hybrid Logical Clock (HLC)** string that is lexically
  sortable and respects causality across devices. Replay is always `ORDER BY hlc ASC`, so all
  devices converge to an identical projection.
- Each event has a per-device gap-free `seq` for detecting missing events during sync.
- The live SQLite DB file never leaves the machine. Future sync exchanges **per-device
  append-only event segments** (each device is the sole writer of its own log file),
  merged via `INSERT OR IGNORE` on event `id` and replayed. No lock file, no whole-DB-file
  sync. (Sync engine is out of scope for this spec but the model is built for it.)

## 4. Event Store (Source of Truth)

### 4.1 Event envelope

Every event shares this envelope:

```ts
type EventId = string; // equals the hlc stamp verbatim — globally unique, sortable

interface LedgerEvent {
  id: EventId;        // = hlc (the HLC already encodes deviceId as its last segment)
  hlc: string;        // hybrid logical clock `{phys}:{ctr}:{deviceId}` (sort key — NOT createdAt)
  deviceId: string;   // where authored (sync + audit); also the hlc's trailing segment
  userId: string;     // who authored (audit) — references users(id)
  seq: number;        // per-device monotonic counter (gap detection)
  type: string;
  payload: unknown;   // event-specific, validated at app layer (e.g. Zod)
  createdAt: number;  // wall clock, for humans only — never used for ordering
}
```

### 4.2 Event store table

This is the single physical source of truth from which all projections rebuild:

```sql
CREATE TABLE events (
  id         TEXT PRIMARY KEY,          -- = hlc stamp (which already ends in deviceId); globally unique, sortable
  hlc        TEXT NOT NULL,
  device_id  TEXT NOT NULL,
  user_id    TEXT NOT NULL,
  seq        INTEGER NOT NULL,
  type       TEXT NOT NULL,
  payload    BLOB NOT NULL,             -- jsonb
  created_at INTEGER NOT NULL,
  UNIQUE (device_id, seq)               -- gap detection per device
);
CREATE INDEX events_hlc ON events (hlc);   -- replay order
```

### 4.3 Bootstrap / genesis

On first launch, the app emits a deterministic genesis sequence. All genesis events use:
- `userId: "system"` — a reserved, well-known id that exists by convention (no
  `UserRegistered` event creates it; it is a constant the projector recognizes).
- `deviceId`: this device's generated UUID.

Genesis sequence (order matters — each event can reference entities from prior events):
1. `UserRegistered` — the owner/operator of this installation.
2. `AccountOpened` × N — the seeded chart of accounts (see §5.2).
3. (Optional) `OpeningBalancesRecorded` — if migrating an existing business (see below).

After genesis, all subsequent events use the real owner's `userId`.

### 4.4 Event vocabulary

#### Setup / master data events (no journal posting)

| Event | Payload (essentials) | Notes |
|---|---|---|
| `UserRegistered` | userId, name, role? | Creates a row in `users`. First real user is seeded at genesis. |
| `AccountOpened` | accountId, name, type, normalSide | Creates a row in `accounts`. |
| `ItemDefined` | itemId, sku, name, unit | Creates a row in `items`. |
| `PartyCreated` | partyId, name, kind ('supplier'\|'customer'\|'both') | Creates a row in `parties`. Must precede any purchase/sale referencing this party. |

#### Master data mutation events (no journal posting)

| Event | Payload (essentials) | Notes |
|---|---|---|
| `UserUpdated` | userId, changes: {name?, role?} | Patches the `users` doc. |
| `AccountUpdated` | accountId, changes: {name?} | Patches the `accounts` doc. Type/normalSide are immutable once opened. |
| `ItemUpdated` | itemId, changes: {name?, sku?, unit?, active?} | Patches the `items` doc. Setting `active: false` deactivates the item. |
| `PartyUpdated` | partyId, changes: {name?, kind?} | Patches the `parties` doc. |

#### Transactional events (with journal postings)

| Event | Payload (essentials) | Implied journal posting |
|---|---|---|
| `PurchaseRecorded` | purchaseId, supplierId, date, terms, lines[]: {itemId, qty, unitCostMinor, lotId} | Dr Inventory / Cr Bank *or* A/P |
| `SaleRecorded` | saleId, customerId, date, terms, lines[]: {itemId, qty, unitPriceMinor, lotConsumption[]: {lotId, qtyTaken, unitCostMinor}} | Dr Bank/AR, Cr Sales **and** Dr COGS, Cr Inventory |
| `PaymentMade` | paymentId, supplierId, amountMinor, date, allocations[]: {purchaseId, amountMinor} | Dr A/P / Cr Bank |
| `PaymentReceived` | paymentId, customerId, amountMinor, date, allocations[]: {saleId, amountMinor} | Dr Bank / Cr A/R |
| `PaymentAllocated` | paymentId, partyId, allocations[]: {targetId, targetType, amountMinor}, date | — (no journal posting; applies an existing unallocated credit to invoices) |
| `ExpenseRecorded` | expenseId, accountId, amountMinor, date, terms, memo? | Dr Expense / Cr Bank *or* A/P |
| `TransferRecorded` | transferId, fromAccountId, toAccountId, amountMinor, date, memo? | Dr toAccount / Cr fromAccount |
| `InventoryAdjusted` | adjustmentId, date, lines[]: {itemId, lotId, qtyDelta (negative only), reasonCode, expenseAccountId} | Dr expenseAccountId / Cr Inventory (write-down of an existing lot) |
| `InventoryFound` | foundId, date, lines[]: {itemId, lotId, qty, unitCostMinor, acquiredAt, incomeAccountId} | Dr Inventory / Cr `incomeAccountId` (defaults to `system_role = 'inventory_gain'`; creates a NEW lot for found stock) |
| `OpeningBalancesRecorded` | date, accountBalances[]: {accountId, debitMinor, creditMinor}, lots[]: {itemId, lotId, qty, unitCostMinor, acquiredAt, supplierId?} | Balanced debits/credits across all accounts; creates initial lots |
| `SaleReturnRecorded` | returnId, originalSaleId, date, lines[]: {itemId, qty, unitPriceMinor, lotReturns[]: {lotId, qtyReturned, unitCostMinor}} | Reverses revenue + restores inventory for returned units only |
| `PurchaseReturnRecorded` | returnId, originalPurchaseId, date, lines[]: {itemId, qty, lotId, unitCostMinor} | Reverses inventory + reduces A/P (or gets refund to bank) |
| `TransactionReversed` | targetEventId, reason | Fully negates the target's postings (use for complete voiding; for partial, use return events) |

### 4.5 Rules

- **Deterministic referenced IDs (rebuild invariant):** any projection row ID that a *later*
  event's frozen payload references must be a deterministic function of its source event, never
  projector-minted. Otherwise a drop-and-rebuild (§3, §10) re-mints different IDs and every
  frozen reference dangles. Concretely: **`lotId` is assigned by the command handler and carried
  in the creating event's payload** (`PurchaseRecorded`, `OpeningBalancesRecorded`,
  `InventoryFound`), so it survives rebuild unchanged. IDs that are *never* referenced by other
  events (e.g. `journal_lines.id`) may be derived deterministically as `${eventId}#${lineIndex}`.
- **`SaleRecorded.lines[].lotConsumption` is chosen at command time** (default oldest-lot-
  first, user-overridable since costing is specific-ID) and **frozen into the event**. COGS is
  computed then, not at replay.
- A sale emits **both** the revenue posting and the cost posting inside the single event, so
  they are atomic.
- **Corrections:** use `TransactionReversed` for full voidings. Use `SaleReturnRecorded` /
  `PurchaseReturnRecorded` for partial returns (returns specific units to specific lots).
  Never a mutation or delete.
- **Master data updates** use patch semantics: the event carries only the changed fields, and
  the projector merges them into the existing `doc` JSONB. Type/normalSide on accounts are
  immutable once set (changing them would silently corrupt historical balances).
- **`InventoryAdjusted` is write-down only** (negative `qtyDelta`): it decrements an existing
  lot's `qty_remaining` and posts Dr `expenseAccountId` / Cr Inventory. `expenseAccountId` is
  carried per line so different reason codes hit different accounts (e.g. damage vs. shrinkage),
  defaulting to `system_role = 'shrinkage'`; `reasonCode` is for reporting. **Found stock is a
  separate event, `InventoryFound`**, which creates a *new* lot (with its own `lotId`,
  `unitCostMinor`, and `acquiredAt`) rather than inflating an existing one — this preserves the
  one-lot-one-cost-layer invariant (§2) and keeps reconciliation check #7
  (`qty_remaining <= qty_received`) universally true.
  `InventoryAdjusted.qtyDelta` must be strictly negative (found stock uses `InventoryFound`);
  the magnitude bound against `qty_remaining` is enforced by the unified oversell guard below.
- **`TransferRecorded`** handles any movement between accounts: cash → bank, bank → bank,
  or internal reclassifications. It's a simple balanced Dr/Cr between two accounts.
- **`OpeningBalancesRecorded`** is a one-time genesis event for existing businesses. It sets
  all account balances and creates initial inventory lots in a single balanced posting. The
  balancing account is typically Owner Capital / Retained Earnings. May only appear once in
  the event log (enforced by the command handler).
- **Payment allocations** may sum to *less* than the payment's `amountMinor`. The remainder
  is an unallocated credit (customer prepayment / supplier deposit). A later `PaymentAllocated`
  event applies that held credit to invoices when they arrive (it moves already-recorded money,
  so it posts no new journal lines — only `payment_allocations` rows and derived balances).
- **Account resolution:** every projector posting resolves well-known accounts by
  `system_role` (e.g. `WHERE system_role = 'inventory'`), never by name or generated id.

**Event categories (guards and projector contracts are phrased over these, not enumerated event names).** When a new
event type is added, it joins the relevant categories and inherits their guards by definition —
so a guard is never silently missed for a sibling event (the failure mode that recurred across
reviews v4–v6). Categories:
- **lot-creating:** creates `inventory_lots` rows — `PurchaseRecorded`, `OpeningBalancesRecorded`,
  `InventoryFound`.
- **lot-consuming:** decrements `qty_remaining` — `SaleRecorded`, `PurchaseReturnRecorded`,
  `InventoryAdjusted`.
- **lot-restoring:** increments `qty_remaining` on existing lots — `SaleReturnRecorded`.
- **allocation-bearing:** carries payment→invoice allocations — `PaymentMade`,
  `PaymentReceived`, `PaymentAllocated`.
- **transactional:** any event with a journal and/or inventory/settlement effect (everything
  except master-data events and `OpeningBalancesRecorded`); the legal `TransactionReversed`
  targets.

- **Oversell guard:** the command handler must reject any **lot-consuming** event whose per-lot
  quantity (`SaleRecorded.lotConsumption[].qtyTaken`, `PurchaseReturnRecorded.lines[].qty`,
  `|InventoryAdjusted.lines[].qtyDelta|`) exceeds the referenced lot's current `qty_remaining`.
  This is a validation rule, not recoverable at replay — no event may be written that would
  drive a lot negative. (Subsumes the former separate oversell / over-return / adjust-bounds
  guards, now unified over the lot-consuming category.)
- **Lot-source void guard:** the command handler must reject `TransactionReversed` targeting
  **any lot-creating event** (`PurchaseRecorded`, `InventoryFound`) if any lot it created has
  been partially or fully consumed (`qty_remaining < qty_received`). The lots can't be
  un-created while later events have drawn from them; the correct path is a return
  (`PurchaseReturnRecorded`) or a compensating `InventoryAdjusted`. (`OpeningBalancesRecorded`
  is not a legal reversal target, so it needs no such guard.)
- **Sale-return over-restore guard:** the command handler must reject `SaleReturnRecorded` if,
  for any lot, the returned qty exceeds the quantity the original sale consumed from that lot
  (per the original sale's `lot_consumptions`), or would raise the lot's `qty_remaining` above
  its `qty_received`. Each referenced `lotId` must be one the original sale actually consumed.
- **Invoice over-allocation guard:** in any **allocation-bearing** event, no allocation line's
  `amountMinor` may exceed the target invoice's current `outstanding_minor`. Prevents driving
  `outstanding_minor` negative.
- **Credit-overdraw guard:** `PaymentAllocated` must reject if the sum of its allocations
  exceeds the party's currently available unallocated credit (`unallocated_cr_minor` for a
  customer, `unallocated_dr_minor` for a supplier). Prevents allocating credit the party
  doesn't hold.
- **Payment-overallocation guard:** for `PaymentMade` / `PaymentReceived`, the sum of
  `allocations[].amountMinor` must not exceed the payment's own `amountMinor`; any remainder
  becomes unallocated credit. (Mirror of the credit-overdraw guard: that one bounds an
  allocation sum against *held* credit, this one bounds it against the *incoming* payment.)
  Without it, two lines can each pass the per-invoice guard yet jointly book more than was paid.
- **Allocation party-ownership guard:** in any **allocation-bearing** event, every allocation
  line must target an invoice that (a) belongs to the payment's party and (b) matches
  direction — a `sale` for inflows (`PaymentReceived`), a `purchase` for outflows
  (`PaymentMade`). Prevents one party's payment settling another party's invoice.
- **Reversal legal-target + double-void guards:** `TransactionReversed` may target **only
  transactional-category** events. It must reject targets outside that category — master-data
  events, `OpeningBalancesRecorded`, and `TransactionReversed` itself. The handler must also
  reject reversing an event that has already been reversed (track reversed target IDs),
  preventing double-negation.
- **Reversal downstream guard:** reject `TransactionReversed` whose target `T` has any blocking
  downstream dependency, so the user reverses dependents first (no implicit cascade), keeping
  `outstanding_minor` / `party_balances` / lot quantities consistent. `T` has a blocking
  dependency if any *later, not-yet-reversed* event:
  1. **allocates against an invoice `T` created** — a `payment_allocations` row whose
     `target_id` is `T`'s sale/purchase; **or**
  2. **is a return against `T`** — a `returns` row whose `original_id` is `T`; **or**
  3. **draws on unallocated credit `T` created** — a `PaymentAllocated` whose `payment_id` is
     `T`'s payment, when `T` is a `PaymentMade` / `PaymentReceived`; **or**
  4. **consumed a lot `T` created** — covered by the lot-source void guard above (listed here
     for completeness of the dependency relation).

  Edge (3) is the non-obvious one: a pure prepayment writes *zero* `payment_allocations` rows on
  itself, so a later `PaymentAllocated` that drew its credit is only discoverable by the reverse
  reference (`PaymentAllocated.payment_id → T`), not by allocations on `T`. Reversing `T` without
  first reversing that `PaymentAllocated` would drive the party's `unallocated_*` credit negative
  (breaks reconciliation check #8).
- **Value validation (all transactional events):** every line `qty > 0`; monetary amounts
  `>= 0` (and `> 0` where zero is meaningless, e.g. payment/expense/transfer amounts); every
  transactional event has at least one line.
- **Self-transfer guard:** `TransferRecorded` must reject `fromAccountId == toAccountId`.
- **Expense-account-type guard:** `ExpenseRecorded.accountId` must resolve to an `expense`-type
  account (otherwise the P&L is silently wrong).
- **Lot/item-match guard:** in any event that references a lot per line (**lot-consuming**,
  **lot-restoring**, and lot-creating events), each `lotId` must belong to the same `itemId`
  as its line. (The oversell guard checks quantity, not item identity.)
- **`TransactionReversed` contract (three-part):** on reversal the projector applies, as
  applicable to the target:
  1. **Financial:** apply the frozen reversal journal lines carried in the payload (computed
     at command time, like COGS). For `PaymentAllocated` targets this is a no-op — they post
     no journal lines.
  2. **Inventory:** apply the inventory *inverse of whatever the target's category did*,
     reading the target's own payload (not the sale-only `lot_consumptions`):
     - **lot-creating** target → remove/zero the lot(s) it created. The lot-source void guard
       already guarantees those lots are unconsumed, so this is safe and leaves no orphan lot
       counting toward check #1 or stock-on-hand.
     - **lot-consuming** target → restore `qty_remaining` by the consumed amount (sales via
       their `lot_consumptions` rows; `InventoryAdjusted` / `PurchaseReturnRecorded` via their
       payload `qtyDelta` / `qty`).
     - **lot-restoring** target → re-decrement `qty_remaining` by the amount it had restored.

     A target in none of these categories (e.g. `PaymentMade`, `TransferRecorded`) has no
     inventory effect and this clause is a no-op for it.
  3. **Allocation/settlement:** for **allocation-bearing** targets, delete the target's
     `payment_allocations` rows and reverse their effect — re-open the `outstanding_minor` they
     had settled and restore the party's `unallocated_*` credit and
     `receivable_minor` / `payable_minor`.

  Clause 3 mirrors clause 2 for the settlement dimension: reversing money movement must unwind
  what it settled, not just its Dr/Cr. Prefer `SaleReturnRecorded` / `PurchaseReturnRecorded`
  for partial/real-world corrections; narrow `TransactionReversed` to full same-day voids of
  erroneous entries.
- **Return → invoice/party-balance contract:** when a `SaleReturnRecorded` targets a **credit**
  sale that is still unpaid (or partially unpaid), the projector must reduce the original
  sale's `outstanding_minor` and the customer's `receivable_minor` by the returned revenue
  (capped at the remaining outstanding — any excess, if the sale was already paid, becomes an
  `unallocated_cr_minor` refund credit for the customer). Symmetrically, `PurchaseReturnRecorded`
  against an unpaid credit purchase reduces the purchase's `outstanding_minor` and the
  supplier's `payable_minor`. A return must never drive `outstanding_minor` below zero.

### 4.6 Date type rule (project-wide)

All **business dates** (when something happened in accounting terms) are stored as
`TEXT 'YYYY-MM-DD'`. This includes: `journal_lines.date`, `sales.date`, `purchases.date`,
`inventory_lots.acquired_at`, and all `date` fields in event payloads.

Only the event envelope's `createdAt` is an epoch-integer wall-clock (for the HLC / audit
trail). Never use epoch integers for business dates — they invite `julianday()` misuse and
conflate recording-time with business-time.

## 5. Master Data (Projections)

All money in integer minor units. All tables use the `doc` (JSONB) + generated-column
pattern: flexible JSON body, scalar columns projected out only where indexed/queried.

### 5.1 `users` — the *who*
Created by `UserRegistered`, updated by `UserUpdated`. The reserved `"system"` id is never
stored as a row — it's a constant the app recognizes for genesis-authored events.
```sql
CREATE TABLE users (
  id          TEXT PRIMARY KEY,
  doc         BLOB NOT NULL,                                 -- jsonb
  name        TEXT GENERATED ALWAYS AS (doc ->> 'name') VIRTUAL,
  created_at  INTEGER NOT NULL
);
```
Stable id referenced by every event's `userId`. One row today; multi-user later needs no
schema change.

### 5.2 `accounts` — chart of accounts
Created by `AccountOpened`, name updated by `AccountUpdated`. Type, normalSide, and
`system_role` are **immutable** once set (changing them would silently corrupt historical
balance calculations and projector lookups).
```sql
CREATE TABLE accounts (
  id            TEXT PRIMARY KEY,
  doc           BLOB NOT NULL,
  name          TEXT GENERATED ALWAYS AS (doc ->> 'name')        VIRTUAL,
  type          TEXT GENERATED ALWAYS AS (doc ->> 'type')        VIRTUAL,  -- asset|liability|equity|income|expense
  normal_side   TEXT GENERATED ALWAYS AS (doc ->> 'normal')      VIRTUAL,  -- 'debit'|'credit'
  system_role   TEXT GENERATED ALWAYS AS (doc ->> 'system_role') VIRTUAL,  -- nullable; well-known account identifier
  balance_minor INTEGER NOT NULL DEFAULT 0                                 -- running, maintained by projector
);
CREATE INDEX accounts_type ON accounts (type);
CREATE UNIQUE INDEX accounts_system_role ON accounts (system_role) WHERE system_role IS NOT NULL;
```
**`system_role`** is how the projector and queries reference well-known accounts without
coupling to user-renameable `name` or auto-generated `id`. Seeded roles (one per seeded
account):

| system_role | Seeded name | Type |
|---|---|---|
| `cash` | Cash | asset |
| `bank` | Bank | asset |
| `inventory` | Inventory | asset |
| `accounts_receivable` | Accounts Receivable | asset |
| `accounts_payable` | Accounts Payable | liability |
| `tax_payable` | Tax Payable | liability |
| `owner_capital` | Owner Capital | equity |
| `retained_earnings` | Retained Earnings | equity |
| `sales` | Sales | income |
| `cogs` | Cost of Goods Sold | expense |
| `shrinkage` | Inventory Shrinkage | expense |
| `inventory_gain` | Inventory Gain (overage) | income |
| `rent` | Rent | expense |
| `wages` | Wages | expense |

User-created accounts have `system_role: null`. All projector postings and system queries
resolve accounts via `WHERE system_role = ?`, never by name or id.

`normal_side` lets the projector apply balance changes uniformly: assets/expenses increase on
debit; liabilities/equity/income increase on credit.

### 5.3 `items` — catalog
Created by `ItemDefined`, updated by `ItemUpdated`. Deactivation is a soft-delete
(`active: false`) — historical events still reference the item; projections filter on `active`
for user-facing selectors but never hide it from reports.
```sql
CREATE TABLE items (
  id     TEXT PRIMARY KEY,
  doc    BLOB NOT NULL,
  sku    TEXT GENERATED ALWAYS AS (doc ->> 'sku')    VIRTUAL,
  name   TEXT GENERATED ALWAYS AS (doc ->> 'name')   VIRTUAL,
  unit   TEXT GENERATED ALWAYS AS (doc ->> 'unit')   VIRTUAL,
  active INTEGER GENERATED ALWAYS AS (doc ->> 'active') VIRTUAL
);
CREATE UNIQUE INDEX items_sku ON items (sku);
```
An item is a *definition* only — no price, quantity, or cost. Stock and cost live in lots,
because with per-lot costing the same item has many costs simultaneously.

### 5.4 `inventory_lots` — the crux
One row per purchase-of-an-item (or per opening-balance / found-stock lot). Created by
`PurchaseRecorded`, `OpeningBalancesRecorded`, or `InventoryFound`; drawn down by
`SaleRecorded`'s lot consumption or `InventoryAdjusted` write-downs; restored by
`SaleReturnRecorded` or `TransactionReversed`. The `lotId` is always assigned by the command
handler and frozen into the creating event (never projector-minted — see §4.5 rebuild invariant).
```sql
CREATE TABLE inventory_lots (
  id              TEXT PRIMARY KEY,
  item_id         TEXT NOT NULL REFERENCES items(id),
  source_event_id TEXT NOT NULL,             -- event that created this lot (any lot-creating type)
  purchase_id     TEXT,                      -- nullable: null for opening-balance lots
  unit_cost_minor INTEGER NOT NULL,          -- exact acquisition cost per unit (immutable)
  qty_received    INTEGER NOT NULL,          -- original quantity
  qty_remaining   INTEGER NOT NULL,          -- decremented as sales consume; never negative
  acquired_at     TEXT NOT NULL,             -- 'YYYY-MM-DD' business date → drives inventory AGE
  supplier_id     TEXT
);
CREATE INDEX lots_item_open ON inventory_lots (item_id, acquired_at)
  WHERE qty_remaining > 0;                    -- partial index: fast "oldest open lot" / on-hand
```
Carries the whole design: frozen `unit_cost_minor` → exact COGS; `qty_remaining` → stock on
hand and its value; `acquired_at` → age. **Aggregate lot value must always equal the Inventory
GL account balance (resolved via `system_role = 'inventory'`)** — the built-in integrity
check. `qty_remaining` must never go negative (enforced by the oversell guard in §4.5).

### 5.5 `parties` — suppliers & customers
Created by `PartyCreated`, updated by `PartyUpdated`. Must exist before any purchase/sale
references the party (the command handler enforces this).
```sql
CREATE TABLE parties (
  id   TEXT PRIMARY KEY,
  doc  BLOB NOT NULL,
  name TEXT GENERATED ALWAYS AS (doc ->> 'name') VIRTUAL,
  kind TEXT GENERATED ALWAYS AS (doc ->> 'kind') VIRTUAL   -- 'supplier'|'customer'|'both'
);
CREATE INDEX parties_kind ON parties (kind);
```

## 6. Transactional Read Model (Projections)

### 6.1 `journal_lines` — universal double-entry ledger
```sql
CREATE TABLE journal_lines (
  id           TEXT PRIMARY KEY,
  event_id     TEXT NOT NULL,              -- source event (→ audit: userId, deviceId, hlc)
  txn_id       TEXT NOT NULL,              -- groups the lines of one transaction
  account_id   TEXT NOT NULL REFERENCES accounts(id),
  debit_minor  INTEGER NOT NULL DEFAULT 0,
  credit_minor INTEGER NOT NULL DEFAULT 0,
  date         TEXT NOT NULL,             -- business date 'YYYY-MM-DD'
  memo         TEXT
);
CREATE INDEX jl_account_date ON journal_lines (account_id, date);
CREATE INDEX jl_txn          ON journal_lines (txn_id);
CREATE INDEX jl_date         ON journal_lines (date);
```
Invariant per `txn_id`: `SUM(debit_minor) = SUM(credit_minor)`. **`date` is the business date,
deliberately separate from `createdAt`/`hlc`** — a January invoice recorded in February must
report in January.

### 6.2 `sales` + `sale_lines` + `lot_consumptions` — profit engine
```sql
CREATE TABLE sales (
  id                TEXT PRIMARY KEY,
  event_id          TEXT NOT NULL,
  customer_id       TEXT,
  date              TEXT NOT NULL,
  terms             TEXT NOT NULL,       -- 'cash'|'credit'
  total_minor       INTEGER NOT NULL,
  outstanding_minor INTEGER NOT NULL DEFAULT 0  -- derived: total_minor − allocated; 0 for cash
);
CREATE INDEX sales_date ON sales (date);
CREATE INDEX sales_outstanding ON sales (outstanding_minor) WHERE outstanding_minor > 0;

CREATE TABLE sale_lines (
  id               TEXT PRIMARY KEY,
  sale_id          TEXT NOT NULL REFERENCES sales(id),
  item_id          TEXT NOT NULL REFERENCES items(id),
  qty              INTEGER NOT NULL,
  unit_price_minor INTEGER NOT NULL,      -- what customer paid per unit
  revenue_minor    INTEGER NOT NULL,      -- qty * unit_price (frozen)
  cogs_minor       INTEGER NOT NULL,      -- frozen cost of goods for this line
  date             TEXT NOT NULL          -- denormalized from sale for fast filtering
);
CREATE INDEX sl_item_date ON sale_lines (item_id, date);
CREATE INDEX sl_sale      ON sale_lines (sale_id);

CREATE TABLE lot_consumptions (
  id              TEXT PRIMARY KEY,
  sale_line_id    TEXT NOT NULL REFERENCES sale_lines(id),
  lot_id          TEXT NOT NULL REFERENCES inventory_lots(id),
  qty_taken       INTEGER NOT NULL,
  unit_cost_minor INTEGER NOT NULL        -- copied from the lot at sale time (frozen)
);
CREATE INDEX lc_lot ON lot_consumptions (lot_id);
```
Realized profit per line = `revenue_minor - cogs_minor` (a subtraction, never a
recomputation). `lot_consumptions` records exactly which lots filled each sale, enabling
traceability and age-at-sale analysis.

### 6.3 `purchases` + `purchase_lines`
```sql
CREATE TABLE purchases (
  id                TEXT PRIMARY KEY,
  event_id          TEXT NOT NULL,
  supplier_id       TEXT,
  date              TEXT NOT NULL,
  terms             TEXT NOT NULL,
  total_minor       INTEGER NOT NULL,
  outstanding_minor INTEGER NOT NULL DEFAULT 0  -- derived: total_minor − allocated; 0 for cash
);
CREATE INDEX purchases_date ON purchases (date);
CREATE INDEX purchases_outstanding ON purchases (outstanding_minor) WHERE outstanding_minor > 0;
CREATE TABLE purchase_lines (
  id              TEXT PRIMARY KEY,
  purchase_id     TEXT NOT NULL REFERENCES purchases(id),
  item_id         TEXT NOT NULL REFERENCES items(id),
  qty             INTEGER NOT NULL,
  unit_cost_minor INTEGER NOT NULL,
  lot_id          TEXT NOT NULL REFERENCES inventory_lots(id)  -- the lot this line created
);
CREATE INDEX pl_purchase ON purchase_lines (purchase_id);
CREATE INDEX pl_item     ON purchase_lines (item_id);
```
Each purchase line creates exactly one `inventory_lot` — the birth of a cost layer.

### 6.4 `payments` — payment records
```sql
CREATE TABLE payments (
  id           TEXT PRIMARY KEY,
  event_id     TEXT NOT NULL,
  party_id     TEXT NOT NULL,
  direction    TEXT NOT NULL,          -- 'in' (received) | 'out' (made)
  amount_minor INTEGER NOT NULL,
  date         TEXT NOT NULL
);
CREATE INDEX payments_party_date ON payments (party_id, date);
CREATE INDEX payments_date        ON payments (date);
```
One row per `PaymentMade` / `PaymentReceived`. Needed because a **pure prepayment** (a payment
with no allocations) writes zero `payment_allocations` rows and would otherwise be invisible to
any allocation-based query. Enables "list all payments with date/amount/party" without scanning
`journal_lines`. (`PaymentAllocated` does not create a `payments` row — it only moves an
existing payment's credit, recorded in `payment_allocations`.)

### 6.5 `payment_allocations` — which payment settles which invoice
```sql
CREATE TABLE payment_allocations (
  id              TEXT PRIMARY KEY,
  event_id        TEXT NOT NULL,              -- event that recorded THIS row (PaymentMade/Received, or a later PaymentAllocated)
  payment_id      TEXT NOT NULL,              -- the source payment whose money/credit is applied
  target_id       TEXT NOT NULL,              -- the purchase or sale being settled
  target_type     TEXT NOT NULL,              -- 'purchase'|'sale'
  amount_minor    INTEGER NOT NULL
);
CREATE INDEX pa_payment ON payment_allocations (payment_id);
CREATE INDEX pa_target  ON payment_allocations (target_id);
```
This is the authoritative mechanism for determining which invoices are outstanding. A
payment's `allocations` may sum to less than `amountMinor` — the remainder is an unallocated
party credit (prepayment / deposit) that can be applied to future invoices.

The dual reference is deliberate: `payment_id` points to the source payment that holds the
money/credit, while `event_id` points to the event that recorded *this specific allocation
row*. For allocations made at payment time these are the same underlying payment event; for a
later `PaymentAllocated` (applying a held credit), `payment_id` is the original payment and
`event_id` is the `PaymentAllocated` event.

### 6.6 `party_balances` — A/R & A/P (derived convenience)
```sql
CREATE TABLE party_balances (
  party_id             TEXT PRIMARY KEY REFERENCES parties(id),
  receivable_minor     INTEGER NOT NULL DEFAULT 0,   -- customer owes us
  payable_minor        INTEGER NOT NULL DEFAULT 0,   -- we owe supplier
  unallocated_cr_minor INTEGER NOT NULL DEFAULT 0,   -- customer prepayments (credits we hold)
  unallocated_dr_minor INTEGER NOT NULL DEFAULT 0    -- supplier prepayments (deposits we've paid)
);
```
Increased by credit `SaleRecorded` / `PurchaseRecorded`, decreased by allocated portions of
`PaymentReceived` / `PaymentMade`. Unallocated columns track prepayments/deposits. Fully
derivable from `payment_allocations` + invoices, so it rebuilds on replay — no drift.

### 6.7 `sales` / `purchases` outstanding tracking
`sales.outstanding_minor` and `purchases.outstanding_minor` (defined in the CREATE TABLE
statements above) are set to `total_minor` on creation for credit-terms invoices (0 for
cash), and decremented by the projector as `payment_allocations` arrive. This is a **derived
convenience** — always re-derivable from `total_minor - SUM(allocated amounts)`. Enables fast
"unpaid invoices" and aging queries without joins (see the partial indexes above).

### 6.8 `returns` + `return_lines` — return tracking
```sql
CREATE TABLE returns (
  id                    TEXT PRIMARY KEY,
  event_id              TEXT NOT NULL,
  return_type           TEXT NOT NULL,      -- 'sale_return'|'purchase_return'
  original_id           TEXT NOT NULL,      -- the sale or purchase being returned against
  date                  TEXT NOT NULL,
  revenue_reversed_minor INTEGER NOT NULL DEFAULT 0,  -- sale returns: qty × original sale price
  cost_restored_minor    INTEGER NOT NULL DEFAULT 0   -- inventory value put back at lot cost
);
CREATE INDEX returns_original ON returns (original_id);

CREATE TABLE return_lines (
  id               TEXT PRIMARY KEY,
  return_id        TEXT NOT NULL REFERENCES returns(id),
  item_id          TEXT NOT NULL REFERENCES items(id),
  qty              INTEGER NOT NULL,
  unit_price_minor INTEGER NOT NULL DEFAULT 0,  -- frozen from original sale (sale returns); 0 for purchase returns
  unit_cost_minor  INTEGER NOT NULL,            -- the lot cost restored
  lot_id           TEXT NOT NULL REFERENCES inventory_lots(id)
);
CREATE INDEX rl_return ON return_lines (return_id);
CREATE INDEX rl_item   ON return_lines (item_id);
```
`revenue_reversed_minor` (customer-facing refund, from original sale price) and
`cost_restored_minor` (inventory value returned at lot cost) are tracked separately because
for a sale return they differ. Purchase returns set `revenue_reversed_minor = 0`.
`return_lines.unit_price_minor` is frozen from the original sale at command time, so revenue
reversal needs no cross-event lookup — consistent with the freeze-at-command-time philosophy.
Enables return-rate-by-item and total-refunds-per-period queries without scanning the event log.

### 6.9 `expenses` — expense tracking
```sql
CREATE TABLE expenses (
  id           TEXT PRIMARY KEY,
  event_id     TEXT NOT NULL,
  account_id   TEXT NOT NULL REFERENCES accounts(id),  -- which expense account (rent, wages, etc.)
  amount_minor INTEGER NOT NULL,
  date         TEXT NOT NULL,
  memo         TEXT,
  terms        TEXT NOT NULL              -- 'cash'|'credit'
);
CREATE INDEX expenses_date    ON expenses (date);
CREATE INDEX expenses_account ON expenses (account_id, date);
```
Provides "list all expenses this month with memo and category" without routing through
`journal_lines` joins. Fully derivable from `ExpenseRecorded` events.

### 6.10 `projection_cursor` — replay bookmark
```sql
CREATE TABLE projection_cursor (
  projection TEXT PRIMARY KEY,
  last_hlc   TEXT NOT NULL
);
```

## 7. Reconciliation & Integrity Checks

Deliberate redundancy between the journal and the specialized tables is a feature — the two
must agree, which catches bugs:
1. **Inventory valuation:** `SUM(qty_remaining * unit_cost_minor)` over open lots must equal
   the Inventory GL account balance (account with `system_role = 'inventory'`).
2. **Gross profit:** `SUM(revenue_minor - cogs_minor)` over `sale_lines` must equal
   (Sales − COGS) from `journal_lines` over the same period (accounts with
   `system_role IN ('sales', 'cogs')`).
3. **Double-entry:** for every `txn_id`, `SUM(debit_minor) = SUM(credit_minor)`.
4. **Party balances:** `party_balances` must equal the per-party A/R and A/P balances derived
   from `journal_lines` (accounts with `system_role IN ('accounts_receivable', 'accounts_payable')`).
5. **Invoice outstanding:** for each sale,
   `outstanding_minor = MAX(0, total_minor − allocated − returned)` where
   `allocated = SUM(payment_allocations.amount_minor WHERE target_id = sale.id)` and
   `returned = SUM(returns.revenue_reversed_minor WHERE original_id = sale.id)`. The identical
   identity holds for purchases (using `payable`/`cost_restored_minor`). Must be `>= 0`.
6. **Non-negative inventory:** `inventory_lots.qty_remaining >= 0` for all rows (enforced at
   command time by the oversell guard, verified here as a backstop).
7. **Lot bounds:** `0 <= inventory_lots.qty_remaining <= inventory_lots.qty_received` for all
   rows (backstops the sale-return over-restore guard).
8. **Non-negative credits:** `party_balances.unallocated_cr_minor >= 0` and
   `unallocated_dr_minor >= 0` (backstops the credit-overdraw guard).

A background/periodic check can assert these and flag drift (which would indicate a projector
bug, since projections are derived).

## 8. Target Queries (validation of the design)

### 8.1 Units of item X sold this / last month
```sql
SELECT strftime('%Y-%m', date) AS month,
       SUM(qty) AS units_sold, SUM(revenue_minor) AS revenue_minor
FROM sale_lines
WHERE item_id = :itemX
  AND date >= date('now','start of month','-1 month')
GROUP BY month ORDER BY month;
```

### 8.2 Realized profit over last 6 months
Gross:
```sql
SELECT SUM(revenue_minor) AS revenue, SUM(cogs_minor) AS cogs,
       SUM(revenue_minor - cogs_minor) AS gross_profit_minor
FROM sale_lines WHERE date >= date('now','-6 months');
```
Net (gross minus non-COGS operating expenses from the journal):
```sql
SELECT
  (SELECT SUM(revenue_minor - cogs_minor) FROM sale_lines
     WHERE date >= date('now','-6 months'))
  -
  (SELECT SUM(debit_minor - credit_minor) FROM journal_lines jl
     JOIN accounts a ON a.id = jl.account_id
     WHERE a.type = 'expense' AND a.system_role IS NOT 'cogs'
       AND jl.date >= date('now','-6 months'))
  AS net_profit_minor;
```
Note: queries reference accounts by `system_role`, never by `name` (which is user-renameable).
**Use the null-safe `IS NOT` operator, not `<>`**: user-created expense accounts have
`system_role = NULL`, and `NULL <> 'cogs'` evaluates to NULL (excluding the row), which would
silently drop all user-defined expense categories. `NULL IS NOT 'cogs'` correctly returns true.

### 8.3 Inventory age
Per open lot for item X:
```sql
SELECT id AS lot_id, qty_remaining, unit_cost_minor,
       CAST(julianday('now') - julianday(acquired_at) AS INT) AS age_days,
       qty_remaining * unit_cost_minor AS value_on_hand_minor
FROM inventory_lots
WHERE item_id = :itemX AND qty_remaining > 0
ORDER BY acquired_at;
```
Aging report (dead-stock detector):
```sql
SELECT CASE
    WHEN julianday('now') - julianday(acquired_at) <= 30  THEN '0-30d'
    WHEN julianday('now') - julianday(acquired_at) <= 90  THEN '31-90d'
    WHEN julianday('now') - julianday(acquired_at) <= 180 THEN '91-180d'
    ELSE '180d+' END AS bucket,
  SUM(qty_remaining) AS units, SUM(qty_remaining * unit_cost_minor) AS value_minor
FROM inventory_lots WHERE qty_remaining > 0 GROUP BY bucket;
```

### 8.4 Reports that fall out for free
- **Stock on hand:** `SELECT item_id, SUM(qty_remaining) FROM inventory_lots WHERE qty_remaining>0 GROUP BY item_id`.
- **Inventory valuation:** `SUM(qty_remaining * unit_cost_minor)` (reconciles with Inventory GL via `system_role = 'inventory'`).
- **Gross margin % per item:** `SUM(revenue-cogs)/SUM(revenue)` from `sale_lines` by item.
- **Best/worst sellers:** `sale_lines` by item, ordered by `SUM(qty)` or profit.
- **Who owes us / whom we owe:** `party_balances` (total and unallocated credits).
- **A/R & A/P aging:** `SELECT * FROM sales WHERE terms='credit' AND outstanding_minor > 0` bucketed by `date` — works because `outstanding_minor` is maintained by the projector from `payment_allocations`.
- **Return rate per item:** `SUM(return_lines.qty) / SUM(sale_lines.qty)` grouped by item.
- **Age-at-sale / turnover:** `lot_consumptions → inventory_lots.acquired_at` vs `sales.date`.
- **P&L / balance sheet:** `journal_lines` grouped by `accounts.type` over a date range.

## 9. Out of Scope (this spec)

- Sync engine implementation (per-device log exchange, merge/replay loop, HLC `observe`).
- Runtime/packaging (Tauri, `plugin-sql`), CI build, code signing.
- UI / frontend.
- Authorization / multi-user access control (the `userId`/`deviceId` fields are audit
  provenance only, not a security boundary).
- Multi-currency (single currency assumed; all amounts one currency in minor units).
- Tax computation logic (a Tax Payable account exists; how tax is calculated is deferred).
- **Partial cash/credit split on one invoice** (known limitation): `terms` is binary
  (`'cash'|'credit'`), so an invoice that is part-paid-now and part-on-terms is not
  representable as a single event. Workaround: record a credit sale followed by an immediate
  partial payment. If this becomes common, model `terms` as a paid-now amount instead of an
  enum.

## 10. Open Questions / Future Considerations

- **Semantic sync conflicts** (e.g. two offline devices both drawing the last unit of a lot):
  resolved by the reversing-entry discipline plus a user-facing reconciliation step — to be
  designed when the sync engine is built. Physical convergence is already guaranteed by the
  event-log model; semantic conflicts are a business decision, not an auto-merge.
- **Incremental vs full projection rebuild** on sync merge: start with full rebuild for
  correctness; optimize to incremental later if needed.
- **Multi-currency and tax computation**: deferred; the account structure leaves room for both.
- **Discounts / promotions**: not modeled. If needed, add a `discount_minor` field to
  `sale_lines` and adjust the revenue posting accordingly. Or handle as a price adjustment
  (different `unitPriceMinor`).
- **Inventory write-down / revaluation**: `InventoryAdjusted` handles quantity adjustments.
  Value write-downs (marking inventory down without physical loss) would need a separate
  `InventoryRevalued` event if required later — not common for specific-ID costing where each
  lot holds its actual cost.
