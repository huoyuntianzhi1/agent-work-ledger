use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("tauri: {0}")]
    Tauri(#[from] tauri::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("regex: {0}")]
    Regex(#[from] regex::Error),
    #[error("zip: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("could not resolve a writable data directory for the app")]
    NoDataDir,
    #[error("connector not registered: {0}")]
    UnknownConnector(String),
    #[error("connector '{0}' reported: {1}")]
    Connector(String, String),
    /// Stage 6 SQLCipher unlock flow for the TRAE SOLO
    /// connector. Wraps the inner `TraeSoloError` so that
    /// `?`-bubble works from the connector into the IPC
    /// handlers.
    #[error("trae_solo: {0}")]
    TraeSolo(String),
    #[error("{0}")]
    Other(String),
}

impl Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

pub type AppResult<T> = Result<T, AppError>;
