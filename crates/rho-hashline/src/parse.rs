use crate::error::HashlineError;

/// A parsed line reference with 1-indexed line number and 2-char hex hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineRef {
    pub line: usize,
    pub hash: String,
}

/// Parse a `"LINE:HASH"` anchor string into a `LineRef`.
///
/// Accepts formats like:
/// - `"5:a3"` (basic)
/// - `"5:a3|some content"` (with display suffix, stripped)
/// - `"5 : a3"` (whitespace around colon, normalized)
pub fn parse_anchor(anchor: &str) -> Result<LineRef, HashlineError> {
    // Strip display-format suffix: "5:ab|some content" -> "5:ab"
    let cleaned = if let Some(idx) = anchor.find('|') {
        &anchor[..idx]
    } else {
        anchor
    };
    // Also strip legacy "  " suffix
    let cleaned = if let Some(idx) = cleaned.find("  ") {
        &cleaned[..idx]
    } else {
        cleaned
    };
    let cleaned = cleaned.trim();
    // Normalize whitespace around colon
    let normalized = cleaned.replace(' ', "");

    // Try strict match: LINE:HASH where HASH is 1-16 hex chars
    let colon_pos = normalized.find(':').ok_or_else(|| {
        HashlineError::InvalidAnchor(format!(
            "Invalid line reference \"{}\". Expected format \"LINE:HASH\" (e.g. \"5:a3\").",
            anchor
        ))
    })?;

    let line_str = &normalized[..colon_pos];
    let hash_str = &normalized[colon_pos + 1..];

    let line: usize = line_str.parse().map_err(|_| {
        HashlineError::InvalidAnchor(format!(
            "Invalid line reference \"{}\". Expected format \"LINE:HASH\" (e.g. \"5:a3\").",
            anchor
        ))
    })?;

    if line < 1 {
        return Err(HashlineError::InvalidAnchor(format!(
            "Line number must be >= 1, got {} in \"{}\".",
            line, anchor
        )));
    }

    // Validate hash: 1-16 hex chars
    if hash_str.is_empty()
        || hash_str.len() > 16
        || !hash_str.chars().all(|c| c.is_ascii_hexdigit())
    {
        return Err(HashlineError::InvalidAnchor(format!(
            "Invalid line reference \"{}\". Expected format \"LINE:HASH\" (e.g. \"5:a3\").",
            anchor
        )));
    }

    Ok(LineRef {
        line,
        hash: hash_str.to_string(),
    })
}

/// Parse a line reference from formatted output `"LINE:HASH|content"`.
///
/// Returns the `LineRef` and the content portion after the pipe.
pub fn parse_line_ref(formatted: &str) -> Result<(LineRef, &str), HashlineError> {
    let pipe_pos = formatted.find('|').ok_or_else(|| {
        HashlineError::InvalidAnchor(format!("No pipe separator found in \"{}\"", formatted))
    })?;

    let prefix = &formatted[..pipe_pos];
    let content = &formatted[pipe_pos + 1..];

    let line_ref = parse_anchor(prefix)?;
    Ok((line_ref, content))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_basic_anchor() {
        let r = parse_anchor("5:a3").unwrap();
        assert_eq!(r.line, 5);
        assert_eq!(r.hash, "a3");
    }

    #[test]
    fn test_parse_anchor_with_display_suffix() {
        let r = parse_anchor("5:a3|some content").unwrap();
        assert_eq!(r.line, 5);
        assert_eq!(r.hash, "a3");
    }

    #[test]
    fn test_parse_anchor_with_whitespace() {
        let r = parse_anchor(" 5 : a3 ").unwrap();
        assert_eq!(r.line, 5);
        assert_eq!(r.hash, "a3");
    }

    #[test]
    fn test_parse_anchor_long_hash() {
        let r = parse_anchor("10:abcdef0123456789").unwrap();
        assert_eq!(r.line, 10);
        assert_eq!(r.hash, "abcdef0123456789");
    }

    #[test]
    fn test_parse_anchor_invalid_no_colon() {
        assert!(parse_anchor("5a3").is_err());
    }

    #[test]
    fn test_parse_anchor_invalid_no_hash() {
        assert!(parse_anchor("5:").is_err());
    }

    #[test]
    fn test_parse_anchor_invalid_non_hex() {
        assert!(parse_anchor("5:zz").is_err());
    }

    #[test]
    fn test_parse_anchor_invalid_line_zero() {
        assert!(parse_anchor("0:ab").is_err());
    }

    #[test]
    fn test_parse_anchor_invalid_line_text() {
        assert!(parse_anchor("abc:ab").is_err());
    }

    #[test]
    fn test_parse_line_ref() {
        let (r, content) = parse_line_ref("5:a3|hello world").unwrap();
        assert_eq!(r.line, 5);
        assert_eq!(r.hash, "a3");
        assert_eq!(content, "hello world");
    }

    #[test]
    fn test_parse_line_ref_empty_content() {
        let (r, content) = parse_line_ref("1:ff|").unwrap();
        assert_eq!(r.line, 1);
        assert_eq!(r.hash, "ff");
        assert_eq!(content, "");
    }

    #[test]
    fn test_parse_line_ref_content_with_pipe() {
        let (r, content) = parse_line_ref("3:ab|a | b").unwrap();
        assert_eq!(r.line, 3);
        assert_eq!(r.hash, "ab");
        assert_eq!(content, "a | b");
    }

    #[test]
    fn test_parse_line_ref_no_pipe() {
        assert!(parse_line_ref("3:ab").is_err());
    }
}
