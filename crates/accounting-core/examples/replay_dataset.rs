//! Replay a generated dataset (JSONL of commands) through the REAL Plan 3
//! command handlers on a fresh in-memory DB, then run the §7 reconciliation
//! checks. This is the authoritative proof that a dataset is a clean load —
//! it exercises the actual guards, projector, and checks, not a model.
//!
//! Usage: cargo run --example replay_dataset -- <path/to/dataset.jsonl>
//!
//! Prints per-command throughput and the reconciliation result; exits non-zero
//! on the first rejected command or any failing check.

use accounting_core::commands::movement::{handle_expense_recorded, handle_transfer_recorded};
use accounting_core::commands::payment::{handle_payment_made, handle_payment_received, AllocInput};
use accounting_core::commands::purchase::{handle_purchase_recorded, PurchaseLineInput};
use accounting_core::commands::reversal::handle_transaction_reversed;
use accounting_core::commands::sale::{
    handle_sale_recorded, handle_sale_return_recorded, SaleLineInput, SaleReturnItemInput,
};
use accounting_core::commands::setup::{handle_item_defined, handle_party_created};
use accounting_core::commands::{CommandContext, CommandError};
use accounting_core::db::open_in_memory_with_schema;
use accounting_core::genesis::run_genesis;
use accounting_core::hlc::Hlc;
use accounting_core::projectors::rebuild;
use accounting_core::reconciliation::{all_passed, run_all_checks};
use serde_json::Value;
use std::time::Instant;

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or_default().to_string()
}
fn i(v: &Value, k: &str) -> i64 {
    v.get(k).and_then(Value::as_i64).unwrap_or(0)
}
fn opt(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(|x| x.to_string())
}

fn apply(ctx: &mut CommandContext, cmd: &str, a: &Value) -> Result<(), CommandError> {
    match cmd {
        "item_defined" => {
            handle_item_defined(ctx, &s(a, "item_id"), &s(a, "sku"), &s(a, "name"), &s(a, "unit"))?;
        }
        "party_created" => {
            handle_party_created(ctx, &s(a, "party_id"), &s(a, "name"), &s(a, "kind"))?;
        }
        "purchase_recorded" => {
            let lines = a["lines"].as_array().unwrap().iter().map(|l| PurchaseLineInput {
                item_id: s(l, "item_id"),
                qty: i(l, "qty"),
                unit_cost_minor: i(l, "unit_cost_minor"),
            }).collect();
            handle_purchase_recorded(ctx, &s(a, "purchase_id"), &s(a, "supplier_id"),
                &s(a, "date"), &s(a, "terms"), lines)?;
        }
        "sale_recorded" => {
            let lines = a["lines"].as_array().unwrap().iter().map(|l| {
                let picks = l.get("lot_picks").and_then(|p| p.as_array()).map(|arr| {
                    arr.iter().map(|p| (s(p, "lot_id"), i(p, "qty"))).collect()
                });
                SaleLineInput {
                    item_id: s(l, "item_id"),
                    qty: i(l, "qty"),
                    unit_price_minor: i(l, "unit_price_minor"),
                    lot_picks: picks,
                }
            }).collect();
            handle_sale_recorded(ctx, &s(a, "sale_id"), &s(a, "customer_id"),
                &s(a, "date"), &s(a, "terms"), lines)?;
        }
        "sale_return_recorded" => {
            let lines = a["lines"].as_array().unwrap().iter().map(|l| SaleReturnItemInput {
                item_id: s(l, "item_id"),
                lot_returns: l["lot_returns"].as_array().unwrap().iter()
                    .map(|pair| {
                        let p = pair.as_array().unwrap();
                        (p[0].as_str().unwrap().to_string(), p[1].as_i64().unwrap())
                    }).collect(),
            }).collect();
            handle_sale_return_recorded(ctx, &s(a, "return_id"), &s(a, "original_sale_id"),
                &s(a, "date"), lines)?;
        }
        "payment_received" => {
            let allocs = alloc_inputs(a);
            handle_payment_received(ctx, &s(a, "payment_id"), &s(a, "customer_id"),
                i(a, "amount_minor"), &s(a, "date"), allocs)?;
        }
        "payment_made" => {
            let allocs = alloc_inputs(a);
            handle_payment_made(ctx, &s(a, "payment_id"), &s(a, "supplier_id"),
                i(a, "amount_minor"), &s(a, "date"), allocs)?;
        }
        "expense_recorded" => {
            let sup = opt(a, "supplier_id");
            let memo = opt(a, "memo");
            handle_expense_recorded(ctx, &s(a, "expense_id"), &s(a, "account_id"),
                i(a, "amount_minor"), &s(a, "date"), &s(a, "terms"),
                sup.as_deref(), memo.as_deref())?;
        }
        "transfer_recorded" => {
            let memo = opt(a, "memo");
            handle_transfer_recorded(ctx, &s(a, "transfer_id"), &s(a, "from_account_id"),
                &s(a, "to_account_id"), i(a, "amount_minor"), &s(a, "date"), memo.as_deref())?;
        }
        "transaction_reversed" => {
            handle_transaction_reversed(ctx, &s(a, "target_event_id"), &s(a, "reason"))?;
        }
        other => return Err(CommandError::Validation(format!("unknown command: {other}"))),
    }
    Ok(())
}

fn alloc_inputs(a: &Value) -> Vec<AllocInput> {
    a["allocations"].as_array().unwrap().iter().map(|al| AllocInput {
        target_id: s(al, "target_id"),
        target_type: s(al, "target_type"),
        amount_minor: i(al, "amount_minor"),
    }).collect()
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: replay_dataset <file.jsonl>");
    let text = std::fs::read_to_string(&path).expect("read dataset");

    let mut conn = open_in_memory_with_schema().unwrap();
    let mut hlc = Hlc::new("deviceA");
    run_genesis(&conn, &mut hlc, 1000, "deviceA", "owner-1", "Owner").unwrap();
    rebuild(&mut conn).unwrap();

    let start = Instant::now();
    let mut applied = 0usize;
    {
        let mut ctx = CommandContext {
            conn: &mut conn, hlc: &mut hlc, physical_now: 1000,
            device_id: "deviceA".into(), user_id: "owner-1".into(),
        };
        for (n, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let obj: Value = serde_json::from_str(line).expect("parse line");
            let cmd = obj["cmd"].as_str().unwrap();
            if let Err(e) = apply(&mut ctx, cmd, &obj["args"]) {
                eprintln!("REJECTED at line {}: {cmd}: {e}", n + 1);
                std::process::exit(1);
            }
            applied += 1;
        }
    }
    let load = start.elapsed();

    let checks = run_all_checks(&conn).unwrap();
    let ok = all_passed(&checks);

    let events: i64 = conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();
    let jl: i64 = conn.query_row("SELECT COUNT(*) FROM journal_lines", [], |r| r.get(0)).unwrap();
    let lots: i64 = conn.query_row("SELECT COUNT(*) FROM inventory_lots", [], |r| r.get(0)).unwrap();

    println!("dataset : {path}");
    println!("applied : {applied} commands, 0 rejected");
    println!("events  : {events} (incl. genesis) | journal_lines: {jl} | lots: {lots}");
    println!("load    : {:.3}s ({:.0} cmd/s)", load.as_secs_f64(),
             applied as f64 / load.as_secs_f64().max(1e-9));
    println!("checks  : {}", if ok { "ALL PASS" } else { "FAILED" });
    for c in &checks {
        if !ok {
            println!("  {} => {:?}", c.name, c.outcome);
        }
    }
    if !ok {
        std::process::exit(2);
    }
}
