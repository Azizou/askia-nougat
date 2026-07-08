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
}

impl AppState {
    pub fn new(conn: Connection, hlc: Hlc) -> Self {
        Self { db: Mutex::new(Db { conn, hlc }) }
    }
}
