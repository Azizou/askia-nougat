use crate::commands::categories::is_transactional;
use crate::commands::{reject, CommandError};
use rusqlite::Connection;
use rusqlite::OptionalExtension;
use std::collections::HashMap;
#[cfg(test)]
use serde_json::Value;

pub(crate) fn check_qty_positive(qty: i64) -> Result<(), CommandError> {
    if qty > 0 { Ok(()) } else { Err(reject(format!("qty must be > 0, got {qty}"))) }
}

pub(crate) fn check_amount_non_negative(amount: i64) -> Result<(), CommandError> {
    if amount >= 0 { Ok(()) } else { Err(reject(format!("amount must be >= 0, got {amount}"))) }
}

pub(crate) fn check_amount_positive(amount: i64) -> Result<(), CommandError> {
    if amount > 0 { Ok(()) } else { Err(reject(format!("amount must be > 0, got {amount}"))) }
}

pub(crate) fn check_at_least_one_line<T>(lines: &[T]) -> Result<(), CommandError> {
    if lines.is_empty() { Err(reject("event must have >= 1 line")) } else { Ok(()) }
}

pub(crate) struct LotDemand {
    claimed: HashMap<String, i64>,
}

impl LotDemand {
    pub(crate) fn new() -> Self { Self { claimed: HashMap::new() } }

    fn committed_remaining(conn: &Connection, lot_id: &str) -> Result<i64, CommandError> {
        conn.query_row("SELECT qty_remaining FROM inventory_lots WHERE id = ?1", [lot_id], |r| r.get(0))
            .optional()?
            .ok_or_else(|| reject(format!("unknown lot: {lot_id}")))
    }

    pub(crate) fn available(&self, conn: &Connection, lot_id: &str) -> Result<i64, CommandError> {
        let rem = Self::committed_remaining(conn, lot_id)?;
        Ok(rem - self.claimed.get(lot_id).copied().unwrap_or(0))
    }

    pub(crate) fn take(&mut self, conn: &Connection, lot_id: &str, qty: i64) -> Result<(), CommandError> {
        let rem = Self::committed_remaining(conn, lot_id)?;
        let prior = self.claimed.get(lot_id).copied().unwrap_or(0);
        let cumulative = prior + qty;
        if cumulative > rem {
            return Err(reject(format!(
                "oversell: lot {lot_id} has {rem} remaining, command already claims {prior}, cannot take {qty} more")));
        }
        self.claimed.insert(lot_id.to_string(), cumulative);
        Ok(())
    }

    pub(crate) fn restore(&mut self, conn: &Connection, lot_id: &str, qty: i64) -> Result<(), CommandError> {
        let (remaining, received): (i64, i64) = conn.query_row(
            "SELECT qty_remaining, qty_received FROM inventory_lots WHERE id = ?1",
            [lot_id], |r| Ok((r.get(0)?, r.get(1)?)),
        ).optional()?.ok_or_else(|| reject(format!("unknown lot: {lot_id}")))?;
        let headroom = received - remaining;
        let prior = self.claimed.get(lot_id).copied().unwrap_or(0);
        let cumulative = prior + qty;
        if cumulative > headroom {
            return Err(reject(format!(
                "over-restore: lot {lot_id} headroom {headroom}, command already restores {prior}, cannot restore {qty} more")));
        }
        self.claimed.insert(lot_id.to_string(), cumulative);
        Ok(())
    }
}

pub(crate) fn check_lot_item_match(conn: &Connection, lot_id: &str, expected_item_id: &str)
    -> Result<(), CommandError> {
    let item: Option<String> = conn.query_row(
        "SELECT item_id FROM inventory_lots WHERE id = ?1", [lot_id], |r| r.get(0),
    ).optional()?;
    match item {
        None => Err(reject(format!("unknown lot: {lot_id}"))),
        Some(it) if it != expected_item_id =>
            Err(reject(format!("lot {lot_id} belongs to item {it}, not {expected_item_id}"))),
        Some(_) => Ok(()),
    }
}

pub(crate) fn check_invoice_not_reversed(conn: &Connection, table: &str, invoice_id: &str)
    -> Result<(), CommandError> {
    use rusqlite::OptionalExtension;
    let sql = format!("SELECT reversed FROM {table} WHERE id = ?1");
    let reversed: Option<i64> = conn.query_row(&sql, [invoice_id], |r| r.get(0)).optional()?;
    match reversed {
        None => Err(reject(format!("unknown {table} invoice: {invoice_id}"))),
        Some(1) => Err(reject(format!("cannot return against reversed (voided) {table} invoice {invoice_id}"))),
        Some(_) => Ok(()),
    }
}

pub(crate) fn check_sale_return_over_restore(
    conn: &Connection, original_sale_id: &str, lot_id: &str, return_qty: i64,
) -> Result<(), CommandError> {
    use rusqlite::OptionalExtension;
    let consumed: Option<i64> = conn.query_row(
        "SELECT SUM(lc.qty_taken)
         FROM lot_consumptions lc
         JOIN sale_lines sl ON sl.id = lc.sale_line_id
         WHERE sl.sale_id = ?1 AND lc.lot_id = ?2",
        rusqlite::params![original_sale_id, lot_id],
        |r| r.get(0),
    ).optional()?.flatten();
    match consumed {
        None | Some(0) =>
            return Err(reject(format!("sale {original_sale_id} did not consume lot {lot_id}"))),
        Some(c) if return_qty > c =>
            return Err(reject(format!("over-restore: sale consumed {c} from lot {lot_id}, cannot return {return_qty}"))),
        Some(_) => {}
    }
    let (remaining, received): (i64, i64) = conn.query_row(
        "SELECT qty_remaining, qty_received FROM inventory_lots WHERE id = ?1",
        [lot_id], |r| Ok((r.get(0)?, r.get(1)?)),
    ).optional()?.ok_or_else(|| reject(format!("unknown lot: {lot_id}")))?;
    if remaining + return_qty > received {
        return Err(reject(format!(
            "over-restore: lot {lot_id} has {remaining}/{received}, returning {return_qty} would exceed qty_received")));
    }
    Ok(())
}

fn invoice_row(conn: &Connection, target_id: &str, target_type: &str)
    -> Result<Option<(i64, String)>, CommandError> {
    let (table, party_col) = match target_type {
        "sale" => ("sales", "customer_id"),
        "purchase" => ("purchases", "supplier_id"),
        other => return Err(reject(format!("invalid target_type: {other}"))),
    };
    let sql = format!("SELECT outstanding_minor, {party_col} FROM {table} WHERE id = ?1");
    Ok(conn.query_row(&sql, [target_id], |r| Ok((r.get(0)?, r.get(1)?))).optional()?)
}

pub(crate) fn check_invoice_over_allocation(
    conn: &Connection, target_id: &str, target_type: &str, amount: i64,
) -> Result<(), CommandError> {
    match invoice_row(conn, target_id, target_type)? {
        None => Err(reject(format!("unknown {target_type}: {target_id}"))),
        Some((outstanding, _)) if amount > outstanding =>
            Err(reject(format!("over-allocation: {target_type} {target_id} outstanding {outstanding}, cannot allocate {amount}"))),
        Some(_) => Ok(()),
    }
}

pub(crate) fn check_invoice_over_allocation_aggregated(
    conn: &Connection, allocs: &[(String, String, i64)],
) -> Result<(), CommandError> {
    let mut per_target: HashMap<(String, String), i64> = HashMap::new();
    for (target_id, target_type, amount) in allocs {
        *per_target.entry((target_id.clone(), target_type.clone())).or_insert(0) += *amount;
    }
    for ((target_id, target_type), total) in per_target {
        check_invoice_over_allocation(conn, &target_id, &target_type, total)?;
    }
    Ok(())
}

pub(crate) fn check_payment_over_allocation(payment_amount: i64, alloc_amounts: &[i64])
    -> Result<(), CommandError> {
    let sum: i64 = alloc_amounts.iter().sum();
    if sum > payment_amount {
        Err(reject(format!("payment over-allocation: allocations sum {sum} exceed payment {payment_amount}")))
    } else { Ok(()) }
}

pub(crate) fn check_allocation_party_ownership(
    conn: &Connection, party_id: &str, direction: &str, target_id: &str, target_type: &str,
) -> Result<(), CommandError> {
    let expected_type = match direction { "in" => "sale", "out" => "purchase", d =>
        return Err(reject(format!("invalid direction: {d}"))) };
    if target_type != expected_type {
        return Err(reject(format!("direction '{direction}' cannot target a {target_type}")));
    }
    match invoice_row(conn, target_id, target_type)? {
        None => Err(reject(format!("unknown {target_type}: {target_id}"))),
        Some((_, owner)) if owner != party_id =>
            Err(reject(format!("{target_type} {target_id} belongs to {owner}, not paying party {party_id}"))),
        Some(_) => Ok(()),
    }
}

pub(crate) fn check_credit_overdraw(
    conn: &Connection, party_id: &str, direction: &str, total_alloc: i64,
) -> Result<(), CommandError> {
    let col = match direction {
        "in" => "unallocated_cr_minor",
        "out" => "unallocated_dr_minor",
        d => return Err(reject(format!("invalid direction: {d}"))),
    };
    let sql = format!("SELECT {col} FROM party_balances WHERE party_id = ?1");
    let held: i64 = conn.query_row(&sql, [party_id], |r| r.get(0)).optional()?.unwrap_or(0);
    if total_alloc > held {
        Err(reject(format!("credit overdraw: party {party_id} holds {held}, cannot allocate {total_alloc}")))
    } else { Ok(()) }
}

/// The seeded walk-in customer and anonymous supplier stand in for counterparties
/// the business never recorded, so they cannot carry a balance: a receivable
/// against "Walk-in Customer" names nobody to collect from. Cash trade with them
/// is the whole point; credit trade is always a mistake.
///
/// The UI clears its auto-selection when the user switches to credit, so this
/// guard is the backstop for a manual pick or an imported command.
pub(crate) fn check_seeded_party_cash_only(party_id: &str, terms: &str)
-> Result<(), CommandError> {
    if terms == "credit"
        && (party_id == crate::genesis::WALKIN_PARTY_ID
            || party_id == crate::genesis::ANON_SUPPLIER_PARTY_ID)
    {
        return Err(reject(format!(
            "{party_id} is a built-in party for cash trade and cannot be used on credit; \
             record the counterparty first"
        )));
    }
    Ok(())
}

pub(crate) fn check_self_transfer(from: &str, to: &str) -> Result<(), CommandError> {
    if from == to { Err(reject(format!("self-transfer: from == to ({from})"))) } else { Ok(()) }
}

pub(crate) fn check_expense_account_type(conn: &Connection, account_id: &str) -> Result<(), CommandError> {
    let acct_type: Option<String> = conn.query_row(
        "SELECT type FROM accounts WHERE id = ?1", [account_id], |r| r.get(0)).optional()?;
    match acct_type.as_deref() {
        None => Err(reject(format!("unknown account: {account_id}"))),
        Some("expense") => Ok(()),
        Some(t) => Err(reject(format!("account {account_id} is type '{t}', expected 'expense'"))),
    }
}

pub(crate) fn check_reversal_legal_target(conn: &Connection, target_event_id: &str)
    -> Result<(), CommandError> {
    let etype: Option<String> = conn.query_row(
        "SELECT type FROM events WHERE id = ?1", [target_event_id], |r| r.get(0)).optional()?;
    match etype {
        None => Err(reject(format!("unknown target event: {target_event_id}"))),
        Some(t) if !is_transactional(&t) =>
            Err(reject(format!("event type '{t}' is not a legal reversal target"))),
        Some(_) => Ok(()),
    }
}

pub(crate) fn check_not_already_reversed(conn: &Connection, target_event_id: &str)
    -> Result<(), CommandError> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM events
         WHERE type = 'TransactionReversed' AND payload ->> 'targetEventId' = ?1",
        [target_event_id], |r| r.get(0))?;
    if n > 0 { Err(reject(format!("event {target_event_id} already reversed"))) } else { Ok(()) }
}

pub(crate) fn check_lot_source_void(conn: &Connection, source_event_id: &str)
    -> Result<(), CommandError> {
    let consumed: i64 = conn.query_row(
        "SELECT COUNT(*) FROM inventory_lots
         WHERE source_event_id = ?1 AND qty_remaining < qty_received",
        [source_event_id], |r| r.get(0))?;
    if consumed > 0 {
        Err(reject(format!("cannot void event {source_event_id}: lots already consumed")))
    } else { Ok(()) }
}

pub(crate) fn check_reversal_downstream(
    conn: &Connection, invoice_type: &str, invoice_id: &str, payment_id: Option<&str>,
) -> Result<(), CommandError> {
    let allocs: i64 = conn.query_row(
        "SELECT COUNT(*) FROM payment_allocations WHERE target_id = ?1 AND target_type = ?2",
        rusqlite::params![invoice_id, invoice_type], |r| r.get(0))?;
    if allocs > 0 {
        return Err(reject(format!("cannot reverse: {invoice_type} {invoice_id} has allocations")));
    }
    let returns: i64 = conn.query_row(
        "SELECT COUNT(*) FROM returns WHERE original_id = ?1", [invoice_id], |r| r.get(0))?;
    if returns > 0 {
        return Err(reject(format!("cannot reverse: {invoice_id} has returns against it")));
    }
    if let Some(pid) = payment_id {
        let draws: i64 = conn.query_row(
            "SELECT COUNT(*) FROM events
             WHERE type = 'PaymentAllocated' AND payload ->> 'paymentId' = ?1",
            [pid], |r| r.get(0))?;
        if draws > 0 {
            return Err(reject(format!("cannot reverse payment {pid}: credit was drawn by a later PaymentAllocated")));
        }
    }
    Ok(())
}

pub(crate) fn check_reversal_lot_restore_reconsumed(conn: &Connection, return_id: &str)
    -> Result<(), CommandError> {
    let mut stmt = conn.prepare(
        "SELECT rl.lot_id, SUM(rl.qty) AS restored, il.qty_remaining
         FROM return_lines rl
         JOIN inventory_lots il ON il.id = rl.lot_id
         WHERE rl.return_id = ?1
         GROUP BY rl.lot_id, il.qty_remaining",
    )?;
    let rows = stmt.query_map([return_id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?))
    })?;
    for row in rows {
        let (lot_id, restored, remaining) = row?;
        if remaining < restored {
            return Err(reject(format!(
                "cannot reverse return {return_id}: lot {lot_id} has {remaining} remaining but restored {restored}")));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rejects_zero_and_negative_quantities() {
        assert!(check_qty_positive(0).is_err());
        assert!(check_qty_positive(-3).is_err());
        assert!(check_qty_positive(1).is_ok());
    }

    #[test]
    fn rejects_negative_amounts_and_optionally_zero() {
        assert!(check_amount_non_negative(-1).is_err());
        assert!(check_amount_non_negative(0).is_ok());
        assert!(check_amount_positive(0).is_err());
        assert!(check_amount_positive(50).is_ok());
    }

    #[test]
    fn rejects_empty_line_set() {
        let empty: Vec<Value> = vec![];
        assert!(check_at_least_one_line(&empty).is_err());
        assert!(check_at_least_one_line(&[json!({"x":1})]).is_ok());
    }

    fn seed_lot(conn: &Connection, lot_id: &str, item_id: &str, qty_remaining: i64) {
        conn.execute(
            "INSERT OR IGNORE INTO items (id, doc) VALUES (?1, jsonb(?2))",
            rusqlite::params![item_id, format!(r#"{{"sku":"{item_id}","name":"{item_id}","unit":"ea","active":1}}"#)],
        ).unwrap();
        conn.execute(
            "INSERT INTO inventory_lots
               (id, item_id, source_event_id, purchase_id, unit_cost_minor,
                qty_received, qty_remaining, acquired_at, supplier_id)
             VALUES (?1, ?2, 'evt', NULL, 100, ?3, ?3, '2026-01-01', NULL)",
            rusqlite::params![lot_id, item_id, qty_remaining],
        ).unwrap();
    }

    #[test]
    fn oversell_guard_rejects_taking_more_than_remaining() {
        let conn = crate::db::open_in_memory_with_schema().unwrap();
        seed_lot(&conn, "lot1", "itemA", 10);
        assert!(LotDemand::new().take(&conn, "lot1", 15).is_err(), "15 > 10 must reject");
        assert!(LotDemand::new().take(&conn, "lot1", 10).is_ok());
        assert!(LotDemand::new().take(&conn, "lot1", 6).is_ok());
    }

    #[test]
    fn oversell_guard_rejects_unknown_lot() {
        let conn = crate::db::open_in_memory_with_schema().unwrap();
        assert!(LotDemand::new().take(&conn, "ghost", 1).is_err(), "unknown lot must reject");
    }

    #[test]
    fn lot_demand_rejects_cumulative_overdraw_within_one_command() {
        let conn = crate::db::open_in_memory_with_schema().unwrap();
        seed_lot(&conn, "lot1", "itemA", 10);
        let mut demand = LotDemand::new();
        assert!(demand.take(&conn, "lot1", 6).is_ok());
        assert!(demand.take(&conn, "lot1", 6).is_err(), "cumulative 12 > 10 must reject");
        assert_eq!(demand.available(&conn, "lot1").unwrap(), 4, "10 - 6 already claimed");
    }

    #[test]
    fn lot_demand_allows_cumulative_within_stock() {
        let conn = crate::db::open_in_memory_with_schema().unwrap();
        seed_lot(&conn, "lot1", "itemA", 10);
        let mut demand = LotDemand::new();
        assert!(demand.take(&conn, "lot1", 6).is_ok());
        assert!(demand.take(&conn, "lot1", 4).is_ok(), "6 + 4 == 10 exactly");
        assert!(demand.take(&conn, "lot1", 1).is_err(), "one more overdraws");
    }

    #[test]
    fn lot_item_match_guard_rejects_wrong_item() {
        let conn = crate::db::open_in_memory_with_schema().unwrap();
        seed_lot(&conn, "lot1", "itemA", 10);
        assert!(check_lot_item_match(&conn, "lot1", "itemB").is_err());
        assert!(check_lot_item_match(&conn, "lot1", "itemA").is_ok());
    }

    #[test]
    fn return_against_reversed_invoice_rejected() {
        let conn = crate::db::open_in_memory_with_schema().unwrap();
        conn.execute("INSERT INTO sales (id,event_id,customer_id,date,terms,total_minor,outstanding_minor,reversed) VALUES ('s1','e','c','2026-02-01','cash',0,0,1)", []).unwrap();
        conn.execute("INSERT INTO sales (id,event_id,customer_id,date,terms,total_minor,outstanding_minor,reversed) VALUES ('s2','e','c','2026-02-01','cash',0,0,0)", []).unwrap();
        assert!(check_invoice_not_reversed(&conn, "sales", "s1").is_err());
        assert!(check_invoice_not_reversed(&conn, "sales", "s2").is_ok());
        assert!(check_invoice_not_reversed(&conn, "sales", "ghost").is_err());
    }

    #[test]
    fn sale_return_over_restore_rejects_more_than_consumed() {
        let conn = crate::db::open_in_memory_with_schema().unwrap();
        seed_lot(&conn, "lot1", "itemA", 4);
        conn.execute("UPDATE inventory_lots SET qty_received=10, unit_cost_minor=500 WHERE id='lot1'", []).unwrap();
        conn.execute("INSERT INTO sales (id,event_id,customer_id,date,terms,total_minor,outstanding_minor,reversed) VALUES ('s1','e','c','2026-02-01','cash',0,0,0)", []).unwrap();
        conn.execute("INSERT INTO sale_lines (id,sale_id,item_id,qty,unit_price_minor,revenue_minor,cogs_minor,date) VALUES ('sl1','s1','itemA',6,1000,6000,3000,'2026-02-01')", []).unwrap();
        conn.execute("INSERT INTO lot_consumptions (id,sale_line_id,lot_id,qty_taken,unit_cost_minor) VALUES ('lc1','sl1','lot1',6,500)", []).unwrap();
        assert!(check_sale_return_over_restore(&conn, "s1", "lot1", 7).is_err());
        assert!(check_sale_return_over_restore(&conn, "s1", "lot1", 6).is_ok());
    }

    #[test]
    fn sale_return_rejects_lot_original_sale_never_consumed() {
        let conn = crate::db::open_in_memory_with_schema().unwrap();
        conn.execute("INSERT INTO sales (id,event_id,customer_id,date,terms,total_minor,outstanding_minor,reversed) VALUES ('s1','e','c','2026-02-01','cash',0,0,0)", []).unwrap();
        assert!(check_sale_return_over_restore(&conn, "s1", "lotX", 1).is_err());
    }

    #[test]
    fn sale_return_second_return_bounded_by_qty_received() {
        let conn = crate::db::open_in_memory_with_schema().unwrap();
        seed_lot(&conn, "lot1", "itemA", 10);
        conn.execute("UPDATE inventory_lots SET qty_received=10, unit_cost_minor=500 WHERE id='lot1'", []).unwrap();
        conn.execute("INSERT INTO sales (id,event_id,customer_id,date,terms,total_minor,outstanding_minor,reversed) VALUES ('s1','e','c','2026-02-01','cash',0,0,0)", []).unwrap();
        conn.execute("INSERT INTO sale_lines (id,sale_id,item_id,qty,unit_price_minor,revenue_minor,cogs_minor,date) VALUES ('sl1','s1','itemA',6,1000,6000,3000,'2026-02-01')", []).unwrap();
        conn.execute("INSERT INTO lot_consumptions (id,sale_line_id,lot_id,qty_taken,unit_cost_minor) VALUES ('lc1','sl1','lot1',6,500)", []).unwrap();
        assert!(check_sale_return_over_restore(&conn, "s1", "lot1", 6).is_err(),
            "lot already at qty_received; further return must be rejected");
    }

    fn seed_credit_sale(conn: &Connection, sale_id: &str, cust: &str, outstanding: i64) {
        conn.execute(
            "INSERT INTO sales (id,event_id,customer_id,date,terms,total_minor,outstanding_minor,reversed)
             VALUES (?1,'e',?2,'2026-01-01','credit',?3,?3,0)",
            rusqlite::params![sale_id, cust, outstanding]).unwrap();
    }

    #[test]
    fn invoice_over_allocation_rejected() {
        let conn = crate::db::open_in_memory_with_schema().unwrap();
        seed_credit_sale(&conn, "s1", "cust1", 5000);
        assert!(check_invoice_over_allocation(&conn, "s1", "sale", 6000).is_err());
        assert!(check_invoice_over_allocation(&conn, "s1", "sale", 5000).is_ok());
        assert!(check_invoice_over_allocation(&conn, "s1", "sale", 4000).is_ok());
    }

    #[test]
    fn invoice_over_allocation_aggregates_per_target() {
        let conn = crate::db::open_in_memory_with_schema().unwrap();
        seed_credit_sale(&conn, "s1", "cust1", 5000);
        let allocs = vec![("s1".to_string(), "sale".to_string(), 5000i64),
                          ("s1".to_string(), "sale".to_string(), 3000i64)];
        assert!(check_invoice_over_allocation_aggregated(&conn, &allocs).is_err());
        let ok = vec![("s1".to_string(), "sale".to_string(), 2000i64),
                      ("s1".to_string(), "sale".to_string(), 3000i64)];
        assert!(check_invoice_over_allocation_aggregated(&conn, &ok).is_ok());
    }

    #[test]
    fn payment_over_allocation_rejects_sum_exceeding_payment() {
        assert!(check_payment_over_allocation(8000, &[3000, 6000]).is_err());
        assert!(check_payment_over_allocation(8000, &[3000, 5000]).is_ok());
        assert!(check_payment_over_allocation(8000, &[3000]).is_ok());
    }

    #[test]
    fn party_ownership_rejects_other_party_and_wrong_direction() {
        let conn = crate::db::open_in_memory_with_schema().unwrap();
        seed_credit_sale(&conn, "s1", "cust1", 5000);
        assert!(check_allocation_party_ownership(&conn, "cust1", "in", "s1", "sale").is_ok());
        assert!(check_allocation_party_ownership(&conn, "cust2", "in", "s1", "sale").is_err());
        assert!(check_allocation_party_ownership(&conn, "cust1", "out", "s1", "sale").is_err());
    }

    fn seed_party(conn: &Connection, party_id: &str, kind: &str) {
        conn.execute(
            "INSERT INTO parties (id, doc) VALUES (?1, jsonb(?2))",
            rusqlite::params![party_id, format!(r#"{{"name":"{party_id}","kind":"{kind}"}}"#)],
        ).unwrap();
    }

    #[test]
    fn credit_overdraw_rejects_more_than_held() {
        let conn = crate::db::open_in_memory_with_schema().unwrap();
        seed_party(&conn, "cust1", "customer");
        conn.execute(
            "INSERT INTO party_balances (party_id,receivable_minor,payable_minor,unallocated_cr_minor,unallocated_dr_minor)
             VALUES ('cust1',0,0,3000,0)", []).unwrap();
        assert!(check_credit_overdraw(&conn, "cust1", "in", 4000).is_err());
        assert!(check_credit_overdraw(&conn, "cust1", "in", 3000).is_ok());
        assert!(check_credit_overdraw(&conn, "cust1", "in", 2000).is_ok());
    }

    #[test]
    fn self_transfer_rejected() {
        assert!(check_self_transfer("a1", "a1").is_err());
        assert!(check_self_transfer("a1", "a2").is_ok());
    }

    #[test]
    fn seeded_parties_may_trade_for_cash_but_not_on_credit() {
        for id in [crate::genesis::WALKIN_PARTY_ID, crate::genesis::ANON_SUPPLIER_PARTY_ID] {
            assert!(check_seeded_party_cash_only(id, "cash").is_ok(), "{id} cash must be allowed");
            assert!(
                check_seeded_party_cash_only(id, "credit").is_err(),
                "{id} on credit would book a balance against nobody"
            );
        }
        // An ordinary party is unaffected on either terms.
        assert!(check_seeded_party_cash_only("party_7", "credit").is_ok());
        assert!(check_seeded_party_cash_only("party_7", "cash").is_ok());
    }

    #[test]
    fn expense_account_type_guard() {
        let conn = crate::db::open_in_memory_with_schema().unwrap();
        conn.execute("INSERT INTO accounts (id, doc, balance_minor) VALUES ('rent', jsonb('{\"name\":\"Rent\",\"type\":\"expense\",\"normal\":\"debit\"}'), 0)", []).unwrap();
        conn.execute("INSERT INTO accounts (id, doc, balance_minor) VALUES ('bank', jsonb('{\"name\":\"Bank\",\"type\":\"asset\",\"normal\":\"debit\"}'), 0)", []).unwrap();
        assert!(check_expense_account_type(&conn, "rent").is_ok());
        assert!(check_expense_account_type(&conn, "bank").is_err());
        assert!(check_expense_account_type(&conn, "ghost").is_err());
    }

    #[test]
    fn credit_overdraw_supplier_uses_dr_column() {
        let conn = crate::db::open_in_memory_with_schema().unwrap();
        seed_party(&conn, "sup1", "supplier");
        conn.execute(
            "INSERT INTO party_balances (party_id,receivable_minor,payable_minor,unallocated_cr_minor,unallocated_dr_minor)
             VALUES ('sup1',0,0,0,1500)", []).unwrap();
        assert!(check_credit_overdraw(&conn, "sup1", "out", 2000).is_err());
        assert!(check_credit_overdraw(&conn, "sup1", "out", 1500).is_ok());
    }

    fn insert_event(conn: &Connection, id: &str, hlc: &str, etype: &str, payload_json: &str) {
        conn.execute(
            "INSERT INTO events (id,hlc,device_id,user_id,seq,type,payload,created_at)
             VALUES (?1,?2,?1,'u',1,?3, jsonb(?4), 0)",
            rusqlite::params![id, hlc, etype, payload_json]).unwrap();
    }

    fn seed_item_row(conn: &Connection, item_id: &str) {
        conn.execute(
            "INSERT OR IGNORE INTO items (id, doc) VALUES (?1, jsonb(?2))",
            rusqlite::params![item_id, format!(r#"{{"sku":"{item_id}","name":"{item_id}","unit":"ea","active":1}}"#)],
        ).unwrap();
    }

    #[test]
    fn legal_target_and_double_void() {
        let conn = crate::db::open_in_memory_with_schema().unwrap();
        insert_event(&conn, "e_item", "h1", "ItemDefined", r#"{"itemId":"i1"}"#);
        insert_event(&conn, "e_sale", "h2", "SaleRecorded", r#"{"saleId":"s1"}"#);
        assert!(check_reversal_legal_target(&conn, "e_item").is_err());
        assert!(check_reversal_legal_target(&conn, "e_sale").is_ok());
        insert_event(&conn, "e_rev", "h3", "TransactionReversed", r#"{"targetEventId":"e_sale"}"#);
        assert!(check_not_already_reversed(&conn, "e_sale").is_err());
        assert!(check_not_already_reversed(&conn, "e_item").is_ok());
        assert!(check_reversal_legal_target(&conn, "ghost").is_err());
        assert!(check_reversal_legal_target(&conn, "e_rev").is_err());
    }

    #[test]
    fn lot_source_void_rejects_consumed_lot() {
        let conn = crate::db::open_in_memory_with_schema().unwrap();
        seed_item_row(&conn, "itemA");
        conn.execute("INSERT INTO inventory_lots (id,item_id,source_event_id,purchase_id,unit_cost_minor,qty_received,qty_remaining,acquired_at,supplier_id) VALUES ('lot1','itemA','e_pur',NULL,100,10,6,'2026-01-01',NULL)", []).unwrap();
        assert!(check_lot_source_void(&conn, "e_pur").is_err());
        conn.execute("INSERT INTO inventory_lots (id,item_id,source_event_id,purchase_id,unit_cost_minor,qty_received,qty_remaining,acquired_at,supplier_id) VALUES ('lot2','itemA','e_pur2',NULL,100,10,10,'2026-01-01',NULL)", []).unwrap();
        assert!(check_lot_source_void(&conn, "e_pur2").is_ok());
    }

    #[test]
    fn downstream_guard_blocks_allocation_return_and_credit_draw() {
        let conn = crate::db::open_in_memory_with_schema().unwrap();
        assert!(check_reversal_downstream(&conn, "sale", "s1", None).is_ok());
        conn.execute("INSERT INTO payment_allocations (id,event_id,payment_id,target_id,target_type,amount_minor) VALUES ('pa1','ep','pay1','s1','sale',100)", []).unwrap();
        assert!(check_reversal_downstream(&conn, "sale", "s1", None).is_err());
        conn.execute("INSERT INTO returns (id,event_id,return_type,original_id,date,revenue_reversed_minor,cost_restored_minor) VALUES ('r1','er','sale_return','s2','2026-02-01',100,50)", []).unwrap();
        assert!(check_reversal_downstream(&conn, "sale", "s2", None).is_err());
        insert_event(&conn, "e_alloc", "h9", "PaymentAllocated", r#"{"paymentId":"pay1"}"#);
        assert!(check_reversal_downstream(&conn, "payment", "invoiceX", Some("pay1")).is_err());
        assert!(check_reversal_downstream(&conn, "payment", "invoiceY", Some("payNONE")).is_ok());
    }

    #[test]
    fn downstream_edge5_blocks_reversing_return_whose_units_were_reconsumed() {
        let conn = crate::db::open_in_memory_with_schema().unwrap();
        seed_item_row(&conn, "itemA");
        conn.execute("INSERT INTO inventory_lots (id,item_id,source_event_id,purchase_id,unit_cost_minor,qty_received,qty_remaining,acquired_at,supplier_id) VALUES ('lot1','itemA','e_pur',NULL,500,10,10,'2026-01-01',NULL)", []).unwrap();
        conn.execute("INSERT INTO returns (id,event_id,return_type,original_id,date,revenue_reversed_minor,cost_restored_minor) VALUES ('ret1','e_ret','sale_return','sOrig','2026-03-01',6000,3000)", []).unwrap();
        conn.execute("INSERT INTO return_lines (id,return_id,item_id,qty,unit_price_minor,unit_cost_minor,lot_id) VALUES ('rl1','ret1','itemA',6,1000,500,'lot1')", []).unwrap();
        assert!(check_reversal_lot_restore_reconsumed(&conn, "ret1").is_ok());
        conn.execute("UPDATE inventory_lots SET qty_remaining = 4 WHERE id='lot1'", []).unwrap();
        assert!(check_reversal_lot_restore_reconsumed(&conn, "ret1").is_err());
    }
}
