use serde::Serialize;

/// Serializable error type for Tauri command responses.
#[derive(Debug, Serialize)]
pub struct AppError {
    pub message: String,
}

impl From<accounting_core::CommandError> for AppError {
    fn from(e: accounting_core::CommandError) -> Self {
        Self { message: e.to_string() }
    }
}

impl From<rusqlite::Error> for AppError {
    fn from(e: rusqlite::Error) -> Self {
        Self { message: format!("database error: {e}") }
    }
}

impl From<accounting_core::ArchiveError> for AppError {
    fn from(e: accounting_core::ArchiveError) -> Self {
        Self { message: e.to_string() }
    }
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        Self { message: format!("file error: {e}") }
    }
}
