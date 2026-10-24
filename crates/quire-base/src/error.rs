//! Shared error type.
//!
//! Layers translate their own failures into these variants and keep the
//! original context in the message. No error is silently discarded.

use std::fmt;

/// Convenient result alias.
pub type Result<T> = std::result::Result<T, QuireError>;

/// Errors that can cross crate boundaries.
#[derive(Debug, thiserror::Error)]
pub enum QuireError {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),

    #[error("invalid document: {0}")]
    Format(String),

    #[error("unsupported feature: {0}")]
    Unsupported(String),

    #[error("layout failed: {0}")]
    Layout(String),

    #[error("edit rejected: {0}")]
    Edit(String),
}

impl QuireError {
    pub fn format(message: impl fmt::Display) -> Self {
        Self::Format(message.to_string())
    }

    pub fn unsupported(message: impl fmt::Display) -> Self {
        Self::Unsupported(message.to_string())
    }

    pub fn layout(message: impl fmt::Display) -> Self {
        Self::Layout(message.to_string())
    }

    pub fn edit(message: impl fmt::Display) -> Self {
        Self::Edit(message.to_string())
    }
}
