# Rust Coding Agent: Fast File Tools & Hashline Editing

## Design Principle

Do not shell out to CLI tools. Use Rust-native libraries that power those CLI tools. The grep/find/replace tools the agent exposes to the LLM should be built on the same crates that power ripgrep, fd, and sd — called as library functions, not subprocesses.

---

## Part 1: Fast File Operations — Crate Selection

### File Discovery & Traversal

**Primary: `ignore` crate** (from ripgrep author)
- .gitignore-aware parallel directory traversal
- Respects `.gitignore`, `.git/info/exclude`, global gitignore, `.ignore`
- Built-in parallel walker with thread pool
- Used internally by both ripgrep and fd

```rust
use ignore::WalkBuilder;

// Parallel, gitignore-aware traversal
WalkBuilder::new(search_path)
    .hidden(false)           // include dotfiles
    .git_ignore(true)        // respect .gitignore
    .max_depth(Some(20))
    .build_parallel()
    .run(|| Box::new(|entry| {
        // process each entry in parallel
        ignore::WalkState::Continue
    }));
```

**For glob matching: `globset` crate** (also from ripgrep)

```rust
use globset::{Glob, GlobSetBuilder};

let mut builder = GlobSetBuilder::new();
builder.add(Glob::new("**/*.rs")?);
builder.add(Glob::new("!**/target/**")?);
let set = builder.build()?;
// set.matches("src/main.rs") → vec of matching pattern indices
```

### Text Search (Grep Tool)

**Primary: `grep-searcher` + `grep-regex` + `grep-matcher`** (ripgrep's library crates)

These are the actual libraries that power ripgrep. Not wrappers. Not shelling out. The real thing.

```rust
use grep_regex::RegexMatcherBuilder;
use grep_searcher::{Searcher, SearcherBuilder, Sink, SinkMatch, SinkContext, SinkError};

// Build matcher (equivalent to rg's pattern handling)
let matcher = RegexMatcherBuilder::new()
    .case_smart(true)
    .build(&pattern)?;

// Build searcher with context lines
let mut searcher = SearcherBuilder::new()
    .line_number(true)
    .before_context(2)
    .after_context(2)
    .build();

// Custom sink that collects results with hashline formatting
struct HashlineSink {
    matches: Vec<GrepMatch>,
    file_lines: Vec<String>,  // for computing hashes
}

impl Sink for HashlineSink {
    type Error = std::io::Error;

    fn matched(&mut self, _searcher: &Searcher, mat: &SinkMatch) -> Result<bool, Self::Error> {
        let line_number = mat.line_number().unwrap() as usize;
        let content = std::str::from_utf8(mat.bytes()).unwrap().trim_end();
        let hash = compute_line_hash(content);
        // Format: >>LINE:HASH|content
        self.matches.push(GrepMatch {
            line: line_number,
            hash,
            content: content.to_string(),
        });
        Ok(true) // continue searching
    }

    fn context(&mut self, _searcher: &Searcher, ctx: &SinkContext) -> Result<bool, Self::Error> {
        // Format:   LINE:HASH|content  (context lines, indented)
        Ok(true)
    }
}

// Execute search
searcher.search_path(&matcher, &file_path, &mut sink)?;
```

**Why not shell out to `rg`**: The grep-searcher crate gives us direct access to match results with line numbers, byte offsets, and context — exactly what we need to format hashline output. Shelling out means parsing rg's text output, which is fragile and slower.

### Fast Find (Find/Glob Tool)

Compose `ignore` + `globset`:

```rust
use ignore::WalkBuilder;
use globset::Glob;

fn find_files(root: &Path, pattern: &str, limit: usize) -> Vec<PathBuf> {
    let glob = Glob::new(pattern).unwrap().compile_matcher();
    let mut results = Vec::new();

    for entry in WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .sort_by_file_path(|a, b| {
            // Sort by modification time (most recent first)
            // Use file metadata in comparator
            a.cmp(b)
        })
        .build()
    {
        if results.len() >= limit { break; }
        let entry = entry.unwrap();
        if glob.is_match(entry.path()) {
            results.push(entry.into_path());
        }
    }
    results
}
```

### Text Replacement

**Use `regex` crate directly** — same engine that powers sd and ripgrep.

For the agent's edit tool, we don't need a replacement CLI. The hashline system handles edits structurally. But for the `replace` fallback mode (fuzzy text matching), use:

```rust
use regex::Regex;

// Fuzzy whitespace matching: normalize both old_text and file content
fn fuzzy_find(content: &str, needle: &str) -> Option<(usize, usize)> {
    // Normalize whitespace in both strings for matching
    // Then map back to original byte offsets
}
```

### Fuzzy Matching (for file/symbol lookup, not editing)

**Primary: `nucleo-matcher`** (from helix editor)
- ~6x faster than skim
- Proper Unicode/grapheme handling
- Production-ready, near 1.0

```rust
use nucleo_matcher::{Matcher, Config, pattern::{Pattern, CaseMatching, AtomKind}};

let mut matcher = Matcher::new(Config::DEFAULT);
let pattern = Pattern::parse("agntlop", CaseMatching::Smart, AtomKind::Fuzzy);
let score = pattern.score(Utf32Str::new("agent-loop.ts", &mut buf), &mut matcher);
// Returns Some(score) if match, None if no match
```

### File I/O

**`memmap2`** for large file reads (zero-copy):

```rust
use memmap2::Mmap;
use std::fs::File;

let file = File::open(path)?;
let mmap = unsafe { Mmap::map(&file)? };
let content = std::str::from_utf8(&mmap)?;
// Process content without copying into a String
```

For normal-sized files (<1MB), just use `std::fs::read_to_string`. Memmap for files where you want streaming hashline output without loading the whole thing.

### Hashing

**`xxhash-rust`** for hashline computation:

```rust
use xxhash_rust::xxh32::xxh32;

fn compute_line_hash(line: &str) -> String {
    let normalized: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    let hash = xxh32(normalized.as_bytes(), 0) % 256;
    format!("{:02x}", hash)
}
```

### Crate Summary Table

| Purpose | Crate | Why |
|---------|-------|-----|
| Directory traversal | `ignore` | .gitignore-aware, parallel, battle-tested |
| Glob matching | `globset` | Same engine as ripgrep |
| Text search | `grep-searcher` + `grep-regex` | Ripgrep as a library |
| Regex | `regex` | Powers everything above |
| Fast hashing | `xxhash-rust` | xxHash32 for hashline hashes |
| Memory-mapped I/O | `memmap2` | Zero-copy large file reads |
| Fuzzy matching | `nucleo-matcher` | Helix editor's matcher, fastest available |
| JSON schema | `serde_json` | Tool parameter validation |
| HTTP + SSE | `reqwest` + `eventsource-stream` | LLM API streaming |
| Async runtime | `tokio` | Everything async |
| Process execution | `tokio::process` | Bash tool |
| Diff generation | `similar` | Unified diff output for edit results |

---

## Part 2: The Hashline System — Complete Specification

### Overview

Hashlines are a line-addressable file reference format where every line is tagged with a short content hash. The hash provides an integrity check: if the file changed since the model last read it, stale references are caught before any mutation occurs.

**Reference implementation**: `reference/fork/hashline.ts` (991 lines)

### Hash Computation

```
INPUT:  "  const x = 1;  "
STEP 1: Strip all whitespace → "constx=1;"
STEP 2: xxHash32("constx=1;") → some u32
STEP 3: hash % 256 → value 0-255
STEP 4: format as 2-char hex → "a3"
```

**Properties**:
- Whitespace-insensitive: `"  const x = 1;"` and `"const x = 1;"` produce the same hash
- Fast: xxHash32 is non-cryptographic, optimized for speed
- Compact: 2 hex chars per line (256 possible values)
- Collisions are fine: hash is always paired with a line number, so collisions between different lines don't matter. The hash catches "this line's content changed", not "this line is unique"

**Pre-computed lookup table** (avoids allocation per line):
```rust
const HASH_TABLE: [&str; 256] = {
    let mut table = [""; 256];
    // ... pre-compute "00" through "ff"
    table
};

fn compute_line_hash(line: &str) -> &'static str {
    let normalized: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    let hash = xxh32(normalized.as_bytes(), 0) as usize % 256;
    HASH_TABLE[hash]
}
```

### Display Format

When the model reads a file, every line is prefixed:

```
1:a3|function hello() {
2:f1|  return "world";
3:0e|}
```

Format: `{line_number}:{hash}|{content}`

- Line numbers are 1-indexed
- Hash is 2-char lowercase hex
- Pipe `|` separates the prefix from content
- No padding on line numbers

### Edit Operations

Four operations, submitted as a JSON array:

#### `set_line` — Replace a single line
```json
{
  "set_line": {
    "anchor": "2:f1",
    "new_text": "  return \"universe\";"
  }
}
```
- `anchor`: `"LINE:HASH"` reference
- `new_text`: replacement content (can contain `\n` for multi-line expansion)
- Empty `new_text` (`""`) deletes the line

#### `replace_lines` — Replace a range
```json
{
  "replace_lines": {
    "start_anchor": "2:f1",
    "end_anchor": "5:ab",
    "new_text": "  // replaced"
  }
}
```
- Both anchors validated before mutation
- Empty `new_text` deletes the entire range

#### `insert_after` — Insert after a line
```json
{
  "insert_after": {
    "anchor": "3:0e",
    "text": "\nfunction goodbye() {\n  return \"farewell\";\n}"
  }
}
```
- Inserts AFTER the anchored line
- `text` must be non-empty

#### `replace` — Fuzzy text match (fallback)
```json
{
  "replace": {
    "old_text": "return \"world\"",
    "new_text": "return \"universe\"",
    "all": false
  }
}
```
- Fuzzy whitespace matching
- `all: false` (default) requires unique match
- This is a fallback for when the model can't use line anchors

### Application Algorithm

**Critical invariant: Validate ALL hashes before ANY mutation.**

```
fn apply_hashline_edits(content: &str, edits: &[Edit]) -> Result<String> {
    let file_lines: Vec<&str> = content.lines().collect();
    let original = file_lines.clone();

    // PHASE 1: Parse all edits
    let parsed = edits.iter().map(parse_edit).collect();

    // PHASE 2: Pre-validate ALL references
    //   - Build hash→line lookup for unique hashes
    //   - For each reference:
    //     a) Check line in bounds
    //     b) Compute actual hash at that line
    //     c) If match → valid
    //     d) If no match but hash exists uniquely elsewhere → RELOCATE (auto-fix drift)
    //     e) If no match and can't relocate → collect mismatch
    //   - If ANY mismatches → throw HashlineMismatchError with correct refs
    validate_all_refs(&parsed, &file_lines)?;

    // PHASE 3: Deduplicate identical edits

    // PHASE 4: Sort bottom-up (highest line number first)
    //   - Primary: descending line number
    //   - Secondary: insert_after gets precedence 1 (applied after same-line edits)
    //   - Tertiary: original index (stable sort)
    parsed.sort_by(|a, b| b.sort_line.cmp(&a.sort_line));

    // PHASE 5: Apply edits
    //   For each edit (now bottom-up, so splices don't invalidate indices):
    //   - Strip hashline prefixes from new_text (model copies them)
    //   - Strip diff '+' markers from new_text
    //   - Strip boundary echo (model copies anchor line into replacement)
    //   - Restore indentation if stripped
    //   - Detect line merges (model merged adjacent lines)
    //   - Apply via splice
    for edit in &parsed {
        let new_lines = cleanup_heuristics(&edit.new_text, &original);
        file_lines.splice(edit.range(), new_lines);
    }

    Ok(file_lines.join("\n"))
}
```

### Hash Relocation (Auto-Fix Line Drift)

If the model reads a file, then another edit shifts lines, the hash can find where the target line actually moved to:

```
Model reads file:
  5:ab|  const x = 1;

Another edit inserts 2 lines above. Now the line is at position 7.

Model submits: anchor "5:ab"
  → Line 5 now has hash "cd" (different content shifted in)
  → But hash "ab" exists uniquely at line 7
  → RELOCATE: rewrite reference to line 7
  → Edit applied correctly at line 7
```

This only works when the hash is unique in the file. If multiple lines share the same hash, relocation is ambiguous and a mismatch error is thrown instead.

### Mismatch Error Format

When validation fails, the error message gives the model exactly what it needs to retry:

```
2 lines have changed since last read. Use the updated LINE:HASH references
shown below (>>> marks changed lines).

    3:7f|  const x = 1;
    4:a2|  const y = 2;
>>> 5:e1|  const z = 3;
    6:b4|  return x + y;
    ...
>>> 12:c9|  console.log(result);
    13:d1|}
```

- Shows ±2 context lines around each mismatch
- `>>>` marks the changed lines
- All displayed lines have correct `LINE:HASH` references
- Non-contiguous regions separated by `...`
- Model can immediately retry with the corrected references (no need to re-read the full file)

### Heuristic Cleanup

Models make predictable mistakes. The system auto-corrects these before applying edits:

#### 1. Strip Hashline Prefixes
Model copies `42:a7|const x` from read output into `new_text`. Strip the `42:a7|` prefix.
- Only fires when ≥50% of non-empty lines have the prefix (avoids false positives)
- Regex: `/^\d+:[0-9a-zA-Z]{1,16}\|/`

#### 2. Strip Diff Plus Markers
Model writes `+const x` (unified diff format). Strip leading `+`.
- Only fires when ≥50% of non-empty lines have the prefix
- Regex: `/^\+(?!\+)/` (single `+`, not `++`)

#### 3. Boundary Echo Stripping
Model copies the context around the edit into the replacement:
- For `insert_after`: if first line of inserted text matches the anchor line, strip it
- For `replace_lines`: if first/last line matches the line before/after the range, strip it
- Only strips when replacement is longer than original (avoids turning replacements into deletions)

#### 4. Indentation Restoration
Model strips leading whitespace from replacement lines. Detect and restore:
- Compare each new line with corresponding original line
- If new line has no indent but original did, prepend original's indent

#### 5. Wrapped Line Restoration
Model reflows a single line into multiple lines (or vice versa) without semantic change:
- Canonicalize by stripping all whitespace and joining
- If canonical form matches an original line uniquely, restore the original
- Minimum 6 chars for match (avoids false positives on tiny lines)

#### 6. Merge Detection
Model replaces one line but actually merged it with an adjacent line:
- **Case A**: Original line ends with continuation (`&&`, `||`, etc.) and next line absorbed
- **Case B**: Previous line was a continuation and got absorbed into this replacement
- Detection: check if replacement's whitespace-stripped content contains both lines' content
- Resolution: expand the splice to cover both lines

#### 7. Confusable Hyphen Normalization
Model uses Unicode dashes (em-dash, en-dash, etc.) instead of ASCII hyphen:
- Regex: `/[\u2010\u2011\u2012\u2013\u2014\u2212\uFE63\uFF0D]/g`
- Replace with `-`

### Integration with Read Tool

The read tool formats output differently based on mode:

```rust
fn format_file_output(content: &str, start_line: usize) -> String {
    content.lines()
        .enumerate()
        .map(|(i, line)| {
            let num = start_line + i;
            let hash = compute_line_hash(line);
            format!("{}:{}|{}", num, hash, line)
        })
        .collect::<Vec<_>>()
        .join("\n")
}
```

### Integration with Grep Tool

Grep results also use hashline format:

```
Match 1: src/main.rs:42
    40:7f|  let config = Config::new();
    41:a2|  let app = App::build(config);
>>  42:e1|  app.run().expect("failed to start");
    43:b4|  println!("Done");
```

- `>>` prefix marks the matching line
- Context lines use `  ` prefix (2 spaces)
- All lines include `LINE:HASH|content` for consistency
- Model can reference any displayed line in a subsequent edit

### Streaming Hashlines (Large Files)

For files too large to load entirely, stream hashline output in chunks:

```rust
async fn stream_hashlines(path: &Path, start_line: usize) -> impl Stream<Item = String> {
    let file = File::open(path).await?;
    let reader = BufReader::new(file);
    let mut line_num = start_line;
    let mut chunk_lines = Vec::new();
    let mut chunk_bytes = 0;

    const MAX_CHUNK_LINES: usize = 200;
    const MAX_CHUNK_BYTES: usize = 64 * 1024;

    while let Some(line) = reader.next_line().await? {
        let formatted = format!("{}:{}|{}", line_num, compute_line_hash(&line), line);
        let line_bytes = formatted.len() + 1; // +1 for newline

        if chunk_lines.len() >= MAX_CHUNK_LINES || chunk_bytes + line_bytes > MAX_CHUNK_BYTES {
            yield chunk_lines.join("\n");
            chunk_lines.clear();
            chunk_bytes = 0;
        }

        chunk_lines.push(formatted);
        chunk_bytes += line_bytes;
        line_num += 1;
    }

    if !chunk_lines.is_empty() {
        yield chunk_lines.join("\n");
    }
}
```

---

## Part 3: Tool Descriptions for the LLM

These are the system prompt descriptions the model sees. Keep them minimal — models already understand these tools from RL training.

### Read
```
Read a file. Returns content with LINE:HASH|content format per line.
Parameters: path (string), offset (int, optional), limit (int, optional)
```

### Edit
```
Edit a file using line-addressed operations. Reference lines by LINE:HASH anchors from read output.
Parameters: path (string), edits (array of operations)
Operations:
  - set_line: { anchor: "LINE:HASH", new_text: "..." }
  - replace_lines: { start_anchor: "LINE:HASH", end_anchor: "LINE:HASH", new_text: "..." }
  - insert_after: { anchor: "LINE:HASH", text: "..." }
  - replace: { old_text: "...", new_text: "...", all: bool }
```

### Write
```
Create or overwrite a file.
Parameters: path (string), content (string)
```

### Bash
```
Execute a shell command.
Parameters: command (string), timeout (int, optional, default 300s)
```

### Grep
```
Search file contents with regex. Returns matches with LINE:HASH|content format.
Parameters: pattern (string), path (string, optional), glob (string, optional), limit (int, optional)
```

### Find
```
Find files by glob pattern.
Parameters: pattern (string), limit (int, optional)
```
