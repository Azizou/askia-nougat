mod commands;
mod error;
mod state;

use accounting_core::{apply_schema, ensure_walkin_party, rebuild, Hlc, rehydrate_from_log, run_genesis};
use state::AppState;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::Manager;

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
}

fn init_state(app_data_dir: PathBuf) -> AppState {
    let dir = app_data_dir.join("accounting");
    fs::create_dir_all(&dir).expect("create data dir");
    let path = dir.join("ledger.db");

    let mut conn = rusqlite::Connection::open(&path).expect("open db");
    conn.pragma_update(None, "journal_mode", "WAL").ok();
    conn.pragma_update(None, "synchronous", "NORMAL").ok();
    conn.pragma_update(None, "foreign_keys", "ON").ok();
    apply_schema(&conn).expect("apply schema");

    let mut hlc = Hlc::new("device-1");
    rehydrate_from_log(&conn, &mut hlc, now_ms()).expect("rehydrate hlc");

    let event_count: i64 = conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();
    if event_count == 0 {
        run_genesis(&conn, &mut hlc, now_ms(), "device-1", "owner-1", "Owner").expect("genesis");
    }

    // Idempotently ensure the shared walk-in customer exists (covers both
    // fresh installs and installs whose genesis predates this party). Must run
    // before rebuild so the event is projected this startup.
    ensure_walkin_party(&conn, &mut hlc, now_ms(), "device-1").expect("seed walk-in party");

    // Always rebuild projections on startup — ensures genesis events are projected
    // and recovers from any interrupted prior session.
    rebuild(&mut conn).expect("rebuild projections");

    AppState::new(conn, hlc)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let data_dir = app.path().app_local_data_dir()
                .expect("failed to resolve app data dir");
            let state = init_state(data_dir);
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::create_item,
            commands::create_party,
            commands::record_purchase,
            commands::record_sale,
            commands::record_payment,
            commands::get_dashboard,
            commands::get_stock,
            commands::get_profit,
            commands::list_items,
            commands::list_parties,
            commands::list_sales,
            commands::list_purchases,
            commands::get_settings,
            commands::set_setting,
            commands::record_payment_made,
            commands::reverse_transaction,
            commands::list_payments,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
