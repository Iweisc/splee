//! Error types for the Claude Agent SDK

use std::path::PathBuf;
use thiserror::Error;

/// Main error type for the Claude Agent SDK
///
/// A variant that wraps another error is transparent because the wrapped error already names itself. Printing it
/// and also returning it as the source made anyhow's `{:#}` show every agent run's failure twice.
#[derive(Debug, Error)]
pub enum ClaudeError {
    /// CLI connection error
    #[error(transparent)]
    Connection(#[from] ConnectionError),

    /// Process error
    #[error(transparent)]
    Process(#[from] ProcessError),

    /// JSON decode error
    #[error(transparent)]
    JsonDecode(#[from] JsonDecodeError),

    /// Message parse error
    #[error(transparent)]
    MessageParse(#[from] MessageParseError),

    /// Transport error
    #[error("Transport error: {0}")]
    Transport(String),

    /// Control protocol error
    #[error("Control protocol error: {0}")]
    ControlProtocol(String),

    /// Invalid configuration
    #[error("Invalid configuration: {0}")]
    InvalidConfig(String),

    /// CLI not found error
    #[error(transparent)]
    CliNotFound(#[from] CliNotFoundError),

    /// Image validation error
    #[error(transparent)]
    ImageValidation(#[from] ImageValidationError),

    /// IO error
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// Other errors
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Error when Claude Code CLI cannot be found
#[derive(Debug, Error)]
#[error("CLI not found: {message}")]
pub struct CliNotFoundError {
    /// Error message
    pub message: String,
    /// Path that was checked
    pub cli_path: Option<PathBuf>,
}

impl CliNotFoundError {
    /// Create a new CLI not found error
    pub fn new(message: impl Into<String>, cli_path: Option<PathBuf>) -> Self {
        Self {
            message: message.into(),
            cli_path,
        }
    }
}

/// Error when connecting to Claude Code CLI
#[derive(Debug, Error)]
#[error("Connection error: {message}")]
pub struct ConnectionError {
    /// Error message
    pub message: String,
}

impl ConnectionError {
    /// Create a new connection error
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Error when the CLI process fails
#[derive(Debug, Error)]
#[error("Process error (exit code {exit_code:?}): {message}")]
pub struct ProcessError {
    /// Error message
    pub message: String,
    /// Process exit code
    pub exit_code: Option<i32>,
    /// stderr output
    pub stderr: Option<String>,
}

impl ProcessError {
    /// Create a new process error
    pub fn new(message: impl Into<String>, exit_code: Option<i32>, stderr: Option<String>) -> Self {
        Self {
            message: message.into(),
            exit_code,
            stderr,
        }
    }
}

/// Error when JSON decoding fails
#[derive(Debug, Error)]
#[error("JSON decode error: {message}")]
pub struct JsonDecodeError {
    /// Error message
    pub message: String,
    /// The line that failed to decode
    pub line: String,
}

impl JsonDecodeError {
    /// Create a new JSON decode error
    pub fn new(message: impl Into<String>, line: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            line: line.into(),
        }
    }
}

/// Error when message parsing fails
#[derive(Debug, Error)]
#[error("Message parse error: {message}")]
pub struct MessageParseError {
    /// Error message
    pub message: String,
    /// The data that failed to parse
    pub data: Option<serde_json::Value>,
}

impl MessageParseError {
    /// Create a new message parse error
    pub fn new(message: impl Into<String>, data: Option<serde_json::Value>) -> Self {
        Self {
            message: message.into(),
            data,
        }
    }
}

/// Image validation error
#[derive(Debug, Error)]
#[error("Image validation error: {message}")]
pub struct ImageValidationError {
    /// Error message
    pub message: String,
}

impl ImageValidationError {
    /// Create a new image validation error
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Result type for the Claude Agent SDK
pub type Result<T> = std::result::Result<T, ClaudeError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wrapped_error_reads_once_in_a_chain() {
        let missing = ClaudeError::from(CliNotFoundError::new("Claude Code CLI not found.", None));
        let e = anyhow::Error::from(missing).context("could not start the Claude CLI");
        assert_eq!(format!("{e:#}"), "could not start the Claude CLI: CLI not found: Claude Code CLI not found.");

        let exited = ClaudeError::from(ProcessError::new("the CLI exited", Some(1), None));
        assert_eq!(format!("{:#}", anyhow::Error::from(exited)), "Process error (exit code Some(1)): the CLI exited");
    }
}
