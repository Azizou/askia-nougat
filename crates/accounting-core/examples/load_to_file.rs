//! Load a dataset into a FILE-BACKED database (the same one the Tauri app uses).
//! Usage: cargo run --example load_to_file --release -- <db_path> <dataset.jsonl>
//!
//! If the DB already has events, it skips genesis and loads on top.
//! Runs reconciliation checks at the end.

use accounting_core::commands::movement::handle_expense_recorded;
use accounting_core::commands::payment::{handle_payment_made, handle_payment_received, AllocInput};
use accounting_core::commands::purchase::{handle_purchase_recorded, PurchaseLineInput};
use accounting_core::commands::sale::{
    handle_sale_recorded, handle_sale_return_recorded, SaleLineInput, SaleReturnItemInput,
};
use accounting_core::commands::setup::{handle_item_defined, handle_party_created};
use accounting_core::commands::{CommandContext, CommandError};
use accounting_core::db::apply_schema;
use accounting_core::genesis::run_genesis;
use accounting_core::hlc::{rehydrate_from_log, Hlc};
use accounting_core::projectors::rebuild;
use accounting_core::reconciliation::{all_passed, run_all_checks};
use rusqlite::Connection;
use serde_json::Value;
use std::fs;
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
                item_id: s(l, "item_id"), qty: i(l, "qty"), unit_cost_minor: i(l, "unit_cost_minor"),
            }).collect();
            handle_purchase_recorded(ctx, &s(a, "purchase_id"), &s(a, "supplier_id"), &s(a, "date"), &s(a, "terms"), lines)?;
        }
        "sale_recorded" => {
            let lines = a["lines"].as_array().unwrap().iter().map(|l| SaleLineInput {
                item_id: s(l, "item_id"), qty: i(l, "qty"), unit_price_minor: i(l, "unit_price_minor"),
                lot_picks: None,
            }).collect();
            handle_sale_recorded(ctx, &s(a, "sale_id"), &s(a, "customer_id"), &s(a, "date"), &s(a, "terms"), lines)?;
        }
        "sale_return_recorded" => {
            let lines = a["lines"].as_array().unwrap().iter().map(|l| {
                let lot_returns = l["lot_returns"].as_array().unwrap().iter().map(|lr| {
                    (s(lr, "lot_id"), i(lr, "qty_returned"))
                }).collect();
                SaleReturnItemInput { item_id: s(l, "item_id"), lot_returns }
            }).collect();
            handle_sale_return_recorded(ctx, &s(a, "return_id"), &s(a, "original_sale_id"), &s(a, "date"), lines)?;
        }
        "payment_received" => {
            let allocs = a["allocations"].as_array().unwrap().iter().map(|al| AllocInput {
                target_id: s(al, "target_id"), target_type: s(al, "target_type"), amount_minor: i(al, "amount_minor"),
            }).collect();
            handle_payment_received(ctx, &s(a, "payment_id"), &s(a, "customer_id"), i(a, "amount_minor"), &s(a, "date"), allocs)?;
        }
        "payment_made" => {
            let allocs = a["allocations"].as_array().unwrap().iter().map(|al| AllocInput {
                target_id: s(al, "target_id"), target_type: s(al, "target_type"), amount_minor: i(al, "amount_minor"),
            }).collect();
            handle_payment_made(ctx, &s(a, "payment_id"), &s(a, "supplier_id"), i(a, "amount_minor"), &s(a, "date"), allocs)?;
        }
        "expense_recorded" => {
            let sup = opt(a, "supplier_id");
            let memo = opt(a, "memo");
            handle_expense_recorded(ctx, &s(a, "expense_id"), &s(a, "account_id"), i(a, "amount_minor"), &s(a, "date"), &s(a, "terms"), sup.as_deref(), memo.as_deref())?;
        }
        other => {
            eprintln!("  [skip] unknown command: {other}");
        }
    }
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("Usage: load_to_file <db_path> <dataset.jsonl>");
        std::process::exit(1);
    }
    let db_path = &args[1];
    let dataset_path = &args[2];

    let mut conn = Connection::open(db_path).expect("open db");
    conn.pragma_update(None, "journal_mode", "WAL").ok();
    conn.pragma_update(None, "synchronous", "NORMAL").ok();
    conn.pragma_update(None, "foreign_keys", "ON").ok();
    apply_schema(&conn).expect("apply schema");

    let mut hlc = Hlc::new("device-1");
    rehydrate_from_log(&conn, &mut hlc, 1000).expect("rehydrate");

    let event_count: i64 = conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();
    if event_count == 0 {
        run_genesis(&conn, &mut hlc, 1000, "device-1", "owner-1", "Owner").expect("genesis");
        rebuild(&mut conn).expect("rebuild after genesis");
        println!("Genesis complete (projected).");
    } else {
        rebuild(&mut conn).expect("rebuild existing");
        println!("DB already has {event_count} events, rebuilt projections.");
    }

    let data = fs::read_to_string(dataset_path).expect("read dataset");
    let lines: Vec<&str> = data.lines().collect();
    println!("Loading {} commands from {dataset_path}...", lines.len());

    let start = Instant::now();
    let mut ok = 0;
    let mut err = 0;
    for (idx, line) in lines.iter().enumerate() {
        let v: Value = serde_json::from_str(line).expect("parse json");
        let cmd = v["cmd"].as_str().unwrap();
        let a = &v["args"];
        let mut ctx = CommandContext {
            conn: &mut conn, hlc: &mut hlc, physical_now: 1000,
            device_id: "device-1".into(), user_id: "owner-1".into(),
        };
        match apply(&mut ctx, cmd, a) {
            Ok(_) => ok += 1,
            Err(e) => {
                err += 1;
                eprintln!("  [REJECT] line {}: {cmd} - {e}", idx + 1);
            }
        }
    }
    let elapsed = start.elapsed();
    println!("Done: {ok} accepted, {err} rejected in {:.2}s ({:.0} cmd/s)",
        elapsed.as_secs_f64(), ok as f64 / elapsed.as_secs_f64());

    println!("Running reconciliation checks...");
    let checks = run_all_checks(&conn).unwrap();
    if all_passed(&checks) {
        println!("All 8 checks PASS.");
    } else {
        for c in &checks {
            if c.outcome != accounting_core::CheckOutcome::Pass {
                eprintln!("  FAIL: {} - {:?}", c.name, c.outcome);
            }
        }
        std::process::exit(1);
    }
}
