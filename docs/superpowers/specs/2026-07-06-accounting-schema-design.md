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

## 4. Event Vocabulary (Source of Truth)

Every event shares this envelope:

```ts
type EventId = string; // `${hlc}-${deviceId}` — globally unique, sortable

interface LedgerEvent {
  id: EventId;
  hlc: string;        // hybrid logical clock (sort key — NOT createdAt)
  deviceId: string;   // where authored (sync + audit)
  userId: string;     // who authored (audit) — references users(id)
  seq: number;        // per-device monotonic counter (gap detection)
  type: string;
  payload: unknown;   // event-specific, validated at app layer (e.g. Zod)
  createdAt: number;  // wall clock, for humans only — never used for ordering
}
```

| Event | Payload (essentials) | Implied journal posting |
|---|---|---|
| `AccountOpened` | accountId, name, type, normalSide | — (chart of accounts setup) |
| `ItemDefined` | itemId, sku, name, unit | — (master data) |
| `PurchaseRecorded` | purchaseId, supplierId, date, terms, lines[]: {itemId, qty, unitCostMinor} | Dr Inventory / Cr Bank *or* A/P |
| `SaleRecorded` | saleId, customerId, date, terms, lines[]: {itemId, qty, unitPriceMinor, lotConsumption[]: {lotId, qtyTaken, unitCostMinor}} | Dr Bank/AR, Cr Sales **and** Dr COGS, Cr Inventory |
| `PaymentMade` | paymentId, supplierId, amountMinor, date | Dr A/P / Cr Bank |
| `PaymentReceived` | paymentId, customerId, amountMinor, date | Dr Bank / Cr A/R |
| `ExpenseRecorded` | expenseId, accountId, amountMinor, date | Dr Expense / Cr Bank *or* A/P |
| `TransactionReversed` | targetEventId, reason | negates the target's postings |

Rules:
- **`SaleRecorded.lines[].lotConsumption` is chosen at command time** (default oldest-lot-
  first, user-overridable since costing is specific-ID) and **frozen into the event**. COGS is
  computed then, not at replay.
- A sale emits **both** the revenue posting and the cost posting inside the single event, so
  they are atomic.
- **Corrections are always `TransactionReversed` + a new event**, never a mutation or delete.

## 5. Master Data (Projections)

All money in integer minor units. All tables use the `doc` (JSONB) + generated-column
pattern: flexible JSON body, scalar columns projected out only where indexed/queried.

### 5.1 `users` — the *who*
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
```sql
CREATE TABLE accounts (
  id          TEXT PRIMARY KEY,
  doc         BLOB NOT NULL,
  name        TEXT GENERATED ALWAYS AS (doc ->> 'name')   VIRTUAL,
  type        TEXT GENERATED ALWAYS AS (doc ->> 'type')   VIRTUAL,  -- asset|liability|equity|income|expense
  normal_side TEXT GENERATED ALWAYS AS (doc ->> 'normal') VIRTUAL,  -- 'debit'|'credit'
  balance_minor INTEGER NOT NULL DEFAULT 0                          -- running, maintained by projector
);
CREATE INDEX accounts_type ON accounts (type);
```
`normal_side` lets the projector apply balance changes uniformly: assets/expenses increase on
debit; liabilities/equity/income increase on credit. Seeded standard accounts at first launch:
Cash, Bank, Inventory, Accounts Receivable (assets); Accounts Payable, Tax Payable
(liabilities); Owner Capital (equity); Sales (income); COGS, Rent, Wages (expenses).

### 5.3 `items` — catalog
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
One row per purchase-of-an-item. Created by `PurchaseRecorded`, drawn down by
`SaleRecorded`'s lot consumption.
```sql
CREATE TABLE inventory_lots (
  id              TEXT PRIMARY KEY,
  item_id         TEXT NOT NULL REFERENCES items(id),
  purchase_id     TEXT NOT NULL,             -- the PurchaseRecorded event that created it
  unit_cost_minor INTEGER NOT NULL,          -- exact acquisition cost per unit (immutable)
  qty_received    INTEGER NOT NULL,          -- original quantity
  qty_remaining   INTEGER NOT NULL,          -- decremented as sales consume it
  acquired_at     INTEGER NOT NULL,          -- purchase date → drives inventory AGE
  supplier_id     TEXT
);
CREATE INDEX lots_item_open ON inventory_lots (item_id, acquired_at)
  WHERE qty_remaining > 0;                    -- partial index: fast "oldest open lot" / on-hand
```
Carries the whole design: frozen `unit_cost_minor` → exact COGS; `qty_remaining` → stock on
hand and its value; `acquired_at` → age. **Aggregate lot value must always equal the Inventory
GL account balance** — the built-in integrity check.

### 5.5 `parties` — suppliers & customers
```sql
CREATE TABLE parties (
  id   TEXT PRIMARY KEY,
  doc  BLOB NOT NULL,
  name TEXT GENERATED ALWAYS AS (doc ->> 'name') VIRTUAL,
  kind TEXT GENERATED ALWAYS AS (doc ->> 'kind') VIRTUAL   -- 'supplier'|'customer'|'both'
);
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
  id          TEXT PRIMARY KEY,
  event_id    TEXT NOT NULL,
  customer_id TEXT,
  date        TEXT NOT NULL,
  terms       TEXT NOT NULL,             -- 'cash'|'credit'
  total_minor INTEGER NOT NULL
);
CREATE INDEX sales_date ON sales (date);

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
  id          TEXT PRIMARY KEY,
  event_id    TEXT NOT NULL,
  supplier_id TEXT,
  date        TEXT NOT NULL,
  terms       TEXT NOT NULL,
  total_minor INTEGER NOT NULL
);
CREATE TABLE purchase_lines (
  id              TEXT PRIMARY KEY,
  purchase_id     TEXT NOT NULL REFERENCES purchases(id),
  item_id         TEXT NOT NULL REFERENCES items(id),
  qty             INTEGER NOT NULL,
  unit_cost_minor INTEGER NOT NULL,
  lot_id          TEXT NOT NULL REFERENCES inventory_lots(id)  -- the lot this line created
);
```
Each purchase line creates exactly one `inventory_lot` — the birth of a cost layer.

### 6.4 `party_balances` — A/R & A/P
```sql
CREATE TABLE party_balances (
  party_id         TEXT PRIMARY KEY REFERENCES parties(id),
  receivable_minor INTEGER NOT NULL DEFAULT 0,   -- customer owes us
  payable_minor    INTEGER NOT NULL DEFAULT 0    -- we owe supplier
);
```
Increased by credit `SaleRecorded` / `PurchaseRecorded`, decreased by `PaymentReceived` /
`PaymentMade`. Fully derivable, so it rebuilds on replay — no drift.

### 6.5 `projection_cursor` — replay bookmark
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
   the Inventory GL account balance.
2. **Gross profit:** `SUM(revenue_minor - cogs_minor)` over `sale_lines` must equal
   (Sales − COGS) from `journal_lines` over the same period.
3. **Double-entry:** for every `txn_id`, `SUM(debit_minor) = SUM(credit_minor)`.
4. **Party balances:** `party_balances` must equal the per-party A/R and A/P balances derived
   from `journal_lines`.

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
     WHERE a.type = 'expense' AND a.name <> 'COGS'
       AND jl.date >= date('now','-6 months'))
  AS net_profit_minor;
```

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
- **Inventory valuation:** `SUM(qty_remaining * unit_cost_minor)` (reconciles with Inventory GL).
- **Gross margin % per item:** `SUM(revenue-cogs)/SUM(revenue)` from `sale_lines` by item.
- **Best/worst sellers:** `sale_lines` by item, ordered by `SUM(qty)` or profit.
- **Who owes us / whom we owe:** `party_balances`.
- **A/R & A/P aging:** unpaid `sales`/`purchases` bucketed by `date`.
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

## 10. Open Questions / Future Considerations

- **Semantic sync conflicts** (e.g. two offline devices both drawing the last unit of a lot):
  resolved by the reversing-entry discipline plus a user-facing reconciliation step — to be
  designed when the sync engine is built. Physical convergence is already guaranteed by the
  event-log model; semantic conflicts are a business decision, not an auto-merge.
- **Incremental vs full projection rebuild** on sync merge: start with full rebuild for
  correctness; optimize to incremental later if needed.
- **Multi-currency and tax computation**: deferred; the account structure leaves room for both.
