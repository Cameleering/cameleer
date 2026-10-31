//! Regular expression engine (std-only, backtracking).
//!
//! PLACEHOLDER: only literal patterns are supported until the full engine lands.

use crate::error::{CamelError, Result};

/// A compiled regular expression.
#[derive(Clone, Debug)]
pub struct Regex {
    pattern: String,
}

impl Regex {
    pub fn new(pattern: &str) -> Result<Regex> {
        if pattern.chars().any(|c| "\\^$.|?*+()[]{}".contains(c)) {
            return Err(CamelError::syntax(format!("regex not yet supported: {}", pattern)));
        }
        Ok(Regex { pattern: pattern.to_string() })
    }
    pub fn as_str(&self) -> &str {
        &self.pattern
    }
    /// True if the regex matches anywhere in `text`.
    pub fn is_match(&self, text: &str) -> bool {
        text.contains(&self.pattern)
    }
    /// True if the regex matches the entire `text` (Java `String.matches`).
    pub fn is_full_match(&self, text: &str) -> bool {
        text == self.pattern
    }
}

impl Regex {
    /// Replace all matches with `replacement` (supports `$1` group references).
    pub fn replace_all(&self, text: &str, replacement: &str) -> String {
        text.replace(&self.pattern, replacement)
    }
}
