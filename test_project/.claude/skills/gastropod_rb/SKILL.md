---
name: gastropod_rb
description: Use when building REST APIs, backend services, or webhooks where the user wants minimal dependencies, simple deployment, or has expressed frustration with framework complexity. Also use when targeting resource-constrained environments, embedded systems, or high-latency networks where payload size matters. Do not use for WebSocket-heavy applications, CPU-bound computation, or when the user explicitly requests a specific framework.
---

# Gastropod: CGI/FastCGI APIs with the OS as the Framework

## 1. Philosophy

Gastropod is a discipline, not a framework.

The premise is simple:

> **The operating system already provides everything needed to build HTTP APIs.
> Do not fight it. Do not replace it. Compose with it.**

### Core invariants

These invariants are non-negotiable. Every handler the model generates must respect them.

**1. The OS owns I/O**

* Programs do not decide where input comes from
* Programs do not decide where output goes
* Programs do not decide what is persisted

Handlers:

* read from environment variables and stdin
* write to stdout and stderr
* exit with meaningful status codes

Persistence happens explicitly via tools (`sqlite3`, redirection).

**2. JSON at the boundaries**

* All request bodies are JSON
* All responses are JSON
* All errors are JSON

This keeps the system legible to humans *and* models.

**3. Closed vocabulary**

Only these primitives are assumed:

* CGI/FastCGI environment variables
* `jq` (JSON parsing and construction)
* `sqlite3` (CLI, not a library)

Handlers can be written in POSIX shell for simplicity or compiled to binaries (e.g., in Zig, Nim, or Rust) for performance. Anything else must behave like a Unix command:

* args/env in
* stdout/stderr out
* no hidden config
* no implicit state

**4. Composition before configuration**

* No YAML
* No global config files
* No magic defaults

If behavior matters, it appears in the handler code.

**5. Memory before reasoning**

The entire mental model must fit in context.
Handlers should be readable top-to-bottom with no indirection.

---

## 2. The Stack (Minimal and Sufficient)

This is the *entire* assumed stack.

### Required

* POSIX shell or compiled binaries (Zig/Nim/Rust recommended for perf) for handlers
* `jq` (JSON parsing and construction)
* `nginx + fcgiwrap` (default for concurrency and scaling; handles CGI semantics with better performance)

```sh
apk add --no-cache jq nginx fcgiwrap
```

### Optional (add only when justified)

* `sqlite3` — persistence
* `lighttpd` — simpler TLS alternative if nginx is overkill
* `redis` — coordination
* `busybox httpd` — absolute minimalism for prototypes (avoid for production due to concurrency limits)
* `python3` — **only** for specialist computation (see antipatterns)

If you cannot explain why a dependency exists in one sentence, it does not belong. For performance-critical handlers, compile to binaries using Zig (preferred for low-level control and tiny size), Nim (for expressive syntax), or Rust (for safety)—these produce self-contained executables that drop-in as CGI/FastCGI handlers without runtime deps.

---

## 3. Patterns (The Closed Vocabulary)

This is the section the model will actually *use*.

### 3.1 Handler Skeleton

Every handler starts by emitting headers. Examples in shell and Zig for comparison.

**Shell Example:**
```sh
#!/bin/sh
set -eu

# Required headers
printf 'Content-Type: application/json\r\n\r\n'
```

**Zig Example (Compiled Binary):**
Compile with `zig build-exe handler.zig -O ReleaseSmall` for a tiny binary.
```zig
const std = @import("std");

pub fn main() !void {
    const stdout = std.io.getStdOut().writer();
    try stdout.print("Content-Type: application/json\r\n\r\n", .{});
    // Rest of handler...
}
```

If you don't emit headers, the response is invalid. Binaries provide better perf for repeated ops without fork overhead in logic.

---

### 3.2 Request Parsing

#### Method and path

**Shell:**
```sh
METHOD="${REQUEST_METHOD:-}"
REQ_PATH="${PATH_INFO:-}"
QUERY="${QUERY_STRING:-}"
```

**Zig:**
```zig
const method = std.os.getenv("REQUEST_METHOD") orelse "";
const req_path = std.os.getenv("PATH_INFO") orelse "";
const query = std.os.getenv("QUERY_STRING") orelse "";
```

Never assume these are set—CGI/FastCGI environments vary.

#### Query string parsing (simple cases)

For simple key=value pairs.

**Shell:**
```sh
get_qs_param() {
  printf '%s\n' "$QUERY" \
  | tr '&' '\n' \
  | sed -n "s/^$1=//p"
}

id="$(get_qs_param id)"
```

**Zig (Native for Perf):**
```zig
fn getQsParam(query: []const u8, key: []const u8) ?[]const u8 {
    var it = std.mem.split(u8, query, "&");
    while (it.next()) |pair| {
        if (std.mem.startsWith(u8, pair, key ++ "=")) {
            return pair[key.len + 1..];
        }
    }
    return null;
}

const id = getQsParam(query, "id") orelse "";
```

For complex cases, prefer JSON bodies. Use binaries like Zig when parsing perf matters.

#### Reading JSON request bodies (POST/PUT)

**Shell:**
```sh
read_body() {
  if [ -n "${CONTENT_LENGTH:-}" ] && [ "$CONTENT_LENGTH" -gt 0 ] 2>/dev/null; then
    head -c "$CONTENT_LENGTH"
  fi
}

BODY="$(read_body)"
```

**Zig:**
```zig
const cl_str = std.os.getenv("CONTENT_LENGTH") orelse "0";
const cl = std.fmt.parseInt(usize, cl_str, 10) catch 0;
var body: [1024 * 1024]u8 = undefined; // Adjust buffer as needed
const body_len = std.io.getStdIn().read(body[0..cl]) catch 0;
const body_slice = body[0..body_len];
```

Parse with `jq` (subprocess in both):

**Shell:**
```sh
name="$(printf '%s' "$BODY" | jq -er '.name')"
```

**Zig:**
```zig
var child = std.ChildProcess.init(&[_][]const u8{"jq", "-er", ".name"}, std.heap.page_allocator);
child.stdin_behavior = .Pipe;
try child.spawn();
_ = try child.stdin.?.write(body_slice);
try child.stdin.?.close();
const output = try child.stdout.?.readToEndAlloc(std.heap.page_allocator, 1024 * 1024);
const name = std.mem.trim(u8, output, "\n");
```

`-e` ensures invalid JSON fails immediately. Use `|| error ...` to catch failures. Binaries reduce overall latency in high-traffic scenarios.

---

### 3.3 Content-Type Validation

When receiving a `POST` or `PUT` request, always check that the `Content-Type` is `application/json`.

**Shell:**
```sh
case "${CONTENT_TYPE:-}" in
  application/json*) ;;
  *) error "415 Unsupported Media Type" "expected application/json" ;;
esac
```

**Zig:**
```zig
const content_type = std.os.getenv("CONTENT_TYPE") orelse "";
if (!std.mem.containsAtLeast(u8, content_type, 1, "application/json")) {
    try errorFn(415, "expected application/json");
}
```

This prevents processing unexpected formats and guards against client errors.

---

### 3.4 Response Patterns

#### Success response

**Shell:**
```sh
printf 'Status: 200 OK\r\n'
printf 'Content-Type: application/json\r\n\r\n'
printf '%s\n' '{"ok":true}'
```

**Zig:**
```zig
const stdout = std.io.getStdOut().writer();
try stdout.print("Status: 200 OK\r\nContent-Type: application/json\r\n\r\n", .{});
try stdout.print("{{\"ok\":true}}\n", .{});
```

#### JSON construction with jq

Never hand-roll JSON for dynamic values.

**Shell:**
```sh
jq -n --arg name "$name" --arg email "$email" \
  '{name:$name, email:$email}'
```

**Zig (Subprocess jq):**
```zig
var child = std.ChildProcess.init(&[_][]const u8{"jq", "-n", "--arg", "name", name, "--arg", "email", email, "{name:$name, email:$email}"}, std.heap.page_allocator);
const json = try child.stdout.?.readToEndAlloc(std.heap.page_allocator, 1024 * 1024);
try stdout.print("{s}\n", .{json});
```

Pipe this directly to stdout. For extreme perf, implement simple JSON in Zig/Nim/Rust, but justify: "to avoid jq subprocess in tight loops."

---

### 3.5 Error Handling

Errors are data, not exceptions.

**Shell Helper:**
```sh
error() {
  printf 'Status: %s\r\n' "$1"
  printf 'Content-Type: application/json\r\n\r\n'
  jq -n --arg e "$2" '{error:$e}'
  exit 0
}
```

**Zig Helper:**
```zig
fn errorFn(status: u16, msg: []const u8) !void {
    const stdout = std.io.getStdOut().writer();
    try stdout.print("Status: {d}\r\nContent-Type: application/json\r\n\r\n", .{status});
    // Subprocess jq for JSON
    var child = std.ChildProcess.init(&[_][]const u8{"jq", "-n", "--arg", "e", msg, "{error:$e}"}, std.heap.page_allocator);
    const err_json = try child.stdout.?.readToEndAlloc(std.heap.page_allocator, 1024);
    try stdout.print("{s}\n", .{err_json});
    std.process.exit(0);
}
```

Usage (similar in both):

**Shell:**
```sh
[ "$METHOD" = "POST" ] || error "405 Method Not Allowed" "POST required"
```

**Zig:**
```zig
if (!std.mem.eql(u8, method, "POST")) try errorFn(405, "POST required");
```

**Rule:**
Handlers should **exit 0** after emitting a response.
The HTTP status is carried in headers, not exit codes.
Exit codes are for the server's benefit (e.g., crash detection), not the client-facing error channel.

---

### 3.6 Persistence: sqlite3 One-Liner Pattern

SQLite is a file. Treat it like one.

```sh
DB="/var/data/app.db"
```

Note: `-json` output requires SQLite 3.33.0+ (2020-08). Alpine's package includes this.

#### Reads

**Shell/Zig (Subprocess):**
```sh
sqlite3 -json "$DB" \
  "SELECT id, name FROM users WHERE id = $id LIMIT 1;"
```

In Zig, use `std.ChildProcess` to exec `sqlite3` similarly.

#### Writes (safe string handling)

**Do not** interpolate raw strings.

**Shell:**
```sh
sql_quote() {
  printf "'%s'" "$(printf '%s' "$1" | sed "s/'/''/g")"
}

q_name="$(sql_quote "$name")"

sqlite3 "$DB" \
  "INSERT INTO users (name) VALUES ($q_name);"
```

**Zig (Native Quote + Subprocess):**
```zig
fn sqlQuote(alloc: std.mem.Allocator, input: []const u8) ![]u8 {
    var quoted = try std.ArrayList(u8).initCapacity(alloc, input.len + 2);
    try quoted.append('\'');
    for (input) |c| {
        if (c == '\'') try quoted.appendSlice("''") else try quoted.append(c);
    }
    try quoted.append('\'');
    return quoted.toOwnedSlice();
}

const q_name = try sqlQuote(alloc, name);
// Then subprocess sqlite3 with query
```

**Do not use** `.param set` — it is unsafe in shell/binary contexts. For perf in binaries, consider linking sqlite3 lib if justified: "for direct access without subprocess in high-write scenarios."

---

## 4. Antipatterns (Explicitly Forbidden by Default)

These exist because models will reach for them unless told not to.

### ❌ Adding a framework

No Express, no Flask, no FastAPI, no Actix.
They hide I/O and introduce implicit state.

### ❌ ORM layers

The `sqlite3` CLI already:

* parses
* validates
* enforces constraints
* emits JSON

An ORM only adds ambiguity. In binaries, avoid ORM libs; stick to subprocess or direct C API if perf-critical.

### ❌ Python by default

Python is allowed **only** when:

* doing specialist math / ML / parsing
* invoked as a command
* takes explicit args/stdin
* emits stdout
* has no hidden config

Always discuss with the user first.

### ❌ Implicit persistence

No auto-writing files.
No background state.
No caches unless explicitly invoked.

### ❌ Language runtimes in production (except for binaries)

Avoid interpreted langs like Lua/Forth unless compiling to binary equivalents. Prefer Zig/Nim/Rust for perf without runtime overhead.

---

## 5. Recipes

### 5.1 CRUD API (Single Resource)

A complete handler for a `users` resource. This lives at `/var/www/cgi-bin/users` (or as a binary). Shell for simplicity; Zig for perf example.

**Shell Version:**
[Same as original, omitted for brevity]

**Zig Binary Version (Compile and Drop-In):**
```zig
const std = @import("std");

fn errorFn(stdout: anytype, status: u16, msg: []const u8) !void {
    try stdout.print("Status: {d}\r\nContent-Type: application/json\r\n\r\n", .{status});
    try stdout.print("{{\"error\":\"{s}\"}}\n", .{msg}); // Simplified; use jq if complex
    std.process.exit(0);
}

fn sqlQuote(alloc: std.mem.Allocator, input: []const u8) ![]u8 {
    // As above
}

pub fn main() !void {
    var gpa = std.heap.GeneralPurposeAllocator(.{}){};
    defer _ = gpa.deinit();
    const alloc = gpa.allocator();

    const stdout = std.io.getStdOut().writer();
    const method = std.os.getenv("REQUEST_METHOD") orelse "";
    const req_path = std.os.getenv("PATH_INFO") orelse "";
    const content_type = std.os.getenv("CONTENT_TYPE") orelse "";
    const cl_str = std.os.getenv("CONTENT_LENGTH") orelse "0";
    const cl = std.fmt.parseInt(usize, cl_str, 10) catch 0;

    var body_buf: [1024 * 1024]u8 = undefined;
    const body_len = try std.io.getStdIn().read(body_buf[0..cl]);
    const body = body_buf[0..body_len];

    const db = "/var/data/app.db";

    if (std.mem.eql(u8, method, "GET")) {
        if (std.mem.eql(u8, req_path, "") or std.mem.eql(u8, req_path, "/")) {
            // list_users: subprocess sqlite3 -json ...
            try stdout.print("Status: 200 OK\r\nContent-Type: application/json\r\n\r\n", .{});
            var child = std.ChildProcess.init(&[_][]const u8{"sqlite3", "-json", db, "SELECT id, name, email FROM users;"}, alloc);
            try child.spawn();
            const rows = try child.stdout.?.readToEndAlloc(alloc, 1024 * 1024);
            try stdout.print("{s}\n", .{rows});
        } else if (req_path.len > 1 and std.ascii.isDigit(req_path[1])) {
            // get_user: similar subprocess
            const id = req_path[1..];
            var child = std.ChildProcess.init(&[_][]const u8{"sqlite3", "-json", db, try std.fmt.allocPrint(alloc, "SELECT id, name, email FROM users WHERE id = {s} LIMIT 1;", .{id})}, alloc);
            try child.spawn();
            const row = try child.stdout.?.readToEndAlloc(alloc, 1024 * 1024);
            if (std.mem.eql(u8, row, "[]\n")) try errorFn(stdout, 404, "user not found");
            try stdout.print("Status: 200 OK\r\nContent-Type: application/json\r\n\r\n{s}\n", .{row});
        } else {
            try errorFn(stdout, 404, "unknown path");
        }
    } else if (std.mem.eql(u8, method, "POST")) {
        if (!std.mem.containsAtLeast(u8, content_type, 1, "application/json")) try errorFn(stdout, 415, "expected application/json");
        if (body.len == 0) try errorFn(stdout, 400, "empty body");

        // Parse name/email with jq subprocesses
        var name_child = std.ChildProcess.init(&[_][]const u8{"jq", "-er", ".name"}, alloc);
        name_child.stdin_behavior = .Pipe;
        try name_child.spawn();
        _ = try name_child.stdin.?.write(body);
        try name_child.stdin.?.close();
        const name_out = try name_child.stdout.?.readToEndAlloc(alloc, 1024);
        const name = std.mem.trim(u8, name_out, "\n");
        if (name.len == 0) try errorFn(stdout, 400, "missing name");

        // Similar for email...

        // Insert
        const q_name = try sqlQuote(alloc, name);
        // Similar for q_email
        var insert_child = std.ChildProcess.init(&[_][]const u8{"sqlite3", db, try std.fmt.allocPrint(alloc, "INSERT INTO users (name, email) VALUES ({s}, {s});", .{q_name, q_email})}, alloc);
        try insert_child.spawn();
        try insert_child.wait();

        // Get id
        var id_child = std.ChildProcess.init(&[_][]const u8{"sqlite3", db, "SELECT last_insert_rowid();"}, alloc);
        try id_child.spawn();
        const id_out = try id_child.stdout.?.readToEndAlloc(alloc, 1024);
        const id = std.mem.trim(u8, id_out, "\n");

        try stdout.print("Status: 201 Created\r\nContent-Type: application/json\r\n\r\n", .{});
        try stdout.print("{{\"id\":\"{s}\", \"name\":\"{s}\", \"email\":\"{s}\"}}\n", .{id, name, email}); // Or use jq
    } else if (std.mem.eql(u8, method, "DELETE")) {
        // Similar logic...
    } else {
        try errorFn(stdout, 405, "supported: GET, POST, DELETE");
    }
}
```

Assumes table exists: `CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, email TEXT);`

Use Zig/Nim/Rust binaries when shell perf is insufficient (e.g., high RPS); they reduce subprocess reliance for parsing.

---

### 5.2 Auth Token Validation (Middleware Pattern)

Auth is just a command or function.

**Shell:**
[Same as original]

**Zig:**
```zig
fn checkAuth(db: []const u8, alloc: std.mem.Allocator) !bool {
    const auth = std.os.getenv("HTTP_AUTHORIZATION") orelse return false;
    const token = if (std.mem.startsWith(u8, auth, "Bearer ")) auth[7..] else return false;
    const q_token = try sqlQuote(alloc, token);
    var child = std.ChildProcess.init(&[_][]const u8{"sqlite3", db, try std.fmt.allocPrint(alloc, "SELECT 1 FROM tokens WHERE token = {s} LIMIT 1;", .{q_token})}, alloc);
    try child.spawn();
    const exists = try child.stdout.?.readToEndAlloc(alloc, 1024);
    return std.mem.containsAtLeast(u8, exists, 1, "1");
}

if (!try checkAuth(db, alloc)) try errorFn(stdout, 401, "invalid token");
```

Exit status is truth. Binaries make this faster for frequent checks.

---

### 5.3 File Upload

CGI/FastCGI gives you raw bytes. Treat them explicitly.

**Shell:**
```sh
UPLOAD="/var/data/uploads/$(date +%s)"
head -c "$CONTENT_LENGTH" > "$UPLOAD"
```

**Zig:**
```zig
const timestamp = std.time.timestamp();
const upload_path = try std.fmt.allocPrint(alloc, "/var/data/uploads/{d}", .{timestamp});
var file = try std.fs.cwd().createFile(upload_path, .{});
defer file.close();
var buf: [4096]u8 = undefined;
while (true) {
    const len = try std.io.getStdIn().read(buf[0..]);
    if (len == 0) break;
    try file.writeAll(buf[0..len]);
}
```

Never assume multipart parsing unless you implement it deliberately. Binaries handle large uploads more efficiently.

---

## 6. Scaling Up (Appendix)

### Default Scaling: nginx + fcgiwrap

Use `nginx + fcgiwrap` as the default transport for concurrency. It wraps handlers (shell or binaries) into FastCGI, reusing processes to avoid CGI's per-request fork overhead—ideal for production perf without changing handler code.

Nginx config snippet:
```
server {
    listen 80;
    location /cgi-bin/ {
        fastcgi_pass unix:/var/run/fcgiwrap.socket;
        include fastcgi_params;
    }
}
```

Spawn fcgiwrap: `fcgiwrap -s unix:/var/run/fcgiwrap.socket`

### When to move past this

* Extreme TLS needs: Add `lighttpd` as alternative.
* Prototypes only: `busybox httpd` for plain CGI.

**Important:**
Scaling changes the *transport*, not the handler semantics.
Handlers must not change—binaries in Zig/Nim/Rust shine here for inner-loop perf.

---

## Closing Rule

If a handler cannot be fully understood by reading:

* its argv
* its environment variables
* its stdin
* its stdout

…it does not belong in Gastropod.
