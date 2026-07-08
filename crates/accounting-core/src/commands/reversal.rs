use crate::commands::categories::{categories_of, EventCategory};
use crate::commands::guards::{check_lot_source_void, check_not_already_reversed,
    check_reversal_downstream, check_reversal_legal_target, check_reversal_lot_restore_reconsumed};
use crate::commands::{commit_event, reject, CommandContext, CommandError};
use rusqlite::OptionalExtension;
use serde_json::json;

fn load_target(conn: &rusqlite::Connection, target_event_id: &str)
    -> Result<(String, serde_json::Value), CommandError> {
    let row: Option<(String, String)> = conn.query_row(
        "SELECT type, json(payload) FROM events WHERE id = ?1",
        [target_event_id], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
    let (etype, payload_text) = row.ok_or_else(|| reject(format!("unknown target event: {target_event_id}")))?;
    let payload: serde_json::Value = serde_json::from_str(&payload_text)
        .map_err(|e| reject(format!("corrupt target payload: {e}")))?;
    Ok((etype, payload))
}

fn freeze_reversal_journal_lines(conn: &rusqlite::Connection, target_event_id: &str)
    -> Result<Vec<serde_json::Value>, CommandError> {
    let mut stmt = conn.prepare(
        "SELECT account_id, debit_minor, credit_minor
         FROM journal_lines WHERE event_id = ?1 ORDER BY id")?;
    let rows = stmt.query_map([target_event_id], |r| {
        Ok((r.get::<_,String>(0)?, r.get::<_,i64>(1)?, r.get::<_,i64>(2)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (account_id, debit, credit) = row?;
        out.push(json!({
            "accountId": account_id, "debitMinor": credit, "creditMinor": debit,
        }));
    }
    Ok(out)
}

pub fn handle_transaction_reversed(
    ctx: &mut CommandContext, target_event_id: &str, reason: &str,
) -> Result<crate::events::LedgerEvent, CommandError> {
    check_reversal_legal_target(ctx.conn, target_event_id)?;
    check_not_already_reversed(ctx.conn, target_event_id)?;

    let (etype, tpayload) = load_target(ctx.conn, target_event_id)?;
    let cats = categories_of(&etype);

    if cats.contains(&EventCategory::LotCreating) {
        check_lot_source_void(ctx.conn, target_event_id)?;
    }
    match etype.as_str() {
        "SaleRecorded" => {
            let sale_id = tpayload["saleId"].as_str().unwrap_or_default();
            check_reversal_downstream(ctx.conn, "sale", sale_id, None)?;
        }
        "PurchaseRecorded" => {
            let purchase_id = tpayload["purchaseId"].as_str().unwrap_or_default();
            check_reversal_downstream(ctx.conn, "purchase", purchase_id, None)?;
        }
        "PaymentMade" | "PaymentReceived" => {
            let payment_id = tpayload["paymentId"].as_str().unwrap_or_default();
            // Empty invoice_id: edges 1-2 (allocations/returns against an invoice)
            // are no-ops; only edge 3 (PaymentAllocated drew this payment's credit) evaluates.
            check_reversal_downstream(ctx.conn, "sale", "", Some(payment_id))?;
        }
        "SaleReturnRecorded" => {
            let return_id = tpayload["returnId"].as_str().unwrap_or_default();
            check_reversal_lot_restore_reconsumed(ctx.conn, return_id)?;
        }
        _ => {}
    }

    let reversal_lines = freeze_reversal_journal_lines(ctx.conn, target_event_id)?;

    let payload = json!({
        "targetEventId": target_event_id, "targetType": etype,
        "reason": reason, "reversalJournalLines": reversal_lines,
    });
    commit_event(ctx, "TransactionReversed", payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::tests::fixture;
    use crate::commands::purchase::{handle_purchase_recorded, PurchaseLineInput};
    use crate::commands::sale::{handle_sale_recorded, handle_sale_return_recorded, SaleLineInput, SaleReturnItemInput};
    use crate::commands::payment::{handle_payment_received, AllocInput};

    fn ctx<'a>(c: &'a mut rusqlite::Connection, h: &'a mut crate::hlc::Hlc) -> CommandContext<'a> {
        CommandContext { conn: c, hlc: h, physical_now: 1000, device_id: "deviceA".into(), user_id: "owner-1".into() }
    }
    fn seed(conn: &mut rusqlite::Connection, hlc: &mut crate::hlc::Hlc) {
        let mut c = ctx(conn, hlc);
        crate::commands::setup::handle_account_opened(&mut c, "bank", "Bank", "asset", "debit", Some("bank")).unwrap();
        crate::commands::setup::handle_account_opened(&mut c, "inv", "Inventory", "asset", "debit", Some("inventory")).unwrap();
        crate::commands::setup::handle_account_opened(&mut c, "ar", "AR", "asset", "debit", Some("accounts_receivable")).unwrap();
        crate::commands::setup::handle_account_opened(&mut c, "ap", "AP", "liability", "credit", Some("accounts_payable")).unwrap();
        crate::commands::setup::handle_account_opened(&mut c, "sales_acct", "Sales", "income", "credit", Some("sales")).unwrap();
        crate::commands::setup::handle_account_opened(&mut c, "cogs_acct", "COGS", "expense", "debit", Some("cogs")).unwrap();
        crate::commands::setup::handle_party_created(&mut c, "sup1", "Sup", "supplier").unwrap();
        crate::commands::setup::handle_party_created(&mut c, "cust1", "Cust", "customer").unwrap();
        crate::commands::setup::handle_item_defined(&mut c, "itemA", "A", "A", "ea").unwrap();
    }

    #[test]
    fn reverse_rejects_master_data_target() {
        let (mut conn, mut hlc) = fixture();
        let item_ev = {
            let mut c = ctx(&mut conn, &mut hlc);
            crate::commands::setup::handle_item_defined(&mut c, "itemA", "A", "A", "ea").unwrap()
        };
        let mut c = ctx(&mut conn, &mut hlc);
        let err = handle_transaction_reversed(&mut c, &item_ev.id, "oops").unwrap_err();
        assert!(matches!(err, CommandError::Validation(_)));
    }

    #[test]
    fn reverse_freezes_negated_journal_lines() {
        let (mut conn, mut hlc) = fixture();
        seed(&mut conn, &mut hlc);
        let pur_ev = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_purchase_recorded(&mut c, "pur1", "sup1", "2026-01-01", "cash",
                vec![PurchaseLineInput{ item_id:"itemA".into(), qty:10, unit_cost_minor:100 }]).unwrap()
        };
        let rev = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_transaction_reversed(&mut c, &pur_ev.id, "entered in error").expect("ok")
        };
        let lines = rev.payload["reversalJournalLines"].as_array().unwrap();
        assert!(!lines.is_empty());
        assert!(lines.iter().all(|l| l["accountId"].is_string()));
        assert!(lines.iter().all(|l| l.get("accountRole").is_none()));
        let inv_line = lines.iter().find(|l| l["accountId"] == "inv").unwrap();
        assert_eq!(inv_line["creditMinor"], 1000);
        let bank_line = lines.iter().find(|l| l["accountId"] == "bank").unwrap();
        assert_eq!(bank_line["debitMinor"], 1000);
        let dr: i64 = lines.iter().map(|l| l["debitMinor"].as_i64().unwrap()).sum();
        let cr: i64 = lines.iter().map(|l| l["creditMinor"].as_i64().unwrap()).sum();
        assert_eq!(dr, cr);
    }

    #[test]
    fn reverse_rejects_consumed_purchase_lot() {
        let (mut conn, mut hlc) = fixture();
        seed(&mut conn, &mut hlc);
        let pur_ev = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_purchase_recorded(&mut c, "pur1", "sup1", "2026-01-01", "cash",
                vec![PurchaseLineInput{ item_id:"itemA".into(), qty:10, unit_cost_minor:100 }]).unwrap()
        };
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_sale_recorded(&mut c, "sale1", "cust1", "2026-02-01", "cash",
                vec![SaleLineInput{ item_id:"itemA".into(), qty:3, unit_price_minor:200, lot_picks: None }]).unwrap();
        }
        let mut c = ctx(&mut conn, &mut hlc);
        let err = handle_transaction_reversed(&mut c, &pur_ev.id, "x").unwrap_err();
        assert!(matches!(err, CommandError::Validation(_)));
    }

    #[test]
    fn reverse_rejects_double_void() {
        let (mut conn, mut hlc) = fixture();
        seed(&mut conn, &mut hlc);
        let pur_ev = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_purchase_recorded(&mut c, "pur1", "sup1", "2026-01-01", "cash",
                vec![PurchaseLineInput{ item_id:"itemA".into(), qty:10, unit_cost_minor:100 }]).unwrap()
        };
        { let mut c = ctx(&mut conn, &mut hlc);
          handle_transaction_reversed(&mut c, &pur_ev.id, "first").expect("first ok"); }
        let mut c = ctx(&mut conn, &mut hlc);
        let err = handle_transaction_reversed(&mut c, &pur_ev.id, "second").unwrap_err();
        assert!(matches!(err, CommandError::Validation(_)));
    }

    #[test]
    fn reverse_rejects_sale_with_allocation() {
        let (mut conn, mut hlc) = fixture();
        seed(&mut conn, &mut hlc);
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_purchase_recorded(&mut c, "pur1", "sup1", "2026-01-01", "cash",
                vec![PurchaseLineInput{ item_id:"itemA".into(), qty:10, unit_cost_minor:100 }]).unwrap();
        }
        let sale_ev = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_sale_recorded(&mut c, "sale1", "cust1", "2026-02-01", "credit",
                vec![SaleLineInput{ item_id:"itemA".into(), qty:5, unit_price_minor:1000, lot_picks: None }]).unwrap()
        };
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_payment_received(&mut c, "pay1", "cust1", 5000, "2026-03-01",
                vec![AllocInput{ target_id:"sale1".into(), target_type:"sale".into(), amount_minor:5000 }]).unwrap();
        }
        let mut c = ctx(&mut conn, &mut hlc);
        let err = handle_transaction_reversed(&mut c, &sale_ev.id, "x").unwrap_err();
        assert!(matches!(err, CommandError::Validation(_)));
    }

    #[test]
    fn reverse_sale_blocked_by_return() {
        let (mut conn, mut hlc) = fixture();
        seed(&mut conn, &mut hlc);
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_purchase_recorded(&mut c, "pur1", "sup1", "2026-01-01", "cash",
                vec![PurchaseLineInput{ item_id:"itemA".into(), qty:10, unit_cost_minor:100 }]).unwrap();
        }
        let sale_ev = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_sale_recorded(&mut c, "sale1", "cust1", "2026-02-01", "credit",
                vec![SaleLineInput{ item_id:"itemA".into(), qty:5, unit_price_minor:1000, lot_picks: None }]).unwrap()
        };
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_sale_return_recorded(&mut c, "sret1", "sale1", "2026-03-01",
                vec![SaleReturnItemInput{ item_id:"itemA".into(), lot_returns: vec![("pur1#lot0".into(), 2)] }]).unwrap();
        }
        let mut c = ctx(&mut conn, &mut hlc);
        let err = handle_transaction_reversed(&mut c, &sale_ev.id, "x").unwrap_err();
        assert!(matches!(err, CommandError::Validation(_)));
    }

    #[test]
    fn reverse_return_blocked_when_restored_units_reconsumed() {
        let (mut conn, mut hlc) = fixture();
        seed(&mut conn, &mut hlc);
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_purchase_recorded(&mut c, "pur1", "sup1", "2026-01-01", "cash",
                vec![PurchaseLineInput{ item_id:"itemA".into(), qty:10, unit_cost_minor:100 }]).unwrap();
            handle_sale_recorded(&mut c, "sale1", "cust1", "2026-02-01", "cash",
                vec![SaleLineInput{ item_id:"itemA".into(), qty:6, unit_price_minor:200, lot_picks: None }]).unwrap();
        }
        let ret_ev = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_sale_return_recorded(&mut c, "sret1", "sale1", "2026-03-01",
                vec![SaleReturnItemInput{ item_id:"itemA".into(), lot_returns: vec![("pur1#lot0".into(), 6)] }]).unwrap()
        };
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_sale_recorded(&mut c, "sale2", "cust1", "2026-04-01", "cash",
                vec![SaleLineInput{ item_id:"itemA".into(), qty:8, unit_price_minor:200, lot_picks: None }]).unwrap();
        }
        let mut c = ctx(&mut conn, &mut hlc);
        let err = handle_transaction_reversed(&mut c, &ret_ev.id, "x").unwrap_err();
        assert!(matches!(err, CommandError::Validation(_)));
    }
}
