use std::fmt;

/// Errors returned by the store. Variants separate caller mistakes
/// (`InvalidRecord`, `Conflict`, `IdempotencyConflict`) from storage faults
/// (`Integrity`, `Database`, `Io`), so callers can decide what to retry.
#[derive(Debug)]
pub enum StoreError {
    /// The record failed contract validation or is not a JSON object with the
    /// required envelope fields.
    InvalidRecord(String),
    /// A different record body is already stored under this `record_id`.
    Conflict(String),
    /// The idempotency key was already used for a different request.
    IdempotencyConflict(String),
    /// A referenced record, upload or object does not exist.
    NotFound(String),
    /// Stored bytes no longer match their recorded SHA-256 digest.
    Integrity(String),
    /// The embedded contract schemas could not be compiled.
    Contract(String),
    Database(rusqlite::Error),
    Io(std::io::Error),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRecord(message) => write!(f, "invalid record: {message}"),
            Self::Conflict(message) => write!(f, "record conflict: {message}"),
            Self::IdempotencyConflict(message) => write!(f, "idempotency conflict: {message}"),
            Self::NotFound(message) => write!(f, "not found: {message}"),
            Self::Integrity(message) => write!(f, "integrity failure: {message}"),
            Self::Contract(message) => write!(f, "contract schemas: {message}"),
            Self::Database(error) => write!(f, "database: {error}"),
            Self::Io(error) => write!(f, "io: {error}"),
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Database(error) => Some(error),
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Database(error)
    }
}

impl From<std::io::Error> for StoreError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

pub type StoreResult<T> = Result<T, StoreError>;
