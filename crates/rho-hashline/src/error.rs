use std::fmt;

use crate::hash::compute_line_hash;

/// Number of context lines shown above/below each mismatched line.
const MISMATCH_CONTEXT: usize = 2;

#[derive(Debug, thiserror::Error)]
pub enum HashlineError {
    #[error("Invalid anchor format: {0}")]
    InvalidAnchor(String),

    #[error("Line {line} out of bounds (file has {total} lines)")]
    LineOutOfBounds { line: usize, total: usize },

    #[error("{0}")]
    Mismatch(HashlineMismatchError),

    #[error("Text not found: {0}")]
    TextNotFound(String),

    #[error("Multiple matches for text (expected unique match)")]
    MultipleMatches,

    #[error("{0}")]
    Other(String),
}

#[derive(Debug)]
pub struct MismatchedLine {
    pub line: usize,
    pub expected_hash: String,
    pub actual_hash: String,
}

#[derive(Debug)]
pub struct HashlineMismatchError {
    pub message: String,
    pub mismatched_lines: Vec<MismatchedLine>,
}

impl fmt::Display for HashlineMismatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for HashlineMismatchError {}

impl HashlineMismatchError {
    /// Build a mismatch error with context lines and `>>>` markers.
    pub fn new(mismatches: Vec<MismatchedLine>, file_lines: &[&str]) -> Self {
        let mismatch_set: std::collections::HashMap<usize, &MismatchedLine> =
            mismatches.iter().map(|m| (m.line, m)).collect();

        // Collect line ranges to display (mismatch lines + context)
        let mut display_lines = std::collections::BTreeSet::new();
        for m in &mismatches {
            let lo = if m.line > MISMATCH_CONTEXT {
                m.line - MISMATCH_CONTEXT
            } else {
                1
            };
            let hi = std::cmp::min(file_lines.len(), m.line + MISMATCH_CONTEXT);
            for i in lo..=hi {
                display_lines.insert(i);
            }
        }

        let mut lines = Vec::new();
        lines.push(format!(
            "{} line{} changed since last read. Use the updated LINE:HASH references shown below (>>> marks changed lines).",
            mismatches.len(),
            if mismatches.len() > 1 { "s have" } else { " has" }
        ));
        lines.push(String::new());

        let sorted: Vec<usize> = display_lines.into_iter().collect();
        let mut prev_line: Option<usize> = None;

        for &line_num in &sorted {
            if let Some(prev) = prev_line {
                if line_num > prev + 1 {
                    lines.push("    ...".to_string());
                }
            }
            prev_line = Some(line_num);

            let content = file_lines[line_num - 1];
            let hash = compute_line_hash(content);
            let prefix = format!("{}:{}", line_num, hash);

            if mismatch_set.contains_key(&line_num) {
                lines.push(format!(">>> {}|{}", prefix, content));
            } else {
                lines.push(format!("    {}|{}", prefix, content));
            }
        }

        let message = lines.join("\n");
        HashlineMismatchError {
            message,
            mismatched_lines: mismatches,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mismatch_error_single_line() {
        let lines = vec![
            "const a = 1;",
            "const b = 2;",
            "const c = 3;",
            "const d = 4;",
            "const e = 5;",
        ];
        let mismatches = vec![MismatchedLine {
            line: 3,
            expected_hash: "xx".to_string(),
            actual_hash: compute_line_hash("const c = 3;").to_string(),
        }];
        let err = HashlineMismatchError::new(mismatches, &lines);
        assert!(err.message.contains("1 line has changed"));
        assert!(err.message.contains(">>>"));
        // Context lines should be present
        assert!(err.message.contains("const a = 1;"));
        assert!(err.message.contains("const e = 5;"));
    }

    #[test]
    fn test_mismatch_error_multiple_lines() {
        let lines = vec![
            "line 1", "line 2", "line 3", "line 4", "line 5", "line 6", "line 7", "line 8",
            "line 9", "line 10",
        ];
        let mismatches = vec![
            MismatchedLine {
                line: 2,
                expected_hash: "xx".to_string(),
                actual_hash: "yy".to_string(),
            },
            MismatchedLine {
                line: 9,
                expected_hash: "aa".to_string(),
                actual_hash: "bb".to_string(),
            },
        ];
        let err = HashlineMismatchError::new(mismatches, &lines);
        assert!(err.message.contains("2 lines have changed"));
        // Non-contiguous regions should have separator
        assert!(err.message.contains("..."));
    }

    #[test]
    fn test_mismatch_error_at_boundaries() {
        let lines = vec!["first", "second", "third"];
        let mismatches = vec![MismatchedLine {
            line: 1,
            expected_hash: "xx".to_string(),
            actual_hash: "yy".to_string(),
        }];
        let err = HashlineMismatchError::new(mismatches, &lines);
        assert!(err.message.contains(">>>"));
        assert!(err.message.contains("first"));
    }
}
