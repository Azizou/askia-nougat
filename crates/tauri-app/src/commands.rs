use crate::error::AppError;
use crate::state::AppState;
use accounting_core::{
    handle_item_defined, handle_party_created, handle_purchase_recorded,
    handle_sale_recorded, handle_payment_received, run_all_checks, all_passed,
    CommandContext, PurchaseLineInput, SaleLineInput, AllocInput,
};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::State;

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
}

macro_rules! with_ctx {
    ($state:expr, |$ctx:ident| $body:expr) => {{
        let mut db = $state.db.lock().unwrap();
        let crate::state::Db { ref mut conn, ref mut hlc } = *db;
        let mut $ctx = CommandContext {
            conn, hlc, physical_now: now_ms(),
            device_id: "device-1".into(), user_id: "owner-1".into(),
        };
        $body
    }};
}

// ---- Setup commands ----

#[derive(Deserialize)]
pub struct ItemInput {
    pub id: String,
    pub sku: String,
    pub name: String,
    pub unit: String,
}

#[tauri::command]
pub fn create_item(state: State<AppState>, input: ItemInput) -> Result<(), AppError> {
    with_ctx!(state, |ctx| {
        handle_item_defined(&mut ctx, &input.id, &input.sku, &input.name, &input.unit)?;
        Ok(())
    })
}

#[derive(Deserialize)]
pub struct PartyInput {
    pub id: String,
    pub name: String,
    pub kind: String,
}

#[tauri::command]
pub fn create_party(state: State<AppState>, input: PartyInput) -> Result<(), AppError> {
    with_ctx!(state, |ctx| {
        handle_party_created(&mut ctx, &input.id, &input.name, &input.kind)?;
        Ok(())
    })
}

// ---- Transaction commands ----

#[derive(Deserialize)]
pub struct PurchaseInput {
    pub id: String,
    pub supplier_id: String,
    pub date: String,
    pub terms: String,
    pub lines: Vec<PurchaseLineDto>,
}

#[derive(Deserialize)]
pub struct PurchaseLineDto {
    pub item_id: String,
    pub qty: i64,
    pub unit_cost_minor: i64,
}

#[tauri::command]
pub fn record_purchase(state: State<AppState>, input: PurchaseInput) -> Result<(), AppError> {
    with_ctx!(state, |ctx| {
        let lines: Vec<PurchaseLineInput> = input.lines.into_iter().map(|l| PurchaseLineInput {
            item_id: l.item_id, qty: l.qty, unit_cost_minor: l.unit_cost_minor,
        }).collect();
        handle_purchase_recorded(&mut ctx, &input.id, &input.supplier_id, &input.date, &input.terms, lines)?;
        Ok(())
    })
}

#[derive(Deserialize)]
pub struct SaleInput {
    pub id: String,
    pub customer_id: String,
    pub date: String,
    pub terms: String,
    pub lines: Vec<SaleLineDto>,
}

#[derive(Deserialize)]
pub struct SaleLineDto {
    pub item_id: String,
    pub qty: i64,
    pub unit_price_minor: i64,
}

#[tauri::command]
pub fn record_sale(state: State<AppState>, input: SaleInput) -> Result<(), AppError> {
    with_ctx!(state, |ctx| {
        let lines: Vec<SaleLineInput> = input.lines.into_iter().map(|l| SaleLineInput {
            item_id: l.item_id, qty: l.qty, unit_price_minor: l.unit_price_minor, lot_picks: None,
        }).collect();
        handle_sale_recorded(&mut ctx, &input.id, &input.customer_id, &input.date, &input.terms, lines)?;
        Ok(())
    })
}

#[derive(Deserialize)]
pub struct PaymentInput {
    pub id: String,
    pub customer_id: String,
    pub amount_minor: i64,
    pub date: String,
    pub allocations: Vec<AllocationDto>,
}

#[derive(Deserialize)]
pub struct AllocationDto {
    pub target_id: String,
    pub target_type: String,
    pub amount_minor: i64,
}

#[tauri::command]
pub fn record_payment(state: State<AppState>, input: PaymentInput) -> Result<(), AppError> {
    with_ctx!(state, |ctx| {
        let allocs: Vec<AllocInput> = input.allocations.into_iter().map(|a| AllocInput {
            target_id: a.target_id, target_type: a.target_type, amount_minor: a.amount_minor,
        }).collect();
        handle_payment_received(&mut ctx, &input.id, &input.customer_id, input.amount_minor, &input.date, allocs)?;
        Ok(())
    })
}

// ---- Query commands ----

#[derive(Serialize)]
pub struct DashboardData {
    pub inventory_value: i64,
    pub total_receivable: i64,
    pub total_payable: i64,
    pub checks_passing: bool,
}

#[tauri::command]
pub fn get_dashboard(state: State<AppState>) -> Result<DashboardData, AppError> {
    let db = state.db.lock().unwrap();
    let inv_val = accounting_core::inventory_valuation(&db.conn)?;
    let balances = accounting_core::party_balances(&db.conn)?;
    let recv: i64 = balances.iter().map(|b| b.receivable_minor).sum();
    let pay: i64 = balances.iter().map(|b| b.payable_minor).sum();
    let checks = run_all_checks(&db.conn)?;
    Ok(DashboardData {
        inventory_value: inv_val,
        total_receivable: recv,
        total_payable: pay,
        checks_passing: all_passed(&checks),
    })
}

#[derive(Serialize)]
pub struct StockRow {
    pub item_id: String,
    pub qty: i64,
}

#[tauri::command]
pub fn get_stock(state: State<AppState>) -> Result<Vec<StockRow>, AppError> {
    let db = state.db.lock().unwrap();
    let stock = accounting_core::stock_on_hand(&db.conn)?;
    Ok(stock.into_iter().map(|s| StockRow { item_id: s.item_id, qty: s.qty }).collect())
}

#[derive(Serialize)]
pub struct ProfitData {
    pub revenue_minor: i64,
    pub cogs_minor: i64,
    pub gross_profit_minor: i64,
    pub net_profit_minor: i64,
}

#[tauri::command]
pub fn get_profit(state: State<AppState>, anchor: String) -> Result<ProfitData, AppError> {
    let db = state.db.lock().unwrap();
    let g = accounting_core::gross_profit(&db.conn, &anchor)?;
    let net = accounting_core::net_profit(&db.conn, &anchor)?;
    Ok(ProfitData {
        revenue_minor: g.revenue_minor,
        cogs_minor: g.cogs_minor,
        gross_profit_minor: g.gross_profit_minor,
        net_profit_minor: net,
    })
}
