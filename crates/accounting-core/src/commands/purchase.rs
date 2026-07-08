use crate::commands::guards::{check_amount_non_negative, check_at_least_one_line,
    check_lot_item_match, check_qty_positive, LotDemand};
use crate::commands::{commit_event, reject, CommandContext, CommandError};
use serde_json::json;

pub struct PurchaseLineInput { pub item_id: String, pub qty: i64, pub unit_cost_minor: i64 }

fn ensure_party(ctx: &CommandContext, party_id: &str, want_kind_in: &[&str]) -> Result<(), CommandError> {
    let kind: Option<String> = {
        use rusqlite::OptionalExtension;
        ctx.conn.query_row("SELECT kind FROM parties WHERE id = ?1", [party_id], |r| r.get(0)).optional()?
    };
    match kind {
        None => Err(reject(format!("unknown party: {party_id}"))),
        Some(k) if k != "both" && !want_kind_in.contains(&k.as_str()) =>
            Err(reject(format!("party {party_id} kind '{k}' not in {want_kind_in:?}"))),
        Some(_) => Ok(()),
    }
}

fn ensure_item(ctx: &CommandContext, item_id: &str) -> Result<(), CommandError> {
    use rusqlite::OptionalExtension;
    let found: Option<String> = ctx.conn.query_row(
        "SELECT id FROM items WHERE id = ?1", [item_id], |r| r.get(0)).optional()?;
    if found.is_none() { Err(reject(format!("unknown item: {item_id}"))) } else { Ok(()) }
}

pub fn handle_purchase_recorded(
    ctx: &mut CommandContext,
    purchase_id: &str,
    supplier_id: &str,
    date: &str,
    terms: &str,
    lines: Vec<PurchaseLineInput>,
) -> Result<crate::events::LedgerEvent, CommandError> {
    check_at_least_one_line(&lines)?;
    if !matches!(terms, "cash"|"credit") { return Err(reject(format!("invalid terms: {terms}"))); }
    ensure_party(ctx, supplier_id, &["supplier"])?;
    for l in &lines {
        check_qty_positive(l.qty)?;
        check_amount_non_negative(l.unit_cost_minor)?;
        ensure_item(ctx, &l.item_id)?;
    }
    let mut json_lines = Vec::with_capacity(lines.len());
    let mut total_minor: i64 = 0;
    for (i, l) in lines.iter().enumerate() {
        total_minor += l.qty * l.unit_cost_minor;
        json_lines.push(json!({
            "itemId": l.item_id, "qty": l.qty, "unitCostMinor": l.unit_cost_minor,
            "lotId": format!("{purchase_id}#lot{i}"),
        }));
    }
    let payload = json!({
        "purchaseId": purchase_id, "supplierId": supplier_id, "date": date,
        "terms": terms, "totalMinor": total_minor, "lines": json_lines,
    });
    commit_event(ctx, "PurchaseRecorded", payload)
}

pub struct PurchaseReturnLineInput {
    pub item_id: String, pub lot_id: String, pub qty: i64, pub unit_cost_minor: i64,
}

fn ensure_purchase_exists(ctx: &CommandContext, purchase_id: &str) -> Result<(), CommandError> {
    use rusqlite::OptionalExtension;
    let found: Option<String> = ctx.conn.query_row(
        "SELECT id FROM purchases WHERE id = ?1", [purchase_id], |r| r.get(0)).optional()?;
    if found.is_none() { Err(reject(format!("unknown purchase: {purchase_id}"))) } else { Ok(()) }
}

pub fn handle_purchase_return_recorded(
    ctx: &mut CommandContext,
    return_id: &str,
    original_purchase_id: &str,
    date: &str,
    lines: Vec<PurchaseReturnLineInput>,
) -> Result<crate::events::LedgerEvent, CommandError> {
    check_at_least_one_line(&lines)?;
    ensure_purchase_exists(ctx, original_purchase_id)?;
    crate::commands::guards::check_invoice_not_reversed(ctx.conn, "purchases", original_purchase_id)?;
    let mut demand = LotDemand::new();
    for l in &lines {
        check_qty_positive(l.qty)?;
        check_amount_non_negative(l.unit_cost_minor)?;
        check_lot_item_match(ctx.conn, &l.lot_id, &l.item_id)?;
        demand.take(ctx.conn, &l.lot_id, l.qty)?;
    }
    let json_lines: Vec<_> = lines.iter().map(|l| json!({
        "itemId": l.item_id, "lotId": l.lot_id, "qty": l.qty, "unitCostMinor": l.unit_cost_minor,
    })).collect();
    let payload = json!({
        "returnId": return_id, "originalPurchaseId": original_purchase_id,
        "date": date, "lines": json_lines,
    });
    commit_event(ctx, "PurchaseReturnRecorded", payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::tests::fixture;

    fn ctx<'a>(c: &'a mut rusqlite::Connection, h: &'a mut crate::hlc::Hlc) -> CommandContext<'a> {
        CommandContext { conn: c, hlc: h, physical_now: 1000, device_id: "deviceA".into(), user_id: "owner-1".into() }
    }
    fn seed_master(conn: &mut rusqlite::Connection, hlc: &mut crate::hlc::Hlc) {
        crate::genesis::run_genesis(conn, hlc, 1000, "deviceA", "owner-1", "Owner").unwrap();
        crate::projectors::rebuild(conn).unwrap();
        let mut c = ctx(conn, hlc);
        crate::commands::setup::handle_party_created(&mut c, "sup1", "Supplier", "supplier").unwrap();
        crate::commands::setup::handle_item_defined(&mut c, "itemA", "SKU-A", "A", "ea").unwrap();
    }

    #[test]
    fn purchase_freezes_deterministic_lot_id_and_creates_lot() {
        let (mut conn, mut hlc) = fixture();
        seed_master(&mut conn, &mut hlc);
        let lines = vec![PurchaseLineInput { item_id: "itemA".into(), qty: 10, unit_cost_minor: 500 }];
        let ev = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_purchase_recorded(&mut c, "pur1", "sup1", "2026-02-01", "credit", lines).expect("ok")
        };
        assert_eq!(ev.payload["lines"][0]["lotId"], "pur1#lot0");
        let (item, rem): (String, i64) = conn.query_row(
            "SELECT item_id, qty_remaining FROM inventory_lots WHERE id='pur1#lot0'",
            [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(item, "itemA");
        assert_eq!(rem, 10);
    }

    #[test]
    fn purchase_rejects_unknown_supplier_and_writes_nothing() {
        let (mut conn, mut hlc) = fixture();
        seed_master(&mut conn, &mut hlc);
        let lines = vec![PurchaseLineInput { item_id: "itemA".into(), qty: 1, unit_cost_minor: 1 }];
        let err = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_purchase_recorded(&mut c, "pur1", "ghost", "2026-02-01", "credit", lines).unwrap_err()
        };
        assert!(matches!(err, CommandError::Validation(_)));
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM events WHERE type='PurchaseRecorded'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn purchase_rejects_zero_qty() {
        let (mut conn, mut hlc) = fixture();
        seed_master(&mut conn, &mut hlc);
        let lines = vec![PurchaseLineInput { item_id: "itemA".into(), qty: 0, unit_cost_minor: 500 }];
        let mut c = ctx(&mut conn, &mut hlc);
        assert!(handle_purchase_recorded(&mut c, "pur1", "sup1", "2026-02-01", "credit", lines).is_err());
    }

    #[test]
    fn purchase_return_over_lot_remaining_rejected() {
        let (mut conn, mut hlc) = fixture();
        seed_master(&mut conn, &mut hlc);
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_purchase_recorded(&mut c, "pur1", "sup1", "2026-02-01", "credit",
                vec![PurchaseLineInput{ item_id:"itemA".into(), qty:10, unit_cost_minor:500 }]).unwrap();
        }
        let err = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_purchase_return_recorded(&mut c, "pret1", "pur1", "2026-03-01",
                vec![PurchaseReturnLineInput{ item_id:"itemA".into(), lot_id:"pur1#lot0".into(), qty:15, unit_cost_minor:500 }]).unwrap_err()
        };
        assert!(matches!(err, CommandError::Validation(_)));
        let rem: i64 = conn.query_row("SELECT qty_remaining FROM inventory_lots WHERE id='pur1#lot0'", [], |r| r.get(0)).unwrap();
        assert_eq!(rem, 10);
    }

    #[test]
    fn purchase_return_valid_draws_down_lot() {
        let (mut conn, mut hlc) = fixture();
        seed_master(&mut conn, &mut hlc);
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_purchase_recorded(&mut c, "pur1", "sup1", "2026-02-01", "credit",
                vec![PurchaseLineInput{ item_id:"itemA".into(), qty:10, unit_cost_minor:500 }]).unwrap();
        }
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_purchase_return_recorded(&mut c, "pret1", "pur1", "2026-03-01",
                vec![PurchaseReturnLineInput{ item_id:"itemA".into(), lot_id:"pur1#lot0".into(), qty:4, unit_cost_minor:500 }]).expect("ok");
        }
        let rem: i64 = conn.query_row("SELECT qty_remaining FROM inventory_lots WHERE id='pur1#lot0'", [], |r| r.get(0)).unwrap();
        assert_eq!(rem, 6);
    }

    #[test]
    fn purchase_return_lot_item_mismatch_rejected() {
        let (mut conn, mut hlc) = fixture();
        seed_master(&mut conn, &mut hlc);
        {
            let mut c = ctx(&mut conn, &mut hlc);
            crate::commands::setup::handle_item_defined(&mut c, "itemB", "SKU-B", "B", "ea").unwrap();
            handle_purchase_recorded(&mut c, "pur1", "sup1", "2026-02-01", "credit",
                vec![PurchaseLineInput{ item_id:"itemA".into(), qty:10, unit_cost_minor:500 }]).unwrap();
        }
        let mut c = ctx(&mut conn, &mut hlc);
        let err = handle_purchase_return_recorded(&mut c, "pret1", "pur1", "2026-03-01",
            vec![PurchaseReturnLineInput{ item_id:"itemB".into(), lot_id:"pur1#lot0".into(), qty:1, unit_cost_minor:500 }]).unwrap_err();
        assert!(matches!(err, CommandError::Validation(_)));
    }
}
