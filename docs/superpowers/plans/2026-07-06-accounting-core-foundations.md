# Accounting Core — Foundations Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the foundational data-model layer for the local-first accounting app — a standalone Rust crate providing the SQLite schema, the append-only event store, the Hybrid Logical Clock, and genesis bootstrap — fully tested against in-memory SQLite, with zero Tauri dependency.

**Architecture:** Event-sourced / CQRS. An append-only `events` table is the source of truth; all read models (built in later plans) are projections rebuilt by replaying events in HLC order. This crate is deliberately runtime-agnostic Rust (`rusqlite`, no ORM, no async): all SQL lives in `.sql` files / string constants so it ports cheaply to a TypeScript/`plugin-sql` runtime later if desired. It will be consumed by the Tauri core in a later plan.

**Tech Stack:** Rust, `rusqlite` 0.32 (with `bundled` SQLite 3.46.x, providing JSONB and generated columns), `serde_json`. Tests via `cargo test` against in-memory SQLite.

**Spec:** `docs/superpowers/specs/2026-07-06-accounting-schema-design.md`

---

## Plan Series Roadmap

This is **Plan 1 of 4** for the data model. Each produces working, testable software and depends on the prior:

1. **Foundations (this plan):** crate scaffold, schema DDL, HLC clock, event store append/read, genesis bootstrap.
2. **Read model + projectors + rebuild:** all §5–§6 projection-table DDL, `apply_event` per event type, the drop-and-replay `rebuild` loop, `projection_cursor`. Built and tested by appending events directly (via Plan 1's `append_event`) and asserting projected state + rebuild determinism.
3. **Command handlers + guards:** the ~15 command entry points, each running its §4.5 validation guards *against the read model*, then appending the event and applying the projection in one transaction. Depends on Plan 2 because guards read projected state (e.g. a lot's `qty_remaining`).
4. **Reconciliation + queries:** the §7 integrity checks and the §8 report queries.

(Order note: projectors precede command handlers because the write path validates against the read model — guards query projections, so projections must exist first.)

Later, outside the data-model series: Tauri IPC wiring, reactivity, UI, and (someday) the sync engine.

---

## File Structure

All paths are inside a new crate at `crates/accounting-core/`.

- `Cargo.toml` — crate manifest and dependencies.
- `src/lib.rs` — crate root; re-exports the public API (`Db`, `Hlc`, `LedgerEvent`, `append_event`, `read_events`, `run_genesis`).
- `src/db.rs` — connection open, PRAGMA setup, schema application. One responsibility: getting a correctly-configured connection with the schema applied.
- `src/schema.sql` — the complete DDL for the `events` table and `projection_cursor` (the only two tables this plan creates; projection tables come in Plan 3). Portable SQL asset.
- `src/hlc.rs` — the Hybrid Logical Clock. One responsibility: producing lexically-sortable, causally-correct timestamp strings.
- `src/events.rs` — the `LedgerEvent` envelope type plus `append_event` / `read_events`. One responsibility: reading and writing the event log.
- `src/genesis.rs` — first-run bootstrap. One responsibility: emitting the deterministic genesis event sequence (system user + seeded chart of accounts) into the log.

---

### Task 1: Crate scaffold and configured connection

**Files:**
- Create: `crates/accounting-core/Cargo.toml`
- Create: `crates/accounting-core/src/lib.rs`
- Create: `crates/accounting-core/src/db.rs`

- [ ] **Step 1: Create the crate manifest**

Create `crates/accounting-core/Cargo.toml`:

```toml
[package]
name = "accounting-core"
version = "0.1.0"
edition = "2021"

[dependencies]
rusqlite = { version = "0.32", features = ["bundled"] }
serde_json = "1"

[dev-dependencies]
```

Note: the `bundled` feature compiles a recent SQLite (≥ 3.46) so JSONB (`jsonb()`, `->>`) and generated columns are available regardless of the host's system SQLite.

- [ ] **Step 2: Write the failing test for opening a connection**

Create `crates/accounting-core/src/db.rs`:

```rust
use rusqlite::Connection;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_in_memory_sets_wal_and_foreign_keys() {
        let conn = open_in_memory().expect("open");
        // foreign_keys is a per-connection PRAGMA; assert it's ON.
        let fk: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fk, 1, "foreign_keys should be ON");
    }
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p accounting-core open_in_memory_sets_wal_and_foreign_keys`
Expected: FAIL — `open_in_memory` not found (compile error).

- [ ] **Step 4: Implement the minimal connection opener**

At the top of `crates/accounting-core/src/db.rs`, above the `#[cfg(test)]` module:

```rust
use rusqlite::Connection;

/// Open an in-memory database with the standard PRAGMAs. Used by tests and
/// ephemeral tooling. Production uses a file-backed `open_at` (added in a later
/// plan).
///
/// DURABILITY OBLIGATION: the future file-backed `open_at` MUST call
/// `hlc::rehydrate_from_log` before the first `tick`, so locally-authored events
/// sort after everything already persisted (see Task 7). The in-memory path does
/// not persist across restarts, so it does not rehydrate. Skipping rehydration in
/// `open_at` silently reintroduces duplicate `events.id` / backward-ordering across
/// restarts — and the in-memory test suite cannot catch it.
pub fn open_in_memory() -> rusqlite::Result<Connection> {
    let conn = Connection::open_in_memory()?;
    configure(&conn)?;
    Ok(conn)
}

/// Apply the connection-level PRAGMAs every connection must have.
fn configure(conn: &Connection) -> rusqlite::Result<()> {
    // WAL is a no-op for :memory: but harmless; set it so the real file path
    // (added later) inherits identical configuration through this one function.
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(())
}
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p accounting-core open_in_memory_sets_wal_and_foreign_keys`
Expected: PASS.

- [ ] **Step 6: Wire up the crate root**

Create `crates/accounting-core/src/lib.rs`:

```rust
pub mod db;

pub use db::open_in_memory;
```

- [ ] **Step 7: Commit**

```bash
git add crates/accounting-core/Cargo.toml crates/accounting-core/src/lib.rs crates/accounting-core/src/db.rs
git commit -m "feat: scaffold accounting-core crate with configured connection"
```

---

### Task 2: Schema DDL and application

**Files:**
- Create: `crates/accounting-core/src/schema.sql`
- Modify: `crates/accounting-core/src/db.rs`

- [ ] **Step 1: Write the schema SQL asset**

Create `crates/accounting-core/src/schema.sql` (the event store + replay bookmark — the only tables this plan needs; projection tables arrive in Plan 3):

```sql
-- Source of truth: append-only, immutable event log.
CREATE TABLE IF NOT EXISTS events (
  id         TEXT PRIMARY KEY,          -- hlc + '-' + device_id (globally unique, sortable)
  hlc        TEXT NOT NULL,
  device_id  TEXT NOT NULL,
  user_id    TEXT NOT NULL,
  seq        INTEGER NOT NULL,
  type       TEXT NOT NULL,
  payload    BLOB NOT NULL,             -- jsonb
  created_at INTEGER NOT NULL,
  UNIQUE (device_id, seq)               -- gap detection + optimistic per-device ordering
);
CREATE INDEX IF NOT EXISTS events_hlc ON events (hlc);   -- replay order

-- Replay bookmark: how far each projection has been applied.
CREATE TABLE IF NOT EXISTS projection_cursor (
  projection TEXT PRIMARY KEY,
  last_hlc   TEXT NOT NULL
);
```

- [ ] **Step 2: Write the failing test for schema application**

Add to the `tests` module in `crates/accounting-core/src/db.rs`:

```rust
    #[test]
    fn apply_schema_creates_events_table() {
        let conn = open_in_memory().unwrap();
        apply_schema(&conn).expect("apply schema");
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='events'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "events table should exist");
    }

    #[test]
    fn apply_schema_is_idempotent() {
        let conn = open_in_memory().unwrap();
        apply_schema(&conn).unwrap();
        apply_schema(&conn).expect("second apply must not error");
    }
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p accounting-core apply_schema`
Expected: FAIL — `apply_schema` not found.

- [ ] **Step 4: Implement schema application**

Add to `crates/accounting-core/src/db.rs`, above the test module:

```rust
/// The full DDL for this crate's tables, embedded at compile time.
const SCHEMA_SQL: &str = include_str!("schema.sql");

/// Apply the schema. Idempotent (all statements use IF NOT EXISTS).
pub fn apply_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(SCHEMA_SQL)
}

/// Open an in-memory database with PRAGMAs applied AND the schema created.
/// Convenience for tests.
pub fn open_in_memory_with_schema() -> rusqlite::Result<Connection> {
    let conn = open_in_memory()?;
    apply_schema(&conn)?;
    Ok(conn)
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p accounting-core apply_schema`
Expected: PASS (both tests).

- [ ] **Step 6: Re-export the new functions**

In `crates/accounting-core/src/lib.rs`, update the re-export line:

```rust
pub use db::{apply_schema, open_in_memory, open_in_memory_with_schema};
```

- [ ] **Step 7: Commit**

```bash
git add crates/accounting-core/src/schema.sql crates/accounting-core/src/db.rs crates/accounting-core/src/lib.rs
git commit -m "feat: add event-store schema and idempotent application"
```

---

### Task 3: HLC — monotonic local tick

**Files:**
- Create: `crates/accounting-core/src/hlc.rs`
- Modify: `crates/accounting-core/src/lib.rs`

Physical time is injected (a `u64` millisecond value passed in), never read internally — this keeps the clock deterministic and unit-testable, and mirrors the design in the spec's sync section.

- [ ] **Step 1: Write the failing test for monotonic ticking**

Create `crates/accounting-core/src/hlc.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_is_strictly_increasing_lexically() {
        let mut hlc = Hlc::new("deviceA");
        // Same physical time three times: counter must advance and strings must sort.
        let a = hlc.tick(1000);
        let b = hlc.tick(1000);
        let c = hlc.tick(1000);
        assert!(a < b, "{a} should sort before {b}");
        assert!(b < c, "{b} should sort before {c}");
    }

    #[test]
    fn tick_resets_counter_when_physical_advances() {
        let mut hlc = Hlc::new("deviceA");
        let _ = hlc.tick(1000);
        let _ = hlc.tick(1000); // counter now 1
        let later = hlc.tick(2000); // physical advanced → counter resets to 0
        assert!(later.starts_with("000000000002000:000000:"), "got {later}");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p accounting-core hlc`
Expected: FAIL — `Hlc` not found.

- [ ] **Step 3: Implement the HLC with tick + encode**

At the top of `crates/accounting-core/src/hlc.rs`, above the test module:

```rust
/// A Hybrid Logical Clock. Produces lexically-sortable timestamp strings of the
/// form `{physical:015}:{counter:06}:{device_id}`, so string comparison equals
/// logical (causal) ordering. Physical time is injected by the caller.
pub struct Hlc {
    device_id: String,
    last_physical: u64,
    counter: u64,
}

impl Hlc {
    pub fn new(device_id: impl Into<String>) -> Self {
        Self { device_id: device_id.into(), last_physical: 0, counter: 0 }
    }

    /// Advance the clock for a locally-authored event and return the stamp.
    pub fn tick(&mut self, physical_now: u64) -> String {
        if physical_now > self.last_physical {
            self.last_physical = physical_now;
            self.counter = 0;
        } else {
            self.counter += 1;
        }
        self.encode()
    }

    // Width caps: 15 digits of ms (safe past year ~5000) and 6 digits of
    // counter (up to 999,999 events per physical ms). Exceeding the counter
    // width would silently break lexical sort, so both widths are load-bearing.
    fn encode(&self) -> String {
        format!("{:015}:{:06}:{}", self.last_physical, self.counter, self.device_id)
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p accounting-core hlc`
Expected: PASS (both tests).

- [ ] **Step 5: Re-export `Hlc`**

In `crates/accounting-core/src/lib.rs`, add:

```rust
pub mod hlc;
pub use hlc::Hlc;
```

- [ ] **Step 6: Commit**

```bash
git add crates/accounting-core/src/hlc.rs crates/accounting-core/src/lib.rs
git commit -m "feat: add Hybrid Logical Clock with monotonic tick"
```

---

### Task 4: HLC — observe (merge a remote stamp)

**Files:**
- Modify: `crates/accounting-core/src/hlc.rs`

`observe` keeps this device's clock ahead of any remote stamp it has seen, so that future local `tick`s sort after events received from other devices. This is unused single-device but is the seam that makes the log sync-ready (spec §3).

- [ ] **Step 1: Write the failing test for observe**

Add to the `tests` module in `crates/accounting-core/src/hlc.rs`:

```rust
    #[test]
    fn observe_pulls_clock_ahead_of_remote() {
        let mut local = Hlc::new("deviceA");
        // Remote device B is far ahead in physical time.
        local.observe("000000000005000:000003:deviceB", 1000);
        // A subsequent local tick (even at low physical time) must sort AFTER
        // the observed remote stamp.
        let next = local.tick(1000);
        assert!(
            next.as_str() > "000000000005000:000003:deviceB",
            "local tick {next} should sort after observed remote"
        );
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p accounting-core observe`
Expected: FAIL — no method named `observe`.

- [ ] **Step 3: Implement `observe`**

Add these methods inside the `impl Hlc` block in `crates/accounting-core/src/hlc.rs`:

```rust
    /// Merge a remote event's stamp, keeping this clock ahead of it.
    pub fn observe(&mut self, remote_hlc: &str, physical_now: u64) {
        let (r_phys, r_ctr) = Self::decode(remote_hlc);
        let max_phys = physical_now.max(self.last_physical).max(r_phys);
        if max_phys == self.last_physical && max_phys == r_phys {
            self.counter = self.counter.max(r_ctr) + 1;
        } else if max_phys == self.last_physical {
            self.counter += 1;
        } else if max_phys == r_phys {
            self.counter = r_ctr + 1;
        } else {
            self.counter = 0;
        }
        self.last_physical = max_phys;
    }

    fn decode(hlc: &str) -> (u64, u64) {
        let mut parts = hlc.splitn(3, ':');
        let phys = parts.next().unwrap_or("0").parse().unwrap_or(0);
        let ctr = parts.next().unwrap_or("0").parse().unwrap_or(0);
        (phys, ctr)
    }
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p accounting-core observe`
Expected: PASS.

- [ ] **Step 5: Run the full HLC suite to confirm no regression**

Run: `cargo test -p accounting-core hlc`
Expected: PASS (all four HLC tests).

- [ ] **Step 6: Commit**

```bash
git add crates/accounting-core/src/hlc.rs
git commit -m "feat: add HLC observe for remote-stamp merge (sync seam)"
```

---

### Task 5: Event envelope and append

**Files:**
- Create: `crates/accounting-core/src/events.rs`
- Modify: `crates/accounting-core/src/lib.rs`

- [ ] **Step 1: Write the failing test for append + per-device seq**

Create `crates/accounting-core/src/events.rs`:

```rust
use crate::db::open_in_memory_with_schema;
use crate::hlc::Hlc;
use rusqlite::Connection;
use serde_json::json;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_assigns_incrementing_seq_per_device() {
        let conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("deviceA");

        let e1 = append_event(&conn, &mut hlc, 1000, "deviceA", "userX",
            "ItemDefined", &json!({"itemId": "i1"})).unwrap();
        let e2 = append_event(&conn, &mut hlc, 1000, "deviceA", "userX",
            "ItemDefined", &json!({"itemId": "i2"})).unwrap();

        assert_eq!(e1.seq, 1);
        assert_eq!(e2.seq, 2);
        // The HLC stamp already ends in the device id and is globally unique,
        // so the event id IS the stamp (no separate device suffix).
        assert_ne!(e1.id, e2.id);
        assert_eq!(e1.id, e1.hlc);
        assert!(e1.id.ends_with(":deviceA"));
    }

    #[test]
    fn payload_round_trips_as_json() {
        let conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("deviceA");
        append_event(&conn, &mut hlc, 1000, "deviceA", "userX",
            "ItemDefined", &json!({"itemId": "i1", "sku": "SKU-1"})).unwrap();

        // Read the payload back out via JSON extraction to prove it stored as JSONB.
        let sku: String = conn
            .query_row("SELECT payload ->> 'sku' FROM events LIMIT 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sku, "SKU-1");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p accounting-core events`
Expected: FAIL — `append_event` / `LedgerEvent` not found.

- [ ] **Step 3: Implement the envelope and `append_event`**

At the top of `crates/accounting-core/src/events.rs`, above the test module:

```rust
/// The immutable event envelope. `payload` is event-type-specific JSON.
#[derive(Debug, Clone)]
pub struct LedgerEvent {
    pub id: String,
    pub hlc: String,
    pub device_id: String,
    pub user_id: String,
    pub seq: i64,
    pub event_type: String,
    pub payload: serde_json::Value,
    pub created_at: i64,
}

/// Append one event to the log. Assigns the next per-device `seq`, stamps the
/// HLC, derives the id, and inserts. `created_at` is the injected wall clock
/// (epoch ms) — used for audit only, never for ordering.
pub fn append_event(
    conn: &Connection,
    hlc: &mut Hlc,
    physical_now: u64,
    device_id: &str,
    user_id: &str,
    event_type: &str,
    payload: &serde_json::Value,
) -> rusqlite::Result<LedgerEvent> {
    let next_seq: i64 = conn.query_row(
        "SELECT COALESCE(MAX(seq), 0) + 1 FROM events WHERE device_id = ?1",
        [device_id],
        |r| r.get(0),
    )?;
    let stamp = hlc.tick(physical_now);
    // The HLC stamp already encodes the device id as its last segment
    // (`{phys}:{ctr}:{device}`), so it is globally unique on its own — the
    // event id is the stamp verbatim. (Spec envelope note updated to match.)
    let id = stamp.clone();
    let payload_str = payload.to_string();

    conn.execute(
        "INSERT INTO events (id, hlc, device_id, user_id, seq, type, payload, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, jsonb(?7), ?8)",
        rusqlite::params![
            id, stamp, device_id, user_id, next_seq, event_type,
            payload_str, physical_now as i64
        ],
    )?;

    Ok(LedgerEvent {
        id,
        hlc: stamp,
        device_id: device_id.to_string(),
        user_id: user_id.to_string(),
        seq: next_seq,
        event_type: event_type.to_string(),
        payload: payload.clone(),
        created_at: physical_now as i64,
    })
}
```

> **Atomicity note for Plans 2–3:** `append_event` takes `&Connection` and does a
> `SELECT MAX(seq)+1` then a separate `INSERT`. That's safe for this single-writer connection
> (and `UNIQUE(device_id, seq)` backstops it), but the spec's write path requires "append event +
> apply projection in **one** SQLite transaction." Because rusqlite's `Transaction` derefs to
> `Connection`, Plans 2–3 will call `append_event(&tx, …)` inside a `conn.transaction()` block with
> no signature change — preserving the atomic event+projection boundary. Do not append and project
> in separate transactions.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p accounting-core events`
Expected: PASS (both tests).

- [ ] **Step 5: Re-export the event API**

In `crates/accounting-core/src/lib.rs`, add:

```rust
pub mod events;
pub use events::{append_event, LedgerEvent};
```

- [ ] **Step 6: Commit**

```bash
git add crates/accounting-core/src/events.rs crates/accounting-core/src/lib.rs
git commit -m "feat: add event envelope and append with per-device seq"
```

---

### Task 6: Read events in replay order and detect gaps

**Files:**
- Modify: `crates/accounting-core/src/events.rs`

- [ ] **Step 1: Write the failing test for ordered read + gap detection**

Add to the `tests` module in `crates/accounting-core/src/events.rs`:

```rust
    #[test]
    fn read_events_returns_hlc_order() {
        let conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("deviceA");
        append_event(&conn, &mut hlc, 1000, "deviceA", "u", "A", &json!({})).unwrap();
        append_event(&conn, &mut hlc, 1000, "deviceA", "u", "B", &json!({})).unwrap();
        append_event(&conn, &mut hlc, 2000, "deviceA", "u", "C", &json!({})).unwrap();

        let events = read_events(&conn).unwrap();
        let types: Vec<&str> = events.iter().map(|e| e.event_type.as_str()).collect();
        assert_eq!(types, vec!["A", "B", "C"], "must be in HLC order");
    }

    #[test]
    fn missing_seq_reports_gap() {
        let conn = open_in_memory_with_schema().unwrap();
        // Deliberately synthetic seq-only fixtures: `missing_seqs` reads only the
        // `seq` column, so these ids are NOT realistic HLC stamps (real ids are
        // `{phys:015}:{ctr:06}:{device}` and equal their hlc — see Task 5). Kept
        // short here purely to exercise gap detection.
        // Manually insert seq 1 and 3 for deviceA (seq 2 missing).
        for (id, seq, t) in [("h1-deviceA", 1, "A"), ("h3-deviceA", 3, "C")] {
            conn.execute(
                "INSERT INTO events (id, hlc, device_id, user_id, seq, type, payload, created_at)
                 VALUES (?1, ?1, 'deviceA', 'u', ?2, ?3, jsonb('{}'), 0)",
                rusqlite::params![id, seq, t],
            ).unwrap();
        }
        let gaps = missing_seqs(&conn, "deviceA").unwrap();
        assert_eq!(gaps, vec![2], "seq 2 should be reported missing");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p accounting-core read_events missing_seq`
Expected: FAIL — `read_events` / `missing_seqs` not found.

- [ ] **Step 3: Implement `read_events` and `missing_seqs`**

Add to `crates/accounting-core/src/events.rs`, below `append_event`:

```rust
/// Read every event in deterministic replay order (by HLC ascending).
pub fn read_events(conn: &Connection) -> rusqlite::Result<Vec<LedgerEvent>> {
    let mut stmt = conn.prepare(
        "SELECT id, hlc, device_id, user_id, seq, type, json(payload), created_at
         FROM events ORDER BY hlc ASC",
    )?;
    let rows = stmt.query_map([], |r| {
        let payload_text: String = r.get(6)?;
        // Propagate a parse failure as an error rather than silently yielding
        // Null — a corrupt payload reading as Null would misdirect the projector.
        let payload = serde_json::from_str(&payload_text).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(6, rusqlite::types::Type::Text, Box::new(e))
        })?;
        Ok(LedgerEvent {
            id: r.get(0)?,
            hlc: r.get(1)?,
            device_id: r.get(2)?,
            user_id: r.get(3)?,
            seq: r.get(4)?,
            event_type: r.get(5)?,
            payload,
            created_at: r.get(7)?,
        })
    })?;
    rows.collect()
}

/// Return the list of missing `seq` values for a device (gap detection for sync).
/// A dense log from seq 1..=max yields an empty vec.
pub fn missing_seqs(conn: &Connection, device_id: &str) -> rusqlite::Result<Vec<i64>> {
    let max_seq: i64 = conn.query_row(
        "SELECT COALESCE(MAX(seq), 0) FROM events WHERE device_id = ?1",
        [device_id],
        |r| r.get(0),
    )?;
    let mut present = std::collections::HashSet::new();
    let mut stmt = conn.prepare("SELECT seq FROM events WHERE device_id = ?1")?;
    let rows = stmt.query_map([device_id], |r| r.get::<_, i64>(0))?;
    for s in rows {
        present.insert(s?);
    }
    Ok((1..=max_seq).filter(|s| !present.contains(s)).collect())
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p accounting-core read_events missing_seq`
Expected: PASS (both tests).

- [ ] **Step 5: Re-export the read API**

In `crates/accounting-core/src/lib.rs`, update the events re-export line:

```rust
pub use events::{append_event, missing_seqs, read_events, LedgerEvent};
```

- [ ] **Step 6: Commit**

```bash
git add crates/accounting-core/src/events.rs crates/accounting-core/src/lib.rs
git commit -m "feat: add ordered event read and per-device gap detection"
```

---

### Task 7: Rehydrate the clock from the persisted log on open

**Files:**
- Modify: `crates/accounting-core/src/hlc.rs`

The `Hlc` is in-memory and resets to zero each time the process starts, but the event log is
**persisted**. Without rehydration, a fresh clock on the next launch — with a wall clock that reads
the same or an earlier millisecond — can reproduce a prior stamp (→ duplicate `events.id`, a PK
violation) or emit events that sort *before* existing ones (→ replay-order violation). This task
seeds the clock from the log's latest stamp on open, using the `observe` primitive built in Task 4.
The single-session in-memory tests cannot catch this by construction, so this test explicitly
simulates a restart by dropping and rebuilding the `Hlc` against the same connection.

- [ ] **Step 1: Write the failing test simulating a restart**

Add to the `tests` module in `crates/accounting-core/src/hlc.rs`:

```rust
    #[test]
    fn rehydrate_from_log_orders_after_last_persisted_stamp() {
        use crate::db::open_in_memory_with_schema;
        use crate::events::append_event;
        use serde_json::json;

        let conn = open_in_memory_with_schema().unwrap();

        // Session 1: append an event at a high physical time.
        let mut hlc1 = Hlc::new("deviceA");
        let e1 = append_event(&conn, &mut hlc1, 5000, "deviceA", "u", "A", &json!({})).unwrap();
        drop(hlc1); // simulate process exit

        // Session 2: brand-new clock, and the wall clock has stepped BACKWARD to 1000.
        let mut hlc2 = Hlc::new("deviceA");
        rehydrate_from_log(&conn, &mut hlc2, 1000).unwrap();
        let e2 = append_event(&conn, &mut hlc2, 1000, "deviceA", "u", "B", &json!({})).unwrap();

        assert!(
            e2.hlc > e1.hlc,
            "post-restart event {} must sort after pre-restart {}",
            e2.hlc, e1.hlc
        );
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p accounting-core rehydrate`
Expected: FAIL — `rehydrate_from_log` not found.

- [ ] **Step 3: Implement `rehydrate_from_log`**

Add to `crates/accounting-core/src/hlc.rs`, above the test module (it needs `Connection`, so add
the import at the top of the file: `use rusqlite::Connection;`):

```rust
/// Seed an in-memory clock from the persisted log on open, so locally-authored
/// events after a restart sort strictly after everything already stored. Reads
/// the maximum HLC across ALL devices (not just this one) and `observe`s it —
/// the same primitive used for remote-stamp merges during sync.
pub fn rehydrate_from_log(
    conn: &Connection,
    hlc: &mut Hlc,
    physical_now: u64,
) -> rusqlite::Result<()> {
    let max_hlc: Option<String> =
        conn.query_row("SELECT MAX(hlc) FROM events", [], |r| r.get(0))?;
    if let Some(stamp) = max_hlc {
        hlc.observe(&stamp, physical_now);
    }
    Ok(())
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p accounting-core rehydrate`
Expected: PASS.

- [ ] **Step 5: Re-export and run the full HLC suite**

In `crates/accounting-core/src/lib.rs`, update the hlc re-export line:

```rust
pub use hlc::{rehydrate_from_log, Hlc};
```

Run: `cargo test -p accounting-core hlc`
Expected: PASS (all five HLC tests).

- [ ] **Step 6: Commit**

```bash
git add crates/accounting-core/src/hlc.rs crates/accounting-core/src/lib.rs
git commit -m "feat: rehydrate HLC from persisted log to survive restarts"
```

---

### Task 8: Genesis bootstrap

**Files:**
- Create: `crates/accounting-core/src/genesis.rs`
- Modify: `crates/accounting-core/src/lib.rs`

Genesis emits the deterministic first-run event sequence (spec §4.3): one `UserRegistered` for the owner, then one `AccountOpened` per seeded chart-of-accounts entry, all stamped `user_id = "system"`. This plan only *emits* these events into the log; projecting them into the `accounts`/`users` tables is Plan 3. The reserved `"system"` id is a constant, never a stored row.

- [ ] **Step 1: Write the failing test for genesis emission**

Create `crates/accounting-core/src/genesis.rs`:

```rust
use crate::db::open_in_memory_with_schema;
use crate::events::read_events;
use crate::hlc::Hlc;
use rusqlite::Connection;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn genesis_emits_user_then_accounts_all_system_authored() {
        let conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("deviceA");
        run_genesis(&conn, &mut hlc, 1000, "deviceA", "owner-1", "Jane Owner").unwrap();

        let events = read_events(&conn).unwrap();
        // First event registers the owner.
        assert_eq!(events[0].event_type, "UserRegistered");
        assert_eq!(events[0].payload["userId"], "owner-1");
        // Every genesis event is authored by the reserved system user.
        assert!(events.iter().all(|e| e.user_id == "system"));
        // The 14 seeded accounts follow (see spec §5.2 + inventory_gain).
        let account_events = events.iter().filter(|e| e.event_type == "AccountOpened").count();
        assert_eq!(account_events, 14);
        // Spot-check that the inventory account carries its system_role.
        let inv = events.iter().find(|e| e.payload["system_role"] == "inventory").unwrap();
        assert_eq!(inv.payload["type"], "asset");
        assert_eq!(inv.payload["normal"], "debit");
    }

    #[test]
    fn genesis_seeds_unique_system_roles() {
        let conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("deviceA");
        run_genesis(&conn, &mut hlc, 1000, "deviceA", "owner-1", "Jane").unwrap();
        let events = read_events(&conn).unwrap();
        let mut roles: Vec<String> = events
            .iter()
            .filter(|e| e.event_type == "AccountOpened")
            .map(|e| e.payload["system_role"].as_str().unwrap().to_string())
            .collect();
        let before = roles.len();
        roles.sort();
        roles.dedup();
        assert_eq!(before, roles.len(), "system_roles must be unique");
    }

    #[test]
    fn genesis_rejects_second_run_and_leaves_log_unchanged() {
        let conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("deviceA");
        run_genesis(&conn, &mut hlc, 1000, "deviceA", "owner-1", "Jane").unwrap();
        let count_after_first: i64 =
            conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();

        let second = run_genesis(&conn, &mut hlc, 2000, "deviceA", "owner-1", "Jane");
        assert!(second.is_err(), "second genesis must be rejected");

        let count_after_second: i64 =
            conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();
        assert_eq!(count_after_first, count_after_second, "log must be unchanged");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p accounting-core genesis`
Expected: FAIL — `run_genesis` not found.

- [ ] **Step 3: Implement genesis emission**

At the top of `crates/accounting-core/src/genesis.rs`, above the test module:

```rust
use crate::events::append_event;
use serde_json::json;

/// The reserved system-user id used to author genesis events (spec §4.3).
pub const SYSTEM_USER_ID: &str = "system";

/// Seeded chart of accounts: (system_role, display name, type, normal side).
/// Matches spec §5.2 exactly.
const SEEDED_ACCOUNTS: &[(&str, &str, &str, &str)] = &[
    ("cash",                "Cash",                     "asset",     "debit"),
    ("bank",                "Bank",                     "asset",     "debit"),
    ("inventory",           "Inventory",                "asset",     "debit"),
    ("accounts_receivable", "Accounts Receivable",      "asset",     "debit"),
    ("accounts_payable",    "Accounts Payable",         "liability", "credit"),
    ("tax_payable",         "Tax Payable",              "liability", "credit"),
    ("owner_capital",       "Owner Capital",            "equity",    "credit"),
    ("retained_earnings",   "Retained Earnings",        "equity",    "credit"),
    ("sales",               "Sales",                    "income",    "credit"),
    ("cogs",                "Cost of Goods Sold",       "expense",   "debit"),
    ("shrinkage",           "Inventory Shrinkage",      "expense",   "debit"),
    ("inventory_gain",      "Inventory Gain (overage)", "income",    "credit"),
    ("rent",                "Rent",                     "expense",   "debit"),
    ("wages",               "Wages",                    "expense",   "debit"),
];
// 14 entries total — the 13 named in spec §5.2 plus `inventory_gain`.

/// Emit the first-run genesis event sequence: register the owner, then open the
/// seeded chart of accounts. All events are authored by `SYSTEM_USER_ID`.
/// Account ids are derived deterministically from the system_role so they are
/// stable across rebuild (spec §4.5 deterministic-referenced-IDs rule).
pub fn run_genesis(
    conn: &Connection,
    hlc: &mut Hlc,
    physical_now: u64,
    device_id: &str,
    owner_user_id: &str,
    owner_name: &str,
) -> rusqlite::Result<()> {
    // Run-once guard: genesis may only populate an empty log. A second call
    // (relaunch bug, retry after partial failure) would duplicate the owner and
    // the entire chart of accounts, colliding on accounts.system_role's UNIQUE
    // index at projection time (Plan 3). Mirrors the spec's "may only appear
    // once" discipline. Fail loudly here.
    let existing: i64 = conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))?;
    if existing > 0 {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
            Some("run_genesis called on a non-empty event log".to_string()),
        ));
    }

    append_event(
        conn, hlc, physical_now, device_id, SYSTEM_USER_ID,
        "UserRegistered",
        &json!({ "userId": owner_user_id, "name": owner_name }),
    )?;

    for (role, name, acct_type, normal) in SEEDED_ACCOUNTS {
        append_event(
            conn, hlc, physical_now, device_id, SYSTEM_USER_ID,
            "AccountOpened",
            &json!({
                "accountId": format!("acct_{role}"),
                "name": name,
                "type": acct_type,
                "normal": normal,
                "system_role": role,
            }),
        )?;
    }
    Ok(())
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p accounting-core genesis`
Expected: PASS (all three tests).

- [ ] **Step 5: Re-export genesis**

In `crates/accounting-core/src/lib.rs`, add:

```rust
pub mod genesis;
pub use genesis::{run_genesis, SYSTEM_USER_ID};
```

- [ ] **Step 6: Run the entire crate test suite**

Run: `cargo test -p accounting-core`
Expected: PASS (all tests across db, hlc, events, genesis).

- [ ] **Step 7: Commit**

```bash
git add crates/accounting-core/src/genesis.rs crates/accounting-core/src/lib.rs
git commit -m "feat: add genesis bootstrap emitting system user and seeded accounts"
```

---

## Definition of Done (Plan 1)

- `cargo test -p accounting-core` passes with all tasks' tests green.
- An in-memory database can be opened, schema applied, events appended with correct per-device seq and HLC ordering, read back in replay order, gaps detected, and the genesis sequence emitted.
- **Restart-safe ordering:** a clock rehydrated from the persisted log emits events that sort strictly after everything already stored, even if the wall clock steps backward across restart (Task 7).
- **Carried obligation for Plan 2+:** the file-backed `open_at` (a later plan) MUST call `hlc::rehydrate_from_log` before the first `tick`. This is recorded in the `open_in_memory` doc comment. It cannot be verified by this plan's in-memory tests, so it is called out explicitly as an acceptance item for whichever plan introduces `open_at`.
- **Genesis is run-once:** a second `run_genesis` on a non-empty log fails loudly and leaves the log unchanged (Task 8).
- The event `id` equals its HLC stamp (globally unique via the device-suffixed clock); no redundant device segment.
- All SQL is isolated in `schema.sql` / inline string constants (no ORM), preserving the TS-portability hedge.
- No projection tables yet (Plan 3), no command validation guards yet (Plan 2) — those are the next plans.
