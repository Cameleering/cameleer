//! Syntax errors with source positions.

use camel_core::error::{kinds, CamelError};

use crate::ast::Dialect;

/// A syntax error at a byte offset of the script.
#[derive(Clone, Debug)]
pub struct SyntaxError {
    pub pos: usize,
    pub msg: String,
}

/// 1-based line and column of a byte offset.
pub fn line_col(src: &str, pos: usize) -> (usize, usize) {
    let pos = pos.min(src.len());
    let mut line = 1;
    let mut col = 1;
    for (i, c) in src.char_indices() {
        if i >= pos {
            break;
        }
        if c == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

impl SyntaxError {
    /// Convert to a Camel `ExpressionIllegalSyntaxException` with line and column.
    pub fn into_camel(self, d: Dialect, src: &str) -> CamelError {
        let (l, c) = line_col(src, self.pos);
        CamelError::new(kinds::EXPRESSION_ILLEGAL_SYNTAX, format!("{} syntax error at line {}, column {}: {} in expression: {}", d.display_name(), l, c, self.msg, src.trim()))
    }
}
