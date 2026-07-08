# Code Review Log — Accounting Core

Maintained by the reviewer agent. Each entry records issues found, severity, and resolution status.

---

## Format

| # | Task | File | Issue | Severity | Principle | Status |
|---|------|------|-------|----------|-----------|--------|

---

## Entries

| 1 | Task 1 | all | No issues — minimal scaffold, clean SRP, testable | — | — | PASS |
| 2 | Task 2 | schema.sql, db.rs | No issues — schema matches §4.1, idempotent, portable SQL | — | — | PASS |
| 3 | Task 3-4 | hlc.rs | Counter overflow past 999999 unguarded (unreachable in practice) | Minor | Design | NOTED |
| 4 | Task 5 | events.rs | No issues — correct envelope, JSONB round-trip, &Connection future-proof | — | — | PASS |
| 5 | Task 6-7 | events.rs, hlc.rs | No issues — replay order correct, gap detection sound, rehydration handles empty log | — | — | PASS |
| 6 | Task 8 | genesis.rs | Partial-genesis risk if append fails mid-batch (deferred to txn wrapper in Plan 2-3) | Minor | Design | NOTED |
| 7 | P2 Task 1 | schema.sql | No issues — all 17 projection tables present, correct generated cols, indexes, reversed columns | — | — | PASS |
| 8 | P2 Tasks 2-4 | projectors.rs | No issues — dispatcher extensible, balance formula correct, patch_doc safe, helpers well-scoped | — | — | PASS |
| 9 | P2 Tasks 5-6 | projectors.rs | No issues — lots created correctly, nested lotConsumption handled, recon #1 verified, double-entry holds | — | — | PASS |
| 10 | P2 Tasks 7-9 | projectors.rs | No issues — full-amount posting correct, prepayment/allocation logic sound, credit-expense party contract | — | — | PASS |
| 11 | P2 Tasks 10-11 | projectors.rs | No issues — qtyDelta negation correct, found lots NULL-safe, opening balances faithful, recon #1 holds | — | — | PASS |
| 12 | P2 Task 12 | projectors.rs | No issues — nested lotReturns correct, outstanding capped, cash/credit routing sound | — | — | PASS |
| 13 | P2 Task 13 | projectors.rs | No issues — four-part contract complete, clause 3 no double-count, all target types handled | — | — | PASS |
| 14 | P2 Task 14 | projectors.rs | No issues — FK-safe delete order, events read before txn, full determinism proven | — | — | PASS |
| 15 | P3 Task 1 | commands/mod.rs | `reject` unused until Task 3 (expected) | Minor | — | PASS |
| 16 | P3 Task 2 | categories.rs | No issues — categories match spec §4.5, clean dispatch table | — | — | PASS |
| 17 | P3 Tasks 3-4 | guards.rs, setup.rs | No issues — value guards reusable, setup handlers enforce uniqueness/immutability | — | — | PASS |
| 18 | P3 Task 5 | guards.rs | No issues — LotDemand aggregation correct, oversell/lot-item-match sound | — | — | PASS |
| 19 | P3 Task 6 | purchase.rs | No issues — deterministic lotId, party kind check, guards-before-write | — | — | PASS |
| 20 | P3 Task 7 | sale.rs | No issues — FIFO selection, LotDemand aggregation, COGS freeze, override validation | — | — | PASS |
| 21 | P3 Task 8 | purchase.rs, guards.rs | No issues — flat shape, LotDemand.take for consuming, return-against-reversed guard | — | — | PASS |
| 22 | P3 Task 9 | sale.rs, guards.rs | No issues — nested lotReturns, dual-clause over-restore, LotDemand.restore, no frozen totals | — | — | PASS |
| 23 | P3 Tasks 10-11 | guards.rs | No issues — aggregated over-allocation, credit-overdraw per direction, party-ownership sound | — | — | PASS |
| 24 | P3 Task 12 | payment.rs | No issues — all 3 handlers correct, guards wired, credit-overdraw bounds PaymentAllocated | — | — | PASS |
| 25 | P3 Task 13 | movement.rs, guards.rs | No issues — credit-expense party guard enforced, self-transfer, write-down-only, found lotId deterministic | — | — | PASS |
| 26 | P3 Task 14 | guards.rs | No issues — all 5 reversal edges correct, no cascade, edge 3/5 sound | — | — | PASS |
| 27 | P3 Task 15 | reversal.rs | No issues — guard orchestration by category, frozen accountId lines, clause 1 only | — | — | PASS |
| 28 | P4 Task 1 | test_support.rs, reconciliation.rs | No issues — fixture drives real handlers, check #1 correct | — | — | PASS |
| 29 | P4 Tasks 2-8 | reconciliation.rs | No issues — all 7 checks correct, net form, terms-aware, null-safe | — | — | PASS |
| 30 | P4 Tasks 9-11 | reconciliation.rs, queries.rs | No issues — aggregator, units_sold, gross/net profit, IS NOT null-safe | — | — | PASS |
| 31 | Tauri IPC | tauri-app/ | No issues — single Mutex, correct invoke patterns, genesis-on-first-open safe | — | — | PASS |
| 32 | Full UI | ui/src/ | No issues — money conversion correct, forms functional, error handling present | — | — | PASS |
