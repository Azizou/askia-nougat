#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod error;
mod state;

use accounting_core::{apply_schema, Hlc, rehydrate_from_log, run_genesis};
use state::AppState;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
}

fn db_path() -> PathBuf {
    let dir = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("accounting");
    fs::create_dir_all(&dir).ok();
    dir.join("ledger.db")
}

fn init_state() -> AppState {
    let path = db_path();
    let conn = rusqlite::Connection::open(&path).expect("open db");

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

    AppState::new(conn, hlc)
}

fn main() {
    tauri::Builder::default()
        .manage(init_state())
        .invoke_handler(tauri::generate_handler![
            commands::create_item,
            commands::create_party,
            commands::record_purchase,
            commands::record_sale,
            commands::record_payment,
            commands::get_dashboard,
            commands::get_stock,
            commands::get_profit,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
