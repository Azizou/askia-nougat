use crate::events::{read_events, LedgerEvent};
use rusqlite::Connection;
use serde_json::{json, Value};

// ---- small payload accessors ----
fn pi(v: &Value, k: &str) -> i64 { v.get(k).and_then(Value::as_i64).unwrap_or(0) }
fn ps<'a>(v: &'a Value, k: &str) -> &'a str { v.get(k).and_then(Value::as_str).unwrap_or("") }
fn pos<'a>(v: &'a Value, k: &str) -> Option<&'a str> { v.get(k).and_then(Value::as_str) }
fn parr<'a>(v: &'a Value, k: &str) -> &'a [Value] {
    v.get(k).and_then(Value::as_array).map(|a| a.as_slice()).unwrap_or(&[])
}

/// Resolve a well-known account's id by its immutable `system_role`.
fn account_id_by_role(tx: &Connection, role: &str) -> rusqlite::Result<String> {
    tx.query_row("SELECT id FROM accounts WHERE system_role = ?1", [role], |r| r.get(0))
}

/// Post one journal line and maintain the account's running balance.
#[allow(clippy::too_many_arguments)]
fn post_line(
    tx: &Connection,
    event_id: &str,
    txn_id: &str,
    line_index: usize,
    account_id: &str,
    debit: i64,
    credit: i64,
    date: &str,
    memo: Option<&str>,
) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO journal_lines
           (id, event_id, txn_id, account_id, debit_minor, credit_minor, date, memo)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            format!("{event_id}#{line_index}"),
            event_id, txn_id, account_id, debit, credit, date, memo
        ],
    )?;
    tx.execute(
        "UPDATE accounts SET balance_minor = balance_minor +
           CASE WHEN normal_side = 'debit' THEN ?2 - ?3 ELSE ?3 - ?2 END
         WHERE id = ?1",
        rusqlite::params![account_id, debit, credit],
    )?;
    Ok(())
}

/// Upsert a party's balance row and apply signed deltas.
fn adjust_party_balance(
    tx: &Connection,
    party_id: &str,
    d_receivable: i64,
    d_payable: i64,
    d_unalloc_cr: i64,
    d_unalloc_dr: i64,
) -> rusqlite::Result<()> {
    tx.execute("INSERT OR IGNORE INTO party_balances (party_id) VALUES (?1)", [party_id])?;
    tx.execute(
        "UPDATE party_balances SET
           receivable_minor     = receivable_minor + ?2,
           payable_minor        = payable_minor + ?3,
           unallocated_cr_minor = unallocated_cr_minor + ?4,
           unallocated_dr_minor = unallocated_dr_minor + ?5
         WHERE party_id = ?1",
        rusqlite::params![party_id, d_receivable, d_payable, d_unalloc_cr, d_unalloc_dr],
    )?;
    Ok(())
}

/// Add delta to a lot's remaining quantity.
fn adjust_lot_remaining(tx: &Connection, lot_id: &str, delta: i64) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE inventory_lots SET qty_remaining = qty_remaining + ?2 WHERE id = ?1",
        rusqlite::params![lot_id, delta],
    )?;
    Ok(())
}

fn lot_unit_cost(tx: &Connection, lot_id: &str) -> rusqlite::Result<i64> {
    tx.query_row("SELECT unit_cost_minor FROM inventory_lots WHERE id = ?1", [lot_id], |r| r.get(0))
}

/// Merge an event's `changes` object into an existing row's `doc` JSONB.
fn patch_doc(tx: &Connection, table: &str, ev: &LedgerEvent, id_key: &str) -> rusqlite::Result<()> {
    let id = ps(&ev.payload, id_key);
    let sel = format!("SELECT json(doc) FROM {table} WHERE id = ?1");
    let doc_text: String = tx.query_row(&sel, [id], |r| r.get(0))?;
    let mut doc: Value = serde_json::from_str(&doc_text).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })?;
    if let Some(changes) = ev.payload.get("changes").and_then(Value::as_object) {
        for (k, v) in changes {
            doc[k] = v.clone();
        }
    }
    let upd = format!("UPDATE {table} SET doc = jsonb(?2) WHERE id = ?1");
    tx.execute(&upd, rusqlite::params![id, doc.to_string()])?;
    Ok(())
}

/// Central dispatcher: apply one event to the read model.
pub fn apply_event(tx: &Connection, ev: &LedgerEvent) -> rusqlite::Result<()> {
    let p = &ev.payload;
    match ev.event_type.as_str() {
        // ---- master data: create (no journal) ----
        "UserRegistered" => {
            tx.execute(
                "INSERT INTO users (id, doc, created_at) VALUES (?1, jsonb(?2), ?3)",
                rusqlite::params![ps(p, "userId"), p.to_string(), ev.created_at],
            )?;
            Ok(())
        }
        "AccountOpened" => {
            tx.execute(
                "INSERT INTO accounts (id, doc, balance_minor) VALUES (?1, jsonb(?2), 0)",
                rusqlite::params![ps(p, "accountId"), p.to_string()],
            )?;
            Ok(())
        }
        "ItemDefined" => {
            let mut doc = p.clone();
            if doc.get("active").is_none() {
                doc["active"] = json!(1);
            }
            tx.execute(
                "INSERT INTO items (id, doc) VALUES (?1, jsonb(?2))",
                rusqlite::params![ps(p, "itemId"), doc.to_string()],
            )?;
            Ok(())
        }
        "PartyCreated" => {
            tx.execute(
                "INSERT INTO parties (id, doc) VALUES (?1, jsonb(?2))",
                rusqlite::params![ps(p, "partyId"), p.to_string()],
            )?;
            Ok(())
        }
        // ---- master data: update (patch) ----
        "UserUpdated" => patch_doc(tx, "users", ev, "userId"),
        "AccountUpdated" => patch_doc(tx, "accounts", ev, "accountId"),
        "ItemUpdated" => patch_doc(tx, "items", ev, "itemId"),
        "PartyUpdated" => patch_doc(tx, "parties", ev, "partyId"),

        "PurchaseRecorded" => purchase_recorded(tx, ev),
        "SaleRecorded" => sale_recorded(tx, ev),
        "PaymentReceived" => payment(tx, ev, "in"),
        "PaymentMade" => payment(tx, ev, "out"),
        "PaymentAllocated" => payment_allocated(tx, ev),
        "ExpenseRecorded" => expense_recorded(tx, ev),
        "TransferRecorded" => transfer_recorded(tx, ev),
        "InventoryAdjusted" => inventory_adjusted(tx, ev),
        "InventoryFound" => inventory_found(tx, ev),
        "OpeningBalancesRecorded" => opening_balances(tx, ev),
        "SaleReturnRecorded" => sale_return(tx, ev),
        "PurchaseReturnRecorded" => purchase_return(tx, ev),
        "TransactionReversed" => transaction_reversed(tx, ev),

        other => Err(unknown(other)),
    }
}

fn unknown(ty: &str) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_MISMATCH),
        Some(format!("projector has no handler for event type: {ty}")),
    )
}

fn purchase_recorded(tx: &Connection, ev: &LedgerEvent) -> rusqlite::Result<()> {
    let p = &ev.payload;
    let purchase_id = ps(p, "purchaseId");
    let date = ps(p, "date");
    let terms = ps(p, "terms");
    let supplier = pos(p, "supplierId");

    let lines = parr(p, "lines");
    let total: i64 = lines.iter().map(|l| pi(l, "qty") * pi(l, "unitCostMinor")).sum();

    let outstanding = if terms == "credit" { total } else { 0 };
    tx.execute(
        "INSERT INTO purchases (id, event_id, supplier_id, date, terms, total_minor, outstanding_minor)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![purchase_id, ev.id, supplier, date, terms, total, outstanding],
    )?;

    for (i, line) in lines.iter().enumerate() {
        let qty = pi(line, "qty");
        let unit_cost = pi(line, "unitCostMinor");
        let lot_id = ps(line, "lotId");
        let item_id = ps(line, "itemId");
        tx.execute(
            "INSERT INTO inventory_lots
               (id, item_id, source_event_id, purchase_id, unit_cost_minor,
                qty_received, qty_remaining, acquired_at, supplier_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?7, ?8)",
            rusqlite::params![lot_id, item_id, ev.id, purchase_id, unit_cost, qty, date, supplier],
        )?;
        tx.execute(
            "INSERT INTO purchase_lines (id, purchase_id, item_id, qty, unit_cost_minor, lot_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![format!("{}#line#{i}", ev.id), purchase_id, item_id, qty, unit_cost, lot_id],
        )?;
    }

    let inventory = account_id_by_role(tx, "inventory")?;
    post_line(tx, &ev.id, purchase_id, 0, &inventory, total, 0, date, None)?;
    if terms == "credit" {
        let ap = account_id_by_role(tx, "accounts_payable")?;
        post_line(tx, &ev.id, purchase_id, 1, &ap, 0, total, date, None)?;
        if let Some(s) = supplier {
            adjust_party_balance(tx, s, 0, total, 0, 0)?;
        }
    } else {
        let bank = account_id_by_role(tx, "bank")?;
        post_line(tx, &ev.id, purchase_id, 1, &bank, 0, total, date, None)?;
    }
    Ok(())
}

fn sale_recorded(tx: &Connection, ev: &LedgerEvent) -> rusqlite::Result<()> {
    let p = &ev.payload;
    let sale_id = ps(p, "saleId");
    let date = ps(p, "date");
    let terms = ps(p, "terms");
    let customer = pos(p, "customerId");

    let lines = parr(p, "lines");
    let total_rev: i64 = lines.iter().map(|l| pi(l, "qty") * pi(l, "unitPriceMinor")).sum();
    let total_cogs: i64 = lines.iter()
        .flat_map(|l| parr(l, "lotConsumption").iter())
        .map(|lc| pi(lc, "qtyTaken") * pi(lc, "unitCostMinor"))
        .sum();

    let outstanding = if terms == "credit" { total_rev } else { 0 };
    tx.execute(
        "INSERT INTO sales (id, event_id, customer_id, date, terms, total_minor, outstanding_minor)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![sale_id, ev.id, customer, date, terms, total_rev, outstanding],
    )?;

    for (i, line) in lines.iter().enumerate() {
        let qty = pi(line, "qty");
        let unit_price = pi(line, "unitPriceMinor");
        let revenue = qty * unit_price;
        let item_id = ps(line, "itemId");
        let sale_line_id = format!("{}#line#{i}", ev.id);

        let line_cogs: i64 = parr(line, "lotConsumption").iter()
            .map(|lc| pi(lc, "qtyTaken") * pi(lc, "unitCostMinor"))
            .sum();

        tx.execute(
            "INSERT INTO sale_lines
               (id, sale_id, item_id, qty, unit_price_minor, revenue_minor, cogs_minor, date)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![sale_line_id, sale_id, item_id, qty, unit_price, revenue, line_cogs, date],
        )?;

        for (j, lc) in parr(line, "lotConsumption").iter().enumerate() {
            let lot_id = ps(lc, "lotId");
            let qty_taken = pi(lc, "qtyTaken");
            let unit_cost = pi(lc, "unitCostMinor");
            tx.execute(
                "INSERT INTO lot_consumptions (id, sale_line_id, lot_id, qty_taken, unit_cost_minor)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![format!("{}#lc#{i}#{j}", ev.id), sale_line_id, lot_id, qty_taken, unit_cost],
            )?;
            adjust_lot_remaining(tx, lot_id, -qty_taken)?;
        }
    }

    let sales_acct = account_id_by_role(tx, "sales")?;
    let debit_acct = if terms == "credit" {
        account_id_by_role(tx, "accounts_receivable")?
    } else {
        account_id_by_role(tx, "bank")?
    };
    post_line(tx, &ev.id, sale_id, 0, &debit_acct, total_rev, 0, date, None)?;
    post_line(tx, &ev.id, sale_id, 1, &sales_acct, 0, total_rev, date, None)?;

    let cogs_acct = account_id_by_role(tx, "cogs")?;
    let inventory = account_id_by_role(tx, "inventory")?;
    post_line(tx, &ev.id, sale_id, 2, &cogs_acct, total_cogs, 0, date, None)?;
    post_line(tx, &ev.id, sale_id, 3, &inventory, 0, total_cogs, date, None)?;

    if terms == "credit" {
        if let Some(c) = customer {
            adjust_party_balance(tx, c, total_rev, 0, 0, 0)?;
        }
    }
    Ok(())
}

fn payment(tx: &Connection, ev: &LedgerEvent, dir: &str) -> rusqlite::Result<()> {
    let p = &ev.payload;
    let payment_id = ps(p, "paymentId");
    let amount = pi(p, "amountMinor");
    let date = ps(p, "date");
    let party = if dir == "in" { ps(p, "customerId") } else { ps(p, "supplierId") };
    let target_key = if dir == "in" { "saleId" } else { "purchaseId" };
    let target_type = if dir == "in" { "sale" } else { "purchase" };
    let target_table = if dir == "in" { "sales" } else { "purchases" };

    tx.execute(
        "INSERT INTO payments (id, event_id, party_id, direction, amount_minor, date)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![payment_id, ev.id, party, dir, amount, date],
    )?;

    let bank = account_id_by_role(tx, "bank")?;
    if dir == "in" {
        let ar = account_id_by_role(tx, "accounts_receivable")?;
        post_line(tx, &ev.id, payment_id, 0, &bank, amount, 0, date, None)?;
        post_line(tx, &ev.id, payment_id, 1, &ar, 0, amount, date, None)?;
    } else {
        let ap = account_id_by_role(tx, "accounts_payable")?;
        post_line(tx, &ev.id, payment_id, 0, &ap, amount, 0, date, None)?;
        post_line(tx, &ev.id, payment_id, 1, &bank, 0, amount, date, None)?;
    }

    let mut allocated = 0i64;
    for (i, a) in parr(p, "allocations").iter().enumerate() {
        let target_id = ps(a, target_key);
        let amt = pi(a, "amountMinor");
        allocated += amt;
        tx.execute(
            "INSERT INTO payment_allocations (id, event_id, payment_id, target_id, target_type, amount_minor)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![format!("{}#alloc#{i}", ev.id), ev.id, payment_id, target_id, target_type, amt],
        )?;
        let upd = format!("UPDATE {target_table} SET outstanding_minor = outstanding_minor - ?2 WHERE id = ?1");
        tx.execute(&upd, rusqlite::params![target_id, amt])?;
        if dir == "in" {
            adjust_party_balance(tx, party, -amt, 0, 0, 0)?;
        } else {
            adjust_party_balance(tx, party, 0, -amt, 0, 0)?;
        }
    }

    let remainder = amount - allocated;
    if remainder != 0 {
        if dir == "in" {
            adjust_party_balance(tx, party, 0, 0, remainder, 0)?;
        } else {
            adjust_party_balance(tx, party, 0, 0, 0, remainder)?;
        }
    }
    Ok(())
}

fn payment_allocated(tx: &Connection, ev: &LedgerEvent) -> rusqlite::Result<()> {
    let p = &ev.payload;
    let payment_id = ps(p, "paymentId");
    let party = ps(p, "partyId");
    for (i, a) in parr(p, "allocations").iter().enumerate() {
        let target_id = ps(a, "targetId");
        let target_type = ps(a, "targetType");
        let amt = pi(a, "amountMinor");
        tx.execute(
            "INSERT INTO payment_allocations (id, event_id, payment_id, target_id, target_type, amount_minor)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![format!("{}#alloc#{i}", ev.id), ev.id, payment_id, target_id, target_type, amt],
        )?;
        let table = if target_type == "sale" { "sales" } else { "purchases" };
        let upd = format!("UPDATE {table} SET outstanding_minor = outstanding_minor - ?2 WHERE id = ?1");
        tx.execute(&upd, rusqlite::params![target_id, amt])?;
        if target_type == "sale" {
            adjust_party_balance(tx, party, -amt, 0, -amt, 0)?;
        } else {
            adjust_party_balance(tx, party, 0, -amt, 0, -amt)?;
        }
    }
    Ok(())
}

fn expense_recorded(tx: &Connection, ev: &LedgerEvent) -> rusqlite::Result<()> {
    let p = &ev.payload;
    let expense_id = ps(p, "expenseId");
    let account_id = ps(p, "accountId");
    let amount = pi(p, "amountMinor");
    let date = ps(p, "date");
    let terms = ps(p, "terms");
    let memo = pos(p, "memo");
    tx.execute(
        "INSERT INTO expenses (id, event_id, account_id, amount_minor, date, memo, terms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![expense_id, ev.id, account_id, amount, date, memo, terms],
    )?;
    post_line(tx, &ev.id, expense_id, 0, account_id, amount, 0, date, memo)?;
    let credit_acct = if terms == "credit" {
        account_id_by_role(tx, "accounts_payable")?
    } else {
        account_id_by_role(tx, "bank")?
    };
    post_line(tx, &ev.id, expense_id, 1, &credit_acct, 0, amount, date, memo)?;
    if terms == "credit" {
        if let Some(s) = pos(p, "supplierId") {
            adjust_party_balance(tx, s, 0, amount, 0, 0)?;
        }
    }
    Ok(())
}

fn transfer_recorded(tx: &Connection, ev: &LedgerEvent) -> rusqlite::Result<()> {
    let p = &ev.payload;
    let transfer_id = ps(p, "transferId");
    let from = ps(p, "fromAccountId");
    let to = ps(p, "toAccountId");
    let amount = pi(p, "amountMinor");
    let date = ps(p, "date");
    let memo = pos(p, "memo");
    post_line(tx, &ev.id, transfer_id, 0, to, amount, 0, date, memo)?;
    post_line(tx, &ev.id, transfer_id, 1, from, 0, amount, date, memo)?;
    Ok(())
}

fn inventory_adjusted(tx: &Connection, ev: &LedgerEvent) -> rusqlite::Result<()> {
    let p = &ev.payload;
    let adj_id = ps(p, "adjustmentId");
    let date = ps(p, "date");
    let inventory = account_id_by_role(tx, "inventory")?;
    let mut li = 0usize;
    for line in parr(p, "lines") {
        let lot_id = ps(line, "lotId");
        let qty = -pi(line, "qtyDelta");
        let unit_cost = lot_unit_cost(tx, lot_id)?;
        let value = qty * unit_cost;
        adjust_lot_remaining(tx, lot_id, -qty)?;
        let expense_acct = match pos(line, "expenseAccountId") {
            Some(a) => a.to_string(),
            None => account_id_by_role(tx, "shrinkage")?,
        };
        post_line(tx, &ev.id, adj_id, li, &expense_acct, value, 0, date, None)?;
        li += 1;
        post_line(tx, &ev.id, adj_id, li, &inventory, 0, value, date, None)?;
        li += 1;
    }
    Ok(())
}

fn inventory_found(tx: &Connection, ev: &LedgerEvent) -> rusqlite::Result<()> {
    let p = &ev.payload;
    let found_id = ps(p, "foundId");
    let date = ps(p, "date");
    let inventory = account_id_by_role(tx, "inventory")?;
    let mut li = 0usize;
    for line in parr(p, "lines") {
        let lot_id = ps(line, "lotId");
        let item_id = ps(line, "itemId");
        let qty = pi(line, "qty");
        let unit_cost = pi(line, "unitCostMinor");
        let acquired = ps(line, "acquiredAt");
        let value = qty * unit_cost;
        tx.execute(
            "INSERT INTO inventory_lots
               (id, item_id, source_event_id, purchase_id, unit_cost_minor,
                qty_received, qty_remaining, acquired_at, supplier_id)
             VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?5, ?6, NULL)",
            rusqlite::params![lot_id, item_id, ev.id, unit_cost, qty, acquired],
        )?;
        let income_acct = match pos(line, "incomeAccountId") {
            Some(a) => a.to_string(),
            None => account_id_by_role(tx, "inventory_gain")?,
        };
        post_line(tx, &ev.id, found_id, li, &inventory, value, 0, date, None)?;
        li += 1;
        post_line(tx, &ev.id, found_id, li, &income_acct, 0, value, date, None)?;
        li += 1;
    }
    Ok(())
}

fn opening_balances(tx: &Connection, ev: &LedgerEvent) -> rusqlite::Result<()> {
    let p = &ev.payload;
    let date = ps(p, "date");
    for (li, ab) in parr(p, "accountBalances").iter().enumerate() {
        let account_id = ps(ab, "accountId");
        let debit = pi(ab, "debitMinor");
        let credit = pi(ab, "creditMinor");
        post_line(tx, &ev.id, &ev.id, li, account_id, debit, credit, date, Some("opening balance"))?;
    }
    for lot in parr(p, "lots") {
        let lot_id = ps(lot, "lotId");
        let item_id = ps(lot, "itemId");
        let qty = pi(lot, "qty");
        let unit_cost = pi(lot, "unitCostMinor");
        let acquired = ps(lot, "acquiredAt");
        let supplier = pos(lot, "supplierId");
        tx.execute(
            "INSERT INTO inventory_lots
               (id, item_id, source_event_id, purchase_id, unit_cost_minor,
                qty_received, qty_remaining, acquired_at, supplier_id)
             VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?5, ?6, ?7)",
            rusqlite::params![lot_id, item_id, ev.id, unit_cost, qty, acquired, supplier],
        )?;
    }
    Ok(())
}

fn sale_return(tx: &Connection, ev: &LedgerEvent) -> rusqlite::Result<()> {
    let p = &ev.payload;
    let return_id = ps(p, "returnId");
    let original_id = ps(p, "originalSaleId");
    let date = ps(p, "date");

    tx.execute(
        "INSERT INTO returns (id, event_id, return_type, original_id, date, revenue_reversed_minor, cost_restored_minor)
         VALUES (?1, ?2, 'sale_return', ?3, ?4, 0, 0)",
        rusqlite::params![return_id, ev.id, original_id, date],
    )?;

    let mut revenue_reversed = 0i64;
    let mut cost_restored = 0i64;
    let mut li = 0usize;
    for line in parr(p, "lines") {
        let item_id = ps(line, "itemId");
        let qty = pi(line, "qty");
        let unit_price = pi(line, "unitPriceMinor");
        revenue_reversed += qty * unit_price;
        for lr in parr(line, "lotReturns") {
            let lot_id = ps(lr, "lotId");
            let qty_ret = pi(lr, "qtyReturned");
            let unit_cost = pi(lr, "unitCostMinor");
            cost_restored += qty_ret * unit_cost;
            adjust_lot_remaining(tx, lot_id, qty_ret)?;
            tx.execute(
                "INSERT INTO return_lines (id, return_id, item_id, qty, unit_price_minor, unit_cost_minor, lot_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![format!("{}#line#{li}", ev.id), return_id, item_id, qty_ret, unit_price, unit_cost, lot_id],
            )?;
            li += 1;
        }
    }

    tx.execute(
        "UPDATE returns SET revenue_reversed_minor = ?2, cost_restored_minor = ?3 WHERE id = ?1",
        rusqlite::params![return_id, revenue_reversed, cost_restored],
    )?;

    let customer: Option<String> = tx.query_row(
        "SELECT customer_id FROM sales WHERE id = ?1", [original_id], |r| r.get(0))?;

    let sales_acct = account_id_by_role(tx, "sales")?;
    let inventory = account_id_by_role(tx, "inventory")?;
    let cogs = account_id_by_role(tx, "cogs")?;
    let refund_acct = if customer.is_some() {
        account_id_by_role(tx, "accounts_receivable")?
    } else {
        account_id_by_role(tx, "bank")?
    };
    post_line(tx, &ev.id, return_id, 0, &sales_acct, revenue_reversed, 0, date, None)?;
    post_line(tx, &ev.id, return_id, 1, &refund_acct, 0, revenue_reversed, date, None)?;
    post_line(tx, &ev.id, return_id, 2, &inventory, cost_restored, 0, date, None)?;
    post_line(tx, &ev.id, return_id, 3, &cogs, 0, cost_restored, date, None)?;

    if let Some(c) = customer {
        let outstanding: i64 = tx.query_row(
            "SELECT outstanding_minor FROM sales WHERE id = ?1", [original_id], |r| r.get(0))?;
        let reduce = revenue_reversed.min(outstanding.max(0));
        let excess = revenue_reversed - reduce;
        tx.execute("UPDATE sales SET outstanding_minor = outstanding_minor - ?2 WHERE id = ?1",
            rusqlite::params![original_id, reduce])?;
        adjust_party_balance(tx, &c, -reduce, 0, excess, 0)?;
    }
    Ok(())
}

fn purchase_return(tx: &Connection, ev: &LedgerEvent) -> rusqlite::Result<()> {
    let p = &ev.payload;
    let return_id = ps(p, "returnId");
    let original_id = ps(p, "originalPurchaseId");
    let date = ps(p, "date");

    tx.execute(
        "INSERT INTO returns (id, event_id, return_type, original_id, date, revenue_reversed_minor, cost_restored_minor)
         VALUES (?1, ?2, 'purchase_return', ?3, ?4, 0, 0)",
        rusqlite::params![return_id, ev.id, original_id, date],
    )?;

    let mut cost_restored = 0i64;
    for (li, line) in parr(p, "lines").iter().enumerate() {
        let item_id = ps(line, "itemId");
        let qty = pi(line, "qty");
        let unit_cost = pi(line, "unitCostMinor");
        let lot_id = ps(line, "lotId");
        cost_restored += qty * unit_cost;
        adjust_lot_remaining(tx, lot_id, -qty)?;
        tx.execute(
            "INSERT INTO return_lines (id, return_id, item_id, qty, unit_price_minor, unit_cost_minor, lot_id)
             VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6)",
            rusqlite::params![format!("{}#line#{li}", ev.id), return_id, item_id, qty, unit_cost, lot_id],
        )?;
    }

    tx.execute(
        "UPDATE returns SET cost_restored_minor = ?2 WHERE id = ?1",
        rusqlite::params![return_id, cost_restored],
    )?;

    let supplier: Option<String> = tx.query_row(
        "SELECT supplier_id FROM purchases WHERE id = ?1", [original_id], |r| r.get(0))?;

    let inventory = account_id_by_role(tx, "inventory")?;
    let refund_acct = if supplier.is_some() {
        account_id_by_role(tx, "accounts_payable")?
    } else {
        account_id_by_role(tx, "bank")?
    };
    post_line(tx, &ev.id, return_id, 0, &refund_acct, cost_restored, 0, date, None)?;
    post_line(tx, &ev.id, return_id, 1, &inventory, 0, cost_restored, date, None)?;

    if let Some(s) = supplier {
        let outstanding: i64 = tx.query_row(
            "SELECT outstanding_minor FROM purchases WHERE id = ?1", [original_id], |r| r.get(0))?;
        let reduce = cost_restored.min(outstanding.max(0));
        let excess = cost_restored - reduce;
        tx.execute("UPDATE purchases SET outstanding_minor = outstanding_minor - ?2 WHERE id = ?1",
            rusqlite::params![original_id, reduce])?;
        adjust_party_balance(tx, &s, 0, -reduce, 0, excess)?;
    }
    Ok(())
}

fn transaction_reversed(tx: &Connection, ev: &LedgerEvent) -> rusqlite::Result<()> {
    let p = &ev.payload;
    let target_id = ps(p, "targetEventId");

    let (target_type, target_payload_text): (String, String) = tx.query_row(
        "SELECT type, json(payload) FROM events WHERE id = ?1", [target_id],
        |r| Ok((r.get(0)?, r.get(1)?)))?;
    let tp: Value = serde_json::from_str(&target_payload_text).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(e))
    })?;
    let post_date = ps(&tp, "date");

    for (i, rl) in parr(p, "reversalJournalLines").iter().enumerate() {
        let account_id = ps(rl, "accountId");
        let debit = pi(rl, "debitMinor");
        let credit = pi(rl, "creditMinor");
        post_line(tx, &ev.id, &ev.id, i, account_id, debit, credit, post_date, Some("reversal"))?;
    }

    match target_type.as_str() {
        "PurchaseRecorded" | "InventoryFound" | "OpeningBalancesRecorded" => {
            tx.execute(
                "UPDATE inventory_lots SET qty_remaining = 0 WHERE source_event_id = ?1",
                [target_id],
            )?;
        }
        "SaleRecorded" => {
            let mut stmt = tx.prepare(
                "SELECT lc.lot_id, lc.qty_taken FROM lot_consumptions lc
                 JOIN sale_lines sl ON sl.id = lc.sale_line_id
                 WHERE sl.sale_id = ?1")?;
            let rows: Vec<(String, i64)> = stmt
                .query_map([ps(&tp, "saleId")], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            for (lot_id, qty) in rows {
                adjust_lot_remaining(tx, &lot_id, qty)?;
            }
        }
        "PurchaseReturnRecorded" => {
            for line in parr(&tp, "lines") {
                adjust_lot_remaining(tx, ps(line, "lotId"), pi(line, "qty"))?;
            }
        }
        "InventoryAdjusted" => {
            for line in parr(&tp, "lines") {
                adjust_lot_remaining(tx, ps(line, "lotId"), -pi(line, "qtyDelta"))?;
            }
        }
        "SaleReturnRecorded" => {
            for line in parr(&tp, "lines") {
                for lr in parr(line, "lotReturns") {
                    adjust_lot_remaining(tx, ps(lr, "lotId"), -pi(lr, "qtyReturned"))?;
                }
            }
        }
        _ => {}
    }

    match target_type.as_str() {
        "PaymentReceived" | "PaymentMade" | "PaymentAllocated" => {
            let is_sale_dir = target_type == "PaymentReceived";
            let mut stmt = tx.prepare(
                "SELECT target_id, target_type, amount_minor FROM payment_allocations WHERE event_id = ?1")?;
            let allocs: Vec<(String, String, i64)> = stmt
                .query_map([target_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<_>>()?;
            for (tid, ttype, amt) in &allocs {
                let table = if ttype == "sale" { "sales" } else { "purchases" };
                let upd = format!("UPDATE {table} SET outstanding_minor = outstanding_minor + ?2 WHERE id = ?1");
                tx.execute(&upd, rusqlite::params![tid, amt])?;
            }
            if target_type == "PaymentAllocated" {
                let party = ps(&tp, "partyId");
                for (_tid, ttype, amt) in &allocs {
                    if ttype == "sale" {
                        adjust_party_balance(tx, party, *amt, 0, *amt, 0)?;
                    } else {
                        adjust_party_balance(tx, party, 0, *amt, 0, *amt)?;
                    }
                }
            } else {
                let payment_id = ps(&tp, "paymentId");
                let party = if is_sale_dir { ps(&tp, "customerId") } else { ps(&tp, "supplierId") };
                let amount: i64 = tx.query_row(
                    "SELECT amount_minor FROM payments WHERE id = ?1", [payment_id], |r| r.get(0))?;
                let allocated: i64 = allocs.iter().map(|(_, _, a)| *a).sum();
                let remainder = amount - allocated;
                if is_sale_dir {
                    adjust_party_balance(tx, party, allocated, 0, -remainder, 0)?;
                } else {
                    adjust_party_balance(tx, party, 0, allocated, 0, -remainder)?;
                }
                tx.execute("DELETE FROM payments WHERE id = ?1", [payment_id])?;
            }
            tx.execute("DELETE FROM payment_allocations WHERE event_id = ?1", [target_id])?;
        }
        "SaleRecorded" => {
            let (customer, total, terms): (Option<String>, i64, String) = tx.query_row(
                "SELECT customer_id, total_minor, terms FROM sales WHERE id = ?1",
                [ps(&tp, "saleId")], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            tx.execute("UPDATE sales SET outstanding_minor = 0 WHERE id = ?1", [ps(&tp, "saleId")])?;
            if terms == "credit" {
                if let Some(c) = customer {
                    adjust_party_balance(tx, &c, -total, 0, 0, 0)?;
                }
            }
        }
        "PurchaseRecorded" => {
            let (supplier, total, terms): (Option<String>, i64, String) = tx.query_row(
                "SELECT supplier_id, total_minor, terms FROM purchases WHERE id = ?1",
                [ps(&tp, "purchaseId")], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            tx.execute("UPDATE purchases SET outstanding_minor = 0 WHERE id = ?1", [ps(&tp, "purchaseId")])?;
            if terms == "credit" {
                if let Some(s) = supplier {
                    adjust_party_balance(tx, &s, 0, -total, 0, 0)?;
                }
            }
        }
        _ => {}
    }

    match target_type.as_str() {
        "SaleRecorded" => {
            tx.execute("UPDATE sales SET reversed = 1 WHERE id = ?1", [ps(&tp, "saleId")])?;
        }
        "PurchaseRecorded" => {
            tx.execute("UPDATE purchases SET reversed = 1 WHERE id = ?1", [ps(&tp, "purchaseId")])?;
        }
        _ => {}
    }
    Ok(())
}

const PROJECTION_TABLES: &[&str] = &[
    "lot_consumptions", "sale_lines", "sales",
    "purchase_lines", "purchases",
    "return_lines", "returns",
    "payment_allocations", "payments",
    "expenses", "journal_lines",
    "party_balances", "inventory_lots",
    "parties", "items", "accounts", "users",
];

/// Drop all projection state and replay the entire event log in HLC order.
pub fn rebuild(conn: &mut Connection) -> rusqlite::Result<()> {
    let events = read_events(conn)?;
    let tx = conn.transaction()?;
    for table in PROJECTION_TABLES {
        tx.execute(&format!("DELETE FROM {table}"), [])?;
    }
    tx.execute("DELETE FROM projection_cursor WHERE projection = 'main'", [])?;
    let mut last_hlc = String::new();
    for ev in &events {
        apply_event(&tx, ev)?;
        last_hlc = ev.hlc.clone();
    }
    if !last_hlc.is_empty() {
        tx.execute(
            "INSERT INTO projection_cursor (projection, last_hlc) VALUES ('main', ?1)
             ON CONFLICT(projection) DO UPDATE SET last_hlc = excluded.last_hlc",
            [last_hlc],
        )?;
    }
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory_with_schema;
    use crate::events::append_event;
    use crate::hlc::Hlc;

    fn record(conn: &mut Connection, hlc: &mut Hlc, phys: u64, ty: &str, payload: Value) -> LedgerEvent {
        let tx = conn.transaction().unwrap();
        let ev = append_event(&tx, hlc, phys, "devA", "userX", ty, &payload).unwrap();
        apply_event(&tx, &ev).unwrap();
        tx.commit().unwrap();
        ev
    }

    fn seed_accounts(conn: &mut Connection, hlc: &mut Hlc) {
        let roles = [
            ("cash", "Cash", "asset", "debit"),
            ("bank", "Bank", "asset", "debit"),
            ("inventory", "Inventory", "asset", "debit"),
            ("accounts_receivable", "Accounts Receivable", "asset", "debit"),
            ("accounts_payable", "Accounts Payable", "liability", "credit"),
            ("owner_capital", "Owner Capital", "equity", "credit"),
            ("sales", "Sales", "income", "credit"),
            ("cogs", "Cost of Goods Sold", "expense", "debit"),
            ("shrinkage", "Inventory Shrinkage", "expense", "debit"),
            ("inventory_gain", "Inventory Gain", "income", "credit"),
            ("rent", "Rent", "expense", "debit"),
        ];
        for (role, name, ty, normal) in roles {
            record(conn, hlc, 1000, "AccountOpened",
                json!({"accountId": format!("acct_{role}"), "name": name,
                       "type": ty, "normal": normal, "system_role": role}));
        }
    }

    #[test]
    fn creates_user_account_item_party_rows() {
        let mut conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("devA");

        record(&mut conn, &mut hlc, 1000, "UserRegistered",
            json!({"userId": "u1", "name": "Jane"}));
        record(&mut conn, &mut hlc, 1000, "AccountOpened",
            json!({"accountId": "a_inv", "name": "Inventory", "type": "asset",
                   "normal": "debit", "system_role": "inventory"}));
        record(&mut conn, &mut hlc, 1000, "ItemDefined",
            json!({"itemId": "i1", "sku": "SKU1", "name": "Widget", "unit": "ea"}));
        record(&mut conn, &mut hlc, 1000, "PartyCreated",
            json!({"partyId": "p1", "name": "Acme", "kind": "supplier"}));

        let uname: String = conn.query_row("SELECT name FROM users WHERE id='u1'", [], |r| r.get(0)).unwrap();
        assert_eq!(uname, "Jane");
        let (atype, bal): (String, i64) = conn.query_row(
            "SELECT type, balance_minor FROM accounts WHERE system_role = 'inventory'",
            [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(atype, "asset");
        assert_eq!(bal, 0);
        let active: i64 = conn.query_row("SELECT active FROM items WHERE id='i1'", [], |r| r.get(0)).unwrap();
        assert_eq!(active, 1);
        let kind: String = conn.query_row("SELECT kind FROM parties WHERE id='p1'", [], |r| r.get(0)).unwrap();
        assert_eq!(kind, "supplier");
    }

    #[test]
    fn updates_patch_only_changed_fields() {
        let mut conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("devA");

        record(&mut conn, &mut hlc, 1000, "ItemDefined",
            json!({"itemId": "i1", "sku": "SKU1", "name": "Widget", "unit": "ea"}));
        record(&mut conn, &mut hlc, 1000, "ItemUpdated",
            json!({"itemId": "i1", "changes": {"name": "Widget v2", "active": 0}}));

        let (name, sku, active): (String, String, i64) = conn.query_row(
            "SELECT name, sku, active FROM items WHERE id='i1'", [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert_eq!(name, "Widget v2");
        assert_eq!(sku, "SKU1", "unchanged field must survive patch");
        assert_eq!(active, 0);
    }

    #[test]
    fn post_line_moves_balance_by_normal_side() {
        let mut conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("devA");

        record(&mut conn, &mut hlc, 1000, "AccountOpened",
            json!({"accountId": "a_inv", "name": "Inventory", "type": "asset",
                   "normal": "debit", "system_role": "inventory"}));
        record(&mut conn, &mut hlc, 1000, "AccountOpened",
            json!({"accountId": "a_ap", "name": "Accounts Payable", "type": "liability",
                   "normal": "credit", "system_role": "accounts_payable"}));

        let tx = conn.transaction().unwrap();
        let inv = account_id_by_role(&tx, "inventory").unwrap();
        let ap = account_id_by_role(&tx, "accounts_payable").unwrap();
        post_line(&tx, "evX", "evX", 0, &inv, 1000, 0, "2026-01-05", None).unwrap();
        post_line(&tx, "evX", "evX", 1, &ap, 0, 1000, "2026-01-05", None).unwrap();
        tx.commit().unwrap();

        let inv_bal: i64 = conn.query_row(
            "SELECT balance_minor FROM accounts WHERE system_role='inventory'", [], |r| r.get(0)).unwrap();
        let ap_bal: i64 = conn.query_row(
            "SELECT balance_minor FROM accounts WHERE system_role='accounts_payable'", [], |r| r.get(0)).unwrap();
        assert_eq!(inv_bal, 1000, "debit-normal account rises on debit");
        assert_eq!(ap_bal, 1000, "credit-normal account rises on credit");

        let (d, c): (i64, i64) = conn.query_row(
            "SELECT SUM(debit_minor), SUM(credit_minor) FROM journal_lines WHERE txn_id='evX'",
            [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(d, c);
    }

    #[test]
    fn purchase_creates_lots_and_posts_inventory() {
        let mut conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("devA");
        seed_accounts(&mut conn, &mut hlc);
        record(&mut conn, &mut hlc, 1000, "ItemDefined",
            json!({"itemId": "i1", "sku": "S1", "name": "Widget", "unit": "ea"}));
        record(&mut conn, &mut hlc, 1000, "PartyCreated",
            json!({"partyId": "sup1", "name": "Acme", "kind": "supplier"}));

        record(&mut conn, &mut hlc, 1000, "PurchaseRecorded",
            json!({"purchaseId": "po1", "supplierId": "sup1", "date": "2026-01-05",
                   "terms": "credit",
                   "lines": [{"itemId": "i1", "qty": 10, "unitCostMinor": 100, "lotId": "lot1"}]}));

        let (recv, rem, cost): (i64, i64, i64) = conn.query_row(
            "SELECT qty_received, qty_remaining, unit_cost_minor FROM inventory_lots WHERE id='lot1'",
            [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert_eq!((recv, rem, cost), (10, 10, 100));

        let lot_value: i64 = conn.query_row(
            "SELECT COALESCE(SUM(qty_remaining * unit_cost_minor), 0) FROM inventory_lots", [], |r| r.get(0)).unwrap();
        let inv_bal: i64 = conn.query_row(
            "SELECT balance_minor FROM accounts WHERE system_role='inventory'", [], |r| r.get(0)).unwrap();
        assert_eq!(lot_value, 1000);
        assert_eq!(lot_value, inv_bal);

        let outstanding: i64 = conn.query_row(
            "SELECT outstanding_minor FROM purchases WHERE id='po1'", [], |r| r.get(0)).unwrap();
        let payable: i64 = conn.query_row(
            "SELECT payable_minor FROM party_balances WHERE party_id='sup1'", [], |r| r.get(0)).unwrap();
        assert_eq!(outstanding, 1000);
        assert_eq!(payable, 1000);
    }

    #[test]
    fn sale_freezes_profit_consumes_lots_and_reconciles() {
        let mut conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("devA");
        seed_accounts(&mut conn, &mut hlc);
        record(&mut conn, &mut hlc, 1000, "ItemDefined",
            json!({"itemId": "i1", "sku": "S1", "name": "Widget", "unit": "ea"}));
        record(&mut conn, &mut hlc, 1000, "PartyCreated",
            json!({"partyId": "cust1", "name": "Bob", "kind": "customer"}));
        record(&mut conn, &mut hlc, 1000, "PurchaseRecorded",
            json!({"purchaseId": "po1", "supplierId": null, "date": "2026-01-01", "terms": "cash",
                   "lines": [{"itemId": "i1", "qty": 10, "unitCostMinor": 100, "lotId": "lot1"}]}));
        record(&mut conn, &mut hlc, 1000, "SaleRecorded",
            json!({"saleId": "so1", "customerId": "cust1", "date": "2026-01-10", "terms": "credit",
                   "lines": [{"itemId": "i1", "qty": 4, "unitPriceMinor": 250,
                              "lotConsumption": [{"lotId": "lot1", "qtyTaken": 4, "unitCostMinor": 100}]}]}));

        let (rev, cogs): (i64, i64) = conn.query_row(
            "SELECT revenue_minor, cogs_minor FROM sale_lines WHERE sale_id='so1'", [],
            |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((rev, cogs), (1000, 400));

        let rem: i64 = conn.query_row("SELECT qty_remaining FROM inventory_lots WHERE id='lot1'", [], |r| r.get(0)).unwrap();
        assert_eq!(rem, 6);

        let lot_value: i64 = conn.query_row(
            "SELECT SUM(qty_remaining * unit_cost_minor) FROM inventory_lots", [], |r| r.get(0)).unwrap();
        let inv_bal: i64 = conn.query_row(
            "SELECT balance_minor FROM accounts WHERE system_role='inventory'", [], |r| r.get(0)).unwrap();
        assert_eq!(lot_value, 600);
        assert_eq!(inv_bal, 600);

        let outstanding: i64 = conn.query_row("SELECT outstanding_minor FROM sales WHERE id='so1'", [], |r| r.get(0)).unwrap();
        let recv: i64 = conn.query_row("SELECT receivable_minor FROM party_balances WHERE party_id='cust1'", [], |r| r.get(0)).unwrap();
        assert_eq!(outstanding, 1000);
        assert_eq!(recv, 1000);

        let (d, c): (i64, i64) = conn.query_row(
            "SELECT SUM(debit_minor), SUM(credit_minor) FROM journal_lines WHERE txn_id='so1'", [],
            |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(d, c);
    }

    #[test]
    fn payment_received_allocates_and_holds_prepayment() {
        let mut conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("devA");
        seed_accounts(&mut conn, &mut hlc);
        record(&mut conn, &mut hlc, 1000, "ItemDefined",
            json!({"itemId": "i1", "sku": "S1", "name": "W", "unit": "ea"}));
        record(&mut conn, &mut hlc, 1000, "PartyCreated",
            json!({"partyId": "cust1", "name": "Bob", "kind": "customer"}));
        record(&mut conn, &mut hlc, 1000, "PurchaseRecorded",
            json!({"purchaseId": "po1", "supplierId": null, "date": "2026-01-01", "terms": "cash",
                   "lines": [{"itemId": "i1", "qty": 10, "unitCostMinor": 100, "lotId": "lot1"}]}));
        record(&mut conn, &mut hlc, 1000, "SaleRecorded",
            json!({"saleId": "so1", "customerId": "cust1", "date": "2026-01-10", "terms": "credit",
                   "lines": [{"itemId": "i1", "qty": 4, "unitPriceMinor": 250,
                              "lotConsumption": [{"lotId": "lot1", "qtyTaken": 4, "unitCostMinor": 100}]}]}));
        record(&mut conn, &mut hlc, 1000, "PaymentReceived",
            json!({"paymentId": "pay1", "customerId": "cust1", "amountMinor": 1200, "date": "2026-01-15",
                   "allocations": [{"saleId": "so1", "amountMinor": 1000}]}));

        let outstanding: i64 = conn.query_row("SELECT outstanding_minor FROM sales WHERE id='so1'", [], |r| r.get(0)).unwrap();
        assert_eq!(outstanding, 0, "invoice fully settled");
        let (recv, ucr): (i64, i64) = conn.query_row(
            "SELECT receivable_minor, unallocated_cr_minor FROM party_balances WHERE party_id='cust1'",
            [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(recv, 0);
        assert_eq!(ucr, 200, "overpayment held as prepayment credit");
        let (dir, amt): (String, i64) = conn.query_row(
            "SELECT direction, amount_minor FROM payments WHERE id='pay1'", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((dir.as_str(), amt), ("in", 1200));
        let ar_bal: i64 = conn.query_row(
            "SELECT balance_minor FROM accounts WHERE system_role='accounts_receivable'", [], |r| r.get(0)).unwrap();
        assert_eq!(ar_bal, -200);
    }

    #[test]
    fn payment_allocated_applies_held_credit_without_journal() {
        let mut conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("devA");
        seed_accounts(&mut conn, &mut hlc);
        record(&mut conn, &mut hlc, 1000, "ItemDefined",
            json!({"itemId": "i1", "sku": "S1", "name": "W", "unit": "ea"}));
        record(&mut conn, &mut hlc, 1000, "PartyCreated",
            json!({"partyId": "cust1", "name": "Bob", "kind": "customer"}));
        record(&mut conn, &mut hlc, 1000, "PaymentReceived",
            json!({"paymentId": "pay1", "customerId": "cust1", "amountMinor": 500, "date": "2026-01-01",
                   "allocations": []}));
        record(&mut conn, &mut hlc, 1000, "PurchaseRecorded",
            json!({"purchaseId": "po1", "supplierId": null, "date": "2026-01-02", "terms": "cash",
                   "lines": [{"itemId": "i1", "qty": 5, "unitCostMinor": 100, "lotId": "lot1"}]}));
        record(&mut conn, &mut hlc, 1000, "SaleRecorded",
            json!({"saleId": "so1", "customerId": "cust1", "date": "2026-01-03", "terms": "credit",
                   "lines": [{"itemId": "i1", "qty": 5, "unitPriceMinor": 100,
                              "lotConsumption": [{"lotId": "lot1", "qtyTaken": 5, "unitCostMinor": 100}]}]}));
        let jl_before: i64 = conn.query_row("SELECT COUNT(*) FROM journal_lines", [], |r| r.get(0)).unwrap();
        record(&mut conn, &mut hlc, 1000, "PaymentAllocated",
            json!({"paymentId": "pay1", "partyId": "cust1", "date": "2026-01-04",
                   "allocations": [{"targetId": "so1", "targetType": "sale", "amountMinor": 500}]}));

        let jl_after: i64 = conn.query_row("SELECT COUNT(*) FROM journal_lines", [], |r| r.get(0)).unwrap();
        assert_eq!(jl_before, jl_after, "PaymentAllocated posts no journal lines");
        let outstanding: i64 = conn.query_row("SELECT outstanding_minor FROM sales WHERE id='so1'", [], |r| r.get(0)).unwrap();
        let (recv, ucr): (i64, i64) = conn.query_row(
            "SELECT receivable_minor, unallocated_cr_minor FROM party_balances WHERE party_id='cust1'",
            [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(outstanding, 0);
        assert_eq!(recv, 0);
        assert_eq!(ucr, 0, "held credit consumed");
        let pay_count: i64 = conn.query_row("SELECT COUNT(*) FROM payments", [], |r| r.get(0)).unwrap();
        assert_eq!(pay_count, 1);
    }

    #[test]
    fn expense_and_transfer_post_correctly() {
        let mut conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("devA");
        seed_accounts(&mut conn, &mut hlc);

        record(&mut conn, &mut hlc, 1000, "ExpenseRecorded",
            json!({"expenseId": "ex1", "accountId": "acct_rent", "amountMinor": 300,
                   "date": "2026-02-01", "terms": "cash", "memo": "Feb rent"}));
        let rent_bal: i64 = conn.query_row("SELECT balance_minor FROM accounts WHERE system_role='rent'", [], |r| r.get(0)).unwrap();
        let bank_bal: i64 = conn.query_row("SELECT balance_minor FROM accounts WHERE system_role='bank'", [], |r| r.get(0)).unwrap();
        assert_eq!(rent_bal, 300, "expense (debit-normal) rises");
        assert_eq!(bank_bal, -300, "bank paid out");
        let (ex_amt, ex_memo): (i64, String) = conn.query_row(
            "SELECT amount_minor, memo FROM expenses WHERE id='ex1'", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((ex_amt, ex_memo.as_str()), (300, "Feb rent"));

        record(&mut conn, &mut hlc, 1000, "PartyCreated",
            json!({"partyId": "sup1", "name": "Landlord Co", "kind": "supplier"}));
        record(&mut conn, &mut hlc, 1000, "ExpenseRecorded",
            json!({"expenseId": "ex2", "accountId": "acct_rent", "amountMinor": 400,
                   "date": "2026-02-01", "terms": "credit", "supplierId": "sup1", "memo": "Feb rent on account"}));
        let ap_bal: i64 = conn.query_row("SELECT balance_minor FROM accounts WHERE system_role='accounts_payable'", [], |r| r.get(0)).unwrap();
        let payable: i64 = conn.query_row("SELECT payable_minor FROM party_balances WHERE party_id='sup1'", [], |r| r.get(0)).unwrap();
        assert_eq!(ap_bal, 400, "credit expense credits A/P GL");
        assert_eq!(payable, 400, "credit expense raises supplier payable");

        record(&mut conn, &mut hlc, 1000, "TransferRecorded",
            json!({"transferId": "tr1", "fromAccountId": "acct_cash", "toAccountId": "acct_bank",
                   "amountMinor": 500, "date": "2026-02-02"}));
        let cash_bal: i64 = conn.query_row("SELECT balance_minor FROM accounts WHERE system_role='cash'", [], |r| r.get(0)).unwrap();
        let bank_bal2: i64 = conn.query_row("SELECT balance_minor FROM accounts WHERE system_role='bank'", [], |r| r.get(0)).unwrap();
        assert_eq!(cash_bal, -500, "credited (from) side");
        assert_eq!(bank_bal2, 200, "-300 + 500 debited (to) side");
    }

    #[test]
    fn inventory_adjusted_and_found_move_lots_and_gl() {
        let mut conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("devA");
        seed_accounts(&mut conn, &mut hlc);
        record(&mut conn, &mut hlc, 1000, "ItemDefined",
            json!({"itemId": "i1", "sku": "S1", "name": "W", "unit": "ea"}));
        record(&mut conn, &mut hlc, 1000, "PurchaseRecorded",
            json!({"purchaseId": "po1", "supplierId": null, "date": "2026-01-01", "terms": "cash",
                   "lines": [{"itemId": "i1", "qty": 10, "unitCostMinor": 100, "lotId": "lot1"}]}));

        record(&mut conn, &mut hlc, 1000, "InventoryAdjusted",
            json!({"adjustmentId": "adj1", "date": "2026-01-05",
                   "lines": [{"itemId": "i1", "lotId": "lot1", "qtyDelta": -2,
                              "reasonCode": "damage", "expenseAccountId": "acct_shrinkage"}]}));
        let rem: i64 = conn.query_row("SELECT qty_remaining FROM inventory_lots WHERE id='lot1'", [], |r| r.get(0)).unwrap();
        assert_eq!(rem, 8);
        let shrink: i64 = conn.query_row("SELECT balance_minor FROM accounts WHERE system_role='shrinkage'", [], |r| r.get(0)).unwrap();
        assert_eq!(shrink, 200);

        record(&mut conn, &mut hlc, 1000, "InventoryFound",
            json!({"foundId": "f1", "date": "2026-01-06",
                   "lines": [{"itemId": "i1", "lotId": "lot2", "qty": 3, "unitCostMinor": 90,
                              "acquiredAt": "2026-01-06"}]}));
        let (recv2, cost2): (i64, i64) = conn.query_row(
            "SELECT qty_received, unit_cost_minor FROM inventory_lots WHERE id='lot2'", [],
            |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((recv2, cost2), (3, 90));
        let gain: i64 = conn.query_row("SELECT balance_minor FROM accounts WHERE system_role='inventory_gain'", [], |r| r.get(0)).unwrap();
        assert_eq!(gain, 270);

        let lot_value: i64 = conn.query_row("SELECT SUM(qty_remaining*unit_cost_minor) FROM inventory_lots", [], |r| r.get(0)).unwrap();
        let inv_bal: i64 = conn.query_row("SELECT balance_minor FROM accounts WHERE system_role='inventory'", [], |r| r.get(0)).unwrap();
        assert_eq!(lot_value, 1070);
        assert_eq!(inv_bal, 1070);
    }

    #[test]
    fn opening_balances_sets_gl_and_creates_lots() {
        let mut conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("devA");
        seed_accounts(&mut conn, &mut hlc);
        record(&mut conn, &mut hlc, 1000, "ItemDefined",
            json!({"itemId": "i1", "sku": "S1", "name": "W", "unit": "ea"}));

        record(&mut conn, &mut hlc, 1000, "OpeningBalancesRecorded",
            json!({"date": "2026-01-01",
                   "accountBalances": [
                       {"accountId": "acct_inventory", "debitMinor": 1000, "creditMinor": 0},
                       {"accountId": "acct_bank", "debitMinor": 500, "creditMinor": 0},
                       {"accountId": "acct_owner_capital", "debitMinor": 0, "creditMinor": 1500}],
                   "lots": [
                       {"itemId": "i1", "lotId": "lotOB", "qty": 10, "unitCostMinor": 100,
                        "acquiredAt": "2025-12-01"}]}));

        let inv_bal: i64 = conn.query_row("SELECT balance_minor FROM accounts WHERE system_role='inventory'", [], |r| r.get(0)).unwrap();
        let cap_bal: i64 = conn.query_row("SELECT balance_minor FROM accounts WHERE system_role='owner_capital'", [], |r| r.get(0)).unwrap();
        assert_eq!(inv_bal, 1000);
        assert_eq!(cap_bal, 1500);

        let (rem, cost): (i64, i64) = conn.query_row(
            "SELECT qty_remaining, unit_cost_minor FROM inventory_lots WHERE id='lotOB'", [],
            |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((rem, cost), (10, 100));
        let lot_value: i64 = conn.query_row("SELECT SUM(qty_remaining*unit_cost_minor) FROM inventory_lots", [], |r| r.get(0)).unwrap();
        assert_eq!(lot_value, inv_bal);
    }

    #[test]
    fn sale_return_restores_inventory_and_reduces_receivable() {
        let mut conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("devA");
        seed_accounts(&mut conn, &mut hlc);
        record(&mut conn, &mut hlc, 1000, "ItemDefined",
            json!({"itemId": "i1", "sku": "S1", "name": "W", "unit": "ea"}));
        record(&mut conn, &mut hlc, 1000, "PartyCreated",
            json!({"partyId": "cust1", "name": "Bob", "kind": "customer"}));
        record(&mut conn, &mut hlc, 1000, "PurchaseRecorded",
            json!({"purchaseId": "po1", "supplierId": null, "date": "2026-01-01", "terms": "cash",
                   "lines": [{"itemId": "i1", "qty": 10, "unitCostMinor": 100, "lotId": "lot1"}]}));
        record(&mut conn, &mut hlc, 1000, "SaleRecorded",
            json!({"saleId": "so1", "customerId": "cust1", "date": "2026-01-10", "terms": "credit",
                   "lines": [{"itemId": "i1", "qty": 4, "unitPriceMinor": 250,
                              "lotConsumption": [{"lotId": "lot1", "qtyTaken": 4, "unitCostMinor": 100}]}]}));
        record(&mut conn, &mut hlc, 1000, "SaleReturnRecorded",
            json!({"returnId": "ret1", "originalSaleId": "so1", "date": "2026-01-12",
                   "lines": [{"itemId": "i1", "qty": 1, "unitPriceMinor": 250,
                              "lotReturns": [{"lotId": "lot1", "qtyReturned": 1, "unitCostMinor": 100}]}]}));

        let rem: i64 = conn.query_row("SELECT qty_remaining FROM inventory_lots WHERE id='lot1'", [], |r| r.get(0)).unwrap();
        assert_eq!(rem, 7, "1 unit restored to lot");
        let (rr, cr): (i64, i64) = conn.query_row(
            "SELECT revenue_reversed_minor, cost_restored_minor FROM returns WHERE id='ret1'", [],
            |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((rr, cr), (250, 100));
        let outstanding: i64 = conn.query_row("SELECT outstanding_minor FROM sales WHERE id='so1'", [], |r| r.get(0)).unwrap();
        let recv: i64 = conn.query_row("SELECT receivable_minor FROM party_balances WHERE party_id='cust1'", [], |r| r.get(0)).unwrap();
        assert_eq!(outstanding, 750, "1000 - 250 returned");
        assert_eq!(recv, 750);
    }

    #[test]
    fn transaction_reversed_unwinds_sale() {
        let mut conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("devA");
        seed_accounts(&mut conn, &mut hlc);
        record(&mut conn, &mut hlc, 1000, "ItemDefined",
            json!({"itemId": "i1", "sku": "S1", "name": "W", "unit": "ea"}));
        record(&mut conn, &mut hlc, 1000, "PartyCreated",
            json!({"partyId": "cust1", "name": "Bob", "kind": "customer"}));
        record(&mut conn, &mut hlc, 1000, "PurchaseRecorded",
            json!({"purchaseId": "po1", "supplierId": null, "date": "2026-01-01", "terms": "cash",
                   "lines": [{"itemId": "i1", "qty": 10, "unitCostMinor": 100, "lotId": "lot1"}]}));
        let sale = record(&mut conn, &mut hlc, 1000, "SaleRecorded",
            json!({"saleId": "so1", "customerId": "cust1", "date": "2026-01-10", "terms": "credit",
                   "lines": [{"itemId": "i1", "qty": 4, "unitPriceMinor": 250,
                              "lotConsumption": [{"lotId": "lot1", "qtyTaken": 4, "unitCostMinor": 100}]}]}));

        let rev = record(&mut conn, &mut hlc, 1000, "TransactionReversed",
            json!({"targetEventId": sale.id, "targetType": "SaleRecorded", "reason": "entry error",
                   "reversalJournalLines": [
                       {"accountId": "acct_sales", "debitMinor": 1000, "creditMinor": 0},
                       {"accountId": "acct_accounts_receivable", "debitMinor": 0, "creditMinor": 1000},
                       {"accountId": "acct_inventory", "debitMinor": 400, "creditMinor": 0},
                       {"accountId": "acct_cogs", "debitMinor": 0, "creditMinor": 400}]}));

        let (rev_lines, rev_d, rev_c): (i64, i64, i64) = conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(debit_minor),0), COALESCE(SUM(credit_minor),0)
             FROM journal_lines WHERE txn_id = ?1", [&rev.id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
        assert_eq!(rev_lines, 4);
        assert_eq!(rev_d, rev_c);
        assert_eq!(rev_d, 1400);

        let rem: i64 = conn.query_row("SELECT qty_remaining FROM inventory_lots WHERE id='lot1'", [], |r| r.get(0)).unwrap();
        assert_eq!(rem, 10);
        let sales_bal: i64 = conn.query_row("SELECT balance_minor FROM accounts WHERE system_role='sales'", [], |r| r.get(0)).unwrap();
        let inv_bal: i64 = conn.query_row("SELECT balance_minor FROM accounts WHERE system_role='inventory'", [], |r| r.get(0)).unwrap();
        assert_eq!(sales_bal, 0);
        assert_eq!(inv_bal, 1000);

        let outstanding: i64 = conn.query_row("SELECT outstanding_minor FROM sales WHERE id='so1'", [], |r| r.get(0)).unwrap();
        let recv: i64 = conn.query_row("SELECT receivable_minor FROM party_balances WHERE party_id='cust1'", [], |r| r.get(0)).unwrap();
        let ar_bal: i64 = conn.query_row("SELECT balance_minor FROM accounts WHERE system_role='accounts_receivable'", [], |r| r.get(0)).unwrap();
        assert_eq!(outstanding, 0);
        assert_eq!(recv, 0);
        assert_eq!(ar_bal, 0);

        let reversed: i64 = conn.query_row("SELECT reversed FROM sales WHERE id='so1'", [], |r| r.get(0)).unwrap();
        assert_eq!(reversed, 1);
        let sale_line_count: i64 = conn.query_row("SELECT COUNT(*) FROM sale_lines WHERE sale_id='so1'", [], |r| r.get(0)).unwrap();
        assert_eq!(sale_line_count, 1, "sale_lines retained for audit");
    }

    fn full_dump(conn: &Connection) -> String {
        let queries: &[(&str, &str)] = &[
            ("users", "SELECT quote(id)||'|'||json(doc)||'|'||quote(created_at) FROM users ORDER BY id"),
            ("accounts", "SELECT quote(id)||'|'||json(doc)||'|'||quote(balance_minor) FROM accounts ORDER BY id"),
            ("items", "SELECT quote(id)||'|'||json(doc) FROM items ORDER BY id"),
            ("parties", "SELECT quote(id)||'|'||json(doc) FROM parties ORDER BY id"),
            ("inventory_lots", "SELECT quote(id)||'|'||quote(item_id)||'|'||quote(source_event_id)||'|'||quote(purchase_id)||'|'||quote(unit_cost_minor)||'|'||quote(qty_received)||'|'||quote(qty_remaining)||'|'||quote(acquired_at)||'|'||quote(supplier_id) FROM inventory_lots ORDER BY id"),
            ("journal_lines", "SELECT quote(id)||'|'||quote(event_id)||'|'||quote(txn_id)||'|'||quote(account_id)||'|'||quote(debit_minor)||'|'||quote(credit_minor)||'|'||quote(date)||'|'||quote(memo) FROM journal_lines ORDER BY id"),
            ("sales", "SELECT quote(id)||'|'||quote(event_id)||'|'||quote(customer_id)||'|'||quote(date)||'|'||quote(terms)||'|'||quote(total_minor)||'|'||quote(outstanding_minor)||'|'||quote(reversed) FROM sales ORDER BY id"),
            ("sale_lines", "SELECT quote(id)||'|'||quote(sale_id)||'|'||quote(item_id)||'|'||quote(qty)||'|'||quote(unit_price_minor)||'|'||quote(revenue_minor)||'|'||quote(cogs_minor)||'|'||quote(date) FROM sale_lines ORDER BY id"),
            ("lot_consumptions", "SELECT quote(id)||'|'||quote(sale_line_id)||'|'||quote(lot_id)||'|'||quote(qty_taken)||'|'||quote(unit_cost_minor) FROM lot_consumptions ORDER BY id"),
            ("purchases", "SELECT quote(id)||'|'||quote(event_id)||'|'||quote(supplier_id)||'|'||quote(date)||'|'||quote(terms)||'|'||quote(total_minor)||'|'||quote(outstanding_minor)||'|'||quote(reversed) FROM purchases ORDER BY id"),
            ("purchase_lines", "SELECT quote(id)||'|'||quote(purchase_id)||'|'||quote(item_id)||'|'||quote(qty)||'|'||quote(unit_cost_minor)||'|'||quote(lot_id) FROM purchase_lines ORDER BY id"),
            ("payments", "SELECT quote(id)||'|'||quote(event_id)||'|'||quote(party_id)||'|'||quote(direction)||'|'||quote(amount_minor)||'|'||quote(date) FROM payments ORDER BY id"),
            ("payment_allocations", "SELECT quote(id)||'|'||quote(event_id)||'|'||quote(payment_id)||'|'||quote(target_id)||'|'||quote(target_type)||'|'||quote(amount_minor) FROM payment_allocations ORDER BY id"),
            ("party_balances", "SELECT quote(party_id)||'|'||quote(receivable_minor)||'|'||quote(payable_minor)||'|'||quote(unallocated_cr_minor)||'|'||quote(unallocated_dr_minor) FROM party_balances ORDER BY party_id"),
            ("returns", "SELECT quote(id)||'|'||quote(event_id)||'|'||quote(return_type)||'|'||quote(original_id)||'|'||quote(date)||'|'||quote(revenue_reversed_minor)||'|'||quote(cost_restored_minor) FROM returns ORDER BY id"),
            ("return_lines", "SELECT quote(id)||'|'||quote(return_id)||'|'||quote(item_id)||'|'||quote(qty)||'|'||quote(unit_price_minor)||'|'||quote(unit_cost_minor)||'|'||quote(lot_id) FROM return_lines ORDER BY id"),
            ("expenses", "SELECT quote(id)||'|'||quote(event_id)||'|'||quote(account_id)||'|'||quote(amount_minor)||'|'||quote(date)||'|'||quote(memo)||'|'||quote(terms) FROM expenses ORDER BY id"),
        ];
        let mut out = String::new();
        for (name, sql) in queries {
            out.push_str("== ");
            out.push_str(name);
            out.push('\n');
            let mut stmt = conn.prepare(sql).unwrap();
            let rows: Vec<String> = stmt
                .query_map([], |r| r.get::<_, String>(0)).unwrap()
                .collect::<rusqlite::Result<_>>().unwrap();
            for row in rows {
                out.push_str(&row);
                out.push('\n');
            }
        }
        out
    }

    #[test]
    fn rebuild_is_deterministic_and_sets_cursor() {
        let mut conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("devA");
        seed_accounts(&mut conn, &mut hlc);
        record(&mut conn, &mut hlc, 1000, "ItemDefined",
            json!({"itemId": "i1", "sku": "S1", "name": "W", "unit": "ea"}));
        record(&mut conn, &mut hlc, 1000, "PartyCreated",
            json!({"partyId": "cust1", "name": "Bob", "kind": "customer"}));
        record(&mut conn, &mut hlc, 1000, "PurchaseRecorded",
            json!({"purchaseId": "po1", "supplierId": null, "date": "2026-01-01", "terms": "cash",
                   "lines": [{"itemId": "i1", "qty": 10, "unitCostMinor": 100, "lotId": "lot1"}]}));
        let sale = record(&mut conn, &mut hlc, 1000, "SaleRecorded",
            json!({"saleId": "so1", "customerId": "cust1", "date": "2026-01-10", "terms": "credit",
                   "lines": [{"itemId": "i1", "qty": 4, "unitPriceMinor": 250,
                              "lotConsumption": [{"lotId": "lot1", "qtyTaken": 4, "unitCostMinor": 100}]}]}));
        record(&mut conn, &mut hlc, 1000, "PaymentReceived",
            json!({"paymentId": "pay1", "customerId": "cust1", "amountMinor": 500, "date": "2026-01-11",
                   "allocations": [{"saleId": "so1", "amountMinor": 500}]}));
        record(&mut conn, &mut hlc, 1000, "SaleReturnRecorded",
            json!({"returnId": "ret1", "originalSaleId": "so1", "date": "2026-01-12",
                   "lines": [{"itemId": "i1", "qty": 1, "unitPriceMinor": 250,
                              "lotReturns": [{"lotId": "lot1", "qtyReturned": 1, "unitCostMinor": 100}]}]}));
        let sale2 = record(&mut conn, &mut hlc, 1000, "SaleRecorded",
            json!({"saleId": "so2", "customerId": null, "date": "2026-01-13", "terms": "cash",
                   "lines": [{"itemId": "i1", "qty": 1, "unitPriceMinor": 250,
                              "lotConsumption": [{"lotId": "lot1", "qtyTaken": 1, "unitCostMinor": 100}]}]}));
        record(&mut conn, &mut hlc, 1000, "TransactionReversed",
            json!({"targetEventId": sale2.id, "targetType": "SaleRecorded", "reason": "oops",
                   "reversalJournalLines": [
                       {"accountId": "acct_sales", "debitMinor": 250, "creditMinor": 0},
                       {"accountId": "acct_bank", "debitMinor": 0, "creditMinor": 250},
                       {"accountId": "acct_inventory", "debitMinor": 100, "creditMinor": 0},
                       {"accountId": "acct_cogs", "debitMinor": 0, "creditMinor": 100}]}));
        let _ = sale;

        let before = full_dump(&conn);
        let last_hlc: String = conn.query_row("SELECT MAX(hlc) FROM events", [], |r| r.get(0)).unwrap();

        rebuild(&mut conn).unwrap();

        let after = full_dump(&conn);
        assert_eq!(before, after, "rebuild must reproduce byte-identical projected state");

        let cursor: String = conn.query_row(
            "SELECT last_hlc FROM projection_cursor WHERE projection='main'", [], |r| r.get(0)).unwrap();
        assert_eq!(cursor, last_hlc);

        let sales: i64 = conn.query_row("SELECT COUNT(*) FROM sales", [], |r| r.get(0)).unwrap();
        let lots: i64 = conn.query_row("SELECT COUNT(*) FROM inventory_lots", [], |r| r.get(0)).unwrap();
        assert_eq!((sales, lots), (2, 1));
        let reversed: i64 = conn.query_row("SELECT reversed FROM sales WHERE id='so2'", [], |r| r.get(0)).unwrap();
        assert_eq!(reversed, 1);
    }
}
