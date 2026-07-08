use crate::commands::guards::{check_amount_non_negative, check_at_least_one_line,
    check_lot_item_match, check_qty_positive, check_sale_return_over_restore,
    check_invoice_not_reversed, LotDemand};
use crate::commands::{commit_event, reject, CommandContext, CommandError};
use rusqlite::OptionalExtension;
use serde_json::json;

pub struct SaleLineInput {
    pub item_id: String,
    pub qty: i64,
    pub unit_price_minor: i64,
    pub lot_picks: Option<Vec<(String, i64)>>,
}

fn select_oldest_first(conn: &rusqlite::Connection, demand: &LotDemand, item_id: &str, qty: i64)
    -> Result<Vec<(String, i64, i64)>, CommandError> {
    let mut stmt = conn.prepare(
        "SELECT id, unit_cost_minor FROM inventory_lots
         WHERE item_id = ?1 AND qty_remaining > 0
         ORDER BY acquired_at ASC, id ASC",
    )?;
    let rows = stmt.query_map([item_id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    })?;
    let mut need = qty;
    let mut picks = Vec::new();
    for row in rows {
        let (lot_id, cost) = row?;
        if need == 0 { break; }
        let usable = demand.available(conn, &lot_id)?;
        if usable <= 0 { continue; }
        let take = need.min(usable);
        picks.push((lot_id, take, cost));
        need -= take;
    }
    if need > 0 {
        return Err(reject(format!("insufficient stock for item {item_id}: short by {need}")));
    }
    Ok(picks)
}

fn lot_cost(conn: &rusqlite::Connection, lot_id: &str) -> Result<i64, CommandError> {
    conn.query_row("SELECT unit_cost_minor FROM inventory_lots WHERE id = ?1", [lot_id], |r| r.get(0))
        .optional()?
        .ok_or_else(|| reject(format!("unknown lot: {lot_id}")))
}

pub fn handle_sale_recorded(
    ctx: &mut CommandContext,
    sale_id: &str,
    customer_id: &str,
    date: &str,
    terms: &str,
    lines: Vec<SaleLineInput>,
) -> Result<crate::events::LedgerEvent, CommandError> {
    check_at_least_one_line(&lines)?;
    if !matches!(terms, "cash"|"credit") { return Err(reject(format!("invalid terms: {terms}"))); }
    {
        let kind: Option<String> = ctx.conn.query_row(
            "SELECT kind FROM parties WHERE id = ?1", [customer_id], |r| r.get(0)).optional()?;
        match kind {
            None => return Err(reject(format!("unknown party: {customer_id}"))),
            Some(k) if k != "customer" && k != "both" =>
                return Err(reject(format!("party {customer_id} is not a customer"))),
            _ => {}
        }
    }

    let mut demand = LotDemand::new();
    let mut json_lines = Vec::with_capacity(lines.len());
    let mut sale_total: i64 = 0;
    for l in &lines {
        check_qty_positive(l.qty)?;
        check_amount_non_negative(l.unit_price_minor)?;

        let picks: Vec<(String, i64, i64)> = match &l.lot_picks {
            Some(user_picks) => {
                let sum: i64 = user_picks.iter().map(|(_, q)| *q).sum();
                if sum != l.qty {
                    return Err(reject(format!(
                        "lot picks for item {} sum to {sum}, expected qty {}", l.item_id, l.qty)));
                }
                let mut out = Vec::with_capacity(user_picks.len());
                for (lot_id, take) in user_picks {
                    check_qty_positive(*take)?;
                    out.push((lot_id.clone(), *take, lot_cost(ctx.conn, lot_id)?));
                }
                out
            }
            None => select_oldest_first(ctx.conn, &demand, &l.item_id, l.qty)?,
        };

        for (lot_id, take, _cost) in &picks {
            check_lot_item_match(ctx.conn, lot_id, &l.item_id)?;
            demand.take(ctx.conn, lot_id, *take)?;
        }

        let revenue_minor = l.qty * l.unit_price_minor;
        let cogs_minor: i64 = picks.iter().map(|(_, q, c)| q * c).sum();
        sale_total += revenue_minor;
        let consumption: Vec<_> = picks.iter().map(|(lot_id, q, c)| json!({
            "lotId": lot_id, "qtyTaken": q, "unitCostMinor": c,
        })).collect();
        json_lines.push(json!({
            "itemId": l.item_id, "qty": l.qty, "unitPriceMinor": l.unit_price_minor,
            "revenueMinor": revenue_minor, "cogsMinor": cogs_minor,
            "lotConsumption": consumption,
        }));
    }

    let payload = json!({
        "saleId": sale_id, "customerId": customer_id, "date": date, "terms": terms,
        "totalMinor": sale_total, "lines": json_lines,
    });
    commit_event(ctx, "SaleRecorded", payload)
}

pub struct SaleReturnItemInput { pub item_id: String, pub lot_returns: Vec<(String, i64)> }

fn original_sale_line_price(conn: &rusqlite::Connection, sale_id: &str, lot_id: &str)
    -> Result<i64, CommandError> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT sl.unit_price_minor
         FROM lot_consumptions lc
         JOIN sale_lines sl ON sl.id = lc.sale_line_id
         WHERE sl.sale_id = ?1 AND lc.lot_id = ?2",
    )?;
    let prices: Vec<i64> = stmt
        .query_map(rusqlite::params![sale_id, lot_id], |r| r.get::<_, i64>(0))?
        .collect::<Result<_, _>>()?;
    match prices.as_slice() {
        [] => Err(reject(format!("sale {sale_id} has no line consuming lot {lot_id}"))),
        [p] => Ok(*p),
        _ => Err(reject(format!(
            "sale {sale_id} filled lot {lot_id} at multiple prices {prices:?}; return each price line separately"))),
    }
}

fn lot_cost_for_return(conn: &rusqlite::Connection, lot_id: &str) -> Result<i64, CommandError> {
    conn.query_row("SELECT unit_cost_minor FROM inventory_lots WHERE id = ?1", [lot_id], |r| r.get(0))
        .optional()?
        .ok_or_else(|| reject(format!("unknown lot: {lot_id}")))
}

pub fn handle_sale_return_recorded(
    ctx: &mut CommandContext,
    return_id: &str,
    original_sale_id: &str,
    date: &str,
    lines: Vec<SaleReturnItemInput>,
) -> Result<crate::events::LedgerEvent, CommandError> {
    check_at_least_one_line(&lines)?;
    check_invoice_not_reversed(ctx.conn, "sales", original_sale_id)?;
    let mut demand = LotDemand::new();
    let mut json_lines = Vec::with_capacity(lines.len());
    for item in &lines {
        if item.lot_returns.is_empty() {
            return Err(reject(format!("return line for item {} has no lotReturns", item.item_id)));
        }
        let mut line_qty: i64 = 0;
        let mut unit_price: Option<i64> = None;
        let mut lot_returns_json = Vec::with_capacity(item.lot_returns.len());
        for (lot_id, qty_returned) in &item.lot_returns {
            check_qty_positive(*qty_returned)?;
            check_lot_item_match(ctx.conn, lot_id, &item.item_id)?;
            check_sale_return_over_restore(ctx.conn, original_sale_id, lot_id, *qty_returned)?;
            demand.restore(ctx.conn, lot_id, *qty_returned)?;
            let price = original_sale_line_price(ctx.conn, original_sale_id, lot_id)?;
            match unit_price {
                None => unit_price = Some(price),
                Some(p) if p != price =>
                    return Err(reject(format!(
                        "item {} returned across lots at differing prices ({p} vs {price}); split into separate lines",
                        item.item_id))),
                _ => {}
            }
            let unit_cost = lot_cost_for_return(ctx.conn, lot_id)?;
            line_qty += *qty_returned;
            lot_returns_json.push(json!({
                "lotId": lot_id, "qtyReturned": qty_returned, "unitCostMinor": unit_cost,
            }));
        }
        json_lines.push(json!({
            "itemId": item.item_id, "qty": line_qty,
            "unitPriceMinor": unit_price.expect("non-empty lotReturns"),
            "lotReturns": lot_returns_json,
        }));
    }
    let payload = json!({
        "returnId": return_id, "originalSaleId": original_sale_id, "date": date,
        "lines": json_lines,
    });
    commit_event(ctx, "SaleReturnRecorded", payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::tests::fixture;
    use crate::commands::purchase::{handle_purchase_recorded, PurchaseLineInput};

    fn ctx<'a>(c: &'a mut rusqlite::Connection, h: &'a mut crate::hlc::Hlc) -> CommandContext<'a> {
        CommandContext { conn: c, hlc: h, physical_now: 1000, device_id: "deviceA".into(), user_id: "owner-1".into() }
    }
    fn seed(conn: &mut rusqlite::Connection, hlc: &mut crate::hlc::Hlc) {
        crate::genesis::run_genesis(conn, hlc, 1000, "deviceA", "owner-1", "Owner").unwrap();
        crate::projectors::rebuild(conn).unwrap();
        let mut c = ctx(conn, hlc);
        crate::commands::setup::handle_party_created(&mut c, "cust1", "Cust", "customer").unwrap();
        crate::commands::setup::handle_party_created(&mut c, "sup1", "Sup", "supplier").unwrap();
        crate::commands::setup::handle_item_defined(&mut c, "itemA", "SKU-A", "A", "ea").unwrap();
        handle_purchase_recorded(&mut c, "purOld", "sup1", "2026-01-01", "cash",
            vec![PurchaseLineInput{ item_id: "itemA".into(), qty: 10, unit_cost_minor: 500 }]).unwrap();
        handle_purchase_recorded(&mut c, "purNew", "sup1", "2026-02-01", "cash",
            vec![PurchaseLineInput{ item_id: "itemA".into(), qty: 10, unit_cost_minor: 700 }]).unwrap();
    }

    #[test]
    fn sale_defaults_oldest_lot_first_and_freezes_cogs() {
        let (mut conn, mut hlc) = fixture();
        seed(&mut conn, &mut hlc);
        let ev = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_sale_recorded(&mut c, "sale1", "cust1", "2026-03-01", "cash",
                vec![SaleLineInput{ item_id:"itemA".into(), qty:12, unit_price_minor:1000, lot_picks: None }]).expect("ok")
        };
        assert_eq!(ev.payload["lines"][0]["revenueMinor"], 12000);
        assert_eq!(ev.payload["lines"][0]["cogsMinor"], 6400);
        let cons = ev.payload["lines"][0]["lotConsumption"].as_array().unwrap();
        assert_eq!(cons[0]["lotId"], "purOld#lot0");
        assert_eq!(cons[0]["qtyTaken"], 10);
        assert_eq!(cons[0]["unitCostMinor"], 500);
        assert_eq!(cons[1]["lotId"], "purNew#lot0");
        assert_eq!(cons[1]["qtyTaken"], 2);
        let old_rem: i64 = conn.query_row("SELECT qty_remaining FROM inventory_lots WHERE id='purOld#lot0'", [], |r| r.get(0)).unwrap();
        let new_rem: i64 = conn.query_row("SELECT qty_remaining FROM inventory_lots WHERE id='purNew#lot0'", [], |r| r.get(0)).unwrap();
        assert_eq!(old_rem, 0);
        assert_eq!(new_rem, 8);
    }

    #[test]
    fn sale_oversell_across_all_lots_rejected_and_writes_nothing() {
        let (mut conn, mut hlc) = fixture();
        seed(&mut conn, &mut hlc);
        let err = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_sale_recorded(&mut c, "sale1", "cust1", "2026-03-01", "cash",
                vec![SaleLineInput{ item_id:"itemA".into(), qty:25, unit_price_minor:1000, lot_picks: None }]).unwrap_err()
        };
        assert!(matches!(err, CommandError::Validation(_)));
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM events WHERE type='SaleRecorded'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
        let old_rem: i64 = conn.query_row("SELECT qty_remaining FROM inventory_lots WHERE id='purOld#lot0'", [], |r| r.get(0)).unwrap();
        assert_eq!(old_rem, 10);
    }

    #[test]
    fn sale_two_same_item_lines_exceeding_one_lot_rejected() {
        let (mut conn, mut hlc) = fixture();
        crate::genesis::run_genesis(&mut conn, &mut hlc, 1000, "deviceA", "owner-1", "Owner").unwrap();
        crate::projectors::rebuild(&mut conn).unwrap();
        {
            let mut c = ctx(&mut conn, &mut hlc);
            crate::commands::setup::handle_party_created(&mut c, "cust1", "Cust", "customer").unwrap();
            crate::commands::setup::handle_party_created(&mut c, "sup1", "Sup", "supplier").unwrap();
            crate::commands::setup::handle_item_defined(&mut c, "itemA", "SKU-A", "A", "ea").unwrap();
            handle_purchase_recorded(&mut c, "purOld", "sup1", "2026-01-01", "cash",
                vec![PurchaseLineInput{ item_id: "itemA".into(), qty: 10, unit_cost_minor: 500 }]).unwrap();
        }
        let err = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_sale_recorded(&mut c, "sale1", "cust1", "2026-03-01", "cash", vec![
                SaleLineInput{ item_id:"itemA".into(), qty:6, unit_price_minor:1000, lot_picks: None },
                SaleLineInput{ item_id:"itemA".into(), qty:6, unit_price_minor:1000, lot_picks: None },
            ]).unwrap_err()
        };
        assert!(matches!(err, CommandError::Validation(_)));
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM events WHERE type='SaleRecorded'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
        let rem: i64 = conn.query_row("SELECT qty_remaining FROM inventory_lots WHERE id='purOld#lot0'", [], |r| r.get(0)).unwrap();
        assert_eq!(rem, 10);
    }

    #[test]
    fn sale_two_picks_same_lot_in_one_line_exceeding_stock_rejected() {
        let (mut conn, mut hlc) = fixture();
        seed(&mut conn, &mut hlc);
        let mut c = ctx(&mut conn, &mut hlc);
        let err = handle_sale_recorded(&mut c, "sale1", "cust1", "2026-03-01", "cash",
            vec![SaleLineInput{ item_id:"itemA".into(), qty:12, unit_price_minor:1000,
                lot_picks: Some(vec![("purOld#lot0".into(), 6), ("purOld#lot0".into(), 6)]) }]).unwrap_err();
        assert!(matches!(err, CommandError::Validation(_)));
    }

    #[test]
    fn sale_user_override_lot_pick_honored_and_oversell_still_enforced() {
        let (mut conn, mut hlc) = fixture();
        seed(&mut conn, &mut hlc);
        let err = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_sale_recorded(&mut c, "sale1", "cust1", "2026-03-01", "cash",
                vec![SaleLineInput{ item_id:"itemA".into(), qty:12, unit_price_minor:1000,
                    lot_picks: Some(vec![("purNew#lot0".into(), 12)]) }]).unwrap_err()
        };
        assert!(matches!(err, CommandError::Validation(_)));
        let ev = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_sale_recorded(&mut c, "sale1", "cust1", "2026-03-01", "cash",
                vec![SaleLineInput{ item_id:"itemA".into(), qty:5, unit_price_minor:1000,
                    lot_picks: Some(vec![("purNew#lot0".into(), 5)]) }]).expect("ok")
        };
        assert_eq!(ev.payload["lines"][0]["cogsMinor"], 3500);
    }

    #[test]
    fn sale_override_qty_mismatch_rejected() {
        let (mut conn, mut hlc) = fixture();
        seed(&mut conn, &mut hlc);
        let mut c = ctx(&mut conn, &mut hlc);
        let err = handle_sale_recorded(&mut c, "sale1", "cust1", "2026-03-01", "cash",
            vec![SaleLineInput{ item_id:"itemA".into(), qty:5, unit_price_minor:1000,
                lot_picks: Some(vec![("purOld#lot0".into(), 4)]) }]).unwrap_err();
        assert!(matches!(err, CommandError::Validation(_)));
    }

    #[test]
    fn sale_return_emits_nested_lot_returns_and_restores_lot() {
        let (mut conn, mut hlc) = fixture();
        seed(&mut conn, &mut hlc);
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_sale_recorded(&mut c, "sale1", "cust1", "2026-03-01", "credit",
                vec![SaleLineInput{ item_id:"itemA".into(), qty:10, unit_price_minor:1000, lot_picks: None }]).unwrap();
        }
        let ev = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_sale_return_recorded(&mut c, "sret1", "sale1", "2026-04-01",
                vec![SaleReturnItemInput{ item_id:"itemA".into(),
                    lot_returns: vec![("purOld#lot0".into(), 3)] }]).expect("ok")
        };
        assert_eq!(ev.payload["lines"][0]["itemId"], "itemA");
        assert_eq!(ev.payload["lines"][0]["qty"], 3);
        assert_eq!(ev.payload["lines"][0]["unitPriceMinor"], 1000);
        let lr = ev.payload["lines"][0]["lotReturns"].as_array().unwrap();
        assert_eq!(lr[0]["lotId"], "purOld#lot0");
        assert_eq!(lr[0]["qtyReturned"], 3);
        assert_eq!(lr[0]["unitCostMinor"], 500);
        assert!(ev.payload.get("revenueReversedMinor").is_none());
        assert!(ev.payload.get("costRestoredMinor").is_none());
        let rem: i64 = conn.query_row("SELECT qty_remaining FROM inventory_lots WHERE id='purOld#lot0'", [], |r| r.get(0)).unwrap();
        assert_eq!(rem, 3);
    }

    #[test]
    fn sale_return_over_consumed_rejected_and_writes_nothing() {
        let (mut conn, mut hlc) = fixture();
        seed(&mut conn, &mut hlc);
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_sale_recorded(&mut c, "sale1", "cust1", "2026-03-01", "credit",
                vec![SaleLineInput{ item_id:"itemA".into(), qty:5, unit_price_minor:1000, lot_picks: None }]).unwrap();
        }
        let err = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_sale_return_recorded(&mut c, "sret1", "sale1", "2026-04-01",
                vec![SaleReturnItemInput{ item_id:"itemA".into(),
                    lot_returns: vec![("purOld#lot0".into(), 8)] }]).unwrap_err()
        };
        assert!(matches!(err, CommandError::Validation(_)));
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM events WHERE type='SaleReturnRecorded'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn sale_return_against_reversed_sale_rejected() {
        let (mut conn, mut hlc) = fixture();
        seed(&mut conn, &mut hlc);
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_sale_recorded(&mut c, "sale1", "cust1", "2026-03-01", "credit",
                vec![SaleLineInput{ item_id:"itemA".into(), qty:5, unit_price_minor:1000, lot_picks: None }]).unwrap();
        }
        conn.execute("UPDATE sales SET reversed = 1 WHERE id='sale1'", []).unwrap();
        let err = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_sale_return_recorded(&mut c, "sret1", "sale1", "2026-04-01",
                vec![SaleReturnItemInput{ item_id:"itemA".into(),
                    lot_returns: vec![("purOld#lot0".into(), 2)] }]).unwrap_err()
        };
        assert!(matches!(err, CommandError::Validation(_)));
    }
}
