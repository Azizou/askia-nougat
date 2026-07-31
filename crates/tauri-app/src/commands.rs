use crate::error::AppError;
use crate::state::AppState;
use accounting_core::{
    handle_item_defined, handle_party_created, handle_purchase_recorded,
    handle_sale_recorded, handle_payment_received, handle_payment_made,
    handle_transaction_reversed, run_all_checks, all_passed,
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
        let device_id = $state.device_id.clone();
        let mut db = $state.db.lock().unwrap();
        let crate::state::Db { ref mut conn, ref mut hlc } = *db;
        let conn = conn.as_mut().ok_or_else(|| crate::error::AppError {
            message: "Restore finished. Please close and reopen the app.".into(),
        })?;
        let mut $ctx = CommandContext {
            conn, hlc, physical_now: now_ms(),
            device_id, user_id: "owner-1".into(),
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

// ---- Settings commands ----

#[tauri::command]
pub fn get_settings(state: State<AppState>) -> Result<std::collections::HashMap<String, String>, AppError> {
    let db = state.db.lock().unwrap();
    accounting_core::get_settings(db.conn()?).map_err(AppError::from)
}

#[tauri::command]
pub fn set_setting(state: State<AppState>, key: String, value: String) -> Result<(), AppError> {
    let db = state.db.lock().unwrap();
    accounting_core::set_setting(db.conn()?, &key, &value).map_err(AppError::from)
}

// ---- Supplier payment + reversal commands ----

#[derive(Deserialize)]
pub struct PaymentMadeInput {
    pub id: String,
    pub supplier_id: String,
    pub amount_minor: i64,
    pub date: String,
    pub allocations: Vec<AllocationDto>,
}

#[tauri::command]
pub fn record_payment_made(state: State<AppState>, input: PaymentMadeInput) -> Result<(), AppError> {
    with_ctx!(state, |ctx| {
        let allocs: Vec<AllocInput> = input.allocations.into_iter().map(|a| AllocInput {
            target_id: a.target_id, target_type: a.target_type, amount_minor: a.amount_minor,
        }).collect();
        handle_payment_made(&mut ctx, &input.id, &input.supplier_id, input.amount_minor, &input.date, allocs)?;
        Ok(())
    })
}

#[derive(Deserialize)]
pub struct ReversalInput {
    pub target_event_id: String,
    pub reason: String,
}

#[tauri::command]
pub fn reverse_transaction(state: State<AppState>, input: ReversalInput) -> Result<(), AppError> {
    with_ctx!(state, |ctx| {
        handle_transaction_reversed(&mut ctx, &input.target_event_id, &input.reason)?;
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
    let inv_val = accounting_core::inventory_valuation(db.conn()?)?;
    let balances = accounting_core::party_balances(db.conn()?)?;
    let recv: i64 = balances.iter().map(|b| b.receivable_minor).sum();
    let pay: i64 = balances.iter().map(|b| b.payable_minor).sum();
    let checks = run_all_checks(db.conn()?)?;
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
    let stock = accounting_core::stock_on_hand(db.conn()?)?;
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
    let g = accounting_core::gross_profit(db.conn()?, &anchor)?;
    let net = accounting_core::net_profit(db.conn()?, &anchor)?;
    Ok(ProfitData {
        revenue_minor: g.revenue_minor,
        cogs_minor: g.cogs_minor,
        gross_profit_minor: g.gross_profit_minor,
        net_profit_minor: net,
    })
}

#[derive(Serialize)]
pub struct ItemRow {
    pub id: String,
    pub name: String,
    pub sku: String,
    pub unit: String,
}

#[tauri::command]
pub fn list_items(state: State<AppState>) -> Result<Vec<ItemRow>, AppError> {
    let db = state.db.lock().unwrap();
    let mut stmt = db.conn()?.prepare(
        "SELECT id, name, sku, unit FROM items ORDER BY name")?;
    let rows = stmt.query_map([], |r| {
        Ok(ItemRow { id: r.get(0)?, name: r.get(1)?, sku: r.get(2)?, unit: r.get(3)? })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
}

#[derive(Serialize)]
pub struct PartyRow {
    pub id: String,
    pub name: String,
    pub kind: String,
}

#[tauri::command]
pub fn list_parties(state: State<AppState>) -> Result<Vec<PartyRow>, AppError> {
    let db = state.db.lock().unwrap();
    let mut stmt = db.conn()?.prepare(
        "SELECT id, name, kind FROM parties ORDER BY name")?;
    let rows = stmt.query_map([], |r| {
        Ok(PartyRow { id: r.get(0)?, name: r.get(1)?, kind: r.get(2)? })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
}

#[derive(Serialize)]
pub struct SaleRow {
    pub id: String,
    pub event_id: String,
    pub customer_id: Option<String>,
    pub date: String,
    pub terms: String,
    pub total_minor: i64,
    pub outstanding_minor: i64,
}

#[tauri::command]
pub fn list_sales(state: State<AppState>) -> Result<Vec<SaleRow>, AppError> {
    let db = state.db.lock().unwrap();
    let mut stmt = db.conn()?.prepare(
        "SELECT id, event_id, customer_id, date, terms, total_minor, outstanding_minor FROM sales WHERE reversed = 0 ORDER BY date DESC")?;
    let rows = stmt.query_map([], |r| {
        Ok(SaleRow { id: r.get(0)?, event_id: r.get(1)?, customer_id: r.get(2)?, date: r.get(3)?, terms: r.get(4)?, total_minor: r.get(5)?, outstanding_minor: r.get(6)? })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
}

#[derive(Serialize)]
pub struct PurchaseRow {
    pub id: String,
    pub event_id: String,
    pub supplier_id: Option<String>,
    pub date: String,
    pub terms: String,
    pub total_minor: i64,
    pub outstanding_minor: i64,
}

#[tauri::command]
pub fn list_purchases(state: State<AppState>) -> Result<Vec<PurchaseRow>, AppError> {
    let db = state.db.lock().unwrap();
    let mut stmt = db.conn()?.prepare(
        "SELECT id, event_id, supplier_id, date, terms, total_minor, outstanding_minor FROM purchases WHERE reversed = 0 ORDER BY date DESC")?;
    let rows = stmt.query_map([], |r| {
        Ok(PurchaseRow { id: r.get(0)?, event_id: r.get(1)?, supplier_id: r.get(2)?, date: r.get(3)?, terms: r.get(4)?, total_minor: r.get(5)?, outstanding_minor: r.get(6)? })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
}

#[derive(Serialize)]
pub struct PaymentRow {
    pub id: String,
    pub event_id: String,
    pub party_id: String,
    pub direction: String,
    pub amount_minor: i64,
    pub date: String,
}

#[tauri::command]
pub fn list_payments(state: State<AppState>) -> Result<Vec<PaymentRow>, AppError> {
    let db = state.db.lock().unwrap();
    let mut stmt = db.conn()?.prepare(
        "SELECT id, event_id, party_id, direction, amount_minor, date FROM payments ORDER BY date DESC")?;
    let rows = stmt.query_map([], |r| {
        Ok(PaymentRow {
            id: r.get(0)?, event_id: r.get(1)?, party_id: r.get(2)?,
            direction: r.get(3)?, amount_minor: r.get(4)?, date: r.get(5)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
}

// ---- Backup / restore commands ----

#[derive(Serialize)]
pub struct BackupResult {
    pub path: String,
    pub bytes: u64,
    /// When this backup was written, Unix ms. Returned so the frontend can
    /// refresh its cached `last_backup_at` — the React settings context only
    /// learns about a setting the UI itself wrote, and this one is written here
    /// in Rust.
    pub at: i64,
}

/// Write a manual snapshot into `dest_dir`, remember the folder for automatic
/// backups, and record the time.
#[tauri::command]
pub fn backup_database(
    state: State<AppState>,
    dest_dir: String,
) -> Result<BackupResult, AppError> {
    let db = state.db.lock().unwrap();
    let conn = db.conn()?;
    let now = now_ms() as i64;

    let dir = std::path::PathBuf::from(&dest_dir);
    let dest = dir.join(crate::backup::snapshot_name(crate::backup::MANUAL_PREFIX, now));
    let bytes = crate::backup::snapshot_to(conn, &dest)?;

    // Remember where the user keeps backups so auto-backup-on-close can use it.
    accounting_core::set_setting(conn, "backup_folder", &dest_dir)?;
    accounting_core::set_setting(conn, "last_backup_at", &now.to_string())?;

    Ok(BackupResult { path: dest.to_string_lossy().into_owned(), bytes, at: now })
}

#[derive(Serialize)]
pub struct RestoreResult {
    /// Where the pre-restore safety copy was written.
    pub rescue_path: String,
}

/// Replace the live ledger with `src_path`.
///
/// Order matters: validate the candidate, safety-copy the current ledger, drop
/// the live connection, then swap. The app must be restarted afterwards —
/// startup rebuilds every projection from the restored log.
#[tauri::command]
pub fn restore_database(
    app: tauri::AppHandle,
    state: State<AppState>,
    src_path: String,
) -> Result<RestoreResult, AppError> {
    use tauri::Manager;

    let candidate = std::path::PathBuf::from(&src_path);
    crate::backup::validate_candidate(&candidate)
        .map_err(|e| AppError { message: e.to_string() })?;

    let data_dir = app
        .path()
        .app_local_data_dir()
        .map_err(|e| AppError { message: format!("cannot locate the data folder: {e}") })?
        .join("accounting");
    let live = data_dir.join("ledger.db");

    // Everything that can reject must reject before `db.conn` is dropped below:
    // past that point the app is unusable until a restart, so failing there would
    // brick the session over a recoverable mistake. `swap_in_place` re-checks
    // this as a last line of defence, but the user-facing rejection belongs here.
    if crate::backup::is_same_file(&candidate, &live) {
        return Err(AppError {
            message: "this backup is the database you are using right now; \
                      choose a backup file instead"
                .into(),
        });
    }

    let mut db = state.db.lock().unwrap();
    let now = now_ms() as i64;

    // Safety copy first: a restore must always be undoable.
    let rescue = crate::backup::ensure_rescue_dir(&data_dir)?;
    let rescue_path = rescue.join(crate::backup::snapshot_name(crate::backup::RESCUE_PREFIX, now));
    {
        let conn = db.conn()?;
        crate::backup::snapshot_to(conn, &rescue_path)?;
    }

    // Close the live connection before overwriting the file it points at.
    db.conn = None;
    crate::backup::swap_in_place(&candidate, &live)?;

    // Prune AFTER the swap, never before. The file dialog can offer the rescue
    // copies themselves as a restore source, so `candidate` may be the oldest
    // `pre-restore-*.db` in this very directory — and pruning first would delete
    // it out from under the swap that is about to read it. Once the swap has
    // copied its bytes into `live`, removing the rescue copy is harmless.
    let _ = crate::backup::prune(&rescue, crate::backup::RESCUE_PREFIX, crate::backup::KEEP_AUTO);

    Ok(RestoreResult { rescue_path: rescue_path.to_string_lossy().into_owned() })
}

#[derive(Serialize)]
pub struct ExportResult {
    pub path: String,
    pub events: usize,
}

/// Write the whole event log as JSONL into `dest_dir`.
#[tauri::command]
pub fn export_event_log(
    state: State<AppState>,
    dest_dir: String,
) -> Result<ExportResult, AppError> {
    let db = state.db.lock().unwrap();
    let conn = db.conn()?;
    let now = now_ms() as i64;

    let name = format!("ledger-{}.jsonl", crate::backup::timestamp_utc(now));
    let dest = std::path::PathBuf::from(&dest_dir).join(name);
    let file = std::fs::File::create(&dest)?;
    let mut writer = std::io::BufWriter::new(file);

    let events = accounting_core::export_jsonl(conn, &mut writer, now, env!("CARGO_PKG_VERSION"))?;

    Ok(ExportResult { path: dest.to_string_lossy().into_owned(), events })
}

#[derive(Serialize)]
pub struct ImportResult {
    pub inserted: usize,
    pub skipped_duplicates: usize,
    pub total_events: usize,
    pub rescue_path: String,
}

/// Merge a JSONL archive into the live ledger.
///
/// Takes the same safety copy as restore, because the merge ends with a
/// reconciliation check that can reject a ledger which was healthy before.
#[tauri::command]
pub fn import_event_log(
    app: tauri::AppHandle,
    state: State<AppState>,
    src_path: String,
) -> Result<ImportResult, AppError> {
    use tauri::Manager;

    let data_dir = app
        .path()
        .app_local_data_dir()
        .map_err(|e| AppError { message: format!("cannot locate the data folder: {e}") })?
        .join("accounting");

    let mut db = state.db.lock().unwrap();
    let now = now_ms() as i64;

    let rescue = crate::backup::ensure_rescue_dir(&data_dir)?;
    let rescue_path = rescue.join(crate::backup::snapshot_name(crate::backup::RESCUE_PREFIX, now));
    {
        let conn = db.conn()?;
        crate::backup::snapshot_to(conn, &rescue_path)?;
    }
    let file = std::fs::File::open(&src_path)?;
    let reader = std::io::BufReader::new(file);

    let crate::state::Db { ref mut conn, ref mut hlc } = *db;
    let conn = conn.as_mut().ok_or_else(|| AppError {
        message: "Restore finished. Please close and reopen the app.".into(),
    })?;
    let summary = accounting_core::import_jsonl(conn, reader)?;

    // Advance the clock past everything merged in, so events this install
    // appends next sort after the imported ones. rehydrate_from_log reads
    // MAX(hlc) and calls Hlc::observe — exactly what is needed here.
    accounting_core::rehydrate_from_log(conn, hlc, now_ms())?;

    // Prune AFTER the source has been read, for the same reason as `restore_database`:
    // never delete files out from under a path the caller supplied. The UI's import
    // dialog filters `.jsonl` while `prune` only matches `.db`, so `src_path` cannot
    // be a prune target today — but relying on that would leave the two commands
    // disagreeing about an invariant, and a filter change would make it live.
    let _ = crate::backup::prune(&rescue, crate::backup::RESCUE_PREFIX, crate::backup::KEEP_AUTO);

    Ok(ImportResult {
        inserted: summary.inserted,
        skipped_duplicates: summary.skipped_duplicates,
        total_events: summary.total_events,
        rescue_path: rescue_path.to_string_lossy().into_owned(),
    })
}
