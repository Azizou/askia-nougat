use crate::commands::guards::{check_allocation_party_ownership, check_amount_positive,
    check_credit_overdraw, check_invoice_over_allocation_aggregated, check_payment_over_allocation,
    check_seeded_party_takes_no_payment};
use crate::commands::{commit_event, reject, CommandContext, CommandError};
use rusqlite::OptionalExtension;
use serde_json::json;

pub struct AllocInput { pub target_id: String, pub target_type: String, pub amount_minor: i64 }

fn ensure_party_kind(ctx: &CommandContext, party_id: &str, allowed: &[&str]) -> Result<(), CommandError> {
    let kind: Option<String> = ctx.conn.query_row(
        "SELECT kind FROM parties WHERE id = ?1", [party_id], |r| r.get(0)).optional()?;
    match kind {
        None => Err(reject(format!("unknown party: {party_id}"))),
        Some(k) if k != "both" && !allowed.contains(&k.as_str()) =>
            Err(reject(format!("party {party_id} kind '{k}' not allowed here"))),
        Some(_) => Ok(()),
    }
}

fn validate_payment_allocations(
    ctx: &CommandContext, party_id: &str, direction: &str,
    payment_amount: i64, allocs: &[AllocInput],
) -> Result<(), CommandError> {
    let mut amounts = Vec::with_capacity(allocs.len());
    let mut agg = Vec::with_capacity(allocs.len());
    for a in allocs {
        check_amount_positive(a.amount_minor)?;
        check_allocation_party_ownership(ctx.conn, party_id, direction, &a.target_id, &a.target_type)?;
        amounts.push(a.amount_minor);
        agg.push((a.target_id.clone(), a.target_type.clone(), a.amount_minor));
    }
    check_invoice_over_allocation_aggregated(ctx.conn, &agg)?;
    check_payment_over_allocation(payment_amount, &amounts)?;
    Ok(())
}

fn alloc_json_by_key(allocs: &[AllocInput], key: &str) -> Vec<serde_json::Value> {
    allocs.iter().map(|a| json!({
        key: a.target_id, "amountMinor": a.amount_minor,
    })).collect()
}

fn alloc_json_targeted(allocs: &[AllocInput]) -> Vec<serde_json::Value> {
    allocs.iter().map(|a| json!({
        "targetId": a.target_id, "targetType": a.target_type, "amountMinor": a.amount_minor,
    })).collect()
}

pub fn handle_payment_made(
    ctx: &mut CommandContext, payment_id: &str, supplier_id: &str,
    amount_minor: i64, date: &str, allocations: Vec<AllocInput>,
) -> Result<crate::events::LedgerEvent, CommandError> {
    check_amount_positive(amount_minor)?;
    check_seeded_party_takes_no_payment(supplier_id)?;
    ensure_party_kind(ctx, supplier_id, &["supplier"])?;
    validate_payment_allocations(ctx, supplier_id, "out", amount_minor, &allocations)?;
    let payload = json!({
        "paymentId": payment_id, "supplierId": supplier_id,
        "amountMinor": amount_minor, "date": date,
        "allocations": alloc_json_by_key(&allocations, "purchaseId"),
    });
    commit_event(ctx, "PaymentMade", payload)
}

pub fn handle_payment_received(
    ctx: &mut CommandContext, payment_id: &str, customer_id: &str,
    amount_minor: i64, date: &str, allocations: Vec<AllocInput>,
) -> Result<crate::events::LedgerEvent, CommandError> {
    check_amount_positive(amount_minor)?;
    check_seeded_party_takes_no_payment(customer_id)?;
    ensure_party_kind(ctx, customer_id, &["customer"])?;
    validate_payment_allocations(ctx, customer_id, "in", amount_minor, &allocations)?;
    let payload = json!({
        "paymentId": payment_id, "customerId": customer_id,
        "amountMinor": amount_minor, "date": date,
        "allocations": alloc_json_by_key(&allocations, "saleId"),
    });
    commit_event(ctx, "PaymentReceived", payload)
}

#[allow(clippy::too_many_arguments)]
pub fn handle_payment_allocated(
    ctx: &mut CommandContext, _alloc_event_id: &str, source_payment_id: &str,
    party_id: &str, date: &str, allocations: Vec<AllocInput>,
) -> Result<crate::events::LedgerEvent, CommandError> {
    if allocations.is_empty() { return Err(reject("PaymentAllocated must have >= 1 allocation")); }
    let direction: String = ctx.conn.query_row(
        "SELECT direction FROM payments WHERE id = ?1", [source_payment_id], |r| r.get(0))
        .optional()?
        .ok_or_else(|| reject(format!("unknown source payment: {source_payment_id}")))?;
    {
        let owner: Option<String> = ctx.conn.query_row(
            "SELECT party_id FROM payments WHERE id = ?1", [source_payment_id], |r| r.get(0)).optional()?;
        if owner.as_deref() != Some(party_id) {
            return Err(reject(format!("payment {source_payment_id} does not belong to party {party_id}")));
        }
    }
    let mut total = 0i64;
    let mut agg = Vec::with_capacity(allocations.len());
    for a in &allocations {
        check_amount_positive(a.amount_minor)?;
        check_allocation_party_ownership(ctx.conn, party_id, &direction, &a.target_id, &a.target_type)?;
        total += a.amount_minor;
        agg.push((a.target_id.clone(), a.target_type.clone(), a.amount_minor));
    }
    check_invoice_over_allocation_aggregated(ctx.conn, &agg)?;
    check_credit_overdraw(ctx.conn, party_id, &direction, total)?;
    let payload = json!({
        "paymentId": source_payment_id, "partyId": party_id,
        "date": date, "allocations": alloc_json_targeted(&allocations),
    });
    commit_event(ctx, "PaymentAllocated", payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::tests::fixture;
    use crate::commands::purchase::{handle_purchase_recorded, PurchaseLineInput};
    use crate::commands::sale::{handle_sale_recorded, SaleLineInput};

    fn ctx<'a>(c: &'a mut rusqlite::Connection, h: &'a mut crate::hlc::Hlc) -> CommandContext<'a> {
        CommandContext { conn: c, hlc: h, physical_now: 1000, device_id: "deviceA".into(), user_id: "owner-1".into() }
    }
    fn seed_credit_sale(conn: &mut rusqlite::Connection, hlc: &mut crate::hlc::Hlc) {
        crate::genesis::run_genesis(conn, hlc, 1000, "deviceA", "owner-1", "Owner").unwrap();
        crate::projectors::rebuild(conn).unwrap();
        let mut c = ctx(conn, hlc);
        crate::commands::setup::handle_party_created(&mut c, "cust1", "Cust", "customer").unwrap();
        crate::commands::setup::handle_party_created(&mut c, "cust2", "Cust2", "customer").unwrap();
        crate::commands::setup::handle_party_created(&mut c, "sup1", "Sup", "supplier").unwrap();
        crate::commands::setup::handle_item_defined(&mut c, "itemA", "A", "A", "ea").unwrap();
        handle_purchase_recorded(&mut c, "pur1", "sup1", "2026-01-01", "cash",
            vec![PurchaseLineInput{ item_id:"itemA".into(), qty:10, unit_cost_minor:100 }]).unwrap();
        handle_sale_recorded(&mut c, "sale1", "cust1", "2026-02-01", "credit",
            vec![SaleLineInput{ item_id:"itemA".into(), qty:5, unit_price_minor:1000, lot_picks: None }]).unwrap();
    }

    #[test]
    fn payment_received_partial_allocation_leaves_credit() {
        let (mut conn, mut hlc) = fixture();
        seed_credit_sale(&mut conn, &mut hlc);
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_payment_received(&mut c, "pay1", "cust1", 6000, "2026-03-01",
                vec![AllocInput{ target_id:"sale1".into(), target_type:"sale".into(), amount_minor:5000 }]).expect("ok");
        }
        let out: i64 = conn.query_row("SELECT outstanding_minor FROM sales WHERE id='sale1'", [], |r| r.get(0)).unwrap();
        assert_eq!(out, 0);
        let cr: i64 = conn.query_row("SELECT unallocated_cr_minor FROM party_balances WHERE party_id='cust1'", [], |r| r.get(0)).unwrap();
        assert_eq!(cr, 1000);
    }

    #[test]
    fn payment_received_over_invoice_rejected_and_writes_nothing() {
        let (mut conn, mut hlc) = fixture();
        seed_credit_sale(&mut conn, &mut hlc);
        let err = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_payment_received(&mut c, "pay1", "cust1", 9000, "2026-03-01",
                vec![AllocInput{ target_id:"sale1".into(), target_type:"sale".into(), amount_minor:6000 }]).unwrap_err()
        };
        assert!(matches!(err, CommandError::Validation(_)));
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM events WHERE type='PaymentReceived'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn payment_over_allocation_sum_rejected() {
        let (mut conn, mut hlc) = fixture();
        seed_credit_sale(&mut conn, &mut hlc);
        let mut c = ctx(&mut conn, &mut hlc);
        let err = handle_payment_received(&mut c, "pay1", "cust1", 4000, "2026-03-01",
            vec![AllocInput{ target_id:"sale1".into(), target_type:"sale".into(), amount_minor:5000 }]).unwrap_err();
        assert!(matches!(err, CommandError::Validation(_)));
    }

    #[test]
    fn payment_received_wrong_party_invoice_rejected() {
        let (mut conn, mut hlc) = fixture();
        seed_credit_sale(&mut conn, &mut hlc);
        let mut c = ctx(&mut conn, &mut hlc);
        let err = handle_payment_received(&mut c, "pay1", "cust2", 5000, "2026-03-01",
            vec![AllocInput{ target_id:"sale1".into(), target_type:"sale".into(), amount_minor:5000 }]).unwrap_err();
        assert!(matches!(err, CommandError::Validation(_)));
    }

    #[test]
    fn payment_two_lines_same_invoice_exceeding_outstanding_rejected() {
        let (mut conn, mut hlc) = fixture();
        seed_credit_sale(&mut conn, &mut hlc);
        let err = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_payment_received(&mut c, "pay1", "cust1", 8000, "2026-03-01", vec![
                AllocInput{ target_id:"sale1".into(), target_type:"sale".into(), amount_minor:5000 },
                AllocInput{ target_id:"sale1".into(), target_type:"sale".into(), amount_minor:3000 },
            ]).unwrap_err()
        };
        assert!(matches!(err, CommandError::Validation(_)));
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM events WHERE type='PaymentReceived'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
        let out: i64 = conn.query_row("SELECT outstanding_minor FROM sales WHERE id='sale1'", [], |r| r.get(0)).unwrap();
        assert_eq!(out, 5000);
    }

    #[test]
    fn payment_allocated_credit_overdraw_rejected() {
        let (mut conn, mut hlc) = fixture();
        seed_credit_sale(&mut conn, &mut hlc);
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_payment_received(&mut c, "prepay", "cust1", 1000, "2026-02-15", vec![]).expect("prepay ok");
        }
        let err = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_payment_allocated(&mut c, "alloc1", "prepay", "cust1", "2026-03-01",
                vec![AllocInput{ target_id:"sale1".into(), target_type:"sale".into(), amount_minor:2000 }]).unwrap_err()
        };
        assert!(matches!(err, CommandError::Validation(_)));
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_payment_allocated(&mut c, "alloc1", "prepay", "cust1", "2026-03-01",
                vec![AllocInput{ target_id:"sale1".into(), target_type:"sale".into(), amount_minor:1000 }]).expect("ok");
        }
        let cr: i64 = conn.query_row("SELECT unallocated_cr_minor FROM party_balances WHERE party_id='cust1'", [], |r| r.get(0)).unwrap();
        assert_eq!(cr, 0);
    }

    /// The seeded parties can never hold an invoice, so a payment to or from them
    /// could only ever be an unallocated prepayment against nobody. The payments
    /// form offers them in its dropdown, so the guard has to be here.
    #[test]
    fn the_seeded_parties_can_neither_send_nor_receive_a_payment() {
        let (mut conn, mut hlc) = fixture();
        seed_credit_sale(&mut conn, &mut hlc);
        crate::genesis::ensure_walkin_party(&conn, &mut hlc, 1000, "deviceA").unwrap();
        crate::genesis::ensure_anon_supplier(&conn, &mut hlc, 1000, "deviceA").unwrap();
        crate::projectors::rebuild(&mut conn).unwrap();

        let recv_err = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_payment_received(&mut c, "pay_walkin", crate::genesis::WALKIN_PARTY_ID,
                5000, "2026-03-01", vec![]).unwrap_err()
        };
        assert!(matches!(recv_err, CommandError::Validation(_)));
        let made_err = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_payment_made(&mut c, "pay_anon", crate::genesis::ANON_SUPPLIER_PARTY_ID,
                5000, "2026-03-01", vec![]).unwrap_err()
        };
        assert!(matches!(made_err, CommandError::Validation(_)));

        let n: i64 = conn.query_row("SELECT COUNT(*) FROM events WHERE type LIKE 'Payment%'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0, "neither rejected payment may reach the log");
        let held: i64 = conn.query_row(
            "SELECT COALESCE(SUM(unallocated_cr_minor + unallocated_dr_minor), 0)
             FROM party_balances WHERE party_id IN (?1, ?2)",
            [crate::genesis::WALKIN_PARTY_ID, crate::genesis::ANON_SUPPLIER_PARTY_ID],
            |r| r.get(0)).unwrap();
        assert_eq!(held, 0, "a seeded party must never hold an undrawable credit");

        // An ordinary party still takes an unallocated prepayment.
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_payment_received(&mut c, "pay_ok", "cust2", 5000, "2026-03-01", vec![])
                .expect("ordinary parties are unaffected");
        }
    }

    /// The guard above is a command guard, so a log imported from a device on an
    /// older build can still carry a credit sale to a seeded party and a prepayment
    /// from it — `import_jsonl` appends raw and rebuilds, which is the D4 design.
    ///
    /// Two things must hold for such a log. It must still replay, or the guard would
    /// brick startup for exactly the users it was meant to protect. And the phantom
    /// balance must remain clearable *at the core level*, which is why
    /// `handle_payment_allocated` is deliberately left unguarded: allocating the stray
    /// prepayment against the stray invoice is the remediation, not a repeat of the
    /// mistake.
    ///
    /// Clearable at the core level is the whole claim, and the distinction matters.
    /// `handle_payment_allocated` is not registered in `generate_handler!`, so there is
    /// no UI route to it and this remediation is not a capability a user has today —
    /// such a user is already stranded by the missing command, not by any guard. What
    /// this test pins is that adding the guard would not be the thing standing in the
    /// way once an allocation command is exposed. See D11.
    #[test]
    fn an_imported_legacy_seeded_party_balance_replays_and_stays_clearable() {
        let (mut conn, mut hlc) = fixture();
        seed_credit_sale(&mut conn, &mut hlc);
        crate::genesis::ensure_walkin_party(&conn, &mut hlc, 1000, "deviceA").unwrap();
        crate::projectors::rebuild(&mut conn).unwrap();
        let walkin = crate::genesis::WALKIN_PARTY_ID;

        // Appended the way an import does it: no guard in the path.
        append_raw(&mut conn, &mut hlc, "SaleRecorded", json!({
            "saleId": "legacy_sale", "customerId": walkin, "date": "2026-02-01",
            "terms": "credit", "totalMinor": 3000,
            "lines": [{"itemId": "itemA", "qty": 3, "unitPriceMinor": 1000,
                       "revenueMinor": 3000, "cogsMinor": 300,
                       "lotConsumption": [{"lotId": "pur1#lot0", "qtyTaken": 3,
                                           "unitCostMinor": 100}]}],
        }));
        append_raw(&mut conn, &mut hlc, "PaymentReceived", json!({
            "paymentId": "legacy_prepay", "customerId": walkin,
            "amountMinor": 3000, "date": "2026-02-02", "allocations": [],
        }));

        // Must replay rather than panic, and the legacy balance must survive intact.
        crate::projectors::rebuild(&mut conn).expect("an imported legacy log must still replay");
        let (recv, cr): (i64, i64) = conn.query_row(
            "SELECT receivable_minor, unallocated_cr_minor FROM party_balances WHERE party_id = ?1",
            [walkin], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((recv, cr), (3000, 3000));

        // Remediation must remain available through the UI.
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_payment_allocated(&mut c, "alloc1", "legacy_prepay", walkin, "2026-03-01",
                vec![AllocInput{ target_id:"legacy_sale".into(), target_type:"sale".into(),
                                 amount_minor:3000 }])
                .expect("clearing an imported phantom balance must stay possible");
        }
        let (recv2, cr2): (i64, i64) = conn.query_row(
            "SELECT receivable_minor, unallocated_cr_minor FROM party_balances WHERE party_id = ?1",
            [walkin], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        let out: i64 = conn.query_row(
            "SELECT outstanding_minor FROM sales WHERE id='legacy_sale'", [], |r| r.get(0)).unwrap();
        assert_eq!((recv2, cr2, out), (0, 0, 0), "the phantom balance must clear completely");
    }

    /// Appends an event exactly as `import_jsonl` does — straight into the log,
    /// with no command guard in the path.
    fn append_raw(
        conn: &mut rusqlite::Connection, hlc: &mut crate::hlc::Hlc,
        etype: &str, payload: serde_json::Value,
    ) {
        let stamp = hlc.tick(3000);
        let seq: i64 = conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM events WHERE device_id = 'deviceB'",
            [], |r| r.get(0)).unwrap();
        let ev = crate::events::LedgerEvent {
            id: format!("legacy-{etype}-{seq}"),
            hlc: stamp,
            device_id: "deviceB".into(),
            user_id: "owner-1".into(),
            seq,
            event_type: etype.to_string(),
            payload,
            created_at: 3000,
        };
        crate::insert_raw_event(conn, &ev).unwrap();
    }
}
