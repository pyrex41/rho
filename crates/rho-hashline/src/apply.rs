use std::collections::{HashMap, HashSet};

use crate::edit::HashlineEdit;
use crate::error::{HashlineError, HashlineMismatchError, MismatchedLine};
use crate::hash::compute_line_hash;
use crate::heuristics;
use crate::parse::{parse_anchor, LineRef};

/// Parsed references for an edit operation.
enum ParsedRefs {
    Single { line_ref: LineRef },
    Range { start: LineRef, end: LineRef },
    InsertAfter { after: LineRef },
}

struct ParsedEdit {
    spec: ParsedRefs,
    dst_lines: Vec<String>,
}

struct AnnotatedEdit {
    spec: ParsedRefs,
    dst_lines: Vec<String>,
    #[allow(dead_code)]
    original_index: usize,
    sort_line: usize,
    precedence: usize, // 0 for set/replace, 1 for insert_after
}

fn split_dst_lines(dst: &str) -> Vec<String> {
    if dst.is_empty() {
        Vec::new()
    } else {
        dst.split('\n').map(|s| s.to_string()).collect()
    }
}

fn parse_hashline_edit(edit: &HashlineEdit) -> Result<ParsedEdit, HashlineError> {
    match edit {
        HashlineEdit::SetLine { set_line } => {
            let line_ref = parse_anchor(&set_line.anchor)?;
            let dst_lines =
                heuristics::strip_new_line_prefixes(&split_dst_lines(&set_line.new_text));
            Ok(ParsedEdit {
                spec: ParsedRefs::Single { line_ref },
                dst_lines,
            })
        }
        HashlineEdit::ReplaceLines { replace_lines } => {
            let start = parse_anchor(&replace_lines.start_anchor)?;
            let end = parse_anchor(&replace_lines.end_anchor)?;
            let dst_lines =
                heuristics::strip_new_line_prefixes(&split_dst_lines(&replace_lines.new_text));
            if start.line == end.line {
                Ok(ParsedEdit {
                    spec: ParsedRefs::Single { line_ref: start },
                    dst_lines,
                })
            } else {
                Ok(ParsedEdit {
                    spec: ParsedRefs::Range { start, end },
                    dst_lines,
                })
            }
        }
        HashlineEdit::InsertAfter { insert_after } => {
            let after = parse_anchor(&insert_after.anchor)?;
            let dst_lines =
                heuristics::strip_new_line_prefixes(&split_dst_lines(&insert_after.text));
            if dst_lines.is_empty() {
                return Err(HashlineError::Other(
                    "Insert-after edit requires non-empty text".to_string(),
                ));
            }
            Ok(ParsedEdit {
                spec: ParsedRefs::InsertAfter { after },
                dst_lines,
            })
        }
        HashlineEdit::Replace { .. } => Err(HashlineError::Other(
            "Replace edits are applied separately via apply_replace_edit".to_string(),
        )),
    }
}

/// Collect all explicitly touched line numbers from parsed edits.
fn collect_touched_lines(parsed: &[ParsedEdit]) -> HashSet<usize> {
    let mut touched = HashSet::new();
    for p in parsed {
        match &p.spec {
            ParsedRefs::Single { line_ref } => {
                touched.insert(line_ref.line);
            }
            ParsedRefs::Range { start, end } => {
                for ln in start.line..=end.line {
                    touched.insert(ln);
                }
            }
            ParsedRefs::InsertAfter { after } => {
                touched.insert(after.line);
            }
        }
    }
    touched
}

/// Apply an array of hashline edits (set_line, replace_lines, insert_after) to file content.
///
/// The algorithm has 5 phases:
/// 1. Parse all edits into resolved operations with line ranges
/// 2. Pre-validate ALL hash references before any mutation
/// 3. Deduplicate identical edits
/// 4. Sort bottom-up (highest line number first)
/// 5. Apply edits with heuristic cleanups
pub fn apply_hashline_edits(
    content: &str,
    edits: &[HashlineEdit],
) -> Result<String, HashlineError> {
    if edits.is_empty() {
        return Ok(content.to_string());
    }

    // Separate replace edits from hashline edits
    let mut hashline_edits = Vec::new();
    let mut replace_edits = Vec::new();
    for edit in edits {
        match edit {
            HashlineEdit::Replace { replace } => replace_edits.push(replace.clone()),
            _ => hashline_edits.push(edit),
        }
    }

    // Apply replace edits first (they work on the full text)
    let mut result = content.to_string();
    for replace in &replace_edits {
        result = apply_replace_edit(&result, replace)?;
    }

    if hashline_edits.is_empty() {
        return Ok(result);
    }

    // PHASE 1: Parse all edits
    let mut parsed: Vec<ParsedEdit> = Vec::new();
    for edit in &hashline_edits {
        parsed.push(parse_hashline_edit(edit)?);
    }

    let file_lines: Vec<&str> = result.split('\n').collect();
    let original_file_lines = file_lines.clone();

    // PHASE 2: Pre-validate ALL references
    // Build hash -> line lookup for unique hashes
    let mut unique_line_by_hash: HashMap<String, usize> = HashMap::new();
    let mut seen_duplicate_hashes: HashSet<String> = HashSet::new();
    for (i, line) in file_lines.iter().enumerate() {
        let line_no = i + 1;
        let hash = compute_line_hash(line).to_string();
        if seen_duplicate_hashes.contains(&hash) {
            continue;
        }
        use std::collections::hash_map::Entry;
        match unique_line_by_hash.entry(hash.clone()) {
            Entry::Occupied(e) => {
                e.remove();
                seen_duplicate_hashes.insert(hash);
            }
            Entry::Vacant(e) => {
                e.insert(line_no);
            }
        }
    }

    let mut mismatches: Vec<MismatchedLine> = Vec::new();

    // Validate or relocate each reference
    for p in &mut parsed {
        match &mut p.spec {
            ParsedRefs::Single { line_ref } => {
                if let ValidateResult::OutOfBounds { line, total } = validate_or_relocate(
                    line_ref,
                    &file_lines,
                    &unique_line_by_hash,
                    &mut mismatches,
                ) {
                    return Err(HashlineError::LineOutOfBounds { line, total });
                }
            }
            ParsedRefs::InsertAfter { after } => {
                if let ValidateResult::OutOfBounds { line, total } =
                    validate_or_relocate(after, &file_lines, &unique_line_by_hash, &mut mismatches)
                {
                    return Err(HashlineError::LineOutOfBounds { line, total });
                }
            }
            ParsedRefs::Range { start, end } => {
                if start.line > end.line {
                    return Err(HashlineError::Other(format!(
                        "Range start line {} must be <= end line {}",
                        start.line, end.line
                    )));
                }

                let original_start = start.line;
                let original_end = end.line;
                let original_count = original_end - original_start + 1;

                let start_result =
                    validate_or_relocate(start, &file_lines, &unique_line_by_hash, &mut mismatches);
                if let ValidateResult::OutOfBounds { line, total } = start_result {
                    return Err(HashlineError::LineOutOfBounds { line, total });
                }
                let end_result =
                    validate_or_relocate(end, &file_lines, &unique_line_by_hash, &mut mismatches);
                if let ValidateResult::OutOfBounds { line, total } = end_result {
                    return Err(HashlineError::LineOutOfBounds { line, total });
                }

                if matches!(start_result, ValidateResult::Ok)
                    && matches!(end_result, ValidateResult::Ok)
                {
                    let relocated_count = end.line - start.line + 1;
                    let start_relocated = start.line != original_start;
                    let end_relocated = end.line != original_end;
                    let changed_by_relocation = start_relocated || end_relocated;
                    let invalid_range = start.line > end.line;
                    let scope_changed = relocated_count != original_count;

                    if changed_by_relocation && (invalid_range || scope_changed) {
                        // Revert relocation and report mismatches
                        start.line = original_start;
                        end.line = original_end;
                        mismatches.push(build_mismatch(start, original_start, &file_lines));
                        mismatches.push(build_mismatch(end, original_end, &file_lines));
                    }
                }
            }
        }
    }

    if !mismatches.is_empty() {
        return Err(HashlineError::Mismatch(HashlineMismatchError::new(
            mismatches,
            &file_lines,
        )));
    }

    // Recompute touched lines after relocation
    let explicitly_touched = collect_touched_lines(&parsed);

    // PHASE 3: Deduplicate identical edits
    let mut seen_keys: HashMap<String, usize> = HashMap::new();
    let mut dedup_indices: HashSet<usize> = HashSet::new();
    for (i, p) in parsed.iter().enumerate() {
        let line_key = match &p.spec {
            ParsedRefs::Single { line_ref } => format!("s:{}", line_ref.line),
            ParsedRefs::Range { start, end } => format!("r:{}:{}", start.line, end.line),
            ParsedRefs::InsertAfter { after } => format!("i:{}", after.line),
        };
        let dst_key = format!("{}|{}", line_key, p.dst_lines.join("\n"));
        match seen_keys.entry(dst_key) {
            std::collections::hash_map::Entry::Occupied(_) => {
                dedup_indices.insert(i);
            }
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(i);
            }
        }
    }

    // Remove duplicates (iterate in reverse to preserve indices)
    let mut filtered_parsed: Vec<(ParsedEdit, usize)> = Vec::new();
    for (i, p) in parsed.into_iter().enumerate() {
        if !dedup_indices.contains(&i) {
            filtered_parsed.push((p, i));
        }
    }

    // PHASE 4: Sort bottom-up
    let mut annotated: Vec<AnnotatedEdit> = filtered_parsed
        .into_iter()
        .map(|(p, idx)| {
            let (sort_line, precedence) = match &p.spec {
                ParsedRefs::Single { line_ref } => (line_ref.line, 0),
                ParsedRefs::Range { end, .. } => (end.line, 0),
                ParsedRefs::InsertAfter { after } => (after.line, 1),
            };
            AnnotatedEdit {
                spec: p.spec,
                dst_lines: p.dst_lines,
                original_index: idx,
                sort_line,
                precedence,
            }
        })
        .collect();

    annotated.sort_by(|a, b| {
        b.sort_line
            .cmp(&a.sort_line)
            .then(a.precedence.cmp(&b.precedence))
            .then(a.original_index.cmp(&b.original_index))
    });

    // PHASE 5: Apply edits bottom-up
    let mut file_lines: Vec<String> = file_lines.iter().map(|s| s.to_string()).collect();

    for edit in &annotated {
        match &edit.spec {
            ParsedRefs::Single { line_ref } => {
                // Try merge detection first
                if let Some((start_line, delete_count, new_lines)) =
                    heuristics::maybe_expand_single_line_merge(
                        line_ref.line,
                        &edit.dst_lines,
                        &original_file_lines.to_vec(),
                        &explicitly_touched,
                    )
                {
                    let orig_lines: Vec<&str> =
                        original_file_lines[start_line - 1..start_line - 1 + delete_count].to_vec();
                    let mut next_lines = heuristics::restore_indent_for_paired_replacement(
                        &[orig_lines[0]],
                        &new_lines,
                    );
                    if orig_lines.join("\n")
                        == next_lines
                            .iter()
                            .map(|s| s.as_str())
                            .collect::<Vec<_>>()
                            .join("\n")
                        && heuristics::any_confusable_hyphens(&orig_lines)
                    {
                        next_lines = heuristics::normalize_confusable_hyphens_in_lines(&next_lines);
                    }
                    if orig_lines.join("\n")
                        != next_lines
                            .iter()
                            .map(|s| s.as_str())
                            .collect::<Vec<_>>()
                            .join("\n")
                    {
                        let end = start_line - 1 + delete_count;
                        file_lines.splice(start_line - 1..end, next_lines);
                    }
                    continue;
                }

                let orig_lines: Vec<&str> = vec![original_file_lines[line_ref.line - 1]];
                let stripped = heuristics::strip_range_boundary_echo(
                    &original_file_lines,
                    line_ref.line,
                    line_ref.line,
                    &edit.dst_lines,
                );
                let stripped = heuristics::restore_old_wrapped_lines(&orig_lines, &stripped);
                let mut new_lines =
                    heuristics::restore_indent_for_paired_replacement(&orig_lines, &stripped);
                if orig_lines.join("\n")
                    == new_lines
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                    && heuristics::any_confusable_hyphens(&orig_lines)
                {
                    new_lines = heuristics::normalize_confusable_hyphens_in_lines(&new_lines);
                }
                // Skip noop
                if orig_lines.join("\n")
                    != new_lines
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                {
                    let start = line_ref.line - 1;
                    file_lines.splice(start..start + 1, new_lines);
                }
            }
            ParsedRefs::Range { start, end } => {
                let count = end.line - start.line + 1;
                let orig_lines: Vec<&str> =
                    original_file_lines[start.line - 1..start.line - 1 + count].to_vec();
                let stripped = heuristics::strip_range_boundary_echo(
                    &original_file_lines,
                    start.line,
                    end.line,
                    &edit.dst_lines,
                );
                let stripped = heuristics::restore_old_wrapped_lines(&orig_lines, &stripped);
                let mut new_lines =
                    heuristics::restore_indent_for_paired_replacement(&orig_lines, &stripped);
                if orig_lines.join("\n")
                    == new_lines
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                    && heuristics::any_confusable_hyphens(&orig_lines)
                {
                    new_lines = heuristics::normalize_confusable_hyphens_in_lines(&new_lines);
                }
                if orig_lines.join("\n")
                    != new_lines
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                {
                    let s = start.line - 1;
                    file_lines.splice(s..s + count, new_lines);
                }
            }
            ParsedRefs::InsertAfter { after } => {
                let anchor_line = original_file_lines[after.line - 1];
                let inserted = heuristics::strip_insert_anchor_echo(anchor_line, &edit.dst_lines);
                if !inserted.is_empty() {
                    let pos = after.line;
                    file_lines.splice(pos..pos, inserted);
                }
            }
        }
    }

    Ok(file_lines.join("\n"))
}

/// Apply a fuzzy text replace operation.
fn apply_replace_edit(
    content: &str,
    replace: &crate::edit::ReplaceOp,
) -> Result<String, HashlineError> {
    let old_text = &replace.old_text;
    let new_text = &replace.new_text;

    // Try exact match first
    let matches: Vec<usize> = content
        .match_indices(old_text)
        .map(|(idx, _)| idx)
        .collect();

    if !matches.is_empty() {
        if !replace.all && matches.len() > 1 {
            return Err(HashlineError::MultipleMatches);
        }
        if replace.all {
            return Ok(content.replace(old_text, new_text));
        }
        // Single match
        let idx = matches[0];
        let mut result = String::with_capacity(content.len());
        result.push_str(&content[..idx]);
        result.push_str(new_text);
        result.push_str(&content[idx + old_text.len()..]);
        return Ok(result);
    }

    // Try whitespace-insensitive match
    let content_stripped = strip_ws_for_search(content);
    let needle_stripped = strip_ws_for_search(old_text);

    if needle_stripped.is_empty() {
        return Err(HashlineError::TextNotFound(old_text.to_string()));
    }

    let ws_matches = find_fuzzy_matches(content, &content_stripped, &needle_stripped);
    if ws_matches.is_empty() {
        return Err(HashlineError::TextNotFound(old_text.to_string()));
    }
    if !replace.all && ws_matches.len() > 1 {
        return Err(HashlineError::MultipleMatches);
    }

    // Apply replacements back-to-front
    let mut result = content.to_string();
    let mut sorted = ws_matches;
    sorted.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    for (start, end) in sorted {
        result = format!("{}{}{}", &result[..start], new_text, &result[end..]);
        if !replace.all {
            break;
        }
    }
    Ok(result)
}

/// Strip whitespace for fuzzy matching, keeping a mapping back to original positions.
fn strip_ws_for_search(s: &str) -> Vec<(char, usize)> {
    s.char_indices()
        .filter(|(_, c)| !c.is_whitespace())
        .map(|(i, c)| (c, i))
        .collect()
}

/// Find fuzzy (whitespace-insensitive) matches, returning (start_byte, end_byte) pairs.
fn find_fuzzy_matches(
    original: &str,
    content_stripped: &[(char, usize)],
    needle_stripped: &[(char, usize)],
) -> Vec<(usize, usize)> {
    let needle_chars: Vec<char> = needle_stripped.iter().map(|(c, _)| *c).collect();
    let mut matches = Vec::new();

    'outer: for i in 0..content_stripped.len() {
        if i + needle_chars.len() > content_stripped.len() {
            break;
        }
        for (j, nc) in needle_chars.iter().enumerate() {
            if content_stripped[i + j].0 != *nc {
                continue 'outer;
            }
        }
        // Found a match
        let start_byte = content_stripped[i].1;
        let last = i + needle_chars.len() - 1;
        let last_byte = content_stripped[last].1;
        // end_byte: include any trailing whitespace after the last matched char
        let end_byte = next_non_ws_or_end(original, last_byte);
        matches.push((start_byte, end_byte));
    }
    matches
}

/// Find the byte position after the matched char, consuming its full char width.
fn next_non_ws_or_end(s: &str, byte_pos: usize) -> usize {
    let mut pos = byte_pos;
    // Advance past the character at byte_pos
    if let Some(c) = s[pos..].chars().next() {
        pos += c.len_utf8();
    }
    pos
}

enum ValidateResult {
    Ok,
    Mismatch,
    OutOfBounds { line: usize, total: usize },
}

fn validate_or_relocate(
    line_ref: &mut LineRef,
    file_lines: &[&str],
    unique_line_by_hash: &HashMap<String, usize>,
    mismatches: &mut Vec<MismatchedLine>,
) -> ValidateResult {
    if line_ref.line < 1 || line_ref.line > file_lines.len() {
        return ValidateResult::OutOfBounds {
            line: line_ref.line,
            total: file_lines.len(),
        };
    }
    let expected = line_ref.hash.to_lowercase();
    let actual_hash = compute_line_hash(file_lines[line_ref.line - 1]);
    if actual_hash == expected {
        return ValidateResult::Ok;
    }

    // Try relocation
    if let Some(&relocated) = unique_line_by_hash.get(&expected) {
        line_ref.line = relocated;
        return ValidateResult::Ok;
    }

    mismatches.push(MismatchedLine {
        line: line_ref.line,
        expected_hash: line_ref.hash.clone(),
        actual_hash: actual_hash.to_string(),
    });
    ValidateResult::Mismatch
}

fn build_mismatch(line_ref: &LineRef, line: usize, file_lines: &[&str]) -> MismatchedLine {
    MismatchedLine {
        line,
        expected_hash: line_ref.hash.clone(),
        actual_hash: compute_line_hash(file_lines[line - 1]).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::*;
    use crate::hash::compute_line_hash;

    fn make_content(lines: &[&str]) -> String {
        lines.join("\n")
    }

    fn hash_at(content: &str, line: usize) -> String {
        let lines: Vec<&str> = content.split('\n').collect();
        compute_line_hash(lines[line - 1]).to_string()
    }

    #[test]
    fn test_set_line_basic() {
        let content = make_content(&["alpha", "beta", "gamma"]);
        let h = hash_at(&content, 2);
        let edits = vec![HashlineEdit::SetLine {
            set_line: SetLineOp {
                anchor: format!("2:{}", h),
                new_text: "BETA".to_string(),
            },
        }];
        let result = apply_hashline_edits(&content, &edits).unwrap();
        assert_eq!(result, "alpha\nBETA\ngamma");
    }

    #[test]
    fn test_set_line_delete() {
        let content = make_content(&["alpha", "beta", "gamma"]);
        let h = hash_at(&content, 2);
        let edits = vec![HashlineEdit::SetLine {
            set_line: SetLineOp {
                anchor: format!("2:{}", h),
                new_text: "".to_string(),
            },
        }];
        let result = apply_hashline_edits(&content, &edits).unwrap();
        assert_eq!(result, "alpha\ngamma");
    }

    #[test]
    fn test_set_line_expand() {
        let content = make_content(&["alpha", "beta", "gamma"]);
        let h = hash_at(&content, 2);
        let edits = vec![HashlineEdit::SetLine {
            set_line: SetLineOp {
                anchor: format!("2:{}", h),
                new_text: "beta1\nbeta2".to_string(),
            },
        }];
        let result = apply_hashline_edits(&content, &edits).unwrap();
        assert_eq!(result, "alpha\nbeta1\nbeta2\ngamma");
    }

    #[test]
    fn test_replace_lines_basic() {
        let content = make_content(&["alpha", "beta", "gamma", "delta"]);
        let h2 = hash_at(&content, 2);
        let h3 = hash_at(&content, 3);
        let edits = vec![HashlineEdit::ReplaceLines {
            replace_lines: ReplaceLinesOp {
                start_anchor: format!("2:{}", h2),
                end_anchor: format!("3:{}", h3),
                new_text: "new_line".to_string(),
            },
        }];
        let result = apply_hashline_edits(&content, &edits).unwrap();
        assert_eq!(result, "alpha\nnew_line\ndelta");
    }

    #[test]
    fn test_replace_lines_delete_range() {
        let content = make_content(&["alpha", "beta", "gamma", "delta"]);
        let h2 = hash_at(&content, 2);
        let h3 = hash_at(&content, 3);
        let edits = vec![HashlineEdit::ReplaceLines {
            replace_lines: ReplaceLinesOp {
                start_anchor: format!("2:{}", h2),
                end_anchor: format!("3:{}", h3),
                new_text: "".to_string(),
            },
        }];
        let result = apply_hashline_edits(&content, &edits).unwrap();
        assert_eq!(result, "alpha\ndelta");
    }

    #[test]
    fn test_insert_after_basic() {
        let content = make_content(&["alpha", "beta", "gamma"]);
        let h2 = hash_at(&content, 2);
        let edits = vec![HashlineEdit::InsertAfter {
            insert_after: InsertAfterOp {
                anchor: format!("2:{}", h2),
                text: "inserted".to_string(),
            },
        }];
        let result = apply_hashline_edits(&content, &edits).unwrap();
        assert_eq!(result, "alpha\nbeta\ninserted\ngamma");
    }

    #[test]
    fn test_insert_after_multiline() {
        let content = make_content(&["alpha", "beta", "gamma"]);
        let h1 = hash_at(&content, 1);
        let edits = vec![HashlineEdit::InsertAfter {
            insert_after: InsertAfterOp {
                anchor: format!("1:{}", h1),
                text: "new1\nnew2".to_string(),
            },
        }];
        let result = apply_hashline_edits(&content, &edits).unwrap();
        assert_eq!(result, "alpha\nnew1\nnew2\nbeta\ngamma");
    }

    #[test]
    fn test_replace_exact() {
        let content = make_content(&["hello world", "foo bar"]);
        let edits = vec![HashlineEdit::Replace {
            replace: ReplaceOp {
                old_text: "hello world".to_string(),
                new_text: "goodbye world".to_string(),
                all: false,
            },
        }];
        let result = apply_hashline_edits(&content, &edits).unwrap();
        assert_eq!(result, "goodbye world\nfoo bar");
    }

    #[test]
    fn test_replace_all() {
        let content = "aaa bbb aaa";
        let edits = vec![HashlineEdit::Replace {
            replace: ReplaceOp {
                old_text: "aaa".to_string(),
                new_text: "ccc".to_string(),
                all: true,
            },
        }];
        let result = apply_hashline_edits(content, &edits).unwrap();
        assert_eq!(result, "ccc bbb ccc");
    }

    #[test]
    fn test_replace_unique_required() {
        let content = "aaa bbb aaa";
        let edits = vec![HashlineEdit::Replace {
            replace: ReplaceOp {
                old_text: "aaa".to_string(),
                new_text: "ccc".to_string(),
                all: false,
            },
        }];
        let result = apply_hashline_edits(content, &edits);
        assert!(result.is_err());
    }

    #[test]
    fn test_replace_not_found() {
        let content = "hello world";
        let edits = vec![HashlineEdit::Replace {
            replace: ReplaceOp {
                old_text: "xyz".to_string(),
                new_text: "abc".to_string(),
                all: false,
            },
        }];
        let result = apply_hashline_edits(content, &edits);
        assert!(result.is_err());
    }

    #[test]
    fn test_mismatch_error() {
        let content = make_content(&["alpha", "beta", "gamma"]);
        // Get real hash and compute a wrong one
        let real_hash = hash_at(&content, 2);
        let wrong_hash = if real_hash == "ff" { "00" } else { "ff" };
        // Ensure the wrong hash doesn't exist uniquely in the file (would cause relocation)
        // Use a content where all 3 lines have different hashes and none match wrong_hash
        let edits = vec![HashlineEdit::SetLine {
            set_line: SetLineOp {
                anchor: format!("2:{}", wrong_hash),
                new_text: "new".to_string(),
            },
        }];
        let result = apply_hashline_edits(&content, &edits);
        assert!(result.is_err());
        match result.unwrap_err() {
            HashlineError::Mismatch(m) => {
                assert!(m.message.contains(">>>"));
                assert_eq!(m.mismatched_lines.len(), 1);
            }
            other => panic!("Expected Mismatch, got: {:?}", other),
        }
    }

    #[test]
    fn test_relocation() {
        // Line 2 originally has "beta", then imagine 2 lines were inserted before it
        // We simulate by building content where "beta" is now at line 4
        // but the edit references line 2 with beta's hash
        let content = make_content(&["alpha", "new1", "new2", "beta", "gamma"]);
        let beta_hash = hash_at(&content, 4); // "beta" is at line 4
                                              // But we reference line 2 with beta's hash — should relocate to line 4
        let edits = vec![HashlineEdit::SetLine {
            set_line: SetLineOp {
                anchor: format!("2:{}", beta_hash),
                new_text: "BETA".to_string(),
            },
        }];
        let result = apply_hashline_edits(&content, &edits).unwrap();
        assert_eq!(result, "alpha\nnew1\nnew2\nBETA\ngamma");
    }

    #[test]
    fn test_deduplication() {
        let content = make_content(&["alpha", "beta", "gamma"]);
        let h2 = hash_at(&content, 2);
        // Same edit twice — should be deduplicated
        let edits = vec![
            HashlineEdit::SetLine {
                set_line: SetLineOp {
                    anchor: format!("2:{}", h2),
                    new_text: "BETA".to_string(),
                },
            },
            HashlineEdit::SetLine {
                set_line: SetLineOp {
                    anchor: format!("2:{}", h2),
                    new_text: "BETA".to_string(),
                },
            },
        ];
        let result = apply_hashline_edits(&content, &edits).unwrap();
        assert_eq!(result, "alpha\nBETA\ngamma");
    }

    #[test]
    fn test_bottom_up_sort() {
        let content = make_content(&["line1", "line2", "line3", "line4"]);
        let h2 = hash_at(&content, 2);
        let h4 = hash_at(&content, 4);
        // Edit line 2 and line 4 — should apply line 4 first (bottom-up)
        let edits = vec![
            HashlineEdit::SetLine {
                set_line: SetLineOp {
                    anchor: format!("2:{}", h2),
                    new_text: "TWO".to_string(),
                },
            },
            HashlineEdit::SetLine {
                set_line: SetLineOp {
                    anchor: format!("4:{}", h4),
                    new_text: "FOUR".to_string(),
                },
            },
        ];
        let result = apply_hashline_edits(&content, &edits).unwrap();
        assert_eq!(result, "line1\nTWO\nline3\nFOUR");
    }

    #[test]
    fn test_empty_edits() {
        let content = "hello\nworld";
        let result = apply_hashline_edits(content, &[]).unwrap();
        assert_eq!(result, content);
    }

    #[test]
    fn test_out_of_bounds() {
        let content = "hello";
        let edits = vec![HashlineEdit::SetLine {
            set_line: SetLineOp {
                anchor: "99:ab".to_string(),
                new_text: "new".to_string(),
            },
        }];
        let result = apply_hashline_edits(content, &edits);
        assert!(result.is_err());
    }

    #[test]
    fn test_hashline_prefix_stripping_in_new_text() {
        let content = make_content(&["alpha", "beta", "gamma"]);
        let h2 = hash_at(&content, 2);
        // Model copies hashline prefix into new_text
        let edits = vec![HashlineEdit::SetLine {
            set_line: SetLineOp {
                anchor: format!("2:{}", h2),
                new_text: format!("2:{}|BETA", h2),
            },
        }];
        let result = apply_hashline_edits(&content, &edits).unwrap();
        assert_eq!(result, "alpha\nBETA\ngamma");
    }

    #[test]
    fn test_insert_after_at_end() {
        let content = make_content(&["alpha", "beta"]);
        let h2 = hash_at(&content, 2);
        let edits = vec![HashlineEdit::InsertAfter {
            insert_after: InsertAfterOp {
                anchor: format!("2:{}", h2),
                text: "gamma".to_string(),
            },
        }];
        let result = apply_hashline_edits(&content, &edits).unwrap();
        assert_eq!(result, "alpha\nbeta\ngamma");
    }

    #[test]
    fn test_mixed_hashline_and_replace() {
        let content = make_content(&["alpha", "beta", "gamma"]);
        let h1 = hash_at(&content, 1);
        let edits = vec![
            HashlineEdit::Replace {
                replace: ReplaceOp {
                    old_text: "gamma".to_string(),
                    new_text: "GAMMA".to_string(),
                    all: false,
                },
            },
            HashlineEdit::SetLine {
                set_line: SetLineOp {
                    anchor: format!("1:{}", h1),
                    new_text: "ALPHA".to_string(),
                },
            },
        ];
        // Replace edits run first on raw content, then hashline edits run
        // But wait: after replace, the content changes, so hash at line 1
        // should still be valid since only line 3 changed.
        let result = apply_hashline_edits(&content, &edits).unwrap();
        assert_eq!(result, "ALPHA\nbeta\nGAMMA");
    }
}
