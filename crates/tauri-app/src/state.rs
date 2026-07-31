use accounting_core::Hlc;
use rusqlite::Connection;
use std::sync::Mutex;

/// The mutable state a command needs: connection + clock.
///
/// `conn` is an Option because a restore must drop the live connection before
/// overwriting the database file. After a restore it stays `None` and every
/// command reports that a restart is required — which the restore flow asks the
/// user to do anyway.
pub struct Db {
    pub conn: Option<Connection>,
    pub hlc: Hlc,
}

impl Db {
    /// Borrow the live connection, or explain that a restart is pending.
    pub fn conn(&self) -> Result<&Connection, crate::error::AppError> {
        self.conn.as_ref().ok_or_else(|| crate::error::AppError {
            message: "Restore finished. Please close and reopen the app.".into(),
        })
    }
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
        Self { db: Mutex::new(Db { conn: Some(conn), hlc }), device_id }
    }
}
