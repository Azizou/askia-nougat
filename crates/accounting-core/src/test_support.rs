#![cfg(test)]

use crate::commands::CommandContext;
use crate::commands::setup::{handle_item_defined, handle_party_created};
use crate::commands::purchase::{handle_purchase_recorded, PurchaseLineInput};
use crate::commands::sale::{handle_sale_recorded, handle_sale_return_recorded, SaleLineInput, SaleReturnItemInput};
use crate::commands::payment::{handle_payment_received, AllocInput};
use crate::commands::movement::handle_expense_recorded;
use crate::commands::reversal::handle_transaction_reversed;
use crate::db::open_in_memory_with_schema;
use crate::genesis::run_genesis;
use crate::hlc::Hlc;
use crate::projectors::rebuild;
use rusqlite::Connection;

pub const ANCHOR: &str = "2026-07-06";
pub const WIDGET: &str = "item_widget";
pub const GADGET: &str = "item_gadget";
pub const CUST: &str = "cust_acme";
pub const SUPP: &str = "supp_globex";
pub const LOT_W1: &str = "pur_1#lot0";
pub const LOT_W2: &str = "pur_2#lot0";
pub const LOT_G1: &str = "pur_3#lot0";
pub const SALE_1: &str = "sale_1";
pub const SALE_2: &str = "sale_2";
pub const PUR_1: &str = "pur_1";
pub const CUST_BETA: &str = "cust_beta";

pub fn open_seeded() -> (Connection, Hlc) {
    let mut conn = open_in_memory_with_schema().unwrap();
    let mut hlc = Hlc::new("deviceA");
    seed_reference_business(&mut conn, &mut hlc);
    (conn, hlc)
}

pub fn seed_reference_business(conn: &mut Connection, hlc: &mut Hlc) {
    run_genesis(conn, hlc, 1000, "deviceA", "owner-1", "Jane Owner").unwrap();
    rebuild(conn).unwrap();

    let mut ctx = CommandContext {
        conn, hlc, physical_now: 1000,
        device_id: "deviceA".into(), user_id: "owner-1".into(),
    };

    handle_item_defined(&mut ctx, WIDGET, "W-1", "Widget", "ea").unwrap();
    handle_item_defined(&mut ctx, GADGET, "G-1", "Gadget", "ea").unwrap();
    handle_party_created(&mut ctx, CUST, "Acme Co", "customer").unwrap();
    handle_party_created(&mut ctx, SUPP, "Globex", "supplier").unwrap();

    handle_purchase_recorded(&mut ctx, PUR_1, SUPP, "2026-06-01", "credit",
        vec![PurchaseLineInput { item_id: WIDGET.into(), qty: 100, unit_cost_minor: 500 }]).unwrap();
    handle_purchase_recorded(&mut ctx, "pur_2", SUPP, "2026-06-15", "credit",
        vec![PurchaseLineInput { item_id: WIDGET.into(), qty: 50, unit_cost_minor: 600 }]).unwrap();
    handle_purchase_recorded(&mut ctx, "pur_3", SUPP, "2026-06-20", "credit",
        vec![PurchaseLineInput { item_id: GADGET.into(), qty: 20, unit_cost_minor: 1000 }]).unwrap();

    handle_sale_recorded(&mut ctx, SALE_1, CUST, "2026-07-02", "credit",
        vec![SaleLineInput { item_id: WIDGET.into(), qty: 60, unit_price_minor: 900, lot_picks: None }]).unwrap();
    handle_sale_recorded(&mut ctx, SALE_2, CUST, "2026-07-03", "cash",
        vec![SaleLineInput { item_id: GADGET.into(), qty: 5, unit_price_minor: 1500, lot_picks: None }]).unwrap();

    handle_expense_recorded(&mut ctx, "exp_1", "acct_rent", 3000, "2026-07-04", "cash",
        None, Some("July rent")).unwrap();

    handle_payment_received(&mut ctx, "pay_1", CUST, 40000, "2026-07-05",
        vec![AllocInput { target_id: SALE_1.into(), target_type: "sale".into(), amount_minor: 40000 }]).unwrap();

    handle_sale_return_recorded(&mut ctx, "ret_1", SALE_1, "2026-07-06",
        vec![SaleReturnItemInput { item_id: WIDGET.into(), lot_returns: vec![(LOT_W1.into(), 10)] }]).unwrap();
}

/// A richer business exercising: a CASH purchase, a pure PREPAYMENT (unallocated
/// credit), a SECOND customer, and a REVERSED sale. Fresh DB.
pub fn open_seeded_extended() -> (Connection, Hlc) {
    let mut conn = open_in_memory_with_schema().unwrap();
    let mut hlc = Hlc::new("deviceA");
    run_genesis(&conn, &mut hlc, 1000, "deviceA", "owner-1", "Jane Owner").unwrap();
    rebuild(&mut conn).unwrap();

    let reversible_id: String;
    {
        let mut ctx = CommandContext {
            conn: &mut conn, hlc: &mut hlc, physical_now: 1000,
            device_id: "deviceA".into(), user_id: "owner-1".into(),
        };

        handle_item_defined(&mut ctx, WIDGET, "W-1", "Widget", "ea").unwrap();
        handle_item_defined(&mut ctx, GADGET, "G-1", "Gadget", "ea").unwrap();
        handle_party_created(&mut ctx, CUST, "Acme Co", "customer").unwrap();
        handle_party_created(&mut ctx, CUST_BETA, "Beta LLC", "customer").unwrap();
        handle_party_created(&mut ctx, SUPP, "Globex", "supplier").unwrap();

        handle_purchase_recorded(&mut ctx, "epur_1", SUPP, "2026-06-01", "credit",
            vec![PurchaseLineInput { item_id: WIDGET.into(), qty: 50, unit_cost_minor: 500 }]).unwrap();
        handle_purchase_recorded(&mut ctx, "epur_2", SUPP, "2026-06-05", "cash",
            vec![PurchaseLineInput { item_id: GADGET.into(), qty: 10, unit_cost_minor: 800 }]).unwrap();

        handle_sale_recorded(&mut ctx, "esale_1", CUST, "2026-07-02", "credit",
            vec![SaleLineInput { item_id: WIDGET.into(), qty: 20, unit_price_minor: 900, lot_picks: None }]).unwrap();

        handle_payment_received(&mut ctx, "epay_1", CUST_BETA, 5000, "2026-07-04", vec![]).unwrap();

        let reversible = handle_sale_recorded(&mut ctx, "esale_2", CUST, "2026-07-05", "cash",
            vec![SaleLineInput { item_id: GADGET.into(), qty: 5, unit_price_minor: 1500, lot_picks: None }]).unwrap();
        reversible_id = reversible.id.clone();
        handle_transaction_reversed(&mut ctx, &reversible_id, "entered in error").unwrap();
    }

    (conn, hlc)
}
