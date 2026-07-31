# Local Backup & Restore Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the desktop app a reliable local backup story — a consistent database snapshot for recovery, plus a portable JSONL event-log export that can be merged back into any install.

**Architecture:** Event/log semantics go in `accounting-core` as pure functions over `&Connection` (in-memory testable, no Tauri). File I/O, dialogs and lifecycle hooks go in `tauri-app`. A per-install UUID `device_id` lands first because merge-by-event-id is unsound while every install is `"device-1"`.

**Tech Stack:** Rust, rusqlite 0.32 (bundled SQLite 3.47), Tauri 2.11, `tauri-plugin-dialog` 2.7, `uuid` 1.23 (v4), React 19 + TypeScript.

**Spec:** `docs/superpowers/specs/2026-07-30-local-backup-restore-design.md`

---

## Background an implementer must know

Read this before Task 1; it explains *why* the tasks are ordered as they are.

**The event log is the source of truth.** `events` (`crates/accounting-core/src/schema.sql:2-12`) is append-only. Every projection table is derived and is wiped + replayed by `rebuild()` (`crates/accounting-core/src/projectors.rs:769`). `crates/tauri-app/src/lib.rs:41` calls `rebuild()` on **every** startup, so restoring a database file needs no projection code at all — restart does it.

**Event identity is the HLC.** `append_event` (`crates/accounting-core/src/events.rs:37`) sets `id = hlc` (`events.rs:33`), and an HLC is formatted `{physical:015}:{counter:06}:{device_id}` (`crates/accounting-core/src/hlc.rs:27`). `seq` is `MAX(seq)+1` **per device_id** (`events.rs:27-30`), and `schema.sql:11` declares `UNIQUE (device_id, seq)`.

**Consequence:** with every install hardcoded to `"device-1"` (`crates/tauri-app/src/commands.rs:23`, `lib.rs:27`, `lib.rs:32`, `lib.rs:38`), two installs mint *identical event ids for different events*, and their `seq` ranges collide. Task 1 fixes this and must come first.

**`app_settings` is not event-sourced.** `schema.sql:22-27`, and it is absent from `PROJECTION_TABLES` (`projectors.rs:758-766`), so `rebuild()` never clears it. It holds currency/locale/theme — and after Task 1, `device_id`. `set_setting` only accepts keys in the `SETTING_KEYS` allowlist (`crates/accounting-core/src/settings.rs:6-13`).

**Payloads are `jsonb` BLOBs.** Written with `jsonb(?7)` (`events.rs:38`), read with `json(payload)` (`events.rs:60`). Export must read via `json()` or it produces garbage.

**The live DB is in WAL mode** (`lib.rs:21-23`) at `app_local_data_dir()/accounting/ledger.db`. Therefore backup uses `VACUUM INTO` (consistent, single file), never `fs::copy` (can capture a torn state). Verified: `VACUUM INTO ?1` accepts a bound parameter, so paths are never interpolated into SQL.

**Error style.** `CommandError` (`crates/accounting-core/src/commands/mod.rs:23-39`) is a plain enum with a manual `Display` and `From<rusqlite::Error>`. Follow that; the project uses no `thiserror`.

**Testing style.** Unit tests live in a `#[cfg(test)] mod tests` at the bottom of the same file, using `open_in_memory_with_schema()` (`crates/accounting-core/src/db.rs:26`). Run the whole suite with `cargo test -p accounting-core`. The suite currently passes 135 tests; every task must keep it green.

---

## File structure

| File | Responsibility | Task |
|---|---|---|
| `crates/accounting-core/src/settings.rs` (modify) | `ensure_device_id`; 3 new allowlisted keys | 1 |
| `crates/accounting-core/src/events.rs` (modify) | `insert_raw_event` — verbatim insert, mints nothing | 3 |
| `crates/accounting-core/src/archive.rs` (create) | JSONL header/record types, `export_jsonl`, `import_jsonl`, `ArchiveError`, `ImportSummary` | 4,5,6 |
| `crates/accounting-core/src/lib.rs` (modify) | declare + re-export the new surface | 1,3,4 |
| `crates/accounting-core/Cargo.toml` (modify) | add `uuid` | 1 |
| `crates/tauri-app/src/backup.rs` (create) | snapshot, candidate validation, safe swap, retention, path/naming | 7,8,9 |
| `crates/tauri-app/src/state.rs` (modify) | `Db.conn` → `Option<Connection>` so restore can drop it | 8 |
| `crates/tauri-app/src/error.rs` (modify) | `From<ArchiveError>`, `From<std::io::Error>` | 4 |
| `crates/tauri-app/src/commands.rs` (modify) | 4 IPC commands; `with_ctx!` unwraps the `Option` | 8,10 |
| `crates/tauri-app/src/lib.rs` (modify) | device-id wiring, register commands, auto-backup on close | 1,10,11 |
| `crates/tauri-app/Cargo.toml` (modify) | add `tauri-plugin-dialog` | 10 |
| `crates/tauri-app/capabilities/default.json` (create) | dialog permissions — without this the picker is denied | 10 |
| `ui/src/i18n/fr.ts`, `ui/src/i18n/en.ts` (modify) | new keys in **both** (fr is the type source) | 12 |
| `ui/src/pages/Preferences.tsx` (modify) | Data panel with 4 actions | 12 |

**Task order rationale:** 1 (identity) → 2 (settings keys) → 3 (raw insert) → 4-6 (archive core) → 7-9 (file layer) → 10-11 (wiring) → 12 (UI). Core before shell; each task's tests pass before the next starts.

---

## Task 1: Per-install device identity

**Files:**
- Modify: `crates/accounting-core/Cargo.toml`
- Modify: `crates/accounting-core/src/settings.rs:6-13` (allowlist) and add `ensure_device_id`
- Modify: `crates/accounting-core/src/lib.rs:37` (re-export)

- [ ] **Step 1: Add the `uuid` dependency**

In `crates/accounting-core/Cargo.toml`, under `[dependencies]`, after the `serde_json` line:

```toml
uuid = { version = "1", features = ["v4"] }
```

- [ ] **Step 2: Write the failing tests**

Add these inside the existing `#[cfg(test)] mod tests` block in `crates/accounting-core/src/settings.rs` (before its closing `}`):

```rust
    #[test]
    fn ensure_device_id_mints_and_persists() {
        let conn = open_in_memory_with_schema().unwrap();
        let id = ensure_device_id(&conn).unwrap();
        assert_eq!(id.len(), 36, "expected a hyphenated UUID v4, got {id:?}");
        let stored = get_settings(&conn).unwrap();
        assert_eq!(stored.get("device_id"), Some(&id));
    }

    #[test]
    fn ensure_device_id_is_idempotent() {
        let conn = open_in_memory_with_schema().unwrap();
        let first = ensure_device_id(&conn).unwrap();
        let second = ensure_device_id(&conn).unwrap();
        assert_eq!(first, second, "must not mint a second identity");
    }

    #[test]
    fn device_id_is_an_allowed_key() {
        let conn = open_in_memory_with_schema().unwrap();
        set_setting(&conn, "device_id", "abc").expect("device_id must be allowlisted");
        assert_eq!(ensure_device_id(&conn).unwrap(), "abc", "must reuse the stored value");
    }
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p accounting-core ensure_device_id`
Expected: FAIL — `cannot find function ensure_device_id in this scope`.

- [ ] **Step 4: Extend the allowlist**

In `crates/accounting-core/src/settings.rs`, replace the `SETTING_KEYS` constant (lines 6-13) with:

```rust
pub const SETTING_KEYS: &[&str] = &[
    "currency_symbol",
    "currency_code",
    "currency_decimals",
    "theme",
    "locale",
    "font_size",
    // Stable identity of this install. Authored into every event's HLC, so it
    // must never change once minted. See ensure_device_id.
    "device_id",
    // Folder remembered from the last manual backup; auto-backup on close
    // writes here. Absent means "no automatic backups yet".
    "backup_folder",
    // Unix ms of the last successful backup, shown in Preferences.
    "last_backup_at",
];
```

- [ ] **Step 5: Implement `ensure_device_id`**

Append to `crates/accounting-core/src/settings.rs`, after `set_setting` and before the `#[cfg(test)]` block:

```rust
/// Return this install's stable device id, minting and persisting one on first
/// call. The id is embedded in every event's HLC (and therefore in every event
/// id), so it must be stable for the life of the install and unique across
/// installs — otherwise two installs mint colliding event ids and clashing
/// `(device_id, seq)` pairs, and logs cannot be merged.
pub fn ensure_device_id(conn: &Connection) -> rusqlite::Result<String> {
    let existing: Option<String> = conn
        .query_row("SELECT value FROM app_settings WHERE key = 'device_id'", [], |r| r.get(0))
        .optional()?;
    if let Some(id) = existing {
        return Ok(id);
    }
    let id = uuid::Uuid::new_v4().to_string();
    set_setting(conn, "device_id", &id)?;
    Ok(id)
}
```

Add `OptionalExtension` to the imports at the top of the file — change line 1 to:

```rust
use rusqlite::{Connection, OptionalExtension};
```

- [ ] **Step 6: Re-export it**

In `crates/accounting-core/src/lib.rs`, change line 37 to:

```rust
pub use settings::{ensure_device_id, get_settings, set_setting, SETTING_KEYS};
```

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test -p accounting-core settings`
Expected: PASS — the 4 pre-existing settings tests plus the 3 new ones.

- [ ] **Step 8: Run the full suite**

Run: `cargo test -p accounting-core`
Expected: PASS, 138 tests (135 existing + 3 new).

- [ ] **Step 9: Commit**

```bash
git add crates/accounting-core/Cargo.toml crates/accounting-core/src/settings.rs crates/accounting-core/src/lib.rs Cargo.lock
git commit -m "feat(core): per-install device_id minted into app_settings"
```

---

## Task 2: Wire the device id into app startup

**Files:**
- Modify: `crates/tauri-app/src/lib.rs:16-44` (`init_state`)
- Modify: `crates/tauri-app/src/commands.rs:17-27` (`with_ctx!`) and `crates/tauri-app/src/state.rs`

The `"device-1"` literal appears in 4 places. It must become the real id, and `ensure_device_id` must run **before** `rehydrate_from_log` so the clock knows its identity before seeding from the log's max HLC.

- [ ] **Step 1: Store the device id on `AppState`**

In `crates/tauri-app/src/state.rs`, replace the whole file with:

```rust
use accounting_core::Hlc;
use rusqlite::Connection;
use std::sync::Mutex;

/// The mutable state a command needs: connection + clock.
pub struct Db {
    pub conn: Connection,
    pub hlc: Hlc,
}

/// Application state. A single Mutex wraps both connection and clock,
/// eliminating any lock-ordering concern (single-device, single-writer).
pub struct AppState {
    pub db: Mutex<Db>,
    /// This install's stable identity, authored into every event. Read once at
    /// startup so commands never re-query it.
    pub device_id: String,
}

impl AppState {
    pub fn new(conn: Connection, hlc: Hlc, device_id: String) -> Self {
        Self { db: Mutex::new(Db { conn, hlc }), device_id }
    }
}
```

- [ ] **Step 2: Use the real id in `init_state`**

In `crates/tauri-app/src/lib.rs`, replace lines 26-44 (from `let mut hlc = Hlc::new("device-1");` through `AppState::new(conn, hlc)`) with:

```rust
    // Mint or read this install's identity BEFORE rehydrating the clock: the
    // clock must know its own device id before it seeds from the log's max HLC.
    let device_id = ensure_device_id(&conn).expect("ensure device id");

    let mut hlc = Hlc::new(device_id.clone());
    rehydrate_from_log(&conn, &mut hlc, now_ms()).expect("rehydrate hlc");

    let event_count: i64 = conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();
    if event_count == 0 {
        run_genesis(&conn, &mut hlc, now_ms(), &device_id, "owner-1", "Owner").expect("genesis");
    }

    // Idempotently ensure the shared walk-in customer exists (covers both
    // fresh installs and installs whose genesis predates this party). Must run
    // before rebuild so the event is projected this startup.
    ensure_walkin_party(&conn, &mut hlc, now_ms(), &device_id).expect("seed walk-in party");

    // Always rebuild projections on startup — ensures genesis events are projected
    // and recovers from any interrupted prior session.
    rebuild(&mut conn).expect("rebuild projections");

    AppState::new(conn, hlc, device_id)
```

Then add `ensure_device_id` to the `accounting_core` import on line 5:

```rust
use accounting_core::{apply_schema, ensure_device_id, ensure_walkin_party, rebuild, Hlc, rehydrate_from_log, run_genesis};
```

- [ ] **Step 3: Use the real id in `with_ctx!`**

In `crates/tauri-app/src/commands.rs`, replace the macro body (lines 17-27) with:

```rust
macro_rules! with_ctx {
    ($state:expr, |$ctx:ident| $body:expr) => {{
        let device_id = $state.device_id.clone();
        let mut db = $state.db.lock().unwrap();
        let crate::state::Db { ref mut conn, ref mut hlc } = *db;
        let mut $ctx = CommandContext {
            conn, hlc, physical_now: now_ms(),
            device_id, user_id: "owner-1".into(),
        };
        $body
    }};
}
```

- [ ] **Step 4: Verify no `"device-1"` literal remains in app code**

Run: `rg -n '"device-1"' crates/tauri-app/src/`
Expected: no matches. (`crates/accounting-core/examples/load_to_file.rs` may keep its literal — it is a standalone dev tool with its own database, not the app.)

- [ ] **Step 5: Build and test**

Run: `cargo build 2>&1 | tail -5 && cargo test -p accounting-core 2>&1 | tail -5`
Expected: build succeeds; 138 tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/tauri-app/src/state.rs crates/tauri-app/src/lib.rs crates/tauri-app/src/commands.rs
git commit -m "feat(app): author events under the per-install device id"
```

---

## Task 3: `insert_raw_event`

**Files:**
- Modify: `crates/accounting-core/src/events.rs` (add fn + tests)
- Modify: `crates/accounting-core/src/lib.rs:15` (re-export)

`append_event` cannot import events: it mints a fresh `id`/`hlc`/`seq` on every call (`events.rs:32-44`). Import needs a verbatim insert.

- [ ] **Step 1: Write the failing tests**

Add inside the existing `#[cfg(test)] mod tests` block in `crates/accounting-core/src/events.rs`:

```rust
    #[test]
    fn insert_raw_event_preserves_identity_verbatim() {
        let conn = open_in_memory_with_schema().unwrap();
        let ev = LedgerEvent {
            id: "000000000001000:000000:devB".into(),
            hlc: "000000000001000:000000:devB".into(),
            device_id: "devB".into(),
            user_id: "userZ".into(),
            seq: 7,
            event_type: "ItemDefined".into(),
            payload: json!({"itemId": "i9", "sku": "SKU-9"}),
            created_at: 4242,
        };
        insert_raw_event(&conn, &ev).unwrap();

        let got = read_events(&conn).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, ev.id, "id must not be re-minted");
        assert_eq!(got[0].hlc, ev.hlc);
        assert_eq!(got[0].device_id, "devB");
        assert_eq!(got[0].seq, 7, "seq must be preserved, not recomputed");
        assert_eq!(got[0].created_at, 4242);
        assert_eq!(got[0].payload, ev.payload, "payload must round-trip as JSON");
    }

    #[test]
    fn insert_raw_event_rejects_duplicate_id() {
        let conn = open_in_memory_with_schema().unwrap();
        let ev = LedgerEvent {
            id: "000000000001000:000000:devB".into(),
            hlc: "000000000001000:000000:devB".into(),
            device_id: "devB".into(),
            user_id: "u".into(),
            seq: 1,
            event_type: "A".into(),
            payload: json!({}),
            created_at: 1,
        };
        insert_raw_event(&conn, &ev).unwrap();
        assert!(insert_raw_event(&conn, &ev).is_err(), "PRIMARY KEY must reject a repeat id");
    }

    #[test]
    fn insert_raw_event_rejects_duplicate_device_seq() {
        let conn = open_in_memory_with_schema().unwrap();
        let a = LedgerEvent {
            id: "000000000001000:000000:devB".into(),
            hlc: "000000000001000:000000:devB".into(),
            device_id: "devB".into(),
            user_id: "u".into(),
            seq: 1,
            event_type: "A".into(),
            payload: json!({}),
            created_at: 1,
        };
        let mut b = a.clone();
        b.id = "000000000002000:000000:devB".into();
        b.hlc = b.id.clone();
        insert_raw_event(&conn, &a).unwrap();
        assert!(
            insert_raw_event(&conn, &b).is_err(),
            "UNIQUE (device_id, seq) must reject a second seq 1 for devB"
        );
    }
```

- [ ] **Step 2: Make `LedgerEvent` cloneable**

The third test clones. In `crates/accounting-core/src/events.rs`, find the `pub struct LedgerEvent` declaration (line 6) and ensure the line above it derives `Clone`. If it currently reads `#[derive(Debug)]`, change it to:

```rust
#[derive(Debug, Clone)]
```

If there is no derive attribute, add `#[derive(Debug, Clone)]` immediately above `pub struct LedgerEvent {`.

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p accounting-core insert_raw_event`
Expected: FAIL — `cannot find function insert_raw_event in this scope`.

- [ ] **Step 4: Implement `insert_raw_event`**

In `crates/accounting-core/src/events.rs`, add immediately after `append_event`'s closing brace (before `read_events`):

```rust
/// Insert an event **verbatim**, preserving its `id`, `hlc`, `device_id`,
/// `user_id`, `seq` and `created_at` exactly as authored elsewhere.
///
/// This is the import path for a foreign log; it is NOT a command path. Unlike
/// `append_event` it mints nothing, which is the whole point: a merged event must
/// keep the identity it was created with, or it would be indistinguishable from a
/// new local event and would be re-imported forever.
///
/// Errors if `id` already exists (PRIMARY KEY) or if `(device_id, seq)` is already
/// taken (UNIQUE) — callers are expected to check both first and report the
/// difference to the user.
pub fn insert_raw_event(conn: &Connection, ev: &LedgerEvent) -> rusqlite::Result<()> {
    let payload_str = ev.payload.to_string();
    conn.execute(
        "INSERT INTO events (id, hlc, device_id, user_id, seq, type, payload, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, jsonb(?7), ?8)",
        rusqlite::params![
            ev.id, ev.hlc, ev.device_id, ev.user_id, ev.seq, ev.event_type,
            payload_str, ev.created_at
        ],
    )?;
    Ok(())
}
```

- [ ] **Step 5: Re-export it**

In `crates/accounting-core/src/lib.rs`, change line 15 to:

```rust
pub use events::{append_event, insert_raw_event, missing_seqs, read_events, LedgerEvent};
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p accounting-core events`
Expected: PASS, including the 3 new tests.

- [ ] **Step 7: Commit**

```bash
git add crates/accounting-core/src/events.rs crates/accounting-core/src/lib.rs
git commit -m "feat(core): insert_raw_event for verbatim event import"
```

---

## Task 4: Archive module — types and export

**Files:**
- Create: `crates/accounting-core/src/archive.rs`
- Modify: `crates/accounting-core/src/lib.rs` (declare module + re-export)

- [ ] **Step 1: Create the module with types and `export_jsonl`**

Create `crates/accounting-core/src/archive.rs`:

```rust
//! Portable event-log archive: newline-delimited JSON.
//!
//! Line 1 is a header (format marker, version, provenance, a copy of
//! `app_settings`); every following line is one event in HLC order. The format
//! marker and version exist so a future change is *detected and rejected*
//! rather than silently misparsed.
//!
//! The archive is the whole history, not a diff: the event log is append-only,
//! so a later export is a strict superset of an earlier one.

use crate::events::{insert_raw_event, read_events, LedgerEvent};
use crate::settings::get_settings;
use rusqlite::{Connection, OptionalExtension};
use std::collections::HashMap;
use std::io::{BufRead, Write};

/// Format marker written to, and required by, every archive header.
pub const ARCHIVE_FORMAT: &str = "accounting-eventlog";
/// Current archive version. Bump only on a breaking layout change.
pub const ARCHIVE_VERSION: u32 = 1;

#[derive(Debug)]
pub enum ArchiveError {
    /// The file is not a recognised archive, or its version is unsupported.
    Format(String),
    /// A line could not be parsed as an event.
    Parse { line: usize, message: String },
    /// An incoming event's `(device_id, seq)` is taken locally by a different
    /// event id. Unmergeable — both logs authored under the same identity.
    Collision { device_id: String, seq: i64 },
    /// The merged ledger failed reconciliation; the import was rolled back.
    Reconciliation(String),
    Db(rusqlite::Error),
    Io(String),
}

impl std::fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArchiveError::Format(m) => write!(f, "unrecognised backup file: {m}"),
            ArchiveError::Parse { line, message } => {
                write!(f, "backup file is damaged at line {line}: {message}")
            }
            ArchiveError::Collision { device_id, seq } => write!(
                f,
                "cannot merge: this backup was made by an older installation that shares \
                 an identity with yours (device {device_id}, entry {seq}). Restore a full \
                 database backup instead."
            ),
            ArchiveError::Reconciliation(m) => {
                write!(f, "import was cancelled because the combined ledger did not balance: {m}")
            }
            ArchiveError::Db(e) => write!(f, "database error: {e}"),
            ArchiveError::Io(m) => write!(f, "could not read or write the file: {m}"),
        }
    }
}

impl std::error::Error for ArchiveError {}

impl From<rusqlite::Error> for ArchiveError {
    fn from(e: rusqlite::Error) -> Self {
        ArchiveError::Db(e)
    }
}

impl From<std::io::Error> for ArchiveError {
    fn from(e: std::io::Error) -> Self {
        ArchiveError::Io(e.to_string())
    }
}

/// What an import actually did. Shown to the user verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportSummary {
    /// Events new to this database and inserted.
    pub inserted: usize,
    /// Events already present (matched by id) and skipped.
    pub skipped_duplicates: usize,
    /// Total event lines read from the archive.
    pub total_events: usize,
}

/// Write the full event log plus a provenance header to `out`.
///
/// Returns the number of event lines written (excludes the header).
pub fn export_jsonl(
    conn: &Connection,
    out: &mut impl Write,
    exported_at: i64,
    app_version: &str,
) -> Result<usize, ArchiveError> {
    let settings: HashMap<String, String> = get_settings(conn)?;
    let device_id = settings.get("device_id").cloned().unwrap_or_default();

    let header = serde_json::json!({
        "format": ARCHIVE_FORMAT,
        "version": ARCHIVE_VERSION,
        "exported_at": exported_at,
        "app_version": app_version,
        "device_id": device_id,
        "settings": settings,
    });
    writeln!(out, "{header}")?;

    // read_events returns HLC order and decodes jsonb payloads to real JSON.
    let events = read_events(conn)?;
    for ev in &events {
        let line = serde_json::json!({
            "id": ev.id,
            "hlc": ev.hlc,
            "device_id": ev.device_id,
            "user_id": ev.user_id,
            "seq": ev.seq,
            "type": ev.event_type,
            "payload": ev.payload,
            "created_at": ev.created_at,
        });
        writeln!(out, "{line}")?;
    }
    out.flush()?;
    Ok(events.len())
}
```

- [ ] **Step 2: Declare and re-export the module**

In `crates/accounting-core/src/lib.rs`, add after line 9 (`pub mod settings;`):

```rust
pub mod archive;
```

and add after the `pub use settings::{...}` line:

```rust
pub use archive::{export_jsonl, ArchiveError, ImportSummary, ARCHIVE_FORMAT, ARCHIVE_VERSION};
```

- [ ] **Step 3: Write the export tests**

Append to `crates/accounting-core/src/archive.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory_with_schema;
    use crate::events::append_event;
    use crate::hlc::Hlc;
    use crate::settings::{ensure_device_id, set_setting};
    use serde_json::json;

    /// A database with a known identity, one setting, and two events.
    fn seeded() -> Connection {
        let conn = open_in_memory_with_schema().unwrap();
        ensure_device_id(&conn).unwrap();
        set_setting(&conn, "currency_symbol", "€").unwrap();
        let mut hlc = Hlc::new("devA");
        append_event(&conn, &mut hlc, 1000, "devA", "u", "ItemDefined", &json!({"itemId": "i1"}))
            .unwrap();
        append_event(&conn, &mut hlc, 2000, "devA", "u", "ItemDefined", &json!({"itemId": "i2"}))
            .unwrap();
        conn
    }

    fn export_to_string(conn: &Connection) -> String {
        let mut buf: Vec<u8> = Vec::new();
        export_jsonl(conn, &mut buf, 555, "0.1.2").unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn export_writes_header_then_one_line_per_event() {
        let conn = seeded();
        let text = export_to_string(&conn);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "1 header + 2 events");

        let header: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(header["format"], ARCHIVE_FORMAT);
        assert_eq!(header["version"], 1);
        assert_eq!(header["exported_at"], 555);
        assert_eq!(header["app_version"], "0.1.2");
        assert_eq!(header["settings"]["currency_symbol"], "€");
    }

    #[test]
    fn export_emits_payloads_as_real_json_not_strings() {
        let conn = seeded();
        let text = export_to_string(&conn);
        let ev: serde_json::Value = serde_json::from_str(text.lines().nth(1).unwrap()).unwrap();
        assert!(ev["payload"].is_object(), "payload must be an object, got {:?}", ev["payload"]);
        assert_eq!(ev["payload"]["itemId"], "i1");
    }

    #[test]
    fn export_preserves_hlc_order() {
        let conn = seeded();
        let text = export_to_string(&conn);
        let first: serde_json::Value = serde_json::from_str(text.lines().nth(1).unwrap()).unwrap();
        let second: serde_json::Value = serde_json::from_str(text.lines().nth(2).unwrap()).unwrap();
        assert!(
            first["hlc"].as_str().unwrap() < second["hlc"].as_str().unwrap(),
            "events must be exported in ascending HLC order"
        );
    }

    #[test]
    fn export_of_empty_log_writes_header_only() {
        let conn = open_in_memory_with_schema().unwrap();
        let mut buf: Vec<u8> = Vec::new();
        let n = export_jsonl(&conn, &mut buf, 1, "0.1.2").unwrap();
        assert_eq!(n, 0);
        assert_eq!(String::from_utf8(buf).unwrap().lines().count(), 1);
    }
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p accounting-core archive`
Expected: PASS, 4 tests.

- [ ] **Step 5: Add the `AppError` conversions**

In `crates/tauri-app/src/error.rs`, append:

```rust
impl From<accounting_core::ArchiveError> for AppError {
    fn from(e: accounting_core::ArchiveError) -> Self {
        Self { message: e.to_string() }
    }
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        Self { message: format!("file error: {e}") }
    }
}
```

- [ ] **Step 6: Build and commit**

Run: `cargo build 2>&1 | tail -3`
Expected: succeeds.

```bash
git add crates/accounting-core/src/archive.rs crates/accounting-core/src/lib.rs crates/tauri-app/src/error.rs
git commit -m "feat(core): JSONL event-log export"
```

---

## Task 5: Archive header parsing and validation

**Files:**
- Modify: `crates/accounting-core/src/archive.rs`

Parsing is split from merging so the failure modes can be tested without a database.

- [ ] **Step 1: Write the failing tests**

Add inside `mod tests` in `crates/accounting-core/src/archive.rs`:

```rust
    #[test]
    fn parse_header_accepts_a_valid_header() {
        let line = r#"{"format":"accounting-eventlog","version":1,"settings":{"locale":"fr"}}"#;
        let h = parse_header(line).unwrap();
        assert_eq!(h.version, 1);
        assert_eq!(h.settings.get("locale").map(String::as_str), Some("fr"));
    }

    #[test]
    fn parse_header_rejects_a_foreign_format() {
        let err = parse_header(r#"{"format":"something-else","version":1}"#).unwrap_err();
        assert!(matches!(err, ArchiveError::Format(_)), "got {err:?}");
    }

    #[test]
    fn parse_header_rejects_a_future_version() {
        let err = parse_header(r#"{"format":"accounting-eventlog","version":99}"#).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("99"), "error should name the unsupported version: {msg}");
    }

    #[test]
    fn parse_header_rejects_non_json() {
        let err = parse_header("this is not json").unwrap_err();
        assert!(matches!(err, ArchiveError::Format(_)), "got {err:?}");
    }

    #[test]
    fn parse_event_reads_all_fields() {
        let line = r#"{"id":"h1","hlc":"h1","device_id":"d","user_id":"u","seq":3,"type":"A","payload":{"x":1},"created_at":9}"#;
        let ev = parse_event(line, 2).unwrap();
        assert_eq!(ev.id, "h1");
        assert_eq!(ev.seq, 3);
        assert_eq!(ev.event_type, "A");
        assert_eq!(ev.payload["x"], 1);
        assert_eq!(ev.created_at, 9);
    }

    #[test]
    fn parse_event_reports_the_line_number_on_damage() {
        let err = parse_event(r#"{"id":"h1","seq":"not-a-number"}"#, 7).unwrap_err();
        match err {
            ArchiveError::Parse { line, .. } => assert_eq!(line, 7),
            other => panic!("expected Parse, got {other:?}"),
        }
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p accounting-core archive::tests::parse`
Expected: FAIL — `cannot find function parse_header`.

- [ ] **Step 3: Implement the parsers**

In `crates/accounting-core/src/archive.rs`, add after `export_jsonl` and before `#[cfg(test)]`:

```rust
/// The first line of an archive.
#[derive(Debug, Clone)]
pub struct ArchiveHeader {
    pub version: u32,
    pub exported_at: i64,
    pub device_id: String,
    /// The exporting install's `app_settings`. Archival only — merge import
    /// never applies these (see `import_jsonl`).
    pub settings: HashMap<String, String>,
}

/// Parse and validate the header line.
///
/// Rejects anything without our format marker, and any version this build does
/// not understand — better a clear refusal than a silent misread of a newer file.
pub fn parse_header(line: &str) -> Result<ArchiveHeader, ArchiveError> {
    let v: serde_json::Value = serde_json::from_str(line)
        .map_err(|e| ArchiveError::Format(format!("the first line is not valid JSON ({e})")))?;

    match v.get("format").and_then(|f| f.as_str()) {
        Some(ARCHIVE_FORMAT) => {}
        Some(other) => {
            return Err(ArchiveError::Format(format!(
                "expected an {ARCHIVE_FORMAT} file but found '{other}'"
            )))
        }
        None => return Err(ArchiveError::Format("no format marker on the first line".into())),
    }

    let version = v.get("version").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
    if version != ARCHIVE_VERSION {
        return Err(ArchiveError::Format(format!(
            "this file is version {version} but this app understands version {ARCHIVE_VERSION}"
        )));
    }

    let settings = v
        .get("settings")
        .and_then(|s| s.as_object())
        .map(|m| {
            m.iter()
                .filter_map(|(k, val)| val.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();

    Ok(ArchiveHeader {
        version,
        exported_at: v.get("exported_at").and_then(|x| x.as_i64()).unwrap_or(0),
        device_id: v.get("device_id").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
        settings,
    })
}

/// Parse one event line. `line_no` is 1-based and only used for error reporting.
pub fn parse_event(line: &str, line_no: usize) -> Result<LedgerEvent, ArchiveError> {
    let v: serde_json::Value = serde_json::from_str(line)
        .map_err(|e| ArchiveError::Parse { line: line_no, message: e.to_string() })?;

    fn text(v: &serde_json::Value, key: &str, line_no: usize) -> Result<String, ArchiveError> {
        v.get(key)
            .and_then(|x| x.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| ArchiveError::Parse {
                line: line_no,
                message: format!("missing text field '{key}'"),
            })
    }
    fn number(v: &serde_json::Value, key: &str, line_no: usize) -> Result<i64, ArchiveError> {
        v.get(key).and_then(|x| x.as_i64()).ok_or_else(|| ArchiveError::Parse {
            line: line_no,
            message: format!("missing numeric field '{key}'"),
        })
    }

    Ok(LedgerEvent {
        id: text(&v, "id", line_no)?,
        hlc: text(&v, "hlc", line_no)?,
        device_id: text(&v, "device_id", line_no)?,
        user_id: text(&v, "user_id", line_no)?,
        seq: number(&v, "seq", line_no)?,
        event_type: text(&v, "type", line_no)?,
        payload: v.get("payload").cloned().unwrap_or(serde_json::Value::Null),
        created_at: number(&v, "created_at", line_no)?,
    })
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p accounting-core archive`
Expected: PASS, 10 tests (4 export + 6 parse).

- [ ] **Step 5: Commit**

```bash
git add crates/accounting-core/src/archive.rs
git commit -m "feat(core): archive header and event line parsing"
```

---

## Task 6: `import_jsonl` — merge by event id

**Files:**
- Modify: `crates/accounting-core/src/archive.rs`
- Modify: `crates/accounting-core/src/lib.rs` (re-export `import_jsonl`)

- [ ] **Step 1: Write the failing tests**

Add inside `mod tests` in `crates/accounting-core/src/archive.rs`:

```rust
    use crate::projectors::rebuild;
    use crate::reconciliation::{all_passed, run_all_checks};

    /// Build a log under a caller-chosen device id, so two "installs" can be
    /// simulated without colliding.
    fn seeded_as(device: &str, items: &[&str]) -> Connection {
        let mut conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new(device);
        let mut physical = 1000;
        for it in items {
            append_event(&conn, &mut hlc, physical, device, "u", "ItemDefined",
                &json!({"itemId": it, "sku": it, "name": it, "unit": "ea"}))
                .unwrap();
            physical += 1000;
        }
        rebuild(&mut conn).unwrap();
        conn
    }

    fn export_string(conn: &Connection) -> String {
        let mut buf: Vec<u8> = Vec::new();
        export_jsonl(conn, &mut buf, 1, "0.1.2").unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn import_into_empty_db_reproduces_identity_exactly() {
        let source = seeded_as("devA", &["i1", "i2"]);
        let text = export_string(&source);

        let mut target = open_in_memory_with_schema().unwrap();
        let summary = import_jsonl(&mut target, text.as_bytes()).unwrap();
        assert_eq!(summary.inserted, 2);
        assert_eq!(summary.skipped_duplicates, 0);
        assert_eq!(summary.total_events, 2);

        let src_events = read_events(&source).unwrap();
        let got_events = read_events(&target).unwrap();
        assert_eq!(got_events.len(), src_events.len());
        for (a, b) in src_events.iter().zip(got_events.iter()) {
            assert_eq!(a.id, b.id, "event id must survive the round trip");
            assert_eq!(a.hlc, b.hlc);
            assert_eq!(a.device_id, b.device_id);
            assert_eq!(a.seq, b.seq);
            assert_eq!(a.payload, b.payload);
        }
    }

    #[test]
    fn import_rebuilds_projections() {
        let source = seeded_as("devA", &["i1", "i2"]);
        let text = export_string(&source);
        let mut target = open_in_memory_with_schema().unwrap();
        import_jsonl(&mut target, text.as_bytes()).unwrap();

        let n: i64 = target.query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 2, "projections must be rebuilt from the merged log");
        assert!(all_passed(&run_all_checks(&target).unwrap()));
    }

    #[test]
    fn reimporting_the_same_archive_is_a_no_op() {
        let source = seeded_as("devA", &["i1", "i2"]);
        let text = export_string(&source);
        let mut target = open_in_memory_with_schema().unwrap();
        import_jsonl(&mut target, text.as_bytes()).unwrap();

        let second = import_jsonl(&mut target, text.as_bytes()).unwrap();
        assert_eq!(second.inserted, 0, "nothing new to insert");
        assert_eq!(second.skipped_duplicates, 2);
        let n: i64 = target.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 2, "the log must not grow on re-import");
    }

    #[test]
    fn merging_two_disjoint_logs_keeps_both() {
        let a = seeded_as("devA", &["i1"]);
        let b = seeded_as("devB", &["i2"]);
        let mut target = open_in_memory_with_schema().unwrap();
        import_jsonl(&mut target, export_string(&a).as_bytes()).unwrap();
        let summary = import_jsonl(&mut target, export_string(&b).as_bytes()).unwrap();

        assert_eq!(summary.inserted, 1);
        let n: i64 = target.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 2, "both logs must be present");

        let events = read_events(&target).unwrap();
        let hlcs: Vec<&str> = events.iter().map(|e| e.hlc.as_str()).collect();
        let mut sorted = hlcs.clone();
        sorted.sort_unstable();
        assert_eq!(hlcs, sorted, "merged log must read back in HLC order");
        assert!(all_passed(&run_all_checks(&target).unwrap()));
    }

    #[test]
    fn colliding_device_seq_aborts_and_changes_nothing() {
        // Two "installs" that both believe they are devA — the pre-UUID case.
        let a = seeded_as("devA", &["i1"]);
        let other = seeded_as("devA", &["i2"]);

        let mut target = open_in_memory_with_schema().unwrap();
        import_jsonl(&mut target, export_string(&a).as_bytes()).unwrap();
        let before: i64 = target.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();

        let err = import_jsonl(&mut target, export_string(&other).as_bytes()).unwrap_err();
        assert!(matches!(err, ArchiveError::Collision { .. }), "got {err:?}");

        let after: i64 = target.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();
        assert_eq!(before, after, "a rejected import must leave the log untouched");
    }

    #[test]
    fn import_never_overwrites_local_settings() {
        let source = seeded_as("devA", &["i1"]);
        set_setting(&source, "currency_symbol", "$").unwrap();
        set_setting(&source, "locale", "en").unwrap();
        let text = export_string(&source);

        let mut target = open_in_memory_with_schema().unwrap();
        let local_id = ensure_device_id(&target).unwrap();
        set_setting(&target, "currency_symbol", "€").unwrap();
        import_jsonl(&mut target, text.as_bytes()).unwrap();

        let s = get_settings(&target).unwrap();
        assert_eq!(s.get("currency_symbol").map(String::as_str), Some("€"), "must keep local currency");
        assert_eq!(s.get("device_id"), Some(&local_id), "device identity must never be replaced");
        assert_eq!(s.get("locale"), None, "must not import the source's locale");
    }

    #[test]
    fn import_rejects_a_damaged_line_without_partial_writes() {
        let source = seeded_as("devA", &["i1", "i2"]);
        let text = export_string(&source);
        let mut lines: Vec<&str> = text.lines().collect();
        lines[2] = "{ this line is broken";
        let damaged = lines.join("\n");

        let mut target = open_in_memory_with_schema().unwrap();
        let err = import_jsonl(&mut target, damaged.as_bytes()).unwrap_err();
        assert!(matches!(err, ArchiveError::Parse { .. }), "got {err:?}");

        let n: i64 = target.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0, "a damaged archive must insert nothing at all");
    }

    #[test]
    fn import_rejects_an_empty_file() {
        let mut target = open_in_memory_with_schema().unwrap();
        let err = import_jsonl(&mut target, &b""[..]).unwrap_err();
        assert!(matches!(err, ArchiveError::Format(_)), "got {err:?}");
    }

    #[test]
    fn import_tolerates_a_trailing_blank_line() {
        let source = seeded_as("devA", &["i1"]);
        let text = format!("{}\n\n", export_string(&source).trim_end());
        let mut target = open_in_memory_with_schema().unwrap();
        let summary = import_jsonl(&mut target, text.as_bytes()).unwrap();
        assert_eq!(summary.inserted, 1);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p accounting-core archive`
Expected: FAIL — `cannot find function import_jsonl`.

- [ ] **Step 3: Implement `import_jsonl`**

In `crates/accounting-core/src/archive.rs`, add after `parse_event` and before `#[cfg(test)]`:

```rust
/// Merge an archive into this database, keyed by event id.
///
/// Events already present (same id) are skipped; the rest are inserted verbatim.
/// All inserts happen in ONE transaction, so any failure — a damaged line, an
/// identity collision, or a post-merge reconciliation failure — leaves the log
/// exactly as it was.
///
/// Deliberately does NOT touch `app_settings`. Merging someone else's log must
/// not silently change your currency or locale, and overwriting `device_id`
/// would destroy this install's identity. Whole-install recovery (settings
/// included) is what a database snapshot restore is for.
pub fn import_jsonl(
    conn: &mut Connection,
    reader: impl BufRead,
) -> Result<ImportSummary, ArchiveError> {
    let mut lines = reader.lines();

    let header_line = lines
        .next()
        .transpose()?
        .ok_or_else(|| ArchiveError::Format("the file is empty".into()))?;
    let _header = parse_header(&header_line)?;

    // Parse everything BEFORE opening the transaction: a damaged file should
    // never reach the database at all.
    let mut events: Vec<LedgerEvent> = Vec::new();
    for (idx, line) in lines.enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue; // tolerate trailing newlines
        }
        events.push(parse_event(&line, idx + 2)?); // +2: 1-based, header is line 1
    }
    let total_events = events.len();

    let tx = conn.transaction()?;
    let mut inserted = 0usize;
    let mut skipped_duplicates = 0usize;

    for ev in &events {
        let existing_id: Option<String> = tx
            .query_row("SELECT id FROM events WHERE id = ?1", [&ev.id], |r| r.get(0))
            .optional()?;
        if existing_id.is_some() {
            skipped_duplicates += 1;
            continue;
        }

        // Same (device_id, seq) under a DIFFERENT id means two installs shared an
        // identity. UNIQUE (device_id, seq) would reject it anyway; fail with an
        // explanation instead of a raw constraint error.
        let clashing: Option<String> = tx
            .query_row(
                "SELECT id FROM events WHERE device_id = ?1 AND seq = ?2",
                rusqlite::params![ev.device_id, ev.seq],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(other) = clashing {
            if other != ev.id {
                return Err(ArchiveError::Collision {
                    device_id: ev.device_id.clone(),
                    seq: ev.seq,
                });
            }
        }

        insert_raw_event(&tx, ev)?;
        inserted += 1;
    }

    tx.commit()?;

    // Projections are derived state: replay the merged log in HLC order.
    crate::projectors::rebuild(conn)?;

    // Accept the merge only if the combined ledger still balances.
    let checks = crate::reconciliation::run_all_checks(conn)?;
    if !crate::reconciliation::all_passed(&checks) {
        let failed: Vec<String> = checks
            .iter()
            .filter(|c| !matches!(c.outcome, crate::reconciliation::CheckOutcome::Pass))
            .map(|c| c.name.to_string())
            .collect();
        return Err(ArchiveError::Reconciliation(failed.join(", ")));
    }

    Ok(ImportSummary { inserted, skipped_duplicates, total_events })
}
```

Note the transaction is dropped (rolled back) automatically on every early `return Err`, because `tx` goes out of scope without `commit()`.

- [ ] **Step 4: Confirm the `Check` field names used above**

The reconciliation failure branch reads `c.outcome` and `c.name`. Verify those are the real field names:

Run: `rg -n "pub struct Check" -A8 crates/accounting-core/src/reconciliation.rs`

If the fields differ, adjust the two lines in Step 3 to match. If `CheckOutcome` has no `Pass` variant, use whatever `all_passed` tests against — read it with `rg -n "pub fn all_passed" -A6 crates/accounting-core/src/reconciliation.rs`.

- [ ] **Step 5: Re-export `import_jsonl`**

In `crates/accounting-core/src/lib.rs`, change the archive re-export line to:

```rust
pub use archive::{export_jsonl, import_jsonl, ArchiveError, ArchiveHeader, ImportSummary,
    ARCHIVE_FORMAT, ARCHIVE_VERSION};
```

- [ ] **Step 6: Run the tests**

Run: `cargo test -p accounting-core archive`
Expected: PASS, 19 tests.

- [ ] **Step 7: Run the full suite**

Run: `cargo test -p accounting-core 2>&1 | tail -5`
Expected: PASS — 138 from Tasks 1-3 plus 19 archive tests.

- [ ] **Step 8: Commit**

```bash
git add crates/accounting-core/src/archive.rs crates/accounting-core/src/lib.rs
git commit -m "feat(core): merge-by-id JSONL import with collision guard"
```

---

## Task 7: Snapshot backup and path naming

**Files:**
- Create: `crates/tauri-app/src/backup.rs`
- Modify: `crates/tauri-app/src/lib.rs` (declare `mod backup;`)

Naming lives in Rust, not the UI, so the retention prefixes stay in one place.

- [ ] **Step 1: Create the module**

Create `crates/tauri-app/src/backup.rs`:

```rust
//! Local snapshot backup, restore, and retention.
//!
//! Backups use `VACUUM INTO`, never a file copy: the live database runs in WAL
//! mode, so copying `ledger.db` alone can capture a torn state with unmerged
//! `-wal` content. `VACUUM INTO` writes one consistent, compacted file.

use rusqlite::Connection;
use std::fs;
use std::path::{Path, PathBuf};

/// Prefix for backups the user asked for. Never pruned.
pub const MANUAL_PREFIX: &str = "ledger-";
/// Prefix for backups written automatically on app close. Pruned to `KEEP_AUTO`.
pub const AUTO_PREFIX: &str = "ledger-auto-";
/// Prefix for the safety copy taken before a restore or import.
pub const RESCUE_PREFIX: &str = "pre-restore-";
/// How many automatic backups and rescue copies to keep.
///
/// Three, not ten: the event log is append-only, so each snapshot is a strict
/// superset of the previous one. Extra copies are nested prefixes of one
/// history, not independent versions, so they buy very little.
pub const KEEP_AUTO: usize = 3;

/// Format a Unix-ms timestamp as `YYYYMMDD-HHMMSS` (UTC).
///
/// Hand-rolled because the project has no date dependency, and a backup
/// filename does not justify adding one. Uses the civil-from-days algorithm
/// (Howard Hinnant), valid for all dates after 1970.
pub fn timestamp_utc(unix_ms: i64) -> String {
    let secs = unix_ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (hh, mm, ss) = (tod / 3600, (tod % 3600) / 60, tod % 60);

    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}{m:02}{d:02}-{hh:02}{mm:02}{ss:02}")
}

/// Build the snapshot filename for a backup of the given kind.
pub fn snapshot_name(prefix: &str, unix_ms: i64) -> String {
    format!("{prefix}{}.db", timestamp_utc(unix_ms))
}

/// Write a consistent snapshot of `conn`'s database to `dest`.
///
/// `VACUUM INTO` refuses to overwrite, so `dest` must not exist — callers use a
/// fresh timestamped name. Returns the size of the written file in bytes.
pub fn snapshot_to(conn: &Connection, dest: &Path) -> rusqlite::Result<u64> {
    // Bound parameter, not string interpolation — the path is user-supplied.
    conn.execute("VACUUM INTO ?1", [dest.to_string_lossy().as_ref()])?;
    Ok(fs::metadata(dest).map(|m| m.len()).unwrap_or(0))
}

/// Delete all but the newest `keep` files in `dir` whose name starts with
/// `prefix` and ends with `.db`.
///
/// Names embed a sortable UTC timestamp, so lexical order is chronological.
/// Returns the paths removed.
pub fn prune(dir: &Path, prefix: &str, keep: usize) -> std::io::Result<Vec<PathBuf>> {
    let mut matching: Vec<PathBuf> = fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with(prefix) && n.ends_with(".db"))
                .unwrap_or(false)
        })
        .collect();

    matching.sort();
    let mut removed = Vec::new();
    if matching.len() > keep {
        for path in &matching[..matching.len() - keep] {
            fs::remove_file(path)?;
            removed.push(path.clone());
        }
    }
    Ok(removed)
}
```

- [ ] **Step 2: Declare the module**

In `crates/tauri-app/src/lib.rs`, add after `mod commands;` (line 1-3 area):

```rust
mod backup;
```

- [ ] **Step 3: Write the tests**

Append to `crates/tauri-app/src/backup.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use accounting_core::{apply_schema, append_event, Hlc};

    /// A unique temp directory, without adding a tempfile dependency.
    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("acct-backup-test-{tag}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn timestamp_is_utc_and_sortable() {
        // 2026-07-30T12:34:56Z = 1785508496 s
        assert_eq!(timestamp_utc(1_785_508_496_000), "20260730-123456");
        assert_eq!(timestamp_utc(0), "19700101-000000");
        assert!(timestamp_utc(1_000) < timestamp_utc(2_000_000_000_000), "must sort chronologically");
    }

    #[test]
    fn snapshot_names_carry_their_prefix() {
        assert_eq!(snapshot_name(AUTO_PREFIX, 0), "ledger-auto-19700101-000000.db");
        assert_eq!(snapshot_name(MANUAL_PREFIX, 0), "ledger-19700101-000000.db");
    }

    /// The core promise: a snapshot of a WAL database with committed-but-unmerged
    /// content is complete. A plain file copy is not.
    #[test]
    fn snapshot_of_a_wal_database_is_complete() {
        let dir = temp_dir("wal");
        let live = dir.join("ledger.db");

        let conn = Connection::open(&live).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        apply_schema(&conn).unwrap();
        let mut hlc = Hlc::new("devA");
        for i in 0..5 {
            append_event(&conn, &mut hlc, 1000 + i, "devA", "u", "ItemDefined",
                &serde_json::json!({"itemId": format!("i{i}")})).unwrap();
        }

        let dest = dir.join("snap.db");
        let bytes = snapshot_to(&conn, &dest).unwrap();
        assert!(bytes > 0, "snapshot should not be empty");

        // The snapshot must contain all 5 events even though the WAL was never
        // checkpointed, and must pass an integrity check on its own.
        let snap = Connection::open(&dest).unwrap();
        let n: i64 = snap.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 5, "WAL content must be included in the snapshot");
        let ok: String = snap.query_row("PRAGMA integrity_check", [], |r| r.get(0)).unwrap();
        assert_eq!(ok, "ok");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_refuses_to_overwrite() {
        let dir = temp_dir("overwrite");
        let conn = Connection::open(dir.join("ledger.db")).unwrap();
        apply_schema(&conn).unwrap();
        let dest = dir.join("snap.db");
        snapshot_to(&conn, &dest).unwrap();
        assert!(snapshot_to(&conn, &dest).is_err(), "VACUUM INTO must not clobber an existing file");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_keeps_the_newest_auto_backups_only() {
        let dir = temp_dir("prune");
        for stamp in ["20260101-000000", "20260102-000000", "20260103-000000", "20260104-000000"] {
            fs::write(dir.join(format!("{AUTO_PREFIX}{stamp}.db")), b"x").unwrap();
        }
        let removed = prune(&dir, AUTO_PREFIX, 3).unwrap();
        assert_eq!(removed.len(), 1);
        assert!(removed[0].to_string_lossy().contains("20260101"), "oldest must go first");
        assert!(dir.join(format!("{AUTO_PREFIX}20260104-000000.db")).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_never_touches_manual_backups() {
        let dir = temp_dir("prune-manual");
        for stamp in ["20260101-000000", "20260102-000000", "20260103-000000", "20260104-000000"] {
            fs::write(dir.join(format!("{MANUAL_PREFIX}{stamp}.db")), b"x").unwrap();
        }
        let removed = prune(&dir, AUTO_PREFIX, 3).unwrap();
        assert!(removed.is_empty(), "manual backups must never be pruned, removed {removed:?}");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 4);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_is_a_no_op_below_the_limit() {
        let dir = temp_dir("prune-few");
        fs::write(dir.join(format!("{AUTO_PREFIX}20260101-000000.db")), b"x").unwrap();
        assert!(prune(&dir, AUTO_PREFIX, 3).unwrap().is_empty());
        let _ = fs::remove_dir_all(&dir);
    }
}
```

Note `prune_never_touches_manual_backups` is the teeth behind the "manual is never pruned" rule: `AUTO_PREFIX` starts with `MANUAL_PREFIX`, so a naive `starts_with(MANUAL_PREFIX)` filter would match auto files too. Pruning always filters on `AUTO_PREFIX`.

- [ ] **Step 4: Add the dev-dependency the tests need**

`backup.rs` tests call `append_event` and use `serde_json`. `serde_json` is already a dependency of `tauri-app`; `accounting-core` is too. No change needed — verify by running the tests.

- [ ] **Step 5: Run the tests**

Run: `cargo test -p tauri-app backup`
Expected: PASS, 7 tests.

- [ ] **Step 6: Commit**

```bash
git add crates/tauri-app/src/backup.rs crates/tauri-app/src/lib.rs
git commit -m "feat(app): VACUUM INTO snapshots, UTC naming, auto-backup retention"
```

---

## Task 8: Restore — validate, safety-copy, safe swap

**Files:**
- Modify: `crates/tauri-app/src/backup.rs`
- Modify: `crates/tauri-app/src/state.rs` (`conn` becomes `Option`)
- Modify: `crates/tauri-app/src/commands.rs` (`with_ctx!` + 13 direct `db.conn` uses)

- [ ] **Step 1: Write the failing validation tests**

Add inside `mod tests` in `crates/tauri-app/src/backup.rs`:

```rust
    #[test]
    fn validate_accepts_a_real_snapshot() {
        let dir = temp_dir("validate-ok");
        let conn = Connection::open(dir.join("ledger.db")).unwrap();
        apply_schema(&conn).unwrap();
        let mut hlc = Hlc::new("devA");
        append_event(&conn, &mut hlc, 1000, "devA", "u", "ItemDefined",
            &serde_json::json!({"itemId": "i1"})).unwrap();
        let snap = dir.join("snap.db");
        snapshot_to(&conn, &snap).unwrap();

        validate_candidate(&snap).expect("a real snapshot must validate");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_rejects_a_non_database() {
        let dir = temp_dir("validate-junk");
        let junk = dir.join("notes.db");
        fs::write(&junk, b"this is not a sqlite file").unwrap();
        assert!(validate_candidate(&junk).is_err(), "garbage must be rejected");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_rejects_a_database_with_no_events_table() {
        let dir = temp_dir("validate-noevents");
        let other = dir.join("other.db");
        let c = Connection::open(&other).unwrap();
        c.execute_batch("CREATE TABLE unrelated (x INTEGER)").unwrap();
        drop(c);
        assert!(validate_candidate(&other).is_err(), "a foreign database must be rejected");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_rejects_an_empty_event_log() {
        let dir = temp_dir("validate-empty");
        let empty = dir.join("empty.db");
        let c = Connection::open(&empty).unwrap();
        apply_schema(&c).unwrap();
        drop(c);
        assert!(validate_candidate(&empty).is_err(), "a snapshot with no events is not a ledger");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_rejects_a_missing_file() {
        let dir = temp_dir("validate-missing");
        assert!(validate_candidate(&dir.join("nope.db")).is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn swap_replaces_the_file_and_removes_stale_wal_sidecars() {
        let dir = temp_dir("swap");
        let live = dir.join("ledger.db");
        let wal = dir.join("ledger.db-wal");
        let shm = dir.join("ledger.db-shm");

        // A live WAL database with 1 event, plus stale sidecars on disk.
        let conn = Connection::open(&live).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        apply_schema(&conn).unwrap();
        let mut hlc = Hlc::new("devA");
        append_event(&conn, &mut hlc, 1000, "devA", "u", "ItemDefined",
            &serde_json::json!({"itemId": "old"})).unwrap();

        // A candidate with 2 events.
        let candidate = dir.join("candidate.db");
        let c2 = Connection::open(&candidate).unwrap();
        apply_schema(&c2).unwrap();
        let mut hlc2 = Hlc::new("devB");
        for i in 0..2 {
            append_event(&c2, &mut hlc2, 1000 + i, "devB", "u", "ItemDefined",
                &serde_json::json!({"itemId": format!("new{i}")})).unwrap();
        }
        drop(c2);

        drop(conn); // release the live connection, as restore_in_place requires
        swap_in_place(&candidate, &live).unwrap();

        assert!(!wal.exists(), "stale -wal must be deleted or SQLite may replay it onto the new file");
        assert!(!shm.exists(), "stale -shm must be deleted");

        let restored = Connection::open(&live).unwrap();
        let n: i64 = restored.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 2, "the live file must now be the candidate");
        let _ = fs::remove_dir_all(&dir);
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p tauri-app backup`
Expected: FAIL — `cannot find function validate_candidate`.

- [ ] **Step 3: Implement validation and the swap**

Append to `crates/tauri-app/src/backup.rs`, before the `#[cfg(test)]` block:

```rust
/// Why a candidate file cannot be restored.
#[derive(Debug)]
pub struct InvalidCandidate(pub String);

impl std::fmt::Display for InvalidCandidate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for InvalidCandidate {}

/// Check a candidate backup **before** the live database is touched.
///
/// Requires: the file exists, opens as SQLite, passes `integrity_check`, has an
/// `events` table, and holds at least one event. Opened read-only so validation
/// can never modify the candidate.
pub fn validate_candidate(path: &Path) -> Result<(), InvalidCandidate> {
    if !path.exists() {
        return Err(InvalidCandidate("the file no longer exists".into()));
    }

    let uri = format!("file:{}?mode=ro", path.to_string_lossy());
    let conn = Connection::open_with_flags(
        &uri,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|e| InvalidCandidate(format!("this file is not a valid backup ({e})")))?;

    let integrity: String = conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .map_err(|e| InvalidCandidate(format!("this file is not a valid backup ({e})")))?;
    if integrity != "ok" {
        return Err(InvalidCandidate(format!("this backup is damaged ({integrity})")));
    }

    let has_events: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='events'",
            [],
            |r| r.get(0),
        )
        .map_err(|e| InvalidCandidate(format!("this file is not a valid backup ({e})")))?;
    if has_events == 0 {
        return Err(InvalidCandidate("this file is not an accounting backup".into()));
    }

    let events: i64 = conn
        .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
        .map_err(|e| InvalidCandidate(format!("this backup cannot be read ({e})")))?;
    if events == 0 {
        return Err(InvalidCandidate("this backup contains no records".into()));
    }

    Ok(())
}

/// Replace `live` with `candidate` and delete the stale WAL sidecars.
///
/// The caller MUST have dropped every connection to `live` first.
///
/// Deleting `-wal` / `-shm` is mandatory, not tidiness: they belong to the file
/// being replaced. Left in place, SQLite may replay the OLD write-ahead log on
/// top of the NEW database and corrupt it.
pub fn swap_in_place(candidate: &Path, live: &Path) -> std::io::Result<()> {
    fs::copy(candidate, live)?;
    for suffix in ["-wal", "-shm"] {
        let sidecar = PathBuf::from(format!("{}{suffix}", live.to_string_lossy()));
        if sidecar.exists() {
            fs::remove_file(&sidecar)?;
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p tauri-app backup`
Expected: PASS, 13 tests.

- [ ] **Step 5: Make the live connection droppable**

Restore must close the connection before overwriting the file. In `crates/tauri-app/src/state.rs`, replace the file with:

```rust
use accounting_core::Hlc;
use rusqlite::Connection;
use std::sync::Mutex;

/// The mutable state a command needs: connection + clock.
///
/// `conn` is an Option because a restore must drop the live connection before
/// overwriting the database file. After a restore it stays `None` and every
/// command reports that a restart is required — which the restore flow asks the
/// user to do anyway.
pub struct Db {
    pub conn: Option<Connection>,
    pub hlc: Hlc,
}

impl Db {
    /// Borrow the live connection, or explain that a restart is pending.
    pub fn conn(&self) -> Result<&Connection, crate::error::AppError> {
        self.conn.as_ref().ok_or_else(|| crate::error::AppError {
            message: "Restore finished. Please close and reopen the app.".into(),
        })
    }
}

/// Application state. A single Mutex wraps both connection and clock,
/// eliminating any lock-ordering concern (single-device, single-writer).
pub struct AppState {
    pub db: Mutex<Db>,
    /// This install's stable identity, authored into every event. Read once at
    /// startup so commands never re-query it.
    pub device_id: String,
}

impl AppState {
    pub fn new(conn: Connection, hlc: Hlc, device_id: String) -> Self {
        Self { db: Mutex::new(Db { conn: Some(conn), hlc }), device_id }
    }
}
```

- [ ] **Step 6: Update `with_ctx!` and every direct `conn` use**

In `crates/tauri-app/src/commands.rs`, replace the macro with:

```rust
macro_rules! with_ctx {
    ($state:expr, |$ctx:ident| $body:expr) => {{
        let device_id = $state.device_id.clone();
        let mut db = $state.db.lock().unwrap();
        let crate::state::Db { ref mut conn, ref mut hlc } = *db;
        let conn = conn.as_mut().ok_or_else(|| crate::error::AppError {
            message: "Restore finished. Please close and reopen the app.".into(),
        })?;
        let mut $ctx = CommandContext {
            conn, hlc, physical_now: now_ms(),
            device_id, user_id: "owner-1".into(),
        };
        $body
    }};
}
```

Then replace every remaining `&db.conn` with `db.conn()?` and every `db.conn.prepare(` with `db.conn()?.prepare(`. The 13 sites are at lines 150, 156, 208, 209, 212, 230, 245, 246, 266, 284, 306, 328, 349.

Find them all with: `rg -n 'db\.conn' crates/tauri-app/src/commands.rs`

- [ ] **Step 7: Build**

Run: `cargo build 2>&1 | tail -20`
Expected: succeeds. If a `db.conn` site was missed, the compiler names it — fix and rebuild.

- [ ] **Step 8: Run everything**

Run: `cargo test 2>&1 | tail -8`
Expected: all tests pass across both crates.

- [ ] **Step 9: Commit**

```bash
git add crates/tauri-app/src/backup.rs crates/tauri-app/src/state.rs crates/tauri-app/src/commands.rs
git commit -m "feat(app): validate candidates and swap safely, dropping stale WAL"
```

---

## Task 9: Backup directory helpers

**Files:**
- Modify: `crates/tauri-app/src/backup.rs`

- [ ] **Step 1: Write the failing test**

Add inside `mod tests` in `crates/tauri-app/src/backup.rs`:

```rust
    #[test]
    fn rescue_dir_is_created_on_demand() {
        let dir = temp_dir("rescue");
        let rescue = rescue_dir(&dir);
        assert!(!rescue.exists(), "should not exist yet");
        let made = ensure_rescue_dir(&dir).unwrap();
        assert!(made.exists() && made.is_dir());
        assert_eq!(made, rescue);
        // Idempotent.
        ensure_rescue_dir(&dir).unwrap();
        let _ = fs::remove_dir_all(&dir);
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p tauri-app rescue_dir`
Expected: FAIL — `cannot find function rescue_dir`.

- [ ] **Step 3: Implement**

Add to `crates/tauri-app/src/backup.rs` before `#[cfg(test)]`:

```rust
/// Where safety copies live: `<app data>/accounting/rescue`.
pub fn rescue_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("rescue")
}

/// Create the rescue directory if absent and return it.
pub fn ensure_rescue_dir(data_dir: &Path) -> std::io::Result<PathBuf> {
    let dir = rescue_dir(data_dir);
    fs::create_dir_all(&dir)?;
    Ok(dir)
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p tauri-app backup`
Expected: PASS, 14 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/tauri-app/src/backup.rs
git commit -m "feat(app): rescue directory helpers"
```

---

## Task 10: The four IPC commands

**Files:**
- Modify: `crates/tauri-app/Cargo.toml`
- Create: `crates/tauri-app/capabilities/default.json`
- Modify: `crates/tauri-app/src/commands.rs`
- Modify: `crates/tauri-app/src/lib.rs`

- [ ] **Step 1: Add the dialog plugin**

In `crates/tauri-app/Cargo.toml`, under `[dependencies]`, after the `tauri` line:

```toml
tauri-plugin-dialog = "2"
```

- [ ] **Step 2: Create the capability file**

Without this the dialog calls are denied at runtime. Create `crates/tauri-app/capabilities/default.json`:

```json
{
  "$schema": "../gen/schemas/desktop-schema.json",
  "identifier": "default",
  "description": "Core window permissions plus native file dialogs for backup and restore.",
  "windows": ["main"],
  "permissions": [
    "core:default",
    "dialog:allow-open",
    "dialog:allow-save",
    "dialog:allow-confirm"
  ]
}
```

- [ ] **Step 3: Register the plugin**

In `crates/tauri-app/src/lib.rs`, in `run()`, add the plugin before `.setup(`:

```rust
        .plugin(tauri_plugin_dialog::init())
```

- [ ] **Step 4: Add the commands**

Append to `crates/tauri-app/src/commands.rs`:

```rust
// ---- Backup / restore commands ----

#[derive(Serialize)]
pub struct BackupResult {
    pub path: String,
    pub bytes: u64,
    /// When this backup was written, Unix ms. Returned so the frontend can
    /// refresh its cached `last_backup_at` — the React settings context only
    /// learns about a setting the UI itself wrote, and this one is written here
    /// in Rust.
    pub at: i64,
}

/// Write a manual snapshot into `dest_dir`, remember the folder for automatic
/// backups, and record the time.
#[tauri::command]
pub fn backup_database(
    state: State<AppState>,
    dest_dir: String,
) -> Result<BackupResult, AppError> {
    let db = state.db.lock().unwrap();
    let conn = db.conn()?;
    let now = now_ms() as i64;

    let dir = std::path::PathBuf::from(&dest_dir);
    let dest = dir.join(crate::backup::snapshot_name(crate::backup::MANUAL_PREFIX, now));
    let bytes = crate::backup::snapshot_to(conn, &dest)?;

    // Remember where the user keeps backups so auto-backup-on-close can use it.
    accounting_core::set_setting(conn, "backup_folder", &dest_dir)?;
    accounting_core::set_setting(conn, "last_backup_at", &now.to_string())?;

    Ok(BackupResult { path: dest.to_string_lossy().into_owned(), bytes, at: now })
}

#[derive(Serialize)]
pub struct RestoreResult {
    /// Where the pre-restore safety copy was written.
    pub rescue_path: String,
}

/// Replace the live ledger with `src_path`.
///
/// Order matters: validate the candidate, safety-copy the current ledger, drop
/// the live connection, then swap. The app must be restarted afterwards —
/// startup rebuilds every projection from the restored log.
#[tauri::command]
pub fn restore_database(
    app: tauri::AppHandle,
    state: State<AppState>,
    src_path: String,
) -> Result<RestoreResult, AppError> {
    use tauri::Manager;

    let candidate = std::path::PathBuf::from(&src_path);
    crate::backup::validate_candidate(&candidate)
        .map_err(|e| AppError { message: e.to_string() })?;

    let data_dir = app
        .path()
        .app_local_data_dir()
        .map_err(|e| AppError { message: format!("cannot locate the data folder: {e}") })?
        .join("accounting");
    let live = data_dir.join("ledger.db");

    let mut db = state.db.lock().unwrap();
    let now = now_ms() as i64;

    // Safety copy first: a restore must always be undoable.
    let rescue = crate::backup::ensure_rescue_dir(&data_dir)?;
    let rescue_path = rescue.join(crate::backup::snapshot_name(crate::backup::RESCUE_PREFIX, now));
    {
        let conn = db.conn()?;
        crate::backup::snapshot_to(conn, &rescue_path)?;
    }
    let _ = crate::backup::prune(&rescue, crate::backup::RESCUE_PREFIX, crate::backup::KEEP_AUTO);

    // Close the live connection before overwriting the file it points at.
    db.conn = None;
    crate::backup::swap_in_place(&candidate, &live)?;

    Ok(RestoreResult { rescue_path: rescue_path.to_string_lossy().into_owned() })
}

#[derive(Serialize)]
pub struct ExportResult {
    pub path: String,
    pub events: usize,
}

/// Write the whole event log as JSONL into `dest_dir`.
#[tauri::command]
pub fn export_event_log(
    state: State<AppState>,
    dest_dir: String,
) -> Result<ExportResult, AppError> {
    let db = state.db.lock().unwrap();
    let conn = db.conn()?;
    let now = now_ms() as i64;

    let name = format!("ledger-{}.jsonl", crate::backup::timestamp_utc(now));
    let dest = std::path::PathBuf::from(&dest_dir).join(name);
    let file = std::fs::File::create(&dest)?;
    let mut writer = std::io::BufWriter::new(file);

    let events = accounting_core::export_jsonl(conn, &mut writer, now, env!("CARGO_PKG_VERSION"))?;

    Ok(ExportResult { path: dest.to_string_lossy().into_owned(), events })
}

#[derive(Serialize)]
pub struct ImportResult {
    pub inserted: usize,
    pub skipped_duplicates: usize,
    pub total_events: usize,
    pub rescue_path: String,
}

/// Merge a JSONL archive into the live ledger.
///
/// Takes the same safety copy as restore, because the merge ends with a
/// reconciliation check that can reject a ledger which was healthy before.
#[tauri::command]
pub fn import_event_log(
    app: tauri::AppHandle,
    state: State<AppState>,
    src_path: String,
) -> Result<ImportResult, AppError> {
    use tauri::Manager;

    let data_dir = app
        .path()
        .app_local_data_dir()
        .map_err(|e| AppError { message: format!("cannot locate the data folder: {e}") })?
        .join("accounting");

    let mut db = state.db.lock().unwrap();
    let now = now_ms() as i64;

    let rescue = crate::backup::ensure_rescue_dir(&data_dir)?;
    let rescue_path = rescue.join(crate::backup::snapshot_name(crate::backup::RESCUE_PREFIX, now));
    {
        let conn = db.conn()?;
        crate::backup::snapshot_to(conn, &rescue_path)?;
    }
    let _ = crate::backup::prune(&rescue, crate::backup::RESCUE_PREFIX, crate::backup::KEEP_AUTO);

    let file = std::fs::File::open(&src_path)?;
    let reader = std::io::BufReader::new(file);

    let crate::state::Db { ref mut conn, ref mut hlc } = *db;
    let conn = conn.as_mut().ok_or_else(|| AppError {
        message: "Restore finished. Please close and reopen the app.".into(),
    })?;
    let summary = accounting_core::import_jsonl(conn, reader)?;

    // Advance the clock past everything merged in, so events this install
    // appends next sort after the imported ones. rehydrate_from_log reads
    // MAX(hlc) and calls Hlc::observe — exactly what is needed here.
    accounting_core::rehydrate_from_log(conn, hlc, now_ms())?;

    Ok(ImportResult {
        inserted: summary.inserted,
        skipped_duplicates: summary.skipped_duplicates,
        total_events: summary.total_events,
        rescue_path: rescue_path.to_string_lossy().into_owned(),
    })
}
```

- [ ] **Step 5: Register the commands**

In `crates/tauri-app/src/lib.rs`, add to the `generate_handler!` list after `commands::list_payments,`:

```rust
            commands::backup_database,
            commands::restore_database,
            commands::export_event_log,
            commands::import_event_log,
```

- [ ] **Step 6: Build**

Run: `cargo build 2>&1 | tail -20`
Expected: succeeds.

- [ ] **Step 7: Run the full suite**

Run: `cargo test 2>&1 | tail -8`
Expected: all pass.

- [ ] **Step 8: Commit**

```bash
git add crates/tauri-app/Cargo.toml crates/tauri-app/capabilities/default.json crates/tauri-app/src/commands.rs crates/tauri-app/src/lib.rs Cargo.lock
git commit -m "feat(app): backup, restore, export and import IPC commands"
```

---

## Task 11: Automatic backup on window close

**Files:**
- Modify: `crates/tauri-app/src/lib.rs`

Skips silently when no folder is remembered — nagging on every exit would be worse than a missed backup. Failures are logged, never surfaced: the app is closing, and a stale `last_backup_at` will show up in Preferences next launch.

- [ ] **Step 1: Add the auto-backup helper**

In `crates/tauri-app/src/lib.rs`, add before `pub fn run()`:

```rust
/// Write an automatic snapshot into the remembered backup folder, then prune.
///
/// Returns `Ok(false)` when there is nothing to do (no folder remembered yet).
/// Never panics: this runs while the window is closing.
fn auto_backup_on_close(state: &AppState) -> Result<bool, String> {
    let mut db = state.db.lock().map_err(|_| "state lock poisoned".to_string())?;
    let conn = match db.conn.as_ref() {
        Some(c) => c,
        None => return Ok(false), // a restore already closed it
    };

    let settings = accounting_core::get_settings(conn).map_err(|e| e.to_string())?;
    let folder = match settings.get("backup_folder") {
        Some(f) if !f.is_empty() => std::path::PathBuf::from(f),
        _ => return Ok(false), // the user has never chosen a folder
    };
    if !folder.is_dir() {
        return Err(format!("backup folder is unavailable: {}", folder.display()));
    }

    let now = now_ms() as i64;
    let dest = folder.join(backup::snapshot_name(backup::AUTO_PREFIX, now));
    backup::snapshot_to(conn, &dest).map_err(|e| e.to_string())?;
    accounting_core::set_setting(conn, "last_backup_at", &now.to_string())
        .map_err(|e| e.to_string())?;

    // Drop the connection before pruning so nothing holds a file we may remove.
    drop(db);
    backup::prune(&folder, backup::AUTO_PREFIX, backup::KEEP_AUTO).map_err(|e| e.to_string())?;
    Ok(true)
}
```

- [ ] **Step 2: Hook the window close event**

In `crates/tauri-app/src/lib.rs`, in `run()`, add after `.plugin(tauri_plugin_dialog::init())`:

```rust
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::Destroyed = event {
                let state = window.state::<AppState>();
                match auto_backup_on_close(&state) {
                    Ok(true) => eprintln!("automatic backup written"),
                    Ok(false) => {}
                    // Deliberately swallowed: the window is already closing, so
                    // there is nowhere to show this. Next launch shows a stale
                    // "last backup" date instead.
                    Err(e) => eprintln!("automatic backup failed: {e}"),
                }
            }
        })
```

`window.state::<AppState>()` needs `tauri::Manager` in scope; it is already imported at `lib.rs:10`.

- [ ] **Step 3: Build**

Run: `cargo build 2>&1 | tail -20`
Expected: succeeds.

- [ ] **Step 4: Verify by hand**

Run: `cargo tauri dev` from `crates/tauri-app`. In the running app go to Preferences (the Data panel arrives in Task 12, so for now confirm only that the app starts and the existing pages work). Close the window and confirm the log shows no `automatic backup failed` line — with no `backup_folder` set it should print nothing.

- [ ] **Step 5: Commit**

```bash
git add crates/tauri-app/src/lib.rs
git commit -m "feat(app): automatic snapshot on window close with pruning"
```

---

## Task 12: Preferences Data panel

**Files:**
- Modify: `ui/src/i18n/fr.ts`, `ui/src/i18n/en.ts`
- Modify: `ui/src/pages/Preferences.tsx`

`fr.ts` is the source of truth for the `Translations` type, so both files must gain the same keys or `tsc` fails.

- [ ] **Step 1: Add the French keys**

In `ui/src/i18n/fr.ts`, inside the `preferences` block, after `saved: "Préférence enregistrée.",`:

```ts
    data: "Données",
    backupNow: "Sauvegarder maintenant",
    backupDone: "Sauvegarde enregistrée.",
    restore: "Restaurer une sauvegarde",
    restoreConfirm:
      "Remplacer toutes les données actuelles par cette sauvegarde ? Une copie de sécurité de vos données actuelles sera conservée.",
    restoreDone: "Restauration terminée. Veuillez fermer et rouvrir l'application.",
    exportLog: "Exporter le journal",
    exportDone: "Journal exporté.",
    importLog: "Importer un journal",
    importConfirm:
      "Fusionner ce journal avec vos données ? Les écritures déjà présentes seront ignorées.",
    importDone: "Import terminé.",
    lastBackup: "Dernière sauvegarde",
    neverBackedUp: "Jamais",
    chooseFolder: "Choisir un dossier",
    chooseFile: "Choisir un fichier",
```

- [ ] **Step 2: Add the matching English keys**

In `ui/src/i18n/en.ts`, in the same position inside `preferences`:

```ts
    data: "Data",
    backupNow: "Back up now",
    backupDone: "Backup saved.",
    restore: "Restore from backup",
    restoreConfirm:
      "Replace all current data with this backup? A safety copy of your current data will be kept.",
    restoreDone: "Restore complete. Please close and reopen the app.",
    exportLog: "Export event log",
    exportDone: "Event log exported.",
    importLog: "Import event log",
    importConfirm:
      "Merge this event log into your data? Entries already present will be skipped.",
    importDone: "Import complete.",
    lastBackup: "Last backup",
    neverBackedUp: "Never",
    chooseFolder: "Choose a folder",
    chooseFile: "Choose a file",
```

- [ ] **Step 3: Install the dialog JS binding**

Run: `cd ui && npm install @tauri-apps/plugin-dialog && cd ..`
Expected: adds the package to `ui/package.json` dependencies.

- [ ] **Step 4: Add the Data panel**

In `ui/src/pages/Preferences.tsx`, change the import on line 4 and add two more:

```tsx
import { formatMoney, errorMessage } from "../lib";
import { invoke } from "@tauri-apps/api/core";
import { confirm, open } from "@tauri-apps/plugin-dialog";
```

Then add this panel inside the returned JSX, after the closing `</section>` of the currency panel (line 97) and before the final `</div>`:

```tsx
      <section className="panel">
        <h2 style={{ marginTop: 0 }}>{t.preferences.data}</h2>
        <p className="muted">
          {t.preferences.lastBackup}:{" "}
          {settings.last_backup_at
            ? new Date(Number(settings.last_backup_at)).toLocaleString(settings.locale)
            : t.preferences.neverBackedUp}
        </p>
        <div className="form-row">
          <button onClick={backupNow}>{t.preferences.backupNow}</button>
          <button onClick={restore}>{t.preferences.restore}</button>
          <button onClick={exportLog}>{t.preferences.exportLog}</button>
          <button onClick={importLog}>{t.preferences.importLog}</button>
        </div>
      </section>
```

And add these handlers inside the `Preferences` component, after the `update` function (line 16):

```tsx
  const backupNow = async () => {
    const dir = await open({ directory: true, title: t.preferences.chooseFolder });
    if (typeof dir !== "string") return;
    try {
      const r = await invoke<{ at: number }>("backup_database", { destDir: dir });
      // Rust wrote these two settings; mirror them into the settings context so
      // the "last backup" line updates without a relaunch.
      await set("backup_folder", dir);
      await set("last_backup_at", String(r.at));
      toast.push(t.preferences.backupDone);
    } catch (e: unknown) {
      toast.push(errorMessage(e), "error");
    }
  };

  const restore = async () => {
    const file = await open({
      title: t.preferences.chooseFile,
      filters: [{ name: "Backup", extensions: ["db"] }],
    });
    if (typeof file !== "string") return;
    if (!(await confirm(t.preferences.restoreConfirm, { kind: "warning" }))) return;
    try {
      await invoke("restore_database", { srcPath: file });
      toast.push(t.preferences.restoreDone);
    } catch (e: unknown) {
      toast.push(errorMessage(e), "error");
    }
  };

  const exportLog = async () => {
    const dir = await open({ directory: true, title: t.preferences.chooseFolder });
    if (typeof dir !== "string") return;
    try {
      await invoke("export_event_log", { destDir: dir });
      toast.push(t.preferences.exportDone);
    } catch (e: unknown) {
      toast.push(errorMessage(e), "error");
    }
  };

  const importLog = async () => {
    const file = await open({
      title: t.preferences.chooseFile,
      filters: [{ name: "Event log", extensions: ["jsonl"] }],
    });
    if (typeof file !== "string") return;
    if (!(await confirm(t.preferences.importConfirm, { kind: "warning" }))) return;
    try {
      const r = await invoke<{ inserted: number; skipped_duplicates: number }>(
        "import_event_log",
        { srcPath: file },
      );
      toast.push(`${t.preferences.importDone} (+${r.inserted}, ${r.skipped_duplicates})`);
    } catch (e: unknown) {
      toast.push(errorMessage(e), "error");
    }
  };
```

`set` comes from the existing `useSettings()` destructure on line 10 — it is already in scope, no change needed there.

- [ ] **Step 5: Typecheck**

Run: `cd ui && npx tsc --noEmit && cd ..`
Expected: no output (clean). If it reports a missing key, `fr.ts` and `en.ts` have drifted — make them identical.

- [ ] **Step 6: Build the frontend**

Run: `cd ui && npm run build && cd ..`
Expected: succeeds.

- [ ] **Step 7: Manual end-to-end check**

Run `cargo tauri dev` from `crates/tauri-app`, then in the app:
1. Preferences → Data → **Back up now**, choose a folder. Expect a success toast and a `ledger-<stamp>.db` file in that folder.
2. **Export event log** into the same folder. Expect `ledger-<stamp>.jsonl`; open it and confirm line 1 is the header and the rest are events.
3. **Import event log**, choosing the file just exported. Expect `(+0, N)` — every event is a duplicate, nothing inserted.
4. Close the app, reopen it. Expect a `ledger-auto-<stamp>.db` in the remembered folder, and "Last backup" showing a recent time.
5. **Restore from backup**, choosing the `.db` from step 1. Expect the "please restart" message. Restart and confirm the data is intact.

- [ ] **Step 8: Commit**

```bash
git add ui/src/i18n/fr.ts ui/src/i18n/en.ts ui/src/pages/Preferences.tsx ui/package.json ui/package-lock.json
git commit -m "feat(ui): Data panel for backup, restore, export and import"
```

---

## Task 13: Final verification

**Files:** none modified.

- [ ] **Step 1: Full Rust suite**

Run: `cargo test 2>&1 | tail -10`
Expected: all pass. Baseline was 135 in `accounting-core`; expect ~157 core plus 14 in `tauri-app`.

- [ ] **Step 2: Typecheck and build the frontend**

Run: `cd ui && npx tsc --noEmit && npm run build && cd ..`
Expected: clean, then a successful build.

- [ ] **Step 3: Release build**

Run: `cargo build --release 2>&1 | tail -5`
Expected: succeeds.

- [ ] **Step 4: Confirm no stray device literal**

Run: `rg -n '"device-1"' crates/tauri-app/src/`
Expected: no matches.

- [ ] **Step 5: Review the whole branch**

Run: `git diff main...HEAD --stat`
Expected: changes confined to the files in the File structure table.

---

## Notes for the implementer

**Do not change these without revisiting the spec:**
- Backups use `VACUUM INTO`, never `fs::copy` — WAL mode makes a plain copy unsafe.
- `swap_in_place` must delete `-wal` and `-shm`. Skipping that can corrupt the restored file.
- `import_jsonl` must not write `app_settings`. Merging a colleague's log must never change local currency, locale, or `device_id`.
- Retention filters on `AUTO_PREFIX`, never `MANUAL_PREFIX`. Since `"ledger-auto-"` starts with `"ledger-"`, filtering on the manual prefix would delete the user's manual backups.
- `ensure_device_id` runs before `rehydrate_from_log`.

**If a test is hard to write, that is a signal.** The core functions take `&Connection` precisely so they can be tested in-memory. Do not reach for Tauri test harnesses.
