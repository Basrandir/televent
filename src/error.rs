use std::fmt;

/// Custom error type for operations
#[derive(Debug)]
pub enum BotError {
    Database(sqlx::Error),
    Telegram(frankenstein::Error),
    Parse(std::num::ParseIntError),
    MissingDraft,
}

// Implement std:fmt::Display for BotError
impl fmt::Display for BotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BotError::Database(e) => write!(f, "Database error: {}", e),
            BotError::Telegram(e) => write!(f, "Telegram API error: {}", e),
            BotError::Parse(e) => write!(f, "Failed to parse data: {}", e),
            BotError::MissingDraft => write!(f, "Event draft not found"),
        }
    }
}

// Implement std::error::Error for BotError
impl std::error::Error for BotError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            BotError::Database(e) => Some(e),
            BotError::Telegram(e) => Some(e),
            BotError::Parse(e) => Some(e),
            BotError::MissingDraft => None,
        }
    }
}

// Implement From for each error type
impl From<sqlx::Error> for BotError {
    fn from(err: sqlx::Error) -> Self {
        BotError::Database(err)
    }
}

impl From<frankenstein::Error> for BotError {
    fn from(err: frankenstein::Error) -> Self {
        BotError::Telegram(err)
    }
}

impl From<std::num::ParseIntError> for BotError {
    fn from(err: std::num::ParseIntError) -> Self {
        BotError::Parse(err)
    }
}
