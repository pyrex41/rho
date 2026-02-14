use regex::Regex;
use std::collections::HashMap;
use std::sync::LazyLock;

/// Pattern matching hashline display format: `LINE:HASH|CONTENT`
static HASHLINE_PREFIX_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\d+:[0-9a-zA-Z]{1,16}\|").unwrap());

/// Check if a line starts with a single `+` (but not `++`).
fn has_diff_plus_prefix(s: &str) -> bool {
    if let Some(stripped) = s.strip_prefix('+') {
        !stripped.starts_with('+')
    } else {
        false
    }
}

/// Strip a leading `+` if it's not `++`.
fn strip_diff_plus(s: &str) -> String {
    if has_diff_plus_prefix(s) {
        s[1..].to_string()
    } else {
        s.to_string()
    }
}

/// Confusable hyphen characters (Unicode dashes)
static CONFUSABLE_HYPHENS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new("[\u{2010}\u{2011}\u{2012}\u{2013}\u{2014}\u{2212}\u{FE63}\u{FF0D}]").unwrap()
});

// ─── Utility functions ───

fn strip_all_whitespace(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

fn leading_whitespace(s: &str) -> &str {
    let end = s
        .find(|c: char| !c.is_whitespace())
        .unwrap_or(s.len());
    &s[..end]
}

fn equals_ignoring_whitespace(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    strip_all_whitespace(a) == strip_all_whitespace(b)
}

fn strip_trailing_continuation_tokens(s: &str) -> String {
    // Strip common trailing continuation tokens for merge detection
    let trimmed = s.trim_end();
    let suffixes = [
        "&&", "||", "??", "?", ":", "=", ",", "+", "-", "*", "/", ".", "(",
    ];
    for suffix in &suffixes {
        if let Some(stripped) = trimmed.strip_suffix(suffix) {
            return stripped.to_string();
        }
    }
    trimmed.to_string()
}

fn strip_merge_operator_chars(s: &str) -> String {
    s.chars().filter(|c| !matches!(c, '|' | '&' | '?')).collect()
}

// ─── Heuristic 1: Strip hashline prefixes ───

/// If >=50% of non-empty lines have hashline prefixes, strip them.
pub fn strip_hashline_prefixes(lines: &[String]) -> Vec<String> {
    let non_empty: Vec<&String> = lines.iter().filter(|l| !l.is_empty()).collect();
    if non_empty.is_empty() {
        return lines.to_vec();
    }

    let hash_count = non_empty
        .iter()
        .filter(|l| HASHLINE_PREFIX_RE.is_match(l))
        .count();

    if hash_count > 0 && hash_count * 2 >= non_empty.len() {
        lines
            .iter()
            .map(|l| HASHLINE_PREFIX_RE.replace(l, "").to_string())
            .collect()
    } else {
        lines.to_vec()
    }
}

// ─── Heuristic 2: Strip diff plus markers ───

/// If >=50% of non-empty lines start with `+` (but not `++`), strip the `+`.
/// Only fires if hashline prefix stripping did NOT fire.
pub fn strip_diff_plus_markers(lines: &[String]) -> Vec<String> {
    let non_empty: Vec<&String> = lines.iter().filter(|l| !l.is_empty()).collect();
    if non_empty.is_empty() {
        return lines.to_vec();
    }

    // Don't strip if hashline prefixes are present
    let hash_count = non_empty
        .iter()
        .filter(|l| HASHLINE_PREFIX_RE.is_match(l))
        .count();
    if hash_count > 0 && hash_count * 2 >= non_empty.len() {
        return lines.to_vec();
    }

    let plus_count = non_empty
        .iter()
        .filter(|l| has_diff_plus_prefix(l))
        .count();

    if plus_count > 0 && plus_count * 2 >= non_empty.len() {
        lines.iter().map(|l| strip_diff_plus(l)).collect()
    } else {
        lines.to_vec()
    }
}

/// Combined strip of hashline prefixes and diff plus markers.
pub fn strip_new_line_prefixes(lines: &[String]) -> Vec<String> {
    let non_empty: Vec<&String> = lines.iter().filter(|l| !l.is_empty()).collect();
    if non_empty.is_empty() {
        return lines.to_vec();
    }

    let hash_count = non_empty
        .iter()
        .filter(|l| HASHLINE_PREFIX_RE.is_match(l))
        .count();
    let diff_count = non_empty
        .iter()
        .filter(|l| has_diff_plus_prefix(l))
        .count();

    let strip_hash = hash_count > 0 && hash_count * 2 >= non_empty.len();
    let strip_plus = !strip_hash && diff_count > 0 && diff_count * 2 >= non_empty.len();

    if !strip_hash && !strip_plus {
        return lines.to_vec();
    }

    lines
        .iter()
        .map(|l| {
            if strip_hash {
                HASHLINE_PREFIX_RE.replace(l, "").to_string()
            } else if strip_plus {
                strip_diff_plus(l)
            } else {
                l.clone()
            }
        })
        .collect()
}

// ─── Heuristic 3: Boundary echo stripping ───

/// For insert_after: if first line of inserted text matches the anchor line, strip it.
pub fn strip_insert_anchor_echo(anchor_line: &str, dst_lines: &[String]) -> Vec<String> {
    if dst_lines.len() <= 1 {
        return dst_lines.to_vec();
    }
    if equals_ignoring_whitespace(&dst_lines[0], anchor_line) {
        dst_lines[1..].to_vec()
    } else {
        dst_lines.to_vec()
    }
}

/// For replace_lines: strip first/last line if they match lines before/after the range.
/// Only when replacement is longer than original.
pub fn strip_range_boundary_echo(
    file_lines: &[&str],
    start_line: usize,
    end_line: usize,
    dst_lines: &[String],
) -> Vec<String> {
    let count = end_line - start_line + 1;
    if dst_lines.len() <= 1 || dst_lines.len() <= count {
        return dst_lines.to_vec();
    }

    let mut out = dst_lines.to_vec();

    // Check if first line echoes the line before the range
    let before_idx = if start_line >= 2 { start_line - 2 } else { usize::MAX };
    if before_idx < file_lines.len()
        && equals_ignoring_whitespace(&out[0], file_lines[before_idx])
    {
        out = out[1..].to_vec();
    }

    // Check if last line echoes the line after the range
    let after_idx = end_line; // 0-indexed for the line after end_line (1-indexed)
    if after_idx < file_lines.len()
        && !out.is_empty()
        && equals_ignoring_whitespace(out.last().unwrap(), file_lines[after_idx])
    {
        out.pop();
    }

    out
}

// ─── Heuristic 4: Indentation restoration ───

/// If new line has no indent but corresponding original did, prepend original's indent.
pub fn restore_indent_for_paired_replacement(
    old_lines: &[&str],
    new_lines: &[String],
) -> Vec<String> {
    if old_lines.len() != new_lines.len() {
        return new_lines.to_vec();
    }
    let mut changed = false;
    let mut out: Vec<String> = Vec::with_capacity(new_lines.len());
    for (old, new) in old_lines.iter().zip(new_lines.iter()) {
        let restored = restore_leading_indent(old, new);
        if restored != *new {
            changed = true;
        }
        out.push(restored);
    }
    if changed {
        out
    } else {
        new_lines.to_vec()
    }
}

fn restore_leading_indent(template_line: &str, line: &str) -> String {
    if line.is_empty() {
        return line.to_string();
    }
    let template_indent = leading_whitespace(template_line);
    if template_indent.is_empty() {
        return line.to_string();
    }
    let indent = leading_whitespace(line);
    if !indent.is_empty() {
        return line.to_string();
    }
    format!("{}{}", template_indent, line)
}

// ─── Heuristic 5: Wrapped line restoration ───

/// If canonicalized new text matches an original line uniquely, restore original.
pub fn restore_old_wrapped_lines(old_lines: &[&str], new_lines: &[String]) -> Vec<String> {
    if old_lines.is_empty() || new_lines.len() < 2 {
        return new_lines.to_vec();
    }

    // Build canonical -> original line lookup (only unique entries)
    let mut canon_to_old: HashMap<String, (String, usize)> = HashMap::new();
    for line in old_lines {
        let canon = strip_all_whitespace(line);
        let entry = canon_to_old.entry(canon).or_insert_with(|| (line.to_string(), 0));
        entry.1 += 1;
    }

    // Find candidate spans in new_lines that collapse to an original line
    struct Candidate {
        start: usize,
        len: usize,
        replacement: String,
        canon: String,
    }

    let mut candidates: Vec<Candidate> = Vec::new();
    for start in 0..new_lines.len() {
        for len in 2..=10.min(new_lines.len() - start) {
            let span: String = new_lines[start..start + len]
                .iter()
                .map(|l| strip_all_whitespace(l))
                .collect::<Vec<_>>()
                .join("");
            if span.len() < 6 {
                continue;
            }
            if let Some((original, count)) = canon_to_old.get(&span) {
                if *count == 1 {
                    candidates.push(Candidate {
                        start,
                        len,
                        replacement: original.clone(),
                        canon: span,
                    });
                }
            }
        }
    }

    if candidates.is_empty() {
        return new_lines.to_vec();
    }

    // Keep only candidates whose canonical match is unique in new output
    let mut canon_counts: HashMap<String, usize> = HashMap::new();
    for c in &candidates {
        *canon_counts.entry(c.canon.clone()).or_insert(0) += 1;
    }
    candidates.retain(|c| *canon_counts.get(&c.canon).unwrap_or(&0) == 1);

    if candidates.is_empty() {
        return new_lines.to_vec();
    }

    // Apply back-to-front
    candidates.sort_by(|a, b| b.start.cmp(&a.start));
    let mut out = new_lines.to_vec();
    for c in &candidates {
        out.splice(c.start..c.start + c.len, std::iter::once(c.replacement.clone()));
    }
    out
}

// ─── Heuristic 6: Merge detection ───

/// Detect if a single-line replacement absorbed an adjacent line.
/// Returns expanded (start_line, delete_count, new_lines) or None.
pub fn maybe_expand_single_line_merge(
    line: usize,
    dst: &[String],
    file_lines: &[&str],
    explicitly_touched: &std::collections::HashSet<usize>,
) -> Option<(usize, usize, Vec<String>)> {
    if dst.len() != 1 {
        return None;
    }
    if line < 1 || line > file_lines.len() {
        return None;
    }

    let new_line = &dst[0];
    let new_canon = strip_all_whitespace(new_line);
    let new_canon_for_merge_ops = strip_merge_operator_chars(&new_canon);
    if new_canon.is_empty() {
        return None;
    }

    let orig = file_lines[line - 1];
    let orig_canon = strip_all_whitespace(orig);
    let orig_canon_for_match = strip_trailing_continuation_tokens(&orig_canon);
    let orig_canon_for_merge_ops = strip_merge_operator_chars(&orig_canon);
    let orig_looks_like_continuation = orig_canon_for_match.len() < orig_canon.len();
    if orig_canon.is_empty() {
        return None;
    }

    let next_idx = line; // 0-indexed for the line after `line`
    let prev_idx = if line >= 2 { line - 2 } else { usize::MAX };

    // Case A: dst absorbed the next continuation line
    if orig_looks_like_continuation
        && next_idx < file_lines.len()
        && !explicitly_touched.contains(&(line + 1))
    {
        let next = file_lines[next_idx];
        let next_canon = strip_all_whitespace(next);
        if let (Some(a), Some(b)) = (
            new_canon.find(&orig_canon_for_match),
            new_canon.find(&next_canon),
        ) {
            if a < b && new_canon.len() <= orig_canon.len() + next_canon.len() + 32 {
                return Some((line, 2, vec![new_line.clone()]));
            }
        }
    }

    // Case B: dst absorbed the previous declaration/continuation line
    if prev_idx < file_lines.len() && !explicitly_touched.contains(&(line - 1)) {
        let prev = file_lines[prev_idx];
        let prev_canon = strip_all_whitespace(prev);
        let prev_canon_for_match = strip_trailing_continuation_tokens(&prev_canon);
        let prev_looks_like_continuation = prev_canon_for_match.len() < prev_canon.len();
        if !prev_looks_like_continuation {
            return None;
        }
        let prev_stripped = strip_merge_operator_chars(&prev_canon_for_match);
        if let (Some(a), Some(b)) = (
            new_canon_for_merge_ops.find(&prev_stripped),
            new_canon_for_merge_ops.find(&orig_canon_for_merge_ops),
        ) {
            if a < b && new_canon.len() <= prev_canon.len() + orig_canon.len() + 32 {
                return Some((line - 1, 2, vec![new_line.clone()]));
            }
        }
    }

    None
}

// ─── Heuristic 7: Confusable hyphen normalization ───

/// Replace Unicode dashes with ASCII hyphen.
pub fn normalize_confusable_hyphens(s: &str) -> String {
    CONFUSABLE_HYPHENS_RE.replace_all(s, "-").to_string()
}

/// Normalize confusable hyphens in all lines.
pub fn normalize_confusable_hyphens_in_lines(lines: &[String]) -> Vec<String> {
    lines.iter().map(|l| normalize_confusable_hyphens(l)).collect()
}

/// Check if any line contains a confusable hyphen.
pub fn any_confusable_hyphens(lines: &[&str]) -> bool {
    lines.iter().any(|l| CONFUSABLE_HYPHENS_RE.is_match(l))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── Heuristic 1: Strip hashline prefixes ───

    #[test]
    fn test_strip_hashline_prefixes_fires() {
        let lines = vec![
            "1:ab|const x = 1;".to_string(),
            "2:cd|const y = 2;".to_string(),
        ];
        let result = strip_hashline_prefixes(&lines);
        assert_eq!(result, vec!["const x = 1;", "const y = 2;"]);
    }

    #[test]
    fn test_strip_hashline_prefixes_below_threshold() {
        let lines = vec![
            "1:ab|const x = 1;".to_string(),
            "no prefix here".to_string(),
            "also no prefix".to_string(),
        ];
        let result = strip_hashline_prefixes(&lines);
        assert_eq!(result, lines);
    }

    #[test]
    fn test_strip_hashline_prefixes_empty() {
        let lines: Vec<String> = vec![];
        let result = strip_hashline_prefixes(&lines);
        assert!(result.is_empty());
    }

    // ─── Heuristic 2: Strip diff plus markers ───

    #[test]
    fn test_strip_diff_plus_fires() {
        let lines = vec![
            "+const x = 1;".to_string(),
            "+const y = 2;".to_string(),
        ];
        let result = strip_diff_plus_markers(&lines);
        assert_eq!(result, vec!["const x = 1;", "const y = 2;"]);
    }

    #[test]
    fn test_strip_diff_plus_preserves_double_plus() {
        let lines = vec![
            "++not a diff".to_string(),
            "+real diff".to_string(),
            "+another diff".to_string(),
        ];
        let result = strip_diff_plus_markers(&lines);
        assert_eq!(result, vec!["++not a diff", "real diff", "another diff"]);
    }

    #[test]
    fn test_strip_diff_plus_below_threshold() {
        let lines = vec![
            "+diff line".to_string(),
            "normal line".to_string(),
            "also normal".to_string(),
        ];
        let result = strip_diff_plus_markers(&lines);
        assert_eq!(result, lines);
    }

    // ─── Heuristic 3: Boundary echo stripping ───

    #[test]
    fn test_strip_insert_anchor_echo() {
        let anchor = "function hello() {";
        let dst = vec![
            "function hello() {".to_string(),
            "  return true;".to_string(),
            "}".to_string(),
        ];
        let result = strip_insert_anchor_echo(anchor, &dst);
        assert_eq!(result, vec!["  return true;", "}"]);
    }

    #[test]
    fn test_strip_insert_anchor_echo_no_match() {
        let anchor = "function hello() {";
        let dst = vec![
            "  return true;".to_string(),
            "}".to_string(),
        ];
        let result = strip_insert_anchor_echo(anchor, &dst);
        assert_eq!(result, dst);
    }

    #[test]
    fn test_strip_insert_anchor_echo_single_line() {
        let anchor = "x";
        let dst = vec!["x".to_string()];
        let result = strip_insert_anchor_echo(anchor, &dst);
        assert_eq!(result, dst); // Don't strip when only 1 line
    }

    #[test]
    fn test_strip_range_boundary_echo() {
        let file_lines = vec![
            "before",       // 0
            "line 1",       // 1 (start_line=2)
            "line 2",       // 2 (end_line=3)
            "after",        // 3
        ];
        let dst = vec![
            "before".to_string(),
            "new content".to_string(),
            "after".to_string(),
        ];
        // start_line=2, end_line=3, replacing 2 lines with 3 (longer)
        let result = strip_range_boundary_echo(&file_lines, 2, 3, &dst);
        assert_eq!(result, vec!["new content"]);
    }

    #[test]
    fn test_strip_range_boundary_echo_not_longer() {
        let file_lines = vec!["before", "line 1", "line 2", "line 3", "after"];
        let dst = vec!["new 1".to_string(), "new 2".to_string()];
        // Replacing 3 lines (2-4) with 2 — shorter, so no stripping
        let result = strip_range_boundary_echo(&file_lines, 2, 4, &dst);
        assert_eq!(result, dst);
    }

    // ─── Heuristic 4: Indentation restoration ───

    #[test]
    fn test_restore_indent() {
        let old = vec!["    const x = 1;"];
        let new_lines = vec!["const x = 2;".to_string()];
        let result = restore_indent_for_paired_replacement(&old, &new_lines);
        assert_eq!(result, vec!["    const x = 2;"]);
    }

    #[test]
    fn test_restore_indent_already_indented() {
        let old = vec!["    const x = 1;"];
        let new_lines = vec!["    const x = 2;".to_string()];
        let result = restore_indent_for_paired_replacement(&old, &new_lines);
        assert_eq!(result, vec!["    const x = 2;"]);
    }

    #[test]
    fn test_restore_indent_length_mismatch() {
        let old = vec!["    a", "    b"];
        let new_lines = vec!["c".to_string()];
        let result = restore_indent_for_paired_replacement(&old, &new_lines);
        assert_eq!(result, vec!["c"]); // Different lengths, no restoration
    }

    // ─── Heuristic 5: Wrapped line restoration ───

    #[test]
    fn test_restore_wrapped_lines() {
        let old = vec!["const result = foo + bar + baz;"];
        let new_lines = vec![
            "const result = foo +".to_string(),
            " bar + baz;".to_string(),
        ];
        let result = restore_old_wrapped_lines(&old, &new_lines);
        assert_eq!(result, vec!["const result = foo + bar + baz;"]);
    }

    #[test]
    fn test_restore_wrapped_lines_no_match() {
        let old = vec!["something completely different"];
        let new_lines = vec!["foo".to_string(), "bar".to_string()];
        let result = restore_old_wrapped_lines(&old, &new_lines);
        assert_eq!(result, new_lines);
    }

    #[test]
    fn test_restore_wrapped_lines_too_short() {
        let old = vec!["ab"];
        let new_lines = vec!["a".to_string(), "b".to_string()];
        let result = restore_old_wrapped_lines(&old, &new_lines);
        assert_eq!(result, new_lines); // Too short (< 6 chars)
    }

    // ─── Heuristic 6: Merge detection ───

    #[test]
    fn test_merge_detection_next_line() {
        let file_lines = vec![
            "if (condition &&",     // line 1, continuation
            "    other_thing) {",   // line 2
            "  body;",              // line 3
        ];
        let touched = std::collections::HashSet::new();
        let dst = vec!["if (condition && other_thing) {".to_string()];
        let result = maybe_expand_single_line_merge(1, &dst, &file_lines, &touched);
        assert!(result.is_some());
        let (start, count, _) = result.unwrap();
        assert_eq!(start, 1);
        assert_eq!(count, 2);
    }

    #[test]
    fn test_merge_detection_touched_line_skipped() {
        let file_lines = vec![
            "if (condition &&",
            "    other_thing) {",
            "  body;",
        ];
        let mut touched = std::collections::HashSet::new();
        touched.insert(2); // next line is touched
        let dst = vec!["if (condition && other_thing) {".to_string()];
        let result = maybe_expand_single_line_merge(1, &dst, &file_lines, &touched);
        assert!(result.is_none());
    }

    // ─── Heuristic 7: Confusable hyphen normalization ───

    #[test]
    fn test_normalize_hyphens() {
        let input = "some\u{2013}thing\u{2014}else";
        let result = normalize_confusable_hyphens(input);
        assert_eq!(result, "some-thing-else");
    }

    #[test]
    fn test_normalize_hyphens_already_ascii() {
        let input = "some-thing-else";
        let result = normalize_confusable_hyphens(input);
        assert_eq!(result, input);
    }

    #[test]
    fn test_normalize_hyphens_in_lines() {
        let lines = vec![
            "first\u{2010}line".to_string(),
            "second-line".to_string(),
        ];
        let result = normalize_confusable_hyphens_in_lines(&lines);
        assert_eq!(result, vec!["first-line", "second-line"]);
    }

    #[test]
    fn test_any_confusable() {
        assert!(any_confusable_hyphens(&["hello\u{2013}world"]));
        assert!(!any_confusable_hyphens(&["hello-world"]));
    }

    // ─── Combined prefix stripping ───

    #[test]
    fn test_strip_new_line_prefixes_hashline_priority() {
        // When both hashline and diff-plus are present, hashline wins
        let lines = vec![
            "1:ab|+line".to_string(),
            "2:cd|+other".to_string(),
        ];
        let result = strip_new_line_prefixes(&lines);
        assert_eq!(result, vec!["+line", "+other"]);
    }
}
