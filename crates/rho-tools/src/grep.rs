// Grep tool — ripgrep-library search with context lines

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use globset::Glob;
use grep_regex::RegexMatcherBuilder;
use grep_searcher::{Searcher, SearcherBuilder, Sink, SinkContext, SinkMatch};
use ignore::WalkBuilder;
use rho_core::tool::{AgentTool, ToolError};
use rho_core::types::{Content, ToolResult};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

pub struct GrepTool {
    working_dir: PathBuf,
}

impl GrepTool {
    pub fn new(working_dir: PathBuf) -> Self {
        Self { working_dir }
    }
}

/// A single line collected during search (either a match or context).
struct CollectedLine {
    line_number: u64,
    content: String,
    is_match: bool,
}

/// A group of contiguous lines (one match plus its surrounding context).
struct MatchGroup {
    file_path: String,
    match_line_number: u64,
    lines: Vec<CollectedLine>,
}

/// Sink that collects matches and context lines, grouped by contiguous regions.
struct GrepSink {
    /// Lines accumulated for the current contiguous group.
    current_lines: Vec<CollectedLine>,
    /// The line number of the match in the current group (0 if no match yet).
    current_match_line: u64,
    /// All completed groups for the current file.
    groups: Vec<MatchGroup>,
    /// Relative path of the current file.
    file_path: String,
    /// Total match count across all files (for limit tracking).
    total_matches: usize,
    /// Max matches to collect.
    limit: usize,
    /// Matches to skip before collecting.
    offset: usize,
    /// Matches seen so far (before offset filtering).
    seen: usize,
}

impl GrepSink {
    fn new(file_path: String, offset: usize, limit: usize, seen: usize, total_matches: usize) -> Self {
        Self {
            current_lines: Vec::new(),
            current_match_line: 0,
            groups: Vec::new(),
            file_path,
            total_matches,
            limit,
            offset,
            seen,
        }
    }

    fn flush_current_group(&mut self) {
        if self.current_match_line > 0 && !self.current_lines.is_empty() {
            self.groups.push(MatchGroup {
                file_path: self.file_path.clone(),
                match_line_number: self.current_match_line,
                lines: std::mem::take(&mut self.current_lines),
            });
            self.current_match_line = 0;
        } else {
            self.current_lines.clear();
        }
    }

    fn at_limit(&self) -> bool {
        self.total_matches >= self.limit
    }
}

impl Sink for GrepSink {
    type Error = std::io::Error;

    fn matched(
        &mut self,
        _searcher: &Searcher,
        mat: &SinkMatch<'_>,
    ) -> Result<bool, Self::Error> {
        self.seen += 1;
        if self.seen <= self.offset {
            // Before offset: skip but keep searching
            self.current_lines.clear();
            return Ok(true);
        }
        if self.at_limit() {
            // Count it but stop collecting
            return Ok(false);
        }

        let line_num = mat.line_number().unwrap_or(0);
        let content = String::from_utf8_lossy(mat.bytes())
            .trim_end_matches('\n')
            .trim_end_matches('\r')
            .to_string();

        self.current_match_line = line_num;
        self.current_lines.push(CollectedLine {
            line_number: line_num,
            content,
            is_match: true,
        });
        self.total_matches += 1;

        Ok(!self.at_limit())
    }

    fn context(
        &mut self,
        _searcher: &Searcher,
        ctx: &SinkContext<'_>,
    ) -> Result<bool, Self::Error> {
        // If we haven't passed the offset yet, skip context
        if self.seen < self.offset {
            return Ok(true);
        }
        if self.at_limit() {
            return Ok(false);
        }

        let line_num = ctx.line_number().unwrap_or(0);
        let content = String::from_utf8_lossy(ctx.bytes())
            .trim_end_matches('\n')
            .trim_end_matches('\r')
            .to_string();

        self.current_lines.push(CollectedLine {
            line_number: line_num,
            content,
            is_match: false,
        });

        Ok(true)
    }

    fn context_break(
        &mut self,
        _searcher: &Searcher,
    ) -> Result<bool, Self::Error> {
        self.flush_current_group();
        Ok(!self.at_limit())
    }

    fn finish(
        &mut self,
        _searcher: &Searcher,
        _: &grep_searcher::SinkFinish,
    ) -> Result<(), Self::Error> {
        self.flush_current_group();
        Ok(())
    }
}

#[async_trait]
impl AgentTool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn is_concurrent_safe(&self) -> bool {
        true
    }

    fn label(&self) -> String {
        "Grep".to_string()
    }

    fn description(&self) -> String {
        "Search file contents with regex. Returns matches with line numbers.".to_string()
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Regex pattern to search for"
                },
                "path": {
                    "type": "string",
                    "description": "File or directory to search (default: working directory)"
                },
                "glob": {
                    "type": "string",
                    "description": "Filter files by glob pattern (e.g., '*.rs', '*.{ts,tsx}')"
                },
                "context_before": {
                    "type": "integer",
                    "description": "Lines of context before each match (default: 0)"
                },
                "context_after": {
                    "type": "integer",
                    "description": "Lines of context after each match (default: 0)"
                },
                "case_sensitive": {
                    "type": "boolean",
                    "description": "Whether the search is case sensitive (default: true)"
                },
                "multiline": {
                    "type": "boolean",
                    "description": "Enable multiline matching (default: false)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Max number of matches to return (default: 50)"
                },
                "offset": {
                    "type": "integer",
                    "description": "Skip first N matches (default: 0)"
                }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(
        &self,
        _tool_call_id: &str,
        params: Value,
        _cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        let pattern = params
            .get("pattern")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ToolError::InvalidParameters("missing or invalid 'pattern' parameter".into())
            })?
            .to_string();

        let search_path = match params.get("path").and_then(|v| v.as_str()) {
            Some(p) => {
                let p = Path::new(p);
                if p.is_absolute() {
                    p.to_path_buf()
                } else {
                    self.working_dir.join(p)
                }
            }
            None => self.working_dir.clone(),
        };

        let glob_pattern = params.get("glob").and_then(|v| v.as_str()).map(String::from);
        let context_before = params
            .get("context_before")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;
        let context_after = params
            .get("context_after")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;
        let case_sensitive = params
            .get("case_sensitive")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let multiline = params
            .get("multiline")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(50) as usize;
        let offset = params
            .get("offset")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;

        let working_dir = self.working_dir.clone();

        // Run search in a blocking task since grep crates are synchronous
        let grep_params = GrepParams {
            pattern,
            search_path,
            working_dir,
            glob_pattern,
            context_before,
            context_after,
            case_sensitive,
            multiline,
            limit,
            offset,
        };
        let result = tokio::task::spawn_blocking(move || run_grep(&grep_params))
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("task join error: {e}")))?;

        result
    }
}

struct GrepParams {
    pattern: String,
    search_path: PathBuf,
    working_dir: PathBuf,
    glob_pattern: Option<String>,
    context_before: usize,
    context_after: usize,
    case_sensitive: bool,
    multiline: bool,
    limit: usize,
    offset: usize,
}

fn run_grep(params: &GrepParams) -> Result<ToolResult, ToolError> {
    let pattern = &params.pattern;
    let search_path = &params.search_path;
    let working_dir = &params.working_dir;
    let glob_pattern = params.glob_pattern.as_deref();
    let context_before = params.context_before;
    let context_after = params.context_after;
    let case_sensitive = params.case_sensitive;
    let multiline = params.multiline;
    let limit = params.limit;
    let offset = params.offset;
    // Build regex matcher
    let matcher = RegexMatcherBuilder::new()
        .case_insensitive(!case_sensitive)
        .multi_line(multiline)
        .build(pattern)
        .map_err(|e| ToolError::InvalidParameters(format!("invalid regex: {e}")))?;

    // Build searcher
    let mut searcher = SearcherBuilder::new()
        .line_number(true)
        .before_context(context_before)
        .after_context(context_after)
        .multi_line(multiline)
        .build();

    // Build glob filter if specified
    let glob_matcher = match glob_pattern {
        Some(g) => Some(
            Glob::new(g)
                .map_err(|e| ToolError::InvalidParameters(format!("invalid glob: {e}")))?
                .compile_matcher(),
        ),
        None => None,
    };

    if !search_path.exists() {
        return Err(ToolError::ExecutionFailed(format!(
            "path not found: {}",
            search_path.display()
        )));
    }

    let mut all_groups: Vec<MatchGroup> = Vec::new();
    let mut total_matches: usize = 0;
    let mut seen: usize = 0;

    let is_file = search_path.is_file();

    if is_file {
        // Search a single file
        let rel_path = make_relative(search_path, working_dir);
        let mut sink = GrepSink::new(rel_path, offset, limit, seen, total_matches);
        let _ = searcher.search_path(&matcher, search_path, &mut sink);
        total_matches = sink.total_matches;
        seen = sink.seen;
        all_groups.extend(sink.groups);
    } else {
        // Walk directory
        let walker = WalkBuilder::new(search_path)
            .hidden(false)
            .git_ignore(true)
            .build();

        for entry in walker {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };

            if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                continue;
            }

            let path = entry.path();

            // Apply glob filter
            if let Some(ref gm) = glob_matcher {
                let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                let rel = make_relative(path, working_dir);
                if !gm.is_match(file_name) && !gm.is_match(&rel) {
                    continue;
                }
            }

            let rel_path = make_relative(path, working_dir);
            let mut sink = GrepSink::new(rel_path, offset, limit, seen, total_matches);
            let _ = searcher.search_path(&matcher, path, &mut sink);
            total_matches = sink.total_matches;
            seen = sink.seen;
            all_groups.extend(sink.groups);

            if total_matches >= limit {
                break;
            }
        }
    }

    // The total number of matches seen (including those before offset)
    let total_seen = seen;

    if all_groups.is_empty() {
        return Ok(ToolResult {
            content: vec![Content::Text {
                text: format!("No matches found for pattern: {pattern}"),
            }],
            details: serde_json::json!({}),
        });
    }

    // Format output
    let mut output = String::new();
    for (i, group) in all_groups.iter().enumerate() {
        if i > 0 {
            output.push('\n');
        }

        // Compute line number padding width for this group
        let max_line_num = group
            .lines
            .iter()
            .map(|l| l.line_number)
            .max()
            .unwrap_or(0);
        let line_width = max_line_num.to_string().len();

        output.push_str(&format!(
            "{}. {}:{}\n",
            i + 1,
            group.file_path,
            group.match_line_number
        ));

        for line in &group.lines {
            let padded = format!("{:>width$}", line.line_number, width = line_width);
            let hash = rho_hashline::compute_line_hash(&line.content);
            if line.is_match {
                output.push_str(&format!(">>{}:{}|{}\n", padded, hash, line.content));
            } else {
                output.push_str(&format!("  {}:{}|{}\n", padded, hash, line.content));
            }
        }
    }

    // Remove trailing newline
    if output.ends_with('\n') {
        output.pop();
    }

    // Add truncation note if applicable
    let effective_total = if offset > 0 {
        total_seen.saturating_sub(offset)
    } else {
        total_seen
    };
    if total_matches >= limit && effective_total > limit {
        output.push_str(&format!("\n\n(showing {} of {} matches)", limit, effective_total));
    }

    Ok(ToolResult {
        content: vec![Content::Text { text: output }],
        details: serde_json::json!({
            "match_count": total_matches,
            "total": effective_total,
        }),
    })
}

fn make_relative(path: &Path, base: &Path) -> String {
    path.strip_prefix(base)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn create_tool(dir: &Path) -> GrepTool {
        GrepTool::new(dir.to_path_buf())
    }

    #[tokio::test]
    async fn basic_pattern_match() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.rs");
        fs::write(&file, "fn main() {\n    println!(\"hello\");\n}\n").unwrap();

        let tool = create_tool(dir.path());
        let params = serde_json::json!({
            "pattern": "println"
        });

        let result = tool
            .execute("call_1", params, CancellationToken::new())
            .await
            .unwrap();

        let text = match &result.content[0] {
            Content::Text { text } => text,
            _ => panic!("expected Text"),
        };
        assert!(text.contains("println"), "should contain match: {text}");
        assert!(text.contains(">>"), "should have match marker: {text}");
        assert!(text.contains("test.rs:2"), "should show file:line: {text}");
    }

    #[tokio::test]
    async fn context_lines() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("ctx.txt");
        fs::write(
            &file,
            "line1\nline2\nline3\nMATCH\nline5\nline6\nline7\n",
        )
        .unwrap();

        let tool = create_tool(dir.path());
        let params = serde_json::json!({
            "pattern": "MATCH",
            "context_before": 2,
            "context_after": 1
        });

        let result = tool
            .execute("call_2", params, CancellationToken::new())
            .await
            .unwrap();

        let text = match &result.content[0] {
            Content::Text { text } => text,
            _ => panic!("expected Text"),
        };

        assert!(text.contains("line2"), "should have before context: {text}");
        assert!(text.contains("line3"), "should have before context: {text}");
        assert!(text.contains("line5"), "should have after context: {text}");
        // line1 should NOT be included (only 2 lines of before context)
        assert!(!text.contains("line1"), "should not have line1: {text}");
    }

    #[tokio::test]
    async fn glob_filtering() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("code.rs"), "fn hello() {}\n").unwrap();
        fs::write(dir.path().join("notes.txt"), "hello world\n").unwrap();

        let tool = create_tool(dir.path());
        let params = serde_json::json!({
            "pattern": "hello",
            "glob": "*.rs"
        });

        let result = tool
            .execute("call_3", params, CancellationToken::new())
            .await
            .unwrap();

        let text = match &result.content[0] {
            Content::Text { text } => text,
            _ => panic!("expected Text"),
        };

        assert!(text.contains("code.rs"), "should match .rs file: {text}");
        assert!(!text.contains("notes.txt"), "should not match .txt file: {text}");
    }

    #[tokio::test]
    async fn case_insensitive() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("mixed.txt");
        fs::write(&file, "Hello\nhello\nHELLO\n").unwrap();

        let tool = create_tool(dir.path());
        let params = serde_json::json!({
            "pattern": "hello",
            "case_sensitive": false
        });

        let result = tool
            .execute("call_4", params, CancellationToken::new())
            .await
            .unwrap();

        let text = match &result.content[0] {
            Content::Text { text } => text,
            _ => panic!("expected Text"),
        };

        // All three lines should match
        let match_count = text.matches(">>").count();
        assert_eq!(match_count, 3, "should find 3 case-insensitive matches: {text}");
    }

    #[tokio::test]
    async fn limit_and_offset() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("many.txt");
        let content: String = (1..=10).map(|i| format!("match_{i}\n")).collect();
        fs::write(&file, &content).unwrap();

        let tool = create_tool(dir.path());

        // Test limit
        let params = serde_json::json!({
            "pattern": "match_",
            "limit": 3
        });
        let result = tool
            .execute("call_5a", params, CancellationToken::new())
            .await
            .unwrap();
        let text = match &result.content[0] {
            Content::Text { text } => text,
            _ => panic!("expected Text"),
        };
        let match_count = text.matches(">>").count();
        assert_eq!(match_count, 3, "should be limited to 3 matches: {text}");

        // Test offset
        let params = serde_json::json!({
            "pattern": "match_",
            "offset": 2,
            "limit": 3
        });
        let result = tool
            .execute("call_5b", params, CancellationToken::new())
            .await
            .unwrap();
        let text = match &result.content[0] {
            Content::Text { text } => text,
            _ => panic!("expected Text"),
        };
        assert!(text.contains("match_3"), "offset 2 should start at match_3: {text}");
        assert!(!text.contains("match_1"), "should skip match_1: {text}");
        assert!(!text.contains("match_2"), "should skip match_2: {text}");
    }

    #[tokio::test]
    async fn no_matches() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("empty_match.txt");
        fs::write(&file, "nothing here\n").unwrap();

        let tool = create_tool(dir.path());
        let params = serde_json::json!({
            "pattern": "zzz_nonexistent"
        });

        let result = tool
            .execute("call_6", params, CancellationToken::new())
            .await
            .unwrap();

        let text = match &result.content[0] {
            Content::Text { text } => text,
            _ => panic!("expected Text"),
        };
        assert!(
            text.contains("No matches found"),
            "should report no matches: {text}"
        );
    }

    #[tokio::test]
    async fn invalid_regex() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("dummy.txt");
        fs::write(&file, "content\n").unwrap();

        let tool = create_tool(dir.path());
        let params = serde_json::json!({
            "pattern": "[invalid"
        });

        let err = tool
            .execute("call_7", params, CancellationToken::new())
            .await
            .unwrap_err();

        match err {
            ToolError::InvalidParameters(msg) => {
                assert!(msg.contains("regex"), "should mention regex: {msg}");
            }
            other => panic!("expected InvalidParameters, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn directory_search() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("subdir");
        fs::create_dir(&sub).unwrap();
        fs::write(dir.path().join("a.txt"), "target line\n").unwrap();
        fs::write(sub.join("b.txt"), "another target\n").unwrap();
        fs::write(dir.path().join("c.txt"), "no match here\n").unwrap();

        let tool = create_tool(dir.path());
        let params = serde_json::json!({
            "pattern": "target"
        });

        let result = tool
            .execute("call_8", params, CancellationToken::new())
            .await
            .unwrap();

        let text = match &result.content[0] {
            Content::Text { text } => text,
            _ => panic!("expected Text"),
        };

        let match_count = text.matches(">>").count();
        assert_eq!(match_count, 2, "should find matches in both files: {text}");
        assert!(text.contains("a.txt"), "should include a.txt: {text}");
        assert!(text.contains("b.txt"), "should include b.txt: {text}");
        assert!(!text.contains("c.txt"), "should not include c.txt: {text}");
    }

    #[tokio::test]
    async fn single_file_path() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("specific.txt");
        fs::write(&file, "findme here\nand findme there\n").unwrap();
        fs::write(dir.path().join("other.txt"), "findme too\n").unwrap();

        let tool = create_tool(dir.path());
        let params = serde_json::json!({
            "pattern": "findme",
            "path": file.to_str().unwrap()
        });

        let result = tool
            .execute("call_9", params, CancellationToken::new())
            .await
            .unwrap();

        let text = match &result.content[0] {
            Content::Text { text } => text,
            _ => panic!("expected Text"),
        };

        // Should only search the specific file
        assert!(text.contains("specific.txt"), "should match in specific.txt: {text}");
        assert!(!text.contains("other.txt"), "should not match in other.txt: {text}");
    }

    #[tokio::test]
    async fn path_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let tool = create_tool(dir.path());
        let params = serde_json::json!({
            "pattern": "test",
            "path": "/nonexistent/path/that/does/not/exist"
        });

        let err = tool
            .execute("call_10", params, CancellationToken::new())
            .await
            .unwrap_err();

        match err {
            ToolError::ExecutionFailed(msg) => {
                assert!(msg.contains("not found"), "should mention path not found: {msg}");
            }
            other => panic!("expected ExecutionFailed, got: {other:?}"),
        }
    }
}
