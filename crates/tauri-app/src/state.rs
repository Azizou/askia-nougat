use accounting_core::Hlc;
use rusqlite::Connection;
use std::sync::Mutex;

/// The mutable state a command needs: connection + clock.
pub struct Db {
    pub conn: Connection,
    pub hlc: Hlc,
}

/// Application state. A single Mutex wraps both connection and clock,
/// eliminating any lock-ordering concern (single-device, single-writer).
pub struct AppState {
    pub db: Mutex<Db>,
    /// This install's stable identity, authored into every event. Read once at
    /// startup so commands never re-query it.
    pub device_id: String,
}

impl AppState {
    pub fn new(conn: Connection, hlc: Hlc, device_id: String) -> Self {
        Self { db: Mutex::new(Db { conn, hlc }), device_id }
    }
}
