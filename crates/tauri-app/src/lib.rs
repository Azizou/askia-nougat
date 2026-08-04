mod backup;
mod commands;
mod error;
mod state;

use accounting_core::{apply_schema, ensure_anon_supplier, ensure_device_id, ensure_walkin_party, rebuild, Hlc, rehydrate_from_log, run_genesis};
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

    // Mint or read this install's identity BEFORE rehydrating the clock: the
    // clock must know its own device id before it seeds from the log's max HLC.
    let device_id = ensure_device_id(&conn).expect("ensure device id");

    let mut hlc = Hlc::new(device_id.clone());
    rehydrate_from_log(&conn, &mut hlc, now_ms()).expect("rehydrate hlc");

    let event_count: i64 = conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();
    if event_count == 0 {
        run_genesis(&conn, &mut hlc, now_ms(), &device_id, "owner-1", "Owner").expect("genesis");
    }

    // Idempotently ensure the shared walk-in customer exists (covers both
    // fresh installs and installs whose genesis predates this party). Must run
    // before rebuild so the event is projected this startup.
    ensure_walkin_party(&conn, &mut hlc, now_ms(), &device_id).expect("seed walk-in party");
    ensure_anon_supplier(&conn, &mut hlc, now_ms(), &device_id).expect("seed anonymous supplier");

    // Always rebuild projections on startup — ensures genesis events are projected
    // and recovers from any interrupted prior session.
    rebuild(&mut conn).expect("rebuild projections");

    AppState::new(conn, hlc, device_id)
}

/// Write an automatic snapshot into the remembered backup folder, then prune.
///
/// Returns `Ok(false)` when there is nothing to do (no folder remembered yet).
/// Never panics: this runs while the window is closing.
fn auto_backup_on_close(state: &AppState) -> Result<bool, String> {
    let db = state.db.lock().map_err(|_| "state lock poisoned".to_string())?;
    let conn = match db.conn.as_ref() {
        Some(c) => c,
        None => return Ok(false), // a restore already closed it
    };

    let settings = accounting_core::get_settings(conn).map_err(|e| e.to_string())?;
    let folder = match settings.get("backup_folder") {
        Some(f) if !f.is_empty() => std::path::PathBuf::from(f),
        _ => return Ok(false), // the user has never chosen a folder
    };
    if !folder.is_dir() {
        return Err(format!("backup folder is unavailable: {}", folder.display()));
    }

    let now = now_ms() as i64;
    let dest = folder.join(backup::snapshot_name(backup::AUTO_PREFIX, now));
    backup::snapshot_to(conn, &dest).map_err(|e| e.to_string())?;
    accounting_core::set_setting(conn, "last_backup_at", &now.to_string())
        .map_err(|e| e.to_string())?;

    // Drop the connection before pruning so nothing holds a file we may remove.
    drop(db);
    backup::prune(&folder, backup::AUTO_PREFIX, backup::KEEP_AUTO).map_err(|e| e.to_string())?;
    Ok(true)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .on_window_event(|window, event| {
            // `CloseRequested`, not `Destroyed`: the latter fires during teardown,
            // after the window is gone, so a `VACUUM INTO` of a large ledger can be
            // cut short by the process exiting. Here the event loop is still alive
            // and the write runs to completion before the window goes away.
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                let state = window.state::<AppState>();
                match auto_backup_on_close(&state) {
                    Ok(true) => eprintln!("automatic backup written"),
                    Ok(false) => {}
                    // Deliberately swallowed: the window is already closing, so
                    // there is nowhere to show this. Next launch shows a stale
                    // "last backup" date instead.
                    Err(e) => eprintln!("automatic backup failed: {e}"),
                }
            }
        })
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
            commands::backup_database,
            commands::restore_database,
            commands::export_event_log,
            commands::import_event_log,
            commands::update_item,
            commands::update_party,
            commands::delete_item,
            commands::delete_party,
            commands::list_open_invoices,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
