use crate::hash::compute_line_hash;

/// Format content with hashline prefixes.
///
/// Each line becomes `LINE:HASH|content` where LINE is 1-indexed
/// (starting from `start_line`), HASH is a 2-char hex hash, and
/// pipe `|` separates the prefix from content.
pub fn format_hashlines(content: &str, start_line: usize) -> String {
    let lines: Vec<&str> = content.split('\n').collect();
    lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            let num = start_line + i;
            let hash = compute_line_hash(line);
            format!("{}:{}|{}", num, hash, line)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_single_line() {
        let result = format_hashlines("hello", 1);
        assert!(result.starts_with("1:"));
        assert!(result.contains("|hello"));
    }

    #[test]
    fn test_multi_line() {
        let result = format_hashlines("a\nb\nc", 1);
        let lines: Vec<&str> = result.split('\n').collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].starts_with("1:"));
        assert!(lines[1].starts_with("2:"));
        assert!(lines[2].starts_with("3:"));
        assert!(lines[0].contains("|a"));
        assert!(lines[1].contains("|b"));
        assert!(lines[2].contains("|c"));
    }

    #[test]
    fn test_start_line_offset() {
        let result = format_hashlines("x\ny", 10);
        let lines: Vec<&str> = result.split('\n').collect();
        assert!(lines[0].starts_with("10:"));
        assert!(lines[1].starts_with("11:"));
    }

    #[test]
    fn test_empty_content() {
        let result = format_hashlines("", 1);
        assert!(result.starts_with("1:"));
        assert!(result.contains("|"));
    }

    #[test]
    fn test_line_with_pipe() {
        let result = format_hashlines("a | b", 1);
        // The pipe in the content should not interfere
        assert!(result.contains("|a | b"));
    }
}
