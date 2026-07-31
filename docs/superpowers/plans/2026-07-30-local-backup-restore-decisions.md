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

<!-- Entries appended below as decisions are made. -->
