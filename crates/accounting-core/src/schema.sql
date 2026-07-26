-- Source of truth: append-only, immutable event log.
CREATE TABLE IF NOT EXISTS events (
  id         TEXT PRIMARY KEY,
  hlc        TEXT NOT NULL,
  device_id  TEXT NOT NULL,
  user_id    TEXT NOT NULL,
  seq        INTEGER NOT NULL,
  type       TEXT NOT NULL,
  payload    BLOB NOT NULL,
  created_at INTEGER NOT NULL,
  UNIQUE (device_id, seq)
);
CREATE INDEX IF NOT EXISTS events_hlc ON events (hlc);

-- Replay bookmark: how far each projection has been applied.
CREATE TABLE IF NOT EXISTS projection_cursor (
  projection TEXT PRIMARY KEY,
  last_hlc   TEXT NOT NULL
);

-- ============================================================
-- Application settings (NOT event-sourced; ignored by rebuild).
-- Holds current-state UI/business configuration, not ledger facts.
-- ============================================================
CREATE TABLE IF NOT EXISTS app_settings (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

-- ============================================================
-- Master data projections (spec §5)
-- ============================================================

-- §5.1 users
CREATE TABLE IF NOT EXISTS users (
  id          TEXT PRIMARY KEY,
  doc         BLOB NOT NULL,
  name        TEXT GENERATED ALWAYS AS (doc ->> 'name') VIRTUAL,
  created_at  INTEGER NOT NULL
);

-- §5.2 accounts
CREATE TABLE IF NOT EXISTS accounts (
  id            TEXT PRIMARY KEY,
  doc           BLOB NOT NULL,
  name          TEXT GENERATED ALWAYS AS (doc ->> 'name')        VIRTUAL,
  type          TEXT GENERATED ALWAYS AS (doc ->> 'type')        VIRTUAL,
  normal_side   TEXT GENERATED ALWAYS AS (doc ->> 'normal')      VIRTUAL,
  system_role   TEXT GENERATED ALWAYS AS (doc ->> 'system_role') VIRTUAL,
  balance_minor INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS accounts_type ON accounts (type);
CREATE UNIQUE INDEX IF NOT EXISTS accounts_system_role ON accounts (system_role) WHERE system_role IS NOT NULL;

-- §5.3 items
CREATE TABLE IF NOT EXISTS items (
  id     TEXT PRIMARY KEY,
  doc    BLOB NOT NULL,
  sku    TEXT GENERATED ALWAYS AS (doc ->> 'sku')    VIRTUAL,
  name   TEXT GENERATED ALWAYS AS (doc ->> 'name')   VIRTUAL,
  unit   TEXT GENERATED ALWAYS AS (doc ->> 'unit')   VIRTUAL,
  active INTEGER GENERATED ALWAYS AS (doc ->> 'active') VIRTUAL
);
CREATE UNIQUE INDEX IF NOT EXISTS items_sku ON items (sku);

-- §5.4 inventory_lots
CREATE TABLE IF NOT EXISTS inventory_lots (
  id              TEXT PRIMARY KEY,
  item_id         TEXT NOT NULL REFERENCES items(id),
  source_event_id TEXT NOT NULL,
  purchase_id     TEXT,
  unit_cost_minor INTEGER NOT NULL,
  qty_received    INTEGER NOT NULL,
  qty_remaining   INTEGER NOT NULL,
  acquired_at     TEXT NOT NULL,
  supplier_id     TEXT
);
CREATE INDEX IF NOT EXISTS lots_item_open ON inventory_lots (item_id, acquired_at)
  WHERE qty_remaining > 0;

-- §5.5 parties
CREATE TABLE IF NOT EXISTS parties (
  id   TEXT PRIMARY KEY,
  doc  BLOB NOT NULL,
  name TEXT GENERATED ALWAYS AS (doc ->> 'name') VIRTUAL,
  kind TEXT GENERATED ALWAYS AS (doc ->> 'kind') VIRTUAL
);
CREATE INDEX IF NOT EXISTS parties_kind ON parties (kind);

-- ============================================================
-- Transactional read model (spec §6)
-- ============================================================

-- §6.1 journal_lines
CREATE TABLE IF NOT EXISTS journal_lines (
  id           TEXT PRIMARY KEY,
  event_id     TEXT NOT NULL,
  txn_id       TEXT NOT NULL,
  account_id   TEXT NOT NULL REFERENCES accounts(id),
  debit_minor  INTEGER NOT NULL DEFAULT 0,
  credit_minor INTEGER NOT NULL DEFAULT 0,
  date         TEXT NOT NULL,
  memo         TEXT
);
CREATE INDEX IF NOT EXISTS jl_account_date ON journal_lines (account_id, date);
CREATE INDEX IF NOT EXISTS jl_txn          ON journal_lines (txn_id);
CREATE INDEX IF NOT EXISTS jl_date         ON journal_lines (date);

-- §6.2 sales + sale_lines + lot_consumptions
CREATE TABLE IF NOT EXISTS sales (
  id                TEXT PRIMARY KEY,
  event_id          TEXT NOT NULL,
  customer_id       TEXT,
  date              TEXT NOT NULL,
  terms             TEXT NOT NULL,
  total_minor       INTEGER NOT NULL,
  outstanding_minor INTEGER NOT NULL DEFAULT 0,
  reversed          INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS sales_date ON sales (date);
CREATE INDEX IF NOT EXISTS sales_outstanding ON sales (outstanding_minor) WHERE outstanding_minor > 0;

CREATE TABLE IF NOT EXISTS sale_lines (
  id               TEXT PRIMARY KEY,
  sale_id          TEXT NOT NULL REFERENCES sales(id),
  item_id          TEXT NOT NULL REFERENCES items(id),
  qty              INTEGER NOT NULL,
  unit_price_minor INTEGER NOT NULL,
  revenue_minor    INTEGER NOT NULL,
  cogs_minor       INTEGER NOT NULL,
  date             TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS sl_item_date ON sale_lines (item_id, date);
CREATE INDEX IF NOT EXISTS sl_sale      ON sale_lines (sale_id);

CREATE TABLE IF NOT EXISTS lot_consumptions (
  id              TEXT PRIMARY KEY,
  sale_line_id    TEXT NOT NULL REFERENCES sale_lines(id),
  lot_id          TEXT NOT NULL REFERENCES inventory_lots(id),
  qty_taken       INTEGER NOT NULL,
  unit_cost_minor INTEGER NOT NULL
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
  outstanding_minor INTEGER NOT NULL DEFAULT 0,
  reversed          INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS purchases_date ON purchases (date);
CREATE INDEX IF NOT EXISTS purchases_outstanding ON purchases (outstanding_minor) WHERE outstanding_minor > 0;

CREATE TABLE IF NOT EXISTS purchase_lines (
  id              TEXT PRIMARY KEY,
  purchase_id     TEXT NOT NULL REFERENCES purchases(id),
  item_id         TEXT NOT NULL REFERENCES items(id),
  qty             INTEGER NOT NULL,
  unit_cost_minor INTEGER NOT NULL,
  lot_id          TEXT NOT NULL REFERENCES inventory_lots(id)
);
CREATE INDEX IF NOT EXISTS pl_purchase ON purchase_lines (purchase_id);
CREATE INDEX IF NOT EXISTS pl_item     ON purchase_lines (item_id);

-- §6.4 payments
CREATE TABLE IF NOT EXISTS payments (
  id           TEXT PRIMARY KEY,
  event_id     TEXT NOT NULL,
  party_id     TEXT NOT NULL,
  direction    TEXT NOT NULL,
  amount_minor INTEGER NOT NULL,
  date         TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS payments_party_date ON payments (party_id, date);
CREATE INDEX IF NOT EXISTS payments_date        ON payments (date);

-- §6.5 payment_allocations
CREATE TABLE IF NOT EXISTS payment_allocations (
  id              TEXT PRIMARY KEY,
  event_id        TEXT NOT NULL,
  payment_id      TEXT NOT NULL,
  target_id       TEXT NOT NULL,
  target_type     TEXT NOT NULL,
  amount_minor    INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS pa_payment ON payment_allocations (payment_id);
CREATE INDEX IF NOT EXISTS pa_target  ON payment_allocations (target_id);

-- §6.6 party_balances
CREATE TABLE IF NOT EXISTS party_balances (
  party_id             TEXT PRIMARY KEY REFERENCES parties(id),
  receivable_minor     INTEGER NOT NULL DEFAULT 0,
  payable_minor        INTEGER NOT NULL DEFAULT 0,
  unallocated_cr_minor INTEGER NOT NULL DEFAULT 0,
  unallocated_dr_minor INTEGER NOT NULL DEFAULT 0
);

-- §6.8 returns + return_lines
CREATE TABLE IF NOT EXISTS returns (
  id                    TEXT PRIMARY KEY,
  event_id              TEXT NOT NULL,
  return_type           TEXT NOT NULL,
  original_id           TEXT NOT NULL,
  date                  TEXT NOT NULL,
  revenue_reversed_minor INTEGER NOT NULL DEFAULT 0,
  cost_restored_minor    INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS returns_original ON returns (original_id);

CREATE TABLE IF NOT EXISTS return_lines (
  id               TEXT PRIMARY KEY,
  return_id        TEXT NOT NULL REFERENCES returns(id),
  item_id          TEXT NOT NULL REFERENCES items(id),
  qty              INTEGER NOT NULL,
  unit_price_minor INTEGER NOT NULL DEFAULT 0,
  unit_cost_minor  INTEGER NOT NULL,
  lot_id           TEXT NOT NULL REFERENCES inventory_lots(id)
);
CREATE INDEX IF NOT EXISTS rl_return ON return_lines (return_id);
CREATE INDEX IF NOT EXISTS rl_item   ON return_lines (item_id);

-- §6.9 expenses
CREATE TABLE IF NOT EXISTS expenses (
  id           TEXT PRIMARY KEY,
  event_id     TEXT NOT NULL,
  account_id   TEXT NOT NULL REFERENCES accounts(id),
  amount_minor INTEGER NOT NULL,
  date         TEXT NOT NULL,
  memo         TEXT,
  terms        TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS expenses_date    ON expenses (date);
CREATE INDEX IF NOT EXISTS expenses_account ON expenses (account_id, date);
