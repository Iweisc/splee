//! Message parser for converting JSON to typed messages

use crate::errors::{MessageParseError, Result};
use crate::types::messages::Message;

/// Message parser for CLI output
pub struct MessageParser;

impl MessageParser {
    /// Parse a JSON value into a Message, consuming the value
    ///
    /// This method consumes the JSON value to avoid unnecessary cloning.
    /// On parse error, the original data is not available in the error
    /// since it was consumed during the parse attempt.
    pub fn parse(data: serde_json::Value) -> Result<Message> {
        serde_json::from_value(data).map_err(|e| {
            MessageParseError::new(
                format!("Failed to parse message: {}", e),
                None, // Don't include original data to avoid cloning overhead
            )
            .into()
        })
    }

    /// Splee patch: newer CLIs emit message types this release doesn't know (e.g. `tool_progress`,
    /// `rate_limit_event`). Those return None so readers skip them instead of failing the response.
    /// A known type that doesn't parse is still an error.
    pub fn parse_known(data: serde_json::Value) -> Option<Result<Message>> {
        let known = matches!(
            data.get("type").and_then(|t| t.as_str()),
            Some("assistant" | "system" | "result" | "stream_event" | "user" | "control_cancel_request")
        );
        match Self::parse(data) {
            Err(_) if !known => None,
            parsed => Some(parsed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn skips_unknown_types_but_not_broken_known_ones() {
        assert!(MessageParser::parse_known(json!({"type": "rate_limit_event", "rate_limit_info": {}})).is_none());
        assert!(MessageParser::parse_known(json!({"type": "tool_progress"})).is_none());
        assert!(matches!(MessageParser::parse_known(json!({"type": "result"})), Some(Err(_))));
    }
}
