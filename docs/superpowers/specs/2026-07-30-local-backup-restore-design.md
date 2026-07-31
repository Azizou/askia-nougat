# Local Backup & Restore — Design

**Status:** Approved 2026-07-30
**Phase:** 1 of 2. Phase 2 (Google Drive upload) is a separate spec and consumes the
artifacts this phase produces.

## Goal

Give a non-technical single-user desktop install a reliable way to protect and recover
its ledger, with no server, no account, and no running cost: a consistent snapshot of
the database for recovery, and a portable event-log export that can be merged back in.

## Context

The app is an event-sourced double-entry accounting desktop app (Tauri 2 + Rust +
SQLite + React). Facts relevant to this design, all verified in the codebase:

- `events` (`crates/accounting-core/src/schema.sql:2-12`) is the append-only source of
  truth. Nothing updates or deletes rows; the only production writer is `append_event`
  (`crates/accounting-core/src/events.rs:37`).
- `UNIQUE (device_id, seq)` is declared on `events` (`schema.sql:11`), and `seq` is
  assigned as `MAX(seq)+1` **per device_id** (`events.rs:27-30`).
- An event's `id` **is** its HLC stamp (`events.rs:33`), formatted
  `{physical:015}:{counter:06}:{device_id}` (`crates/accounting-core/src/hlc.rs:27`).
- `Hlc::observe` (`hlc.rs:34`) already exists for merging foreign stamps — the clock was
  designed for multi-device; only the device identity was stubbed.
- Payloads are stored as `jsonb` BLOBs (`events.rs:38`) and read back via `json(payload)`
  (`events.rs:60`).
- `app_settings` (`schema.sql:22-27`) is deliberately **not** event-sourced and is
  excluded from `PROJECTION_TABLES` (`crates/accounting-core/src/projectors.rs:758-766`),
  so `rebuild` neither clears nor repopulates it.
- `SETTING_KEYS` (`crates/accounting-core/src/settings.rs:6-13`) is an allowlist;
  `set_setting` rejects any key not listed.
- Startup calls `rebuild()` unconditionally on every launch
  (`crates/tauri-app/src/lib.rs:41`), so projections are always derived fresh from the log.
- The database lives at `app_local_data_dir()/accounting/ledger.db` in **WAL** mode
  (`lib.rs:16-24`).
- `device_id` is currently the hardcoded literal `"device-1"` in four places
  (`commands.rs:23`, `lib.rs:27`, `lib.rs:32`, `lib.rs:38`).
- No backup, export, filesystem, or HTTP code exists. No Tauri plugins are installed and
  no `capabilities/` directory exists. `uuid` 1.23.4 is already in `Cargo.lock`
  transitively. Tauri is 2.11.5.

### Why both a snapshot and an event log

Because the log is append-only, snapshot N+1 is a strict superset of snapshot N — many
snapshots are nested prefixes of one history, not independent versions. That makes deep
retention nearly worthless (hence 3, not 10) and makes an incremental event export the
natural shape for this store. The snapshot is kept as the recovery mechanism because it
captures `app_settings` too and needs no new import machinery; the event log is kept
because it is portable, mergeable, and re-validates the whole ledger on import.

## Scope

**In scope:** per-install device identity; snapshot backup and restore; automatic backup
on app close with retention; JSONL event-log export; JSONL import that merges by event id.

**Out of scope:** Google Drive (Phase 2); encryption; live multi-device sync; Android
(desktop only — loopback/file-picker behaviour differs).

---

## A. Per-install device identity

Merge-by-id is **unsound** without this, in two ways:

1. Every install is `device-1` and every install numbers `seq` from 1, so importing a
   foreign log violates `UNIQUE (device_id, seq)` as soon as ranges overlap — immediately.
2. Worse, silently: because `id` is the HLC and embeds `device_id`, two installs that
   append in the same millisecond with the same counter mint a **byte-identical `id` for
   two different events**. Dedupe-by-id would discard a real event. Genesis makes this
   near-certain, since both installs run `run_genesis` as their first action.

**Change.** Add `"device_id"` to `SETTING_KEYS`. New function in `settings.rs`:

```rust
pub fn ensure_device_id(conn: &Connection) -> rusqlite::Result<String>
```

Returns the stored `app_settings.device_id`; if absent, generates `Uuid::new_v4()`,
stores it, and returns it. Idempotent. `lib.rs:27` becomes
`Hlc::new(ensure_device_id(&conn)?)`, and the `"device-1"` literals at `commands.rs:23`,
`lib.rs:32`, and `lib.rs:38` are replaced by that value threaded through.

**Ordering requirement.** `ensure_device_id` must run **before** `rehydrate_from_log`
(`lib.rs:28`), so the clock knows its identity before seeding from the log's max HLC.

**Upgrade path.** An existing v0.1.1 install has `device-1` events and no `device_id`
setting. It mints a UUID and authors all future events under it. Historical `device-1`
events are never rewritten. The log then legitimately contains two device_ids; because
`seq` is per-device, the new identity starts cleanly at `seq = 1` with no collision, and
`missing_seqs` (`events.rs:83`) is already per-device so gap detection still works.

`uuid` is declared with the `v4` feature; it is already in the dependency tree, so no new
third-party code enters the build.

---

## B. Snapshot backup and restore

### Backup

`VACUUM INTO ?1` — one statement, yielding a consistent, compacted, single-file copy.
This is required rather than `fs::copy`: the database is in WAL mode, so a raw file copy
can capture a torn state with unmerged `-wal` content. `VACUUM INTO` fails if the target
exists, so a fresh timestamped path is generated per run rather than overwriting.

Filenames distinguish the two origins, because retention treats them differently:

- manual: `ledger-YYYYMMDD-HHMMSS.db`
- automatic: `ledger-auto-YYYYMMDD-HHMMSS.db`

Retention only ever considers files matching the `ledger-auto-` prefix, so a manual backup
can never be pruned even if it sits in the same folder.

### Restore

Strict order — each step exists to protect the next:

1. **Validate the candidate first.** Open read-only; require `PRAGMA integrity_check` to
   return `ok`; require an `events` table to exist with row count ≥ 1. A truncated or
   foreign file is rejected *before* the live database is at risk.
2. **Safety-copy** the live ledger with `VACUUM INTO` to
   `app_local_data_dir()/accounting/rescue/pre-restore-<timestamp>.db`.
3. **Swap.** Close the live connection, copy the candidate over `ledger.db`, then delete
   the stale `ledger.db-wal` and `ledger.db-shm`. Deleting the sidecars is mandatory:
   leaving them lets SQLite replay the *old* WAL onto the *new* file and corrupt it. This
   is the sharpest edge in the feature.
4. **Prompt restart.** The live connection is gone and all projections must be rederived.
   Because `lib.rs:41` already rebuilds unconditionally at startup, restart recovery needs
   no new code.

### Retention

Automatic backups keep the newest **3**, identified solely by the `ledger-auto-` filename
prefix; manual backups are never pruned (the user chose those paths deliberately). Rescue
copies also keep the newest 3, by the `pre-restore-` prefix. Three rather than ten because
of the append-only prefix property described above.

### Automatic backup on close

Writes a snapshot to a remembered folder, new setting `backup_folder`, which is set the
first time the user performs a manual backup. If unset, auto-backup is skipped silently
rather than nagging on every exit. A failure during close is logged and swallowed —
blocking or erroring while the window closes is worse than a missed backup, and the stale
`last_backup_at` on next launch surfaces the problem.

New setting keys added to `SETTING_KEYS`: `backup_folder`, `last_backup_at`
(plus `device_id` from section A).

---

## C. JSONL event-log export

New core module `crates/accounting-core/src/archive.rs`. Newline-delimited JSON: a header
line, then one event per line in HLC order (`read_events` already guarantees that order).

```
{"format":"accounting-eventlog","version":1,"exported_at":<ms>,"app_version":"0.1.2","device_id":"<uuid>","settings":{...}}
{"id":"...","hlc":"...","device_id":"...","user_id":"...","seq":1,"type":"ItemDefined","payload":{...},"created_at":...}
```

Filename: `ledger-YYYYMMDD-HHMMSS.jsonl`.

The explicit `format` + `version` pair lets a future format change be **detected and
rejected** rather than misparsed. Payloads are read with `json(payload)` so they serialize
as real nested JSON, not an escaped string. The `settings` map is carried for archival and
human recovery only — see the settings rule in section D.

---

## D. JSONL import — merge by event id

New raw insert in `events.rs`, placed beside `append_event` so it inherits that file's
invariant tests:

```rust
pub fn insert_raw_event(conn: &Connection, ev: &LedgerEvent) -> rusqlite::Result<()>
```

It preserves `id`, `hlc`, `device_id`, `user_id`, `seq`, and `created_at` verbatim and
mints nothing. `append_event` cannot serve as the import path precisely because it mints a
fresh identity on every call (`events.rs:32-44`).

```rust
pub fn import_jsonl(conn: &mut Connection, reader: impl BufRead)
    -> Result<ImportSummary, ArchiveError>
```

Algorithm. All event inserts occur in **one transaction**, so a mid-file failure leaves
the log untouched:

1. Parse and validate the header; reject an unknown `format` or `version`.
2. For each event, if `id` already exists locally → skip, count as duplicate.
3. **Collision guard:** if an incoming event carries a `(device_id, seq)` that exists
   locally under a *different* `id`, abort the entire import with an explanatory error.
   This is the legacy `device-1` case from section A; `schema.sql:11` would reject it
   anyway, and failing loudly beats surfacing a raw UNIQUE-constraint error.
4. Otherwise insert via `insert_raw_event`.
5. Commit, then `rebuild(&mut conn)`, then `run_all_checks` — a merge is accepted only if
   the combined ledger still reconciles.
6. `hlc.observe()` the maximum imported HLC (`hlc.rs:34`) so future local events sort
   after everything merged in.

Returns `ImportSummary { inserted, skipped_duplicates, total_events }`.

Because step 5 can fail on a ledger that was healthy beforehand, import takes the same
safety-copy as restore (section B, step 2) before starting.

### Settings rule

Merge import **never** writes `app_settings`. The header's `settings` map is archival
only. Two reasons: merging another log must not silently change your currency or locale,
and `device_id` must never be overwritten or the install loses its identity. Whole-install
settings recovery is what snapshot restore is for, since it swaps the entire file.

**Rule: merge import = events only. Snapshot restore = everything.**

---

## Components and boundaries

All event and log semantics live in `accounting-core` as pure functions over
`&Connection`, testable in-memory with no Tauri — matching how the rest of the core is
tested. All file I/O, dialogs, and lifecycle hooks live in `tauri-app`.

| File | Responsibility |
|---|---|
| `crates/accounting-core/src/settings.rs` (modify) | `ensure_device_id`; three new allowlisted keys |
| `crates/accounting-core/src/events.rs` (modify) | `insert_raw_event` |
| `crates/accounting-core/src/archive.rs` (create) | JSONL export/import, merge, `ArchiveError`, `ImportSummary` |
| `crates/accounting-core/src/lib.rs` (modify) | re-export the new surface |
| `crates/tauri-app/src/backup.rs` (create) | `VACUUM INTO`, validation, safe swap, retention, paths |
| `crates/tauri-app/src/commands.rs` (modify) | four IPC commands |
| `crates/tauri-app/src/lib.rs` (modify) | device id wiring, register commands, auto-backup on close |
| `crates/tauri-app/src/state.rs` (modify) | `Db.conn` becomes `Option<Connection>` so restore can drop it |
| `crates/tauri-app/src/error.rs` (modify) | `From<ArchiveError>`, `From<std::io::Error>` |
| `crates/tauri-app/capabilities/default.json` (create) | dialog permissions |
| `ui/src/pages/Preferences.tsx` (modify) | Data panel |
| `ui/src/i18n/fr.ts`, `ui/src/i18n/en.ts` (modify) | new keys, both files |

### IPC surface

```
backup_database(dest_dir: String) -> BackupResult { path, bytes }
restore_database(src_path: String) -> RestoreResult { rescue_path }
export_event_log(dest_dir: String) -> ExportResult { path, events }
import_event_log(src_path: String) -> ImportSummary { inserted, skipped_duplicates, total_events, rescue_path }
```

The frontend supplies `dest_dir`/`src_path` from the native picker; filenames are generated
in Rust so naming and the retention prefixes stay in one place.

### Connection lifecycle for restore

`AppState` holds `Mutex<Db>` with a live `Connection` (`crates/tauri-app/src/state.rs`).
`restore_database` must drop that connection before overwriting the file, so `Db.conn`
becomes an `Option<Connection>` — taken and dropped during the swap, then left `None`.
Every subsequent command errors with "restore complete, please restart" until relaunch,
which is the correct terminal state given step 4 already requires a restart. This keeps the
`with_ctx!` macro (`commands.rs:17-27`) as the single place that unwraps the connection.

---

## UI

A new `Data` panel in `ui/src/pages/Preferences.tsx`, following the existing
`<section className="panel">` pattern, with four actions — Back up now, Restore from
backup, Export event log, Import event log — and a "last backup" line reading
`last_backup_at`. The two destructive actions require confirmation naming the safety-copy
location. Restore and import end with a "please restart the app" message.

New i18n keys go in **both** `fr.ts` and `en.ts`. `fr.ts` is the source of truth for the
`Translations` type, so `en.ts` must match exactly or `tsc` fails. French is the default
locale.

`tauri-plugin-dialog` is added for native file pickers. Because no `capabilities/`
directory exists yet, a capability file must be created granting `dialog:allow-open` and
`dialog:allow-save` — without it the plugin calls are denied at runtime.

## Error handling

`AppError` (`error.rs`) gains `From<ArchiveError>` and `From<std::io::Error>`; conversions
for `rusqlite::Error` and `CommandError` already exist. Every failure must be actionable in
plain language, distinctly for: unreadable or corrupt file, insufficient disk space,
format/version mismatch, `(device_id, seq)` collision, and post-merge reconciliation
failure.

## Testing

Core tests are in-memory and Tauri-free, matching the existing 135-test suite.

- `ensure_device_id` mints once, is idempotent, and survives reopen.
- Export → import into an empty DB: event `id`/`hlc`/`seq` identical to source, and
  projections plus `run_all_checks` match.
- Re-import the same file: everything skipped, zero inserted, ledger unchanged
  (idempotence).
- Merge two disjoint logs: both event sets present, HLC order correct, checks pass.
- `(device_id, seq)` collision under a different `id`: aborts, log unchanged.
- Malformed header, unknown version, truncated final line, empty file: clean errors, no
  partial writes.
- Snapshot round-trip against a **file-backed** database in WAL mode with pending writes —
  proves `VACUUM INTO` is correct where `fs::copy` is not.
- Retention keeps exactly the 3 newest `ledger-auto-` files and never prunes a manual
  `ledger-` file sitting in the same folder.
- Restore rejects a candidate whose `integrity_check` fails, and rejects one with zero
  events, leaving the live ledger and its WAL untouched.

## Consequences

- The `device_id` change is schema-adjacent and permanent; it is also the prerequisite for
  any future multi-device sync, which `Hlc::observe` was already built for.
- One install's log may legitimately contain two device_ids after upgrade.
- Merging logs from two *pre-UUID* installs is not supported and fails loudly by design.
- Phase 2 uploads the artifacts defined here without changing their formats.
