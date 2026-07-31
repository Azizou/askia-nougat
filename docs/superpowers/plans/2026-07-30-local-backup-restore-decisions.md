# Local Backup & Restore — Autonomous Execution Decisions

Running log of judgment calls made while executing
`docs/superpowers/plans/2026-07-30-local-backup-restore.md` without check-ins.
Each entry: what was decided, why, and what would change it.

**Mode:** subagent-driven — fresh implementer per task, then a spec-compliance
reviewer, then a code-quality reviewer. Reviewers are read-only.

---

## D0. Standing rules for this run

- **Reviewers are read-only.** They report; the implementer fixes. This keeps the
  reviewer's judgment independent of authorship.
- **No task advances with an open review finding.** Spec compliance must be clean
  before code quality starts.
- **Baseline is 135 `accounting-core` tests.** Any drop is a regression, not an
  acceptable trade.
- **`feeback-v2.md` stays untracked** and out of every commit — it predates this
  work and is not mine to commit.
- **Branch:** `feat/local-backup-restore`, off clean `main`. Nothing lands on
  `main` in this run.

## D1. Model tier per role

Cheap/fast tier for mechanical tasks with a complete spec; standard tier for
multi-file integration; most capable for review.

Rationale: the plan specifies exact code for most tasks, which makes them
mechanical. Review is where judgment pays, so that is where the capable model
goes. Revisit if a cheap-tier implementer returns BLOCKED for reasons of
reasoning rather than missing context.

---

## D2. Implementers get the standard tier, not the cheap tier

The plan specifies exact code, which by D1 argues for a cheap model. Overriding
that for Rust specifically: the borrow checker turns small misreadings into
compile-error loops, and a cheap model iterating against `cargo build` costs more
wall-clock and tokens than getting it right once. Reviewers stay on the most
capable tier, where judgment is the whole job.

Revisit if Tasks 1-3 land clean on the first try with no review findings — that
would be evidence the cheap tier is adequate for the remaining mechanical tasks.

## D3. Dev server left stopped during the run

`cargo tauri dev` is not running (the tauri/vite processes on this machine belong
to unrelated projects). Leaving it stopped: a running dev server holds the
`target/` build lock and would serialize or fail every implementer's `cargo test`.
Task 11 and 12 call for manual in-app verification, so it gets started then.

## D4. Reviewers are read-only by tool grant, not just by instruction

Spec and quality reviewers are dispatched as `Explore`, which has no Edit/Write.
An instruction not to edit is a request; removing the tool is a guarantee. It also
keeps the "implementer fixes, reviewer re-reviews" loop from collapsing into the
reviewer quietly patching its own findings.

---

## D5. Review cadence changed: one combined reviewer, at layer boundaries

**Observed:** the two Task 1 reviewers (spec + quality, dispatched in parallel) ran
~20 minutes without returning a report. At 3 agents/task × 13 tasks that cadence
does not finish.

**Decision:** collapse to a **single reviewer** covering spec compliance *and* code
quality, dispatched at **layer boundaries** rather than after every task:

| Reviewed together | Why this is a coherent unit |
|---|---|
| Tasks 1-3 | core primitives: identity + raw insert |
| Tasks 4-6 | `archive.rs` — export, parse, merge |
| Tasks 7-9 | `backup.rs` — snapshot, validate/swap, paths |
| Tasks 10-11 | Tauri wiring: IPC + lifecycle |
| Task 12 | UI |
| Task 13 | whole-branch final review |

**Why this is still sound:** the standing rule is that no task advances unreviewed,
and it still holds — a boundary review gates the *next* layer, and layers are where
integration defects actually live (a bug in `insert_raw_event` shows up when
`import_jsonl` uses it, not in isolation). Reviewing a coherent unit also gives the
reviewer the context to judge the seams, which per-task review structurally cannot.

**What I do instead, per task, myself:** independently verify the diff (`git show`),
confirm the test count moved as predicted, confirm only the permitted files changed,
and confirm nothing out-of-scope was touched. That is a real gate, not a rubber stamp
— it caught nothing on Task 1, which is a pass, not an absence of checking.

**Reverts if:** a boundary review returns Critical findings that a per-task review
would plainly have caught earlier. That would be evidence the cadence is too coarse.

## D6. Task 1 accepted; test assertion strengthened

My own verification of `f5c19a3`: 138/138 tests (135 + 3, exactly as predicted);
only the 4 permitted files touched; all four `"device-1"` literals still present in
`tauri-app` (3 in `lib.rs`, 1 in `commands.rs`) as Task 2 requires; `feeback-v2.md`
not committed; `uuid` added with only the `v4` feature. Separately confirmed via a
file-backed SQLite check that `app_settings.device_id` survives a reopen — the
plan's tests use in-memory DBs and structurally cannot show that.

**One deviation from the plan, applied deliberately:** the plan's test asserted
`id.len() == 36`, which passes for *any* 36-character string. Since a wrong
`device_id` silently corrupts event identity, the assertion now parses the value as
a UUID and asserts version 4. Strengthening a test for a load-bearing, write-once
invariant is worth a small departure from the plan text; the plan's intent
("expected a hyphenated UUID v4") is preserved, only the check is made real.

**Residual minor, accepted:** `ensure_device_id` hardcodes `'device_id'` in its SQL
while the allowlist holds the same string separately. Two spellings of one key could
drift. Left as-is — extracting a constant for a single call site is churn, and the
`device_id_is_an_allowed_key` test fails loudly if they ever diverge.

---

## D7. Task 1 reopened: `device_id` must not be writable through IPC

The Task 1 reviewers eventually returned (~20 min late, which is what prompted D5).
The quality reviewer raised one finding I verified myself and accept as a **real
defect**, not a style note:

- `crates/tauri-app/src/commands.rs:154` — the `set_setting` IPC command accepts an
  arbitrary `key: String` and forwards it unchecked.
- `ui/src/settings.tsx:66-69` — the frontend `set()` is a generic key/value passthrough.
- `crates/accounting-core/src/settings.rs:44-48` — `set_setting` is an unconditional
  upsert (`ON CONFLICT(key) DO UPDATE`).

Adding `device_id` to `SETTING_KEYS` therefore made
`invoke("set_setting", {key: "device_id", value: ...})` a permanent, unrecoverable
overwrite of the install's identity — the precise thing the allowlist's own comment
(`settings.rs:4-5`) says it exists to prevent, and which the new comment
(`settings.rs:13-14`, "must never change once minted") asserts but nothing enforces.

**Decision:** harden it. `ensure_device_id` mints with
`INSERT ... ON CONFLICT(key) DO NOTHING` followed by a re-`SELECT`, and `device_id`
comes back **out** of `SETTING_KEYS`. This closes the IPC hole and the non-atomic
read-then-write race (the reviewer's second finding) in one change: the loser of a
race converges on the winner's id instead of diverging, and "never overwritten" holds
at the SQL level rather than by convention. `get_settings` ignores the allowlist, so
the UI can still *read* the value — nothing legitimate loses the ability to write it,
since merge-import deliberately never writes `app_settings` and snapshot restore
swaps the whole file.

**Deviation from the plan, accepted:** the plan prescribed routing the mint through
`set_setting`, which is what forced the allowlist entry. The plan was wrong here.

**Also adopted from that review:** a test pinning the `app_settings`-survives-`rebuild`
coupling. The whole reason identity lives in `app_settings` is that it is absent from
`PROJECTION_TABLES` (`projectors.rs:758-766`) while `rebuild()` runs on every startup
(`tauri-app/src/lib.rs:41`) — a load-bearing coupling across two files that is asserted
nowhere. Someone tidying that list would wipe every install's identity with the suite
still green.

**Not adopted:** extracting a `"device_id"` constant (the reviewer itself argued
against it, and the atomic insert reduces three spellings to two); rustfmt cleanup
(17 other files in the crate are already fmt-dirty, there is no `rustfmt.toml` and no
CI fmt gate — fixing one file is noise, not consistency).

**Sequencing:** the fix is held until the archive implementer stops touching
`accounting-core`, to avoid two writers in one working tree. It is not urgent in
minutes — no current UI code path writes that key, so the hole is reachable only by
someone deliberately calling the IPC command.

## D8. The late Task 1 reviews do not revert D5

D5 said the cadence reverts if a boundary review surfaces a Critical finding a
per-task review would plainly have caught earlier. These *were* per-task reviews, and
what they found was one Important issue in a 16-line function — reachable only via a
deliberate IPC call, with no current caller. That is worth fixing (D7) and was not
worth 3 agents × 13 tasks of wall-clock. Cadence stands.

Worth recording that the two reviewers disagreed: the spec reviewer passed the
`id.len() == 36` assertion as "not a tautology," while the quality reviewer correctly
observed the whole suite would stay green against a hardcoded constant. The quality
reviewer was right, and I had already proven and fixed that in D6 before either
reported. Spec-compliance review and quality review are not redundant, and where they
conflict the one reasoning about what the test would *fail* to catch wins.

## D9. Archive layer (Tasks 4-6) accepted; one wording defect fixed

Verified `e2cb2a4`, `5684875`, `a9919cb` myself: 160 → 163 tests (the 19 archive tests
include all 9 merge tests), clippy clean, only permitted files touched, `feeback-v2.md`
still untracked.

**One real defect found and fixed (`68e632c`).** `ArchiveError::Reconciliation`'s doc
said "the import was rolled back" and its user-facing message said "import was
cancelled" — but `tx.commit()` runs *before* the reconciliation check, so the events
stay committed. Telling a user their ledger is untouched when it has been modified
sends them looking for the wrong recovery. The commit-then-check ordering is itself
correct and deliberate (design section D step 5: the checks read projections, which
cannot exist until the merged events are committed and replayed), so the fix was to
the wording, not the control flow. The message now names the real remedy — the safety
copy the caller takes before importing.

This is the second instance of the same class of bug in this run: a comment asserting
an invariant the code does not enforce (D7 was the first). Worth watching for
specifically in the remaining tasks.

**Implementer deviation, accepted:** the test fixture calls `run_genesis` before
appending events. My spec's fixture omitted it, but `run_all_checks` needs the chart
of accounts to exist, so without genesis the reconciliation assertions would have been
vacuous or failing. Correct call.

## D10. Layer sequencing under two concurrent agents

The backup layer (Tasks 7-9, `crates/tauri-app/`) is dispatched to run while the core
layer review runs read-only over `crates/accounting-core/`. Different directories, and
the reviewer cannot write, so there is no conflict on one working tree. The reviewer is
explicitly told to ignore anything newer than `b0d5cae` so it does not review the
backup layer half-written.

The D7 `device_id` fix was deliberately applied *before* dispatching the backup
implementer rather than concurrently — `settings.rs` is core, and two writers in one
tree is how you get lost work.

## D11. `main` moved under the branch; the user's WIP is theirs, not mine to restore

At the start of this run `git status` showed 7 files modified by the user plus an
untracked `feeback-v2.md`. Partway through they were all gone from the working tree,
which looks exactly like lost work. Checked before touching anything further:

- `main` advanced from `0aa52e5` to `49f77fd` (3 new commits), and those commits
  contain precisely the 7 files that had been dirty (`ui/src/i18n/{en,fr}.ts`,
  `ui/src/lib.ts`, `ui/src/pages/{Parties,Payments,Sales}.tsx`, `ui/vite.config.ts`,
  `crates/tauri-app/tauri.conf.json`).
- `git merge-base --is-ancestor main HEAD` → true: this branch is rebased onto the
  new tip, so nothing was dropped.
- `feeback-v2.md` survives in `stash@{0}`'s untracked parent (`stash@{0}^3`), 111 lines.

**Decision:** leave all of it alone. The user committed their own work to `main` and
rebased my branch; that is their call, not a problem to fix. `feeback-v2.md` stays
stashed and uncommitted per D0 — I do not restore it to the working tree, because
popping a stash would also be a change to their tree that they did not ask for.

**Verified the rebase did not break anything** before continuing: 163 `accounting-core`
+ 14 `tauri-app` tests still pass. This mattered because the new `main` commits touch
`tauri.conf.json` and both `Cargo.toml` version fields.

**Re-verified Task 12's anchors against the moved files** rather than trusting the plan:
`errorMessage` is still exported at `ui/src/lib.ts:40`, `formatMoney` gained a `locale`
parameter but keeps its first two arguments, and every line number Task 12 cites in
`Preferences.tsx` and the two i18n `preferences` blocks is still accurate. A plan written
against a pre-rebase tree is exactly where stale line references bite.

## D12. Pre-existing `postcss` advisory not fixed on this branch

`npm audit` reports 1 high-severity advisory: `postcss <= 8.5.17`, GHSA-r28c-9q8g-f849
(path traversal via source-map auto-loading, arbitrary `.map` disclosure). Traced it:
`postcss@8.5.16` arrives only through `vite@6.4.3`. It is not something
`@tauri-apps/plugin-dialog` introduced — that install added 2 packages — so it predates
this work.

**Decision:** report it, do not fix it here. `npm audit fix` would bump vite/postcss,
which is a build-toolchain change outside the scope the user approved for this feature.
It is also a dev-dependency-only exposure for a desktop app that serves no untrusted
source maps. Raised to the user as a finding instead.

## D13. Critical: restoring the live ledger onto itself destroyed it (fixed, `1457b8f`)

Found by me during Task 13, not by any reviewer, and not in the plan.

**The bug.** The restore file dialog lets the user pick `ledger.db` — the database
they are using right now. `validate_candidate` accepts it, correctly: it genuinely
is a valid SQLite file with a populated `events` table. Then `swap_in_place` calls
`fs::copy(live, live)`, which **truncates the file to zero bytes and returns
`Ok(0)`** (unlike Python's `shutil.copyfile`, which raises `SameFileError`), and the
mandatory `-wal`/`-shm` deletion immediately afterwards throws away the only
remaining copy of everything not yet checkpointed. Reproduced end to end against a
2334-event WAL ledger:

```
events before      : 2334
main .db bytes     : 4386816
validate_candidate : ACCEPTED the live database as a restore source
main .db bytes     : 0            <-- after swap_in_place(live, live)
-wal still there?  : false
events after       : UNREADABLE — no such table: events
```

Worse than loud loss: a 0-byte file passes `PRAGMA integrity_check` as `"ok"`, so
the next launch opens a silently empty ledger. And this install's real data lives
almost entirely in the WAL — the user's live ledger was a 4 KB `.db` with a 1.8 MB
`-wal` — so the sidecar deletion is where the data actually goes.

**The fix.** `swap_in_place` refuses when candidate and live resolve to the same
file. Identity is decided by `canonicalize` (so `dir/sub/../ledger.db` is caught)
and, on Unix, by device/inode (so two hard links to one file are caught — paths
that canonicalize differently yet share storage, where a copy still clobbers the
source). Two tests written first and confirmed failing: `tauri-app` 14 → 16.

**Secondary defect found while fixing it.** `restore_database` sets `db.conn = None`
*before* `swap_in_place`. A rejection from inside the swap would therefore leave the
app in the "Restore finished. Please close and reopen the app." state over a
recoverable misclick. So the user-facing rejection was moved ahead of the drop, and
the guard in `swap_in_place` stays as a last line of defence for any future caller.
General rule this encodes: **everything that can reject must reject before the live
connection is dropped.**

**Third instance of the same class** (D7, D9, now this): a comment asserting an
invariant the code does not enforce. `restore_database`'s doc says "a restore must
always be undoable" and the safety copy exists to make that true — but the
self-restore path destroyed the source before that promise could apply. The pattern
is now reliable enough to be worth grepping for deliberately rather than hoping a
reviewer notices.

**Deviation from the plan, accepted:** the plan has no such guard, in any task. The
plan's `swap_in_place` is exactly what shipped up to `1457b8f`. Shipping known
silent total data loss to satisfy plan fidelity is not a trade worth making.

## D14. Restore pruned its own source before the swap (fixed, `17ae47b` + test `27fb02d`)

Found by me during Task 13's close-out, again not in the plan and not by any
reviewer. Second data-loss defect on the restore path, on the *undo* mechanism the
safety copy exists to provide.

**The bug.** `restore_database` ran, in order: snapshot the live ledger into
`rescue/pre-restore-<now>.db`, **prune** the rescue directory to `KEEP_AUTO=3`, drop
the connection, then `swap_in_place(candidate, live)`. The restore file dialog can
offer the rescue copies themselves as a source — that is the whole point of keeping
them — so `candidate` may be the oldest `pre-restore-*.db` in that directory. Prune
(keep=3) deletes the oldest matching `.db`, which is exactly that candidate, and the
swap then `fs::copy`s from a path that no longer exists. Reproduced by simulation:
four `pre-restore-*.db`, prune-first leaves the chosen oldest one gone; `fs::copy`
from a missing source errors, so restoring to the oldest undo point fails outright.

**The fix.** Move the prune to *after* the swap. Once `swap_in_place` has copied the
candidate's bytes into `live`, deleting its rescue copy is harmless. `import_event_log`
is unaffected: it reads `.jsonl`, and `prune` matches only `.db`.

**Test.** Pinned at the `backup.rs` primitive level (no Tauri harness, per the plan's
closing note): three rescue copies with markers, restore from the oldest, write the
fourth pre-restore copy, then swap-then-prune, and assert `live` carries the oldest
copy's marker. `tauri-app` 16 → 17.

**Pattern worth naming.** Both restore defects (D13, D14) share a root: **the restore
source and the files restore manages can be the same file.** D13 was source == live;
D14 was source == a rescue copy in the directory being pruned. Any operation that
both consumes a user-chosen path and mutates the filesystem around it must ask "what
if the path *is* one of the things I'm about to touch?" That question, asked once for
the whole restore path, would have caught both. Grepping for asserted-but-unenforced
invariants (D7/D9/D13) is one habit; this is its sibling — check aliasing between
inputs and side-effect targets.

## D15. Whole-branch review: two more Criticals, and a process failure of mine

A whole-branch reviewer returned seven findings. I verified each with my own
executable probe before acting — worth doing, because the probes corrected the
report's details more than once (the foreign-ledger import trips on `users.id`,
not `accounts.system_role` as reported).

**Critical 1 — importing another business's log bricked the app permanently
(fixed, `7a70780`).** `import_jsonl` committed the merged events and only then ran
reconciliation. A log from an independently founded ledger has its own
`UserRegistered` genesis, so the merged log opens two of every system account and
cannot be replayed at all. Probe: `events before=15 after=30`, then
`rebuild err: UNIQUE constraint failed: users.id`. Because `init_state` calls
`rebuild(...).expect(...)`, every subsequent launch would panic — the user's app
would simply never open again, with no in-app way back. Fixed with a
`ForeignLedger` guard *before* the transaction: compare the archive's genesis
event id against the local log's earliest `UserRegistered`. Refusing pre-commit is
the whole point; a post-commit rejection cannot undo an unreplayable log. Verified
the guard both catches the bug (temporarily neutered it to `if false && …`,
confirmed the test FAILED, restored it) and does not over-reject: an empty local
log still imports, and same-genesis branches still merge.

**Critical 2 — restore cloned the making install's identity (fixed, `76f6d48`).**
`VACUUM INTO` copies the whole file, `app_settings` included, and `app_settings` is
deliberately outside `PROJECTION_TABLES` so it survives `rebuild`. Restoring
install A's backup onto install B therefore made B author as A. Probe:
`AAAA-install-A` → `BBBB-install-B`. Two installs sharing a `device_id` mint
byte-identical event ids for *different* events and collide on `(device_id, seq)`
— precisely the unmergeable state per-install identity exists to prevent, so this
silently destroyed the merge capability the user asked to keep in scope. Fixed
with `remint_device_id`, called when the restored id differs from the running one.
Kept as a separate function rather than a `SETTING_KEYS` entry so the write-once
IPC hardening from D7 survives; a test pins that `set_setting(conn, "device_id",
…)` still errors.

**Important — `import_event_log` had the D14 defect too (fixed, `cda0764`).** The
same prune-before-read ordering, at the mirror call site. I have "when you fix an
ordering bug, check the mirror call site" in memory as a habit and still missed it;
the reviewer caught it. Reading D14's own closing paragraph would have found it.

**Fixed without dispute:** close-time backup moved from `WindowEvent::Destroyed`
to `CloseRequested` (`8b87a08`) — `Destroyed` fires during teardown, so a
`VACUUM INTO` of a large ledger could be cut short by process exit. Export header
no longer carries `backup_folder` (`cb02277`) — an export is the file a user emails
to whoever is helping them, and the header disclosed their home directory layout
for no archival benefit, since import never applies the header's settings.
`KEEP_AUTO`'s doc rationale corrected (`876f6d9`) — "each snapshot is a strict
superset of the previous one" is true for auto-backups and false for the
`pre-restore-*` copies sharing the constant, since a restore replaces history
rather than appending to it.

**Finding 7 accepted — the swap is now atomic (`cce15d0`).** The reviewer noted
`swap_in_place` used `fs::copy` onto the live path and judged it minor because the
rescue copy makes it recoverable. Fixed anyway: a copy interrupted partway leaves
the ledger a hybrid of two databases, and startup's `rebuild(...).expect(...)`
turns a corrupt ledger into an app that will not open — the same unlaunchable
outcome as Critical 1, reached by a different route, and "recoverable" there means
the user finding a rescue file with the app dead. Now stages beside the live file
and `fs::rename`s over it. Sidecar deletion moved *before* the rename: between
renaming in a new database and deleting the old `-wal`, a crash would leave SQLite
ready to replay the old write-ahead log onto the new file.

**Coverage gap closed — the reviewer's sharpest point (`f598ffe`).** "No test
drives `import_event_log`/`restore_database` end to end, which is where findings
1-3 live." Exactly right, and it explains the *shape* of this branch's bug list:
three data-loss defects all lived in one unreachable sequence, because a
`#[tauri::command]` cannot be called from a test. Extracted the sequence into
`backup::perform_restore` and rewired the command to call it, so the tests cover
the real path rather than a parallel copy. It takes `&mut Option<Connection>`
rather than `&Connection` because *when* the ledger closes is part of what must be
right: the rescue copy needs it open, the swap needs it closed, and every
rejection must happen while it is still open. Each of the three new end-to-end
tests was confirmed to fail with its defect reintroduced.

**Finding 5 accepted, no code change.** `last_backup_at` lags one session, because
the close-time write lands after the UI is gone. The reviewer itself called this
acceptable; the comment at the call site says so.

**My process failure, recorded because it is the most transferable item here.** I
declared the reviewer hung after ~35 minutes and stopped it. It had in fact
completed and returned all seven findings. Two Criticals — either of which ends
with the user's app refusing to launch — were in that report I nearly discarded.
The judgment error was treating "longer than I expected" as evidence of failure
when I had no signal either way, on a task whose whole value was catching what I
had missed. A review of a large branch legitimately takes a long time. Absent an
actual error, wait.

**What the seven findings have in common.** Six of the seven are one question asked
at six places: *what is the state of the ledger if this stops in the middle, or if
the file I am reading is the file I am writing?* D14 named the aliasing half of
that. This adds the interruption half — and the reason both matter more here than
in most code is that the failure mode is not a bad result but an app that will not
start, on a single-install local-first ledger with no server-side copy. Where an
invariant's violation is unrecoverable by the user, the guard belongs before the
commit, not after.

<!-- Entries appended below as decisions are made. -->

