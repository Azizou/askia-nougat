# Decisions — Master-Data CRUDL

Logged for later review. Branch `feat/crudl-master-data`. The findings referenced as F1–F6
are in `2026-08-03-crudl-master-data.md`; each was established by running a probe against the
real schema, not by reading code and inferring.

## D1 — Delete is two-tiered: archive, plus hard delete only when unreferenced

*Archive* flips `active` to false through the existing `ItemUpdated`/`PartyUpdated` events, so it
needed no new event type and is reversible. *Hard delete* appends new `ItemDeleted`/`PartyDeleted`
events and is permitted only when nothing in the read model points at the row.

**Why:** the two operations answer different needs. A discontinued product must stop appearing in
new sales without erasing the sales that already mention it — that is archive. A record created by
typo should disappear completely — that is delete. Offering only one of them forces the user to
either accumulate junk or lose history.

## D2 — `parties.active` required an explicit `ALTER TABLE`, not a `schema.sql` edit

**Why (F1):** `apply_schema` is `execute_batch(SCHEMA_SQL)` where every statement is
`CREATE ... IF NOT EXISTS`, which is a no-op against a table that already exists, and `rebuild`
clears projections with `DELETE FROM`, never `DROP`. So an existing install keeps its original
table *definitions* forever. Adding `active` to the `CREATE TABLE` block would have reached fresh
installs only, and every upgraded install would have failed each parties query with
`no such column: active`. `migrate_schema` now runs after the DDL and guards on
`pragma_table_xinfo` (`table_xinfo`, not `table_info`, because generated columns are invisible to
the latter).

Verified outside the test suite by building a genuine legacy-shape `parties` table in a file
database, confirming `SELECT active` failed, then running the real `apply_schema` against it: the
column appeared, the legacy row read `active = NULL`, and a second apply was a no-op.

## D3 — Every `active` filter is null-safe

All filters use `COALESCE(active, 1) = 1`.

**Why (F2):** rows written before the field existed project `active` as NULL. On the legacy
database above, `WHERE active = 1` returned **0** rows where `COALESCE(active,1) = 1` returned 1.
A bare `active = 1` would therefore have made every pre-existing party silently vanish from the
app — the worst class of bug here, because it looks like data loss to the user and leaves no error.

## D4 — Guards reject strictly; projectors degrade and never fail

`ensure_unreferenced` refuses an interactive delete of anything referenced. The projector's
`delete_master` re-counts references at replay time and **archives instead of deleting** when it
finds any, and `set_active`/`patch_doc` treat a missing row as a no-op rather than an error.

**Why (F3):** `PRAGMA foreign_keys = ON` and `init_state` calls
`rebuild(&mut conn).expect("rebuild projections")`. An event that cannot be projected is therefore
not an error message — it panics at startup and the app cannot be opened at all, permanently,
because the event is immutable in the log. `import_event_log` merges two devices' logs by HLC
order, so device A's delete of an item it never used can legitimately sort *before* device B's
sale of that item. Command-time validation and replay see different worlds; only the guard has a
user to reject to.

Both halves consult the same `refs.rs` helpers so they cannot drift — a projector laxer than its
guard is precisely the bricking case.

Proven by sabotage: replacing `delete_master`'s body with an unconditional `DELETE` makes
`a_delete_ordered_before_the_sale_that_uses_the_item_archives_instead_of_failing` fail with
`FOREIGN KEY constraint failed` (SQLite extended code 787). The test earns its place.

## D5 — Deletes are not reversible

`ItemDeleted`/`PartyDeleted` map to `vec![]` in `categories_of`, so `is_transactional` is false and
`check_reversal_legal_target` refuses them as void targets.

**Why (F5):** undoing a hard delete means recreating the record, which is a create, not a reversal.
Archive already covers the reversible case, so a reversible delete would be a second, redundant
mechanism with worse semantics.

## D6 — The seeded parties can be renamed but neither archived nor deleted

Enforced in `handle_party_updated` / `handle_party_deleted`; the UI additionally hides the buttons
for them rather than rendering controls that always error.

**Why (F6):** `party_walkin` is auto-selected for cash sales and `party_anon_supplier` for cash
purchases. Archiving either removes it from the dropdown that the default path depends on, breaking
the most common flow for the users least equipped to diagnose it. Renaming is harmless and is
allowed — a French-speaking user may prefer their own wording.

## D7 — Payment allocation needed no backend change

**Why (F4):** `handle_payment_received` and `handle_payment_made` already accepted and validated a
`Vec<AllocInput>`, and both Tauri commands already deserialized `allocations`. The projector reads
`saleId` for direction `in` and `purchaseId` for `out`, matching exactly what `alloc_json_by_key`
emits. The only defect was `Payments.tsx` hardcoding `allocations: []`, which made every payment an
unallocated prepayment and left `outstanding_minor` untouched — invoices could never be settled.

Recorded deliberately: I first suspected a producer/consumer field-name mismatch on this path and
said so. Reading `fn payment` in `projectors.rs` disproved it. The suspicion was wrong and the
contract was already consistent; only `list_open_invoices` was added, to supply the UI with legal
allocation targets.

## D8 — `handle_item_updated` gained a SKU-collision guard

**Why:** `items_sku` is a UNIQUE index, so renaming an item onto another's SKU failed inside the
projector. `commit_event` wraps the append and the projection in one transaction, so the failure
rolled back safely — no corruption — but the user saw a raw SQLite constraint error. The guard
turns it into "another item already uses SKU 'S1'". This was not in the original scope; it was
added because exposing Update for the first time made the path reachable.

## D9 — Documented but deliberately not built

Accounts, users, expenses, transfers, returns, and inventory adjustments still have no UI Update or
Delete. Notably `handle_account_updated` and `handle_user_updated` exist in the core and remain
unreachable from the frontend — the same gap this work closed for items and parties. Left out per
the agreed scope, recorded here so it is a known deferral rather than a rediscovery.

## D10 — The seeded parties are cash-only, enforced in the core

`check_seeded_party_cash_only` refuses `terms = "credit"` for `party_walkin` and
`party_anon_supplier` in `handle_sale_recorded`, `handle_purchase_recorded`, and the credit branch
of `handle_expense_recorded`. The UI additionally clears its auto-selection when the user switches
to credit, so the error is normally unreachable.

**Why:** found while verifying this branch, not predicted by the plan. The auto-select effect
predates the work, but changing the default terms to cash is what made the bad path easy: the form
now opens with the seeded party already selected, and switching to credit left it selected. A probe
confirmed the core accepted it — a credit sale to the walk-in customer booked
`receivable_minor = 3000` against a party that by construction identifies nobody, so the receivable
is uncollectable and permanently inflates receivables. The mirror case booked an unpayable payable
to "Cash Supplier".

The guard lives in the core rather than only in the form because `import_event_log` can carry a
command from another device, and because three separate call sites shared the same shape — the
expense case was found by checking the mirror of the two obvious ones.

## Outstanding

The allocation flow is verified at the Rust level against a copy of a real 242-event ledger, in both
directions: a credit sale settles to zero outstanding with the party receivable returning to zero,
and the mirror credit purchase does the same via `purchaseId`. Over-allocating a settled invoice is
refused. What has **not** run is the React allocation table itself in the Tauri GUI — the UI calls
`invoke` directly with no browser fallback, so no headless path exercises it. The `invoke` payloads
were instead checked field by field against the Rust `Deserialize` structs.

Gates at the close of this work: 216 Rust tests, `cargo clippy --all-targets -- -D warnings`,
`tsc --noEmit`, and `vite build` all clean.
