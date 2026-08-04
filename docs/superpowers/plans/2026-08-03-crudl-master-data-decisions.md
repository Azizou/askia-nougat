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

`ensure_unreferenced` refuses an interactive delete of anything referenced. The projector never
fails: `set_active`/`patch_doc` treat a missing row as a no-op rather than an error, and
`delete_master` does not delete at all (see D12).

**Why (F3):** `PRAGMA foreign_keys = ON` and `init_state` calls
`rebuild(&mut conn).expect("rebuild projections")`. An event that cannot be projected is therefore
not an error message — it panics at startup and the app cannot be opened at all, permanently,
because the event is immutable in the log. `import_event_log` merges two devices' logs by HLC
order, so device A's delete of an item it never used can legitimately sort *before* device B's
sale of that item. Command-time validation and replay see different worlds; only the guard has a
user to reject to.

**Corrected — the original version of this decision was wrong, and D12 records the fix.** It said
`delete_master` "re-counts references at replay time and archives instead of deleting when it finds
any", and argued that "both halves consult the same `refs.rs` helpers so they cannot drift". Sharing
the helpers does settle *what counts as a reference*, but the drift is on the *when* axis, and no
amount of shared code addresses that: a reference count taken mid-replay is not a fact about the
ledger, it is an artifact of position in the stream. In the very ordering this decision names, the
count is legitimately zero, the DELETE succeeds, and the foreign key fails later inside the
*referencing* INSERT — which `delete_master` does not control.

The sabotage proof went red for the wrong reason. The test it named,
`a_delete_ordered_before_the_sale_that_uses_the_item_archives_instead_of_failing`, in fact
appended the sale *first* and the delete second — the opposite ordering from its name. It was
therefore exercising the case the projector genuinely did handle, and certified a claim it never
tested. It is now named
`a_delete_arriving_after_the_purchase_that_uses_the_item_keeps_the_row`, and two new tests cover the
ordering the old name claimed.

The lesson worth keeping: a sabotage proof only licenses the claim if the test that goes red is the
test that exercises the claim. Read the test body, not the test name.

## D12 — `delete_master` marks; a separate compaction pass removes

`delete_master` sets `active = false` plus a `deleted` flag inside the row's JSON doc and never
removes anything, so it is total in either ordering. `compact_deleted_master` then removes every
marked row that nothing references. A row that turns out to be referenced stays as an archived
tombstone: the delete loses to the transaction that needs it, which keeps history intact and keeps
the row out of every new-transaction dropdown.

**Why:** it moves the reference count to the only place it means what it says — after the whole log
has been applied. Deferring costs one flag and buys projector totality, which is the property that
prevents an unlaunchable app.

Compaction runs in exactly two places, and the split is load-bearing:

- `rebuild`, once after the event loop, inside the same transaction as the replay, so the projection
  is never observable in the intermediate state.
- `commit_event`, where the projection is already current and the command guard has just proved the
  row unreferenced, so an interactive delete still takes effect immediately.

It is deliberately **not** on the per-event path. Putting it there re-introduces the bug, which is
how the seam was found: adding it to the replay-simulating test helper made the two new tests fail.
The helper compacts at neither point, matching raw replay.

Established by execution rather than by review. Two independent reviewers flagged the defect; both
new tests — `a_delete_applied_before_the_transaction_that_needs_the_item_stays_replayable` and its
party mirror — were written first and watched fail with SQLite extended code **787**
(`SQLITE_CONSTRAINT_FOREIGNKEY`) before any code changed.

One SQL detail cost a debugging cycle and is worth recording: `json_extract` unwraps a JSON boolean
to integer `1`/`0`, not to `json('true')`. The first compaction query compared against `json('true')`
and silently matched nothing.

Verified against a consistent WAL-set copy of the real 242-event ledger (source untouched): full
replay clean, `run_all_checks` passes, deleting a referenced item is refused by the guard with "used
by 41 existing record(s)", and the merge ordering that previously bricked startup replays with the
contested row kept at `active = 0`.

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

## D11 — The seeded parties also take no payment

`check_seeded_party_takes_no_payment` refuses `handle_payment_received` and
`handle_payment_made` for both seeded ids, and the payments dropdown filters them out.

**Why:** the argument is *not* "cash trade needs no payment", which is a claim about business
practice and would be arguable. It is structural: D10 closed credit trade, which means a seeded party
can never hold an invoice.
A payment therefore has nothing to settle, so it could only land as an *unallocated
prepayment* — `unallocated_cr_minor` credited to a party that identifies nobody, and which
no future invoice can ever draw down, because no future invoice can exist. That is the same
defect as D10 one route further along: a permanent balance owed to no one.

The strongest counter-case is a customer deposit — money taken up front against goods collected
later. It does not survive: the business has to know whose deposit it is to hand the goods over, so
recording it against "Walk-in Customer" makes it unrecoverable in precisely the way D10 describes.
The correct workflow is to create the party. A cash refund is the other candidate, and it is a
return rather than a payment — that case lands in D13, not here.

Found by asking what else can move a party balance after fixing the terms paths. The answer
was payments, and unlike the sales and purchases forms the payments form has no
auto-selection — but it did list both seeded parties in its dropdown, so the path was
reachable by an ordinary click rather than only by import.

`handle_payment_allocated` is deliberately left **unguarded**, and my first reasoning for
that was wrong in a way worth recording. I argued it was unreachable because it needs an
existing payment owned by the party. It is not unreachable: `import_jsonl` calls
`insert_raw_event` then `rebuild`, so it never runs command guards at all — that is the D4
design, not a hole. A log written by a device on an older build can therefore carry both a
credit sale to the walk-in customer and a prepayment from it.

In exactly that scenario, allocating the imported prepayment against the imported invoice is
the *remediation*: it draws the phantom credit down and reduces the bad balance to zero. So the
guard belongs on the two commands that create the bad state and not on the one that resolves it.
`check_credit_overdraw` still bounds it to the credit actually held.

**Corrected — the stranding argument I first gave for this was wrong, and the difference is
load-bearing.** I wrote that guarding `handle_payment_allocated` "would strand a user with legacy
data, holding a balance they cannot clear through the UI." There is no UI route to
`handle_payment_allocated`: it is absent from `generate_handler!`, and its only non-test reference
outside the core is the `pub use` re-export. `record_payment` does accept an `allocations` array,
but those ride inline in the `PaymentReceived`/`PaymentMade` payload — a different event type — so
that path never reaches it either. A user importing such a log is therefore *already* stranded, by
the missing command rather than by any guard, and `Payments.tsx` cannot even offer the seeded party
because D11's own dropdown filter removes it.

The honest version: the guard would be harmless today, because the function is unreachable either
way. It stays off so the remediation is available *if and when* an allocation command is exposed.
Same conclusion, different and much narrower reason.

This also downgrades what the sabotage proved. Adding the guard does fail
`an_imported_legacy_seeded_party_balance_replays_and_stays_clearable`, but that shows the guard
would block the *core function* — not that it would block a user remediation, which is what the
doc claimed. Structurally the same error as the D4 sabotage above: the test went red for a narrower
reason than the prose asserted. Twice now, so the pattern is the lesson, not the instance — a
sabotage licenses exactly the claim the assertion makes and no more, and "the test failed" is not
the same as "the failure means what I said it means."

**Which side of the line this branch is on:** making the remediation real needs a Tauri command for
`handle_payment_allocated`, a UI entry point, and an exception in the payments dropdown filter for a
seeded party holding a nonzero balance. That is new scope and is *not* taken here. Recorded as a
deferral, alongside the returns commands in D13 — the two unreachable-by-omission facts belong in
the same place, because both are load-bearing and neither is obvious from the code.

Checked against the user's real 242-event ledger before adding the guard: it contains no
credit sale to the walk-in customer, no credit purchase from the anonymous supplier, no
payment referencing either, and no `party_balances` row for either. So neither D10 nor D11
can reject anything already recorded. Both are command guards in any case, so replay of an
existing log is unaffected — per D4, only guards reject.

## D13 — A return settles by the original's terms, never by whether a party is named

`sale_return` and `purchase_return` chose the refund account and the balance update by asking whether
the original transaction had a customer or supplier. Both now ask the original's `terms` instead:

```sql
SELECT CASE WHEN terms = 'credit' THEN customer_id END FROM sales WHERE id = ?1
```

**Why:** a cash sale also stores a `customer_id`. It is the walk-in party for an anonymous counter
sale, and a named party when the customer is known but paid on the spot. So the presence of a
customer says nothing about how a refund must be settled — only the terms do. Gating on the party
refunded every cash return to Accounts Receivable instead of the till, and additionally credited the
customer an unallocated credit they were never owed: a cash sale has `outstanding_minor = 0`, so
`reduce` computed to 0 and the whole refund fell through the excess branch. A probe on a cash sale
return printed `unallocated_cr=3000 AR_credit=3000 bank_credit=0`, which is both halves of the
defect in one line. The `purchase_return` mirror was identical and was found by checking it in the
same pass rather than after a second report.

`terms` is `NOT NULL` in both tables and predates this work, so no migration was needed.

**Reachable today only through import.** No return command is registered in `generate_handler!`, so
the UI cannot reach either projector; `import_event_log` can, because per D4 it replays raw events
without guards. That omission is load-bearing for the defect's blast radius, which is exactly why it
is recorded here rather than relied on silently — registering a return command later re-opens the
path, and the fix has to already be in place when that happens. This amends D9, which lists returns
as deferred but does not say that the deferral is what kept a real projector bug out of reach.

`handle_payment_allocated` is unreachable for the identical reason; see the correction in D11. Two
distinct real defects have now been found sitting behind a missing `generate_handler!` entry, so the
omission is not a safety property to lean on — it is an accident that happens to be holding, and the
next command registered is what collects the bill.

**This also closes the seeded-party route the returns projectors opened, without needing a guard.**
A cash sale to the walk-in party, returned, used to land `unallocated_cr_minor = 3000` on it — the
exact state D10 and D11 exist to prevent, reached with no credit transaction anywhere. It was
`customer.is_some()` gating that did it, and gating on `terms` removes it: a cash return now refunds
the till and touches no party balance at all. `purchase_return` likewise. So the fifth and sixth call
sites of the D10/D11 shape are closed structurally rather than by a sixth guard, which is why no
`check_seeded_party_*` call appears in either projector — and per D4 a projector is the wrong place
for one regardless.

## D14 — The walk-in seed states `active` explicitly

`ensure_walkin_party` omitted `active` from its payload, so the projection wrote `active = NULL`,
while `ensure_anon_supplier` stated `"active": true`. Now both state it.

**Why:** symmetry and future-proofing, not a live bug — and the difference matters, so it is stated
plainly. Every SQL read path is null-safe per D3 (`list_parties` selects `COALESCE(active, 1)`),
which I checked exhaustively rather than assumed. What the asymmetry threatened was a query that
ever returned the raw column: `p.active` in the sales form is a truthiness test, so a `NULL` would
silently drop the walk-in party from the very dropdown the cash default depends on.

**Limitation, stated because it is easy to misread:** the seed is guarded on the log, so this reaches
fresh installs only. An install that has already emitted the event keeps the payload it has, forever.
D3's null-safety is what protects those, not this line.

## D15 — An archived party stays payable while it still owes or is owed

`queries::payable_parties(conn, direction)` returns the parties the payments form may offer: those of
the right kind that are active, **plus** archived ones that still have an unreversed invoice with
`outstanding_minor > 0` or a non-zero unallocated balance. The form applies only the seeded-party rule
on top. Archived rows sort last and are labelled — "(archived — still outstanding)" /
"(archivé — solde en cours)".

**Why:** archiving a party with an open invoice is deliberately allowed, and `list_open_invoices` has
no `active` predicate, so the debt stayed visible in aging. Only the form was refusing. The result was
a balance the user could see and had no screen to settle. The unallocated clause is not redundant with
the invoice clause: a prepayment has no invoice at all, and drawing it down is precisely what
`PaymentAllocated` needs the party present for.

The rule lives in the core rather than in the Tauri command because `#[tauri::command]` functions
cannot be unit-tested; five tests cover it, including the inverse (an archived party with nothing
outstanding drops out) and an unknown direction.

A first sabotage of this SQL was worthless and the reason is worth keeping: I replaced the first `OR`
with `AND` and all four archived tests still passed, because `AND` binds tighter than `OR` and the
balance clause still matched on its own. Replacing the whole disjunction with `AND COALESCE(active,1)=1`
turned three of them red. A sabotage has to actually remove the behaviour, not just perturb the text.

## D16 — The cash default keys on the terms transition, not on the current selection

The auto-select effects in the sales and purchases forms now depend on `[terms]` alone and read the
current value through an updater callback. Post-submit resets set the seeded party explicitly.

**Why:** with the party id in the deps the effect re-fired on every dropdown change, so a user on cash
who cleared the selection back to the placeholder had the default re-imposed instantly and could never
reach the empty option. The default belongs to the *transition* between terms, not to the current
selection. The explicit reset after submit is required by the same change: the effect no longer
re-fires when terms were already `"cash"`, so it cannot restore the default on its own.

## D17 — The reference-list drift check derives from the schema, not from a second list

`every_referencing_column_is_listed` now walks `sqlite_master`, collects every column named `item_id`,
`party_id`, `customer_id` or `supplier_id`, and fails naming any that `ITEM_REFS`/`PARTY_REFS` omit.
It keeps the converse (every listed column exists) and asserts the two counts match.

**Why:** the previous version enumerated the same nine columns the constants already hold and asserted
each was present — "my list is a subset of my list". A new table with an `item_id` passed it untouched,
which is exactly the drift it existed to catch, and the consequence is a delete guard counting zero
references for a row that has some.

The naming convention is the ground truth here, not `PRAGMA foreign_key_list`: only five of the nine
columns declare a real FK — `sales.customer_id` and `payments.party_id` among those that do not — so
an FK-derived check would miss the majority.

Necessity was proven rather than argued. Appending a `sabotage_stocktake(item_id REFERENCES items(id))`
table makes the new test fail with `["sabotage_stocktake.item_id"]`; the same schema change run against
the stashed old test passed.

## Outstanding

The allocation flow is verified at the Rust level against a copy of a real 242-event ledger, in both
directions: a credit sale settles to zero outstanding with the party receivable returning to zero,
and the mirror credit purchase does the same via `purchaseId`. Over-allocating a settled invoice is
refused. What has **not** run is the React allocation table itself in the Tauri GUI — the UI calls
`invoke` directly with no browser fallback, so no headless path exercises it. The `invoke` payloads
were instead checked field by field against the Rust `Deserialize` structs.

Gates at the close of this work: 228 Rust tests (205 in `accounting-core`, 23 in `tauri-app`),
`cargo clippy --all-targets -- -D warnings`, `tsc --noEmit`, and `vite build` all clean.

Four checks need the running Tauri app and have not been performed: that archiving an item
removes it from the new-sale dropdown while leaving it on past sales, that deleting an unused
item succeeds where a sold one is refused, that a cash purchase left untouched books against
the Cash Supplier, and the React allocation table above. Each is verified at the Rust and query
level; what is unverified is only the wiring in between.

Remaining low-severity items, left open deliberately: the Items and Parties panel headers count
unfiltered rows, so the number includes archived entries the list may be hiding; each page shares
one `error`/`submitting` pair between its create and edit forms, so an error from one shows above
the other; `noOpenInvoices` renders during the in-flight fetch before any row arrives; two
`displayPartyName` calls in `Sales.tsx` omit the anonymous-supplier argument, which is harmless
because a sale can never reference it; and `common.status` is an unreferenced i18n key.
