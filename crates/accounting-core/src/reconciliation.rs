use rusqlite::Connection;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    Pass,
    Fail(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub name: &'static str,
    pub outcome: CheckOutcome,
}

pub fn check_inventory_valuation(conn: &Connection) -> rusqlite::Result<CheckOutcome> {
    let gl: i64 = conn.query_row(
        "SELECT balance_minor FROM accounts WHERE system_role = 'inventory'", [], |r| r.get(0))?;
    let lots: i64 = conn.query_row(
        "SELECT COALESCE(SUM(qty_remaining * unit_cost_minor), 0) FROM inventory_lots", [], |r| r.get(0))?;
    Ok(if gl == lots {
        CheckOutcome::Pass
    } else {
        CheckOutcome::Fail(format!("inventory valuation drift: lot value {lots} != Inventory GL {gl}"))
    })
}

// --- Check #2: gross profit engine == journal Sales-COGS (spec §7.2) ---
pub fn check_gross_profit(conn: &Connection) -> rusqlite::Result<CheckOutcome> {
    let engine: i64 = conn.query_row(
        "SELECT
          (SELECT COALESCE(SUM(sl.revenue_minor - sl.cogs_minor), 0)
             FROM sale_lines sl JOIN sales s ON s.id = sl.sale_id WHERE s.reversed = 0)
          -
          (SELECT COALESCE(SUM(revenue_reversed_minor - cost_restored_minor), 0)
             FROM returns WHERE return_type = 'sale_return')",
        [], |r| r.get(0))?;
    let journal: i64 = conn.query_row(
        "SELECT
          (SELECT COALESCE(SUM(credit_minor - debit_minor), 0) FROM journal_lines jl
             JOIN accounts a ON a.id = jl.account_id WHERE a.system_role = 'sales')
          -
          (SELECT COALESCE(SUM(debit_minor - credit_minor), 0) FROM journal_lines jl
             JOIN accounts a ON a.id = jl.account_id WHERE a.system_role = 'cogs')",
        [], |r| r.get(0))?;
    Ok(if engine == journal { CheckOutcome::Pass }
    else { CheckOutcome::Fail(format!("gross profit drift: engine {engine} != journal {journal}")) })
}

// --- Check #3: per-txn double-entry (spec §7.3) ---
pub fn check_double_entry(conn: &Connection) -> rusqlite::Result<CheckOutcome> {
    let mut stmt = conn.prepare(
        "SELECT txn_id, SUM(debit_minor) AS d, SUM(credit_minor) AS c
         FROM journal_lines GROUP BY txn_id
         HAVING SUM(debit_minor) <> SUM(credit_minor) LIMIT 1")?;
    let mut rows = stmt.query([])?;
    if let Some(row) = rows.next()? {
        let txn: String = row.get(0)?;
        let d: i64 = row.get(1)?;
        let c: i64 = row.get(2)?;
        return Ok(CheckOutcome::Fail(format!("unbalanced txn {txn}: debit {d} != credit {c}")));
    }
    Ok(CheckOutcome::Pass)
}

// --- Check #4: net-form party balances == A/R and A/P GL (spec §7.4) ---
pub fn check_party_balances(conn: &Connection) -> rusqlite::Result<CheckOutcome> {
    let (sum_recv, sum_cr, sum_pay, sum_dr): (i64, i64, i64, i64) = conn.query_row(
        "SELECT COALESCE(SUM(receivable_minor),0), COALESCE(SUM(unallocated_cr_minor),0),
                COALESCE(SUM(payable_minor),0), COALESCE(SUM(unallocated_dr_minor),0)
         FROM party_balances", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
    let ar_gl: i64 = conn.query_row(
        "SELECT balance_minor FROM accounts WHERE system_role = 'accounts_receivable'", [], |r| r.get(0))?;
    let ap_gl: i64 = conn.query_row(
        "SELECT balance_minor FROM accounts WHERE system_role = 'accounts_payable'", [], |r| r.get(0))?;
    let net_recv = sum_recv - sum_cr;
    let net_pay = sum_pay - sum_dr;
    if net_recv != ar_gl {
        return Ok(CheckOutcome::Fail(format!(
            "receivable drift: net Σreceivable-Σunallocated_cr {net_recv} != A/R GL {ar_gl}")));
    }
    if net_pay != ap_gl {
        return Ok(CheckOutcome::Fail(format!(
            "payable drift: net Σpayable-Σunallocated_dr {net_pay} != A/P GL {ap_gl}")));
    }
    Ok(CheckOutcome::Pass)
}

// --- Check #5: terms-aware invoice outstanding (spec §7.5) ---
fn verify_outstanding(conn: &Connection, sql: &str, kind: &str) -> rusqlite::Result<Option<String>> {
    let mut stmt = conn.prepare(sql)?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let terms: String = row.get(1)?;
        let total: i64 = row.get(2)?;
        let outstanding: i64 = row.get(3)?;
        let allocated: i64 = row.get(4)?;
        let returned: i64 = row.get(5)?;
        if outstanding < 0 {
            return Ok(Some(format!("{kind} {id}: outstanding {outstanding} is negative")));
        }
        let expected = if terms == "credit" { (total - allocated - returned).max(0) } else { 0 };
        if outstanding != expected {
            return Ok(Some(format!(
                "{kind} {id} ({terms}): outstanding {outstanding} != expected {expected} (total {total} - allocated {allocated} - returned {returned})")));
        }
    }
    Ok(None)
}

pub fn check_invoice_outstanding(conn: &Connection) -> rusqlite::Result<CheckOutcome> {
    let sales_sql = "\
        SELECT s.id, s.terms, s.total_minor, s.outstanding_minor,
          COALESCE((SELECT SUM(amount_minor) FROM payment_allocations WHERE target_id = s.id AND target_type = 'sale'), 0),
          COALESCE((SELECT SUM(revenue_reversed_minor) FROM returns WHERE original_id = s.id AND return_type = 'sale_return'), 0)
        FROM sales s";
    let purchases_sql = "\
        SELECT p.id, p.terms, p.total_minor, p.outstanding_minor,
          COALESCE((SELECT SUM(amount_minor) FROM payment_allocations WHERE target_id = p.id AND target_type = 'purchase'), 0),
          COALESCE((SELECT SUM(cost_restored_minor) FROM returns WHERE original_id = p.id AND return_type = 'purchase_return'), 0)
        FROM purchases p";
    if let Some(msg) = verify_outstanding(conn, sales_sql, "sale")? {
        return Ok(CheckOutcome::Fail(msg));
    }
    if let Some(msg) = verify_outstanding(conn, purchases_sql, "purchase")? {
        return Ok(CheckOutcome::Fail(msg));
    }
    Ok(CheckOutcome::Pass)
}

// --- Check #6: non-negative inventory (spec §7.6) ---
pub fn check_non_negative_inventory(conn: &Connection) -> rusqlite::Result<CheckOutcome> {
    let mut stmt = conn.prepare(
        "SELECT id, qty_remaining FROM inventory_lots WHERE qty_remaining < 0 LIMIT 1")?;
    let mut rows = stmt.query([])?;
    if let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let qty: i64 = row.get(1)?;
        return Ok(CheckOutcome::Fail(format!("lot {id} has negative qty_remaining {qty}")));
    }
    Ok(CheckOutcome::Pass)
}

// --- Check #7: lot bounds 0 <= remaining <= received (spec §7.7) ---
pub fn check_lot_bounds(conn: &Connection) -> rusqlite::Result<CheckOutcome> {
    let mut stmt = conn.prepare(
        "SELECT id, qty_remaining, qty_received FROM inventory_lots
         WHERE qty_remaining < 0 OR qty_remaining > qty_received LIMIT 1")?;
    let mut rows = stmt.query([])?;
    if let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let rem: i64 = row.get(1)?;
        let recv: i64 = row.get(2)?;
        return Ok(CheckOutcome::Fail(format!(
            "lot {id} out of bounds: qty_remaining {rem} not in [0, {recv}]")));
    }
    Ok(CheckOutcome::Pass)
}

// --- Check #8: non-negative unallocated credits (spec §7.8) ---
pub fn check_non_negative_credits(conn: &Connection) -> rusqlite::Result<CheckOutcome> {
    let mut stmt = conn.prepare(
        "SELECT party_id, unallocated_cr_minor, unallocated_dr_minor FROM party_balances
         WHERE unallocated_cr_minor < 0 OR unallocated_dr_minor < 0 LIMIT 1")?;
    let mut rows = stmt.query([])?;
    if let Some(row) = rows.next()? {
        let party: String = row.get(0)?;
        let cr: i64 = row.get(1)?;
        let dr: i64 = row.get(2)?;
        return Ok(CheckOutcome::Fail(format!(
            "party {party} has negative unallocated credit: cr {cr}, dr {dr}")));
    }
    Ok(CheckOutcome::Pass)
}

pub fn run_all_checks(conn: &Connection) -> rusqlite::Result<Vec<Check>> {
    Ok(vec![
        Check { name: "inventory_valuation",    outcome: check_inventory_valuation(conn)? },
        Check { name: "gross_profit",           outcome: check_gross_profit(conn)? },
        Check { name: "double_entry",           outcome: check_double_entry(conn)? },
        Check { name: "party_balances",         outcome: check_party_balances(conn)? },
        Check { name: "invoice_outstanding",    outcome: check_invoice_outstanding(conn)? },
        Check { name: "non_negative_inventory", outcome: check_non_negative_inventory(conn)? },
        Check { name: "lot_bounds",             outcome: check_lot_bounds(conn)? },
        Check { name: "non_negative_credits",   outcome: check_non_negative_credits(conn)? },
    ])
}

pub fn all_passed(checks: &[Check]) -> bool {
    checks.iter().all(|c| c.outcome == CheckOutcome::Pass)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::open_seeded;

    #[test]
    fn inventory_valuation_matches_gl_on_correct_state() {
        let (conn, _hlc) = open_seeded();
        assert_eq!(check_inventory_valuation(&conn).unwrap(), CheckOutcome::Pass);
    }

    #[test]
    fn inventory_valuation_fails_when_a_lot_is_corrupted() {
        let (conn, _hlc) = open_seeded();
        conn.execute("UPDATE inventory_lots SET qty_remaining = qty_remaining + 1 WHERE id = 'pur_1#lot0'", []).unwrap();
        match check_inventory_valuation(&conn).unwrap() {
            CheckOutcome::Fail(msg) => assert!(msg.contains("lot value"), "got: {msg}"),
            CheckOutcome::Pass => panic!("check must FAIL on corrupted lot"),
        }
    }

    #[test]
    fn gross_profit_matches_journal_on_correct_state() {
        let (conn, _) = open_seeded();
        assert_eq!(check_gross_profit(&conn).unwrap(), CheckOutcome::Pass);
    }

    #[test]
    fn gross_profit_fails_when_cogs_corrupted() {
        let (conn, _) = open_seeded();
        conn.execute("UPDATE sale_lines SET cogs_minor = cogs_minor - 1000 WHERE sale_id = 'sale_1'", []).unwrap();
        assert!(matches!(check_gross_profit(&conn).unwrap(), CheckOutcome::Fail(_)));
    }

    #[test]
    fn gross_profit_fails_when_return_netting_corrupted() {
        let (conn, _) = open_seeded();
        // Corrupt the returns revenue_reversed so the engine's netting term drifts
        // from the journal (which already has the return's Dr Sales posted). This
        // proves the returns-netting term in check #2 is load-bearing — without it,
        // the check would still pass because both sides would be equally wrong.
        conn.execute(
            "UPDATE returns SET revenue_reversed_minor = revenue_reversed_minor + 500 WHERE return_type = 'sale_return'",
            []).unwrap();
        match check_gross_profit(&conn).unwrap() {
            CheckOutcome::Fail(msg) => assert!(msg.contains("engine") || msg.contains("journal"), "got {msg}"),
            CheckOutcome::Pass => panic!("check must FAIL when return netting is corrupted"),
        }
    }

    #[test]
    fn double_entry_balances_on_correct_state() {
        let (conn, _) = open_seeded();
        assert_eq!(check_double_entry(&conn).unwrap(), CheckOutcome::Pass);
    }

    #[test]
    fn double_entry_fails_when_unbalanced() {
        let (conn, _) = open_seeded();
        conn.execute("UPDATE journal_lines SET debit_minor = debit_minor + 1 WHERE rowid = (SELECT rowid FROM journal_lines LIMIT 1)", []).unwrap();
        assert!(matches!(check_double_entry(&conn).unwrap(), CheckOutcome::Fail(_)));
    }

    #[test]
    fn party_balances_match_gl_on_correct_state() {
        let (conn, _) = open_seeded();
        assert_eq!(check_party_balances(&conn).unwrap(), CheckOutcome::Pass);
    }

    #[test]
    fn party_balances_fail_when_receivable_drifts() {
        let (conn, _) = open_seeded();
        conn.execute("UPDATE party_balances SET receivable_minor = receivable_minor + 100 WHERE party_id = 'cust_acme'", []).unwrap();
        assert!(matches!(check_party_balances(&conn).unwrap(), CheckOutcome::Fail(_)));
    }

    #[test]
    fn invoice_outstanding_matches_on_correct_state() {
        let (conn, _) = open_seeded();
        assert_eq!(check_invoice_outstanding(&conn).unwrap(), CheckOutcome::Pass);
    }

    #[test]
    fn invoice_outstanding_fails_when_corrupted() {
        let (conn, _) = open_seeded();
        conn.execute("UPDATE sales SET outstanding_minor = 12345 WHERE id = 'sale_1'", []).unwrap();
        assert!(matches!(check_invoice_outstanding(&conn).unwrap(), CheckOutcome::Fail(_)));
    }

    #[test]
    fn invoice_outstanding_fails_on_nonzero_cash() {
        let (conn, _) = open_seeded();
        conn.execute("UPDATE sales SET outstanding_minor = 100 WHERE id = 'sale_2'", []).unwrap();
        assert!(matches!(check_invoice_outstanding(&conn).unwrap(), CheckOutcome::Fail(_)));
    }

    #[test]
    fn invoice_outstanding_fails_on_negative() {
        let (conn, _) = open_seeded();
        conn.execute("UPDATE purchases SET outstanding_minor = -1 WHERE id = 'pur_1'", []).unwrap();
        assert!(matches!(check_invoice_outstanding(&conn).unwrap(), CheckOutcome::Fail(_)));
    }

    #[test]
    fn non_negative_inventory_holds_on_correct_state() {
        let (conn, _) = open_seeded();
        assert_eq!(check_non_negative_inventory(&conn).unwrap(), CheckOutcome::Pass);
    }

    #[test]
    fn non_negative_inventory_fails_on_negative_lot() {
        let (conn, _) = open_seeded();
        conn.execute("UPDATE inventory_lots SET qty_remaining = -1 WHERE id = 'pur_2#lot0'", []).unwrap();
        assert!(matches!(check_non_negative_inventory(&conn).unwrap(), CheckOutcome::Fail(_)));
    }

    #[test]
    fn lot_bounds_hold_on_correct_state() {
        let (conn, _) = open_seeded();
        assert_eq!(check_lot_bounds(&conn).unwrap(), CheckOutcome::Pass);
    }

    #[test]
    fn lot_bounds_fail_when_remaining_exceeds_received() {
        let (conn, _) = open_seeded();
        conn.execute("UPDATE inventory_lots SET qty_remaining = qty_received + 1 WHERE id = 'pur_2#lot0'", []).unwrap();
        assert!(matches!(check_lot_bounds(&conn).unwrap(), CheckOutcome::Fail(_)));
    }

    #[test]
    fn non_negative_credits_hold_on_correct_state() {
        let (conn, _) = open_seeded();
        assert_eq!(check_non_negative_credits(&conn).unwrap(), CheckOutcome::Pass);
    }

    #[test]
    fn non_negative_credits_fail_on_negative() {
        let (conn, _) = open_seeded();
        conn.execute("UPDATE party_balances SET unallocated_cr_minor = -5 WHERE party_id = 'cust_acme'", []).unwrap();
        assert!(matches!(check_non_negative_credits(&conn).unwrap(), CheckOutcome::Fail(_)));
    }

    #[test]
    fn run_all_checks_all_pass_on_reference_business() {
        let (conn, _) = open_seeded();
        let checks = run_all_checks(&conn).unwrap();
        assert_eq!(checks.len(), 8);
        for c in &checks {
            assert_eq!(c.outcome, CheckOutcome::Pass, "{} should pass: {:?}", c.name, c.outcome);
        }
        assert!(all_passed(&checks));
    }

    #[test]
    fn extended_fixture_passes_all_checks_and_survives_rebuild() {
        use crate::test_support::{open_seeded_extended, ANCHOR, GADGET, WIDGET};
        use crate::queries::*;

        let (mut conn, _hlc) = open_seeded_extended();

        let checks = run_all_checks(&conn).unwrap();
        assert!(all_passed(&checks), "checks failed: {:?}",
            checks.iter().filter(|c| c.outcome != CheckOutcome::Pass).collect::<Vec<_>>());

        let before_a = (
            units_sold_by_month(&conn, WIDGET, ANCHOR).unwrap(),
            gross_profit(&conn, ANCHOR).unwrap(),
            net_profit(&conn, ANCHOR).unwrap(),
            lot_ages(&conn, GADGET, ANCHOR).unwrap(),
            aging_buckets(&conn, ANCHOR).unwrap(),
            stock_on_hand(&conn).unwrap(),
            inventory_valuation(&conn).unwrap(),
            gross_margin_per_item(&conn).unwrap(),
        );
        let before_b = (
            sellers_by_units(&conn).unwrap(),
            party_balances(&conn).unwrap(),
            receivable_aging(&conn, ANCHOR).unwrap(),
            payable_aging(&conn, ANCHOR).unwrap(),
            return_rate_per_item(&conn).unwrap(),
            age_at_sale(&conn).unwrap(),
            profit_and_loss(&conn, "2026-01-01", "2026-12-31").unwrap(),
            balance_sheet(&conn).unwrap(),
        );

        crate::projectors::rebuild(&mut conn).unwrap();

        let after_a = (
            units_sold_by_month(&conn, WIDGET, ANCHOR).unwrap(),
            gross_profit(&conn, ANCHOR).unwrap(),
            net_profit(&conn, ANCHOR).unwrap(),
            lot_ages(&conn, GADGET, ANCHOR).unwrap(),
            aging_buckets(&conn, ANCHOR).unwrap(),
            stock_on_hand(&conn).unwrap(),
            inventory_valuation(&conn).unwrap(),
            gross_margin_per_item(&conn).unwrap(),
        );
        let after_b = (
            sellers_by_units(&conn).unwrap(),
            party_balances(&conn).unwrap(),
            receivable_aging(&conn, ANCHOR).unwrap(),
            payable_aging(&conn, ANCHOR).unwrap(),
            return_rate_per_item(&conn).unwrap(),
            age_at_sale(&conn).unwrap(),
            profit_and_loss(&conn, "2026-01-01", "2026-12-31").unwrap(),
            balance_sheet(&conn).unwrap(),
        );
        assert_eq!(before_a, after_a, "a §8 report drifted across rebuild — projector is non-deterministic");
        assert_eq!(before_b, after_b, "a §8 report drifted across rebuild — projector is non-deterministic");

        assert!(all_passed(&run_all_checks(&conn).unwrap()));
    }

    #[test]
    fn run_all_checks_flags_the_failing_one() {
        let (conn, _) = open_seeded();
        conn.execute("UPDATE inventory_lots SET qty_remaining = -1 WHERE id = 'pur_3#lot0'", []).unwrap();
        let checks = run_all_checks(&conn).unwrap();
        assert!(!all_passed(&checks));
        let failed: Vec<&str> = checks.iter()
            .filter(|c| c.outcome != CheckOutcome::Pass)
            .map(|c| c.name).collect();
        assert!(failed.contains(&"non_negative_inventory"), "got {failed:?}");
    }
}
