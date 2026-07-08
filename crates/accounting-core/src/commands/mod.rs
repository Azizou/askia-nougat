use crate::events::{append_event, LedgerEvent};
use crate::projectors::apply_event;
use rusqlite::Connection;

pub mod categories;
pub mod guards;
pub mod setup;
pub mod purchase;
pub mod sale;
pub mod payment;
pub mod movement;
pub mod reversal;

pub struct CommandContext<'a> {
    pub conn: &'a mut Connection,
    pub hlc: &'a mut crate::hlc::Hlc,
    pub physical_now: u64,
    pub device_id: String,
    pub user_id: String,
}

#[derive(Debug)]
pub enum CommandError {
    Validation(String),
    Db(rusqlite::Error),
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CommandError::Validation(m) => write!(f, "validation error: {m}"),
            CommandError::Db(e) => write!(f, "db error: {e}"),
        }
    }
}
impl std::error::Error for CommandError {}
impl From<rusqlite::Error> for CommandError {
    fn from(e: rusqlite::Error) -> Self { CommandError::Db(e) }
}

pub(crate) fn reject(msg: impl Into<String>) -> CommandError {
    CommandError::Validation(msg.into())
}

pub fn commit_event(
    ctx: &mut CommandContext,
    event_type: &str,
    payload: serde_json::Value,
) -> Result<LedgerEvent, CommandError> {
    let tx = ctx.conn.transaction()?;
    let ev = append_event(
        &tx, ctx.hlc, ctx.physical_now, &ctx.device_id, &ctx.user_id, event_type, &payload,
    )?;
    apply_event(&tx, &ev)?;
    tx.commit()?;
    Ok(ev)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory_with_schema;
    use crate::hlc::Hlc;
    use serde_json::json;

    pub(crate) fn fixture() -> (Connection, Hlc) {
        let conn = open_in_memory_with_schema().unwrap();
        let hlc = Hlc::new("deviceA");
        (conn, hlc)
    }

    #[test]
    fn commit_event_appends_and_projects_atomically() {
        let (mut conn, mut hlc) = fixture();
        let mut ctx = CommandContext {
            conn: &mut conn, hlc: &mut hlc, physical_now: 1000,
            device_id: "deviceA".into(), user_id: "owner-1".into(),
        };
        let ev = commit_event(&mut ctx, "ItemDefined",
            json!({"itemId": "i1", "sku": "SKU-1", "name": "Widget", "unit": "ea", "active": true}))
            .expect("commit");
        assert_eq!(ev.event_type, "ItemDefined");
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        let items: i64 = conn.query_row("SELECT COUNT(*) FROM items WHERE id='i1'", [], |r| r.get(0)).unwrap();
        assert_eq!(items, 1, "projection must be applied atomically with append");
    }

    #[test]
    fn commit_event_rolls_back_both_on_projection_failure() {
        let (mut conn, mut hlc) = fixture();
        let mut ctx = CommandContext {
            conn: &mut conn, hlc: &mut hlc, physical_now: 1000,
            device_id: "deviceA".into(), user_id: "owner-1".into(),
        };
        let res = commit_event(&mut ctx, "TotallyUnknownEventType", json!({}));
        assert!(res.is_err(), "unknown event must fail in projector");
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0, "append must roll back when projection fails");
    }
}

#[cfg(test)]
mod e2e {
    use super::*;
    use crate::db::open_in_memory_with_schema;
    use crate::hlc::Hlc;
    use crate::commands::setup::*;
    use crate::commands::purchase::*;
    use crate::commands::sale::*;
    use crate::commands::payment::*;
    use crate::commands::reversal::handle_transaction_reversed;
    use crate::projectors::rebuild;

    fn ctx<'a>(c: &'a mut rusqlite::Connection, h: &'a mut Hlc) -> CommandContext<'a> {
        CommandContext { conn: c, hlc: h, physical_now: 1000, device_id: "deviceA".into(), user_id: "owner-1".into() }
    }

    #[test]
    fn business_day_then_rebuild_is_identical() {
        let mut conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("deviceA");
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_account_opened(&mut c, "bank", "Bank", "asset", "debit", Some("bank")).unwrap();
            handle_account_opened(&mut c, "inv", "Inventory", "asset", "debit", Some("inventory")).unwrap();
            handle_account_opened(&mut c, "ar", "AR", "asset", "debit", Some("accounts_receivable")).unwrap();
            handle_account_opened(&mut c, "ap", "AP", "liability", "credit", Some("accounts_payable")).unwrap();
            handle_account_opened(&mut c, "sales_acct", "Sales", "income", "credit", Some("sales")).unwrap();
            handle_account_opened(&mut c, "cogs_acct", "COGS", "expense", "debit", Some("cogs")).unwrap();
            handle_party_created(&mut c, "sup1", "Sup", "supplier").unwrap();
            handle_party_created(&mut c, "cust1", "Cust", "customer").unwrap();
            handle_item_defined(&mut c, "itemA", "A", "A", "ea").unwrap();
            handle_purchase_recorded(&mut c, "pur1", "sup1", "2026-01-01", "credit",
                vec![PurchaseLineInput{ item_id:"itemA".into(), qty:10, unit_cost_minor:500 }]).unwrap();
            handle_sale_recorded(&mut c, "sale1", "cust1", "2026-02-01", "credit",
                vec![SaleLineInput{ item_id:"itemA".into(), qty:4, unit_price_minor:1000, lot_picks: None }]).unwrap();
            handle_payment_received(&mut c, "pay1", "cust1", 4000, "2026-03-01",
                vec![AllocInput{ target_id:"sale1".into(), target_type:"sale".into(), amount_minor:4000 }]).unwrap();
        }

        let snap = |conn: &rusqlite::Connection| -> (i64, i64, i64, String) {
            let lot_rem: i64 = conn.query_row("SELECT qty_remaining FROM inventory_lots WHERE id='pur1#lot0'", [], |r| r.get(0)).unwrap();
            let cogs: i64 = conn.query_row("SELECT cogs_minor FROM sale_lines WHERE sale_id='sale1'", [], |r| r.get(0)).unwrap();
            let out: i64 = conn.query_row("SELECT outstanding_minor FROM sales WHERE id='sale1'", [], |r| r.get(0)).unwrap();
            let lot_id: String = conn.query_row("SELECT lot_id FROM lot_consumptions LIMIT 1", [], |r| r.get(0)).unwrap();
            (lot_rem, cogs, out, lot_id)
        };
        let before = snap(&conn);
        assert_eq!(before, (6, 2000, 0, "pur1#lot0".to_string()));

        let unbalanced: i64 = conn.query_row(
            "SELECT COUNT(*) FROM (
               SELECT txn_id FROM journal_lines
               GROUP BY txn_id
               HAVING SUM(debit_minor) <> SUM(credit_minor))",
            [], |r| r.get(0)).unwrap();
        assert_eq!(unbalanced, 0, "every txn_id must have SUM(debit)=SUM(credit)");

        rebuild(&mut conn).unwrap();
        let after = snap(&conn);
        assert_eq!(before, after, "rebuild must reproduce identical projection state");
        let unbalanced_after: i64 = conn.query_row(
            "SELECT COUNT(*) FROM (
               SELECT txn_id FROM journal_lines
               GROUP BY txn_id
               HAVING SUM(debit_minor) <> SUM(credit_minor))",
            [], |r| r.get(0)).unwrap();
        assert_eq!(unbalanced_after, 0, "double-entry balance must survive rebuild");
    }

    #[test]
    fn rejected_command_leaves_log_untouched() {
        let mut conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("deviceA");
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_account_opened(&mut c, "bank", "Bank", "asset", "debit", Some("bank")).unwrap();
            handle_account_opened(&mut c, "inv", "Inventory", "asset", "debit", Some("inventory")).unwrap();
            handle_account_opened(&mut c, "ap", "AP", "liability", "credit", Some("accounts_payable")).unwrap();
            handle_party_created(&mut c, "sup1", "Sup", "supplier").unwrap();
            handle_item_defined(&mut c, "itemA", "A", "A", "ea").unwrap();
            handle_purchase_recorded(&mut c, "pur1", "sup1", "2026-01-01", "cash",
                vec![PurchaseLineInput{ item_id:"itemA".into(), qty:10, unit_cost_minor:500 }]).unwrap();
        }
        let count_before: i64 = conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();
        {
            let mut c = ctx(&mut conn, &mut hlc);
            let bad = handle_party_created(&mut c, "sup1", "dup", "supplier");
            assert!(bad.is_err());
        }
        let count_after: i64 = conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();
        assert_eq!(count_before, count_after, "rejected command must not append");
    }

    #[test]
    fn sale_return_roundtrip_restores_inventory_and_writes_return_lines() {
        let mut conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("deviceA");
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_account_opened(&mut c, "bank", "Bank", "asset", "debit", Some("bank")).unwrap();
            handle_account_opened(&mut c, "inv", "Inventory", "asset", "debit", Some("inventory")).unwrap();
            handle_account_opened(&mut c, "ar", "AR", "asset", "debit", Some("accounts_receivable")).unwrap();
            handle_account_opened(&mut c, "sales_acct", "Sales", "income", "credit", Some("sales")).unwrap();
            handle_account_opened(&mut c, "cogs_acct", "COGS", "expense", "debit", Some("cogs")).unwrap();
            handle_party_created(&mut c, "sup1", "Sup", "supplier").unwrap();
            handle_party_created(&mut c, "cust1", "Cust", "customer").unwrap();
            handle_item_defined(&mut c, "itemA", "A", "A", "ea").unwrap();
            handle_purchase_recorded(&mut c, "pur1", "sup1", "2026-01-01", "cash",
                vec![PurchaseLineInput{ item_id:"itemA".into(), qty:10, unit_cost_minor:500 }]).unwrap();
            handle_sale_recorded(&mut c, "sale1", "cust1", "2026-02-01", "cash",
                vec![SaleLineInput{ item_id:"itemA".into(), qty:6, unit_price_minor:1000, lot_picks: None }]).unwrap();
        }
        let before: i64 = conn.query_row("SELECT qty_remaining FROM inventory_lots WHERE id='pur1#lot0'", [], |r| r.get(0)).unwrap();
        assert_eq!(before, 4);
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_sale_return_recorded(&mut c, "sret1", "sale1", "2026-03-01",
                vec![SaleReturnItemInput{ item_id:"itemA".into(), lot_returns: vec![("pur1#lot0".into(), 3)] }]).unwrap();
        }
        let after: i64 = conn.query_row("SELECT qty_remaining FROM inventory_lots WHERE id='pur1#lot0'", [], |r| r.get(0)).unwrap();
        assert_eq!(after, 7, "return must restore 3 units — drift would leave at 4");
        let rl_count: i64 = conn.query_row("SELECT COUNT(*) FROM return_lines rl JOIN returns r ON r.id=rl.return_id WHERE r.id='sret1'", [], |r| r.get(0)).unwrap();
        assert_eq!(rl_count, 1);
        let cost_restored: i64 = conn.query_row("SELECT cost_restored_minor FROM returns WHERE id='sret1'", [], |r| r.get(0)).unwrap();
        assert_eq!(cost_restored, 1500);
    }

    #[test]
    fn transaction_reversed_roundtrip_flattens_the_journal() {
        let mut conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("deviceA");
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_account_opened(&mut c, "bank", "Bank", "asset", "debit", Some("bank")).unwrap();
            handle_account_opened(&mut c, "inv", "Inventory", "asset", "debit", Some("inventory")).unwrap();
            handle_account_opened(&mut c, "ap", "AP", "liability", "credit", Some("accounts_payable")).unwrap();
            handle_party_created(&mut c, "sup1", "Sup", "supplier").unwrap();
            handle_item_defined(&mut c, "itemA", "A", "A", "ea").unwrap();
        }
        let bal = |conn: &rusqlite::Connection, role: &str| -> i64 {
            conn.query_row(
                "SELECT COALESCE(SUM(jl.debit_minor - jl.credit_minor),0)
                 FROM journal_lines jl JOIN accounts a ON a.id = jl.account_id
                 WHERE a.system_role = ?1", [role], |r| r.get(0)).unwrap()
        };
        let inv_before = bal(&conn, "inventory");
        let pur_ev = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_purchase_recorded(&mut c, "pur1", "sup1", "2026-01-01", "cash",
                vec![PurchaseLineInput{ item_id:"itemA".into(), qty:10, unit_cost_minor:500 }]).unwrap()
        };
        assert_eq!(bal(&conn, "inventory"), inv_before + 5000);
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_transaction_reversed(&mut c, &pur_ev.id, "entered in error").unwrap();
        }
        assert_eq!(bal(&conn, "inventory"), inv_before,
            "reversal must net Inventory back — mis-keyed reversalJournalLines would leave +5000");
        let reversed: i64 = conn.query_row("SELECT reversed FROM purchases WHERE id='pur1'", [], |r| r.get(0)).unwrap();
        assert_eq!(reversed, 1, "projector must set reversed = 1");
    }

    #[test]
    fn payment_reversal_reopens_invoice_and_restores_party() {
        let mut conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("deviceA");
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_account_opened(&mut c, "bank", "Bank", "asset", "debit", Some("bank")).unwrap();
            handle_account_opened(&mut c, "inv", "Inventory", "asset", "debit", Some("inventory")).unwrap();
            handle_account_opened(&mut c, "ar", "AR", "asset", "debit", Some("accounts_receivable")).unwrap();
            handle_account_opened(&mut c, "sales_acct", "Sales", "income", "credit", Some("sales")).unwrap();
            handle_account_opened(&mut c, "cogs_acct", "COGS", "expense", "debit", Some("cogs")).unwrap();
            handle_party_created(&mut c, "sup1", "Sup", "supplier").unwrap();
            handle_party_created(&mut c, "cust1", "Cust", "customer").unwrap();
            handle_item_defined(&mut c, "itemA", "A", "A", "ea").unwrap();
            handle_purchase_recorded(&mut c, "pur1", "sup1", "2026-01-01", "cash",
                vec![PurchaseLineInput{ item_id:"itemA".into(), qty:10, unit_cost_minor:100 }]).unwrap();
            handle_sale_recorded(&mut c, "sale1", "cust1", "2026-02-01", "credit",
                vec![SaleLineInput{ item_id:"itemA".into(), qty:5, unit_price_minor:1000, lot_picks: None }]).unwrap();
        }
        // Payment settles the sale fully (5000).
        let pay_ev = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_payment_received(&mut c, "pay1", "cust1", 5000, "2026-03-01",
                vec![AllocInput{ target_id:"sale1".into(), target_type:"sale".into(), amount_minor:5000 }]).unwrap()
        };
        // Verify settled state.
        let out_before: i64 = conn.query_row("SELECT outstanding_minor FROM sales WHERE id='sale1'", [], |r| r.get(0)).unwrap();
        assert_eq!(out_before, 0);
        let recv_before: i64 = conn.query_row("SELECT receivable_minor FROM party_balances WHERE party_id='cust1'", [], |r| r.get(0)).unwrap();
        assert_eq!(recv_before, 0);
        // Reverse the payment.
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_transaction_reversed(&mut c, &pay_ev.id, "payment error").unwrap();
        }
        // Invoice must re-open, party receivable must restore.
        let out_after: i64 = conn.query_row("SELECT outstanding_minor FROM sales WHERE id='sale1'", [], |r| r.get(0)).unwrap();
        assert_eq!(out_after, 5000, "invoice must re-open after payment reversal");
        let recv_after: i64 = conn.query_row("SELECT receivable_minor FROM party_balances WHERE party_id='cust1'", [], |r| r.get(0)).unwrap();
        assert_eq!(recv_after, 5000, "receivable must restore");
        // Payment row must be deleted.
        let pay_count: i64 = conn.query_row("SELECT COUNT(*) FROM payments WHERE id='pay1'", [], |r| r.get(0)).unwrap();
        assert_eq!(pay_count, 0, "payments row must be deleted by reversal clause 3");
    }
}
