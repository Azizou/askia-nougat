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
