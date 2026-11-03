use std::fmt;

use crate::constants::{MH_ERR, MH_INVALID_ARGUMENT, MH_NOT_SUPPORTED};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendError {
    pub status: i32,
    pub message: String,
}

impl BackendError {
    pub fn new(status: i32, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    pub fn failed(message: impl Into<String>) -> Self {
        Self::new(MH_ERR, message)
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new(MH_NOT_SUPPORTED, message)
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(MH_INVALID_ARGUMENT, message)
    }
}

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "backend error {}: {}", self.status, self.message)
    }
}

impl std::error::Error for BackendError {}

pub type BackendResult<T> = Result<T, BackendError>;
