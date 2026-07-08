use crate::commands::guards::{check_amount_non_negative, check_amount_positive,
    check_at_least_one_line, check_expense_account_type, check_lot_item_match,
    check_qty_positive, check_self_transfer, LotDemand};
use crate::commands::{commit_event, reject, CommandContext, CommandError};
use rusqlite::OptionalExtension;
use serde_json::json;

pub struct AdjustLineInput { pub item_id: String, pub lot_id: String, pub qty_delta: i64, pub reason_code: String, pub expense_account_id: String }
pub struct FoundLineInput { pub item_id: String, pub qty: i64, pub unit_cost_minor: i64, pub acquired_at: String, pub income_account_id: String }

fn ensure_account(ctx: &CommandContext, account_id: &str) -> Result<(), CommandError> {
    let found: Option<String> = ctx.conn.query_row(
        "SELECT id FROM accounts WHERE id = ?1", [account_id], |r| r.get(0)).optional()?;
    if found.is_none() { Err(reject(format!("unknown account: {account_id}"))) } else { Ok(()) }
}

#[allow(clippy::too_many_arguments)]
pub fn handle_expense_recorded(
    ctx: &mut CommandContext, expense_id: &str, account_id: &str, amount_minor: i64,
    date: &str, terms: &str, supplier_id: Option<&str>, memo: Option<&str>,
) -> Result<crate::events::LedgerEvent, CommandError> {
    check_amount_positive(amount_minor)?;
    if !matches!(terms, "cash"|"credit") { return Err(reject(format!("invalid terms: {terms}"))); }
    check_expense_account_type(ctx.conn, account_id)?;
    match (terms, supplier_id) {
        ("credit", None) => return Err(reject("credit expense requires a supplierId")),
        ("credit", Some(sup)) => {
            let found: Option<String> = ctx.conn.query_row(
                "SELECT kind FROM parties WHERE id = ?1", [sup], |r| r.get(0)).optional()?;
            match found {
                None => return Err(reject(format!("unknown supplier: {sup}"))),
                Some(k) if k != "supplier" && k != "both" =>
                    return Err(reject(format!("party {sup} is not a supplier"))),
                _ => {}
            }
        }
        ("cash", Some(_)) => return Err(reject("cash expense must not carry a supplierId")),
        _ => {}
    }
    let mut p = json!({ "expenseId": expense_id, "accountId": account_id,
        "amountMinor": amount_minor, "date": date, "terms": terms });
    if let Some(s) = supplier_id { p["supplierId"] = json!(s); }
    if let Some(m) = memo { p["memo"] = json!(m); }
    commit_event(ctx, "ExpenseRecorded", p)
}

pub fn handle_transfer_recorded(
    ctx: &mut CommandContext, transfer_id: &str, from_account_id: &str, to_account_id: &str,
    amount_minor: i64, date: &str, memo: Option<&str>,
) -> Result<crate::events::LedgerEvent, CommandError> {
    check_amount_positive(amount_minor)?;
    check_self_transfer(from_account_id, to_account_id)?;
    ensure_account(ctx, from_account_id)?;
    ensure_account(ctx, to_account_id)?;
    let mut p = json!({ "transferId": transfer_id, "fromAccountId": from_account_id,
        "toAccountId": to_account_id, "amountMinor": amount_minor, "date": date });
    if let Some(m) = memo { p["memo"] = json!(m); }
    commit_event(ctx, "TransferRecorded", p)
}

pub fn handle_inventory_adjusted(
    ctx: &mut CommandContext, adjustment_id: &str, date: &str, lines: Vec<AdjustLineInput>,
) -> Result<crate::events::LedgerEvent, CommandError> {
    check_at_least_one_line(&lines)?;
    let mut demand = LotDemand::new();
    for l in &lines {
        if l.qty_delta >= 0 {
            return Err(reject(format!("InventoryAdjusted qtyDelta must be < 0, got {}", l.qty_delta)));
        }
        check_expense_account_type(ctx.conn, &l.expense_account_id)?;
        check_lot_item_match(ctx.conn, &l.lot_id, &l.item_id)?;
        demand.take(ctx.conn, &l.lot_id, -l.qty_delta)?;
    }
    let json_lines: Vec<_> = lines.iter().map(|l| json!({
        "itemId": l.item_id, "lotId": l.lot_id, "qtyDelta": l.qty_delta,
        "reasonCode": l.reason_code, "expenseAccountId": l.expense_account_id,
    })).collect();
    commit_event(ctx, "InventoryAdjusted",
        json!({ "adjustmentId": adjustment_id, "date": date, "lines": json_lines }))
}

pub fn handle_inventory_found(
    ctx: &mut CommandContext, found_id: &str, date: &str, lines: Vec<FoundLineInput>,
) -> Result<crate::events::LedgerEvent, CommandError> {
    check_at_least_one_line(&lines)?;
    for l in &lines {
        check_qty_positive(l.qty)?;
        check_amount_non_negative(l.unit_cost_minor)?;
        ensure_account(ctx, &l.income_account_id)?;
        let found: Option<String> = ctx.conn.query_row(
            "SELECT id FROM items WHERE id = ?1", [&l.item_id], |r| r.get(0)).optional()?;
        if found.is_none() { return Err(reject(format!("unknown item: {}", l.item_id))); }
    }
    let json_lines: Vec<_> = lines.iter().enumerate().map(|(i, l)| json!({
        "itemId": l.item_id, "lotId": format!("{found_id}#lot{i}"), "qty": l.qty,
        "unitCostMinor": l.unit_cost_minor, "acquiredAt": l.acquired_at,
        "incomeAccountId": l.income_account_id,
    })).collect();
    commit_event(ctx, "InventoryFound",
        json!({ "foundId": found_id, "date": date, "lines": json_lines }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::tests::fixture;
    use crate::commands::purchase::{handle_purchase_recorded, PurchaseLineInput};

    fn ctx<'a>(c: &'a mut rusqlite::Connection, h: &'a mut crate::hlc::Hlc) -> CommandContext<'a> {
        CommandContext { conn: c, hlc: h, physical_now: 1000, device_id: "deviceA".into(), user_id: "owner-1".into() }
    }
    fn seed_accounts_items(conn: &mut rusqlite::Connection, hlc: &mut crate::hlc::Hlc) {
        let mut c = ctx(conn, hlc);
        crate::commands::setup::handle_account_opened(&mut c, "bank", "Bank", "asset", "debit", Some("bank")).unwrap();
        crate::commands::setup::handle_account_opened(&mut c, "rent", "Rent", "expense", "debit", Some("rent")).unwrap();
        crate::commands::setup::handle_account_opened(&mut c, "shrink", "Shrinkage", "expense", "debit", Some("shrinkage")).unwrap();
        crate::commands::setup::handle_account_opened(&mut c, "invgain", "Gain", "income", "credit", Some("inventory_gain")).unwrap();
        crate::commands::setup::handle_account_opened(&mut c, "inv", "Inventory", "asset", "debit", Some("inventory")).unwrap();
        crate::commands::setup::handle_account_opened(&mut c, "ap", "A/P", "liability", "credit", Some("accounts_payable")).unwrap();
        crate::commands::setup::handle_party_created(&mut c, "sup1", "Sup", "supplier").unwrap();
        crate::commands::setup::handle_item_defined(&mut c, "itemA", "A", "A", "ea").unwrap();
        handle_purchase_recorded(&mut c, "pur1", "sup1", "2026-01-01", "cash",
            vec![PurchaseLineInput{ item_id:"itemA".into(), qty:10, unit_cost_minor:100 }]).unwrap();
    }

    #[test]
    fn expense_rejects_non_expense_account_and_zero_amount() {
        let (mut conn, mut hlc) = fixture();
        seed_accounts_items(&mut conn, &mut hlc);
        let mut c = ctx(&mut conn, &mut hlc);
        assert!(handle_expense_recorded(&mut c, "e1", "bank", 500, "2026-02-01", "cash", None, None).is_err());
        assert!(handle_expense_recorded(&mut c, "e2", "rent", 0, "2026-02-01", "cash", None, None).is_err());
        assert!(handle_expense_recorded(&mut c, "e3", "rent", 500, "2026-02-01", "cash", None, None).is_ok());
    }

    #[test]
    fn expense_credit_requires_supplier_and_cash_forbids_one() {
        let (mut conn, mut hlc) = fixture();
        seed_accounts_items(&mut conn, &mut hlc);
        let mut c = ctx(&mut conn, &mut hlc);
        assert!(handle_expense_recorded(&mut c, "e1", "rent", 500, "2026-02-01", "credit", None, None).is_err());
        assert!(handle_expense_recorded(&mut c, "e2", "rent", 500, "2026-02-01", "credit", Some("ghost"), None).is_err());
        assert!(handle_expense_recorded(&mut c, "e3", "rent", 500, "2026-02-01", "credit", Some("sup1"), None).is_ok());
        assert!(handle_expense_recorded(&mut c, "e4", "rent", 500, "2026-02-01", "cash", Some("sup1"), None).is_err());
    }

    #[test]
    fn transfer_rejects_self_and_zero() {
        let (mut conn, mut hlc) = fixture();
        seed_accounts_items(&mut conn, &mut hlc);
        let mut c = ctx(&mut conn, &mut hlc);
        assert!(handle_transfer_recorded(&mut c, "t1", "bank", "bank", 100, "2026-02-01", None).is_err());
        assert!(handle_transfer_recorded(&mut c, "t2", "bank", "rent", 0, "2026-02-01", None).is_err());
        assert!(handle_transfer_recorded(&mut c, "t3", "bank", "rent", 100, "2026-02-01", None).is_ok());
    }

    #[test]
    fn inventory_adjusted_requires_negative_delta_and_bounds() {
        let (mut conn, mut hlc) = fixture();
        seed_accounts_items(&mut conn, &mut hlc);
        {
            let mut c = ctx(&mut conn, &mut hlc);
            assert!(handle_inventory_adjusted(&mut c, "adj1", "2026-02-01",
                vec![AdjustLineInput{ item_id:"itemA".into(), lot_id:"pur1#lot0".into(), qty_delta:3, reason_code:"x".into(), expense_account_id:"shrink".into() }]).is_err());
        }
        {
            let mut c = ctx(&mut conn, &mut hlc);
            assert!(handle_inventory_adjusted(&mut c, "adj2", "2026-02-01",
                vec![AdjustLineInput{ item_id:"itemA".into(), lot_id:"pur1#lot0".into(), qty_delta:-15, reason_code:"x".into(), expense_account_id:"shrink".into() }]).is_err());
        }
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_inventory_adjusted(&mut c, "adj3", "2026-02-01",
                vec![AdjustLineInput{ item_id:"itemA".into(), lot_id:"pur1#lot0".into(), qty_delta:-4, reason_code:"damage".into(), expense_account_id:"shrink".into() }]).expect("ok");
        }
        let rem: i64 = conn.query_row("SELECT qty_remaining FROM inventory_lots WHERE id='pur1#lot0'", [], |r| r.get(0)).unwrap();
        assert_eq!(rem, 6);
    }

    #[test]
    fn inventory_found_creates_new_lot_with_frozen_id() {
        let (mut conn, mut hlc) = fixture();
        seed_accounts_items(&mut conn, &mut hlc);
        let ev = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_inventory_found(&mut c, "found1", "2026-02-01",
                vec![FoundLineInput{ item_id:"itemA".into(), qty:5, unit_cost_minor:120, acquired_at:"2026-02-01".into(), income_account_id:"invgain".into() }]).expect("ok")
        };
        assert_eq!(ev.payload["lines"][0]["lotId"], "found1#lot0");
        let (item, rem): (String, i64) = conn.query_row(
            "SELECT item_id, qty_remaining FROM inventory_lots WHERE id='found1#lot0'", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(item, "itemA");
        assert_eq!(rem, 5);
    }
}
