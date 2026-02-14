---
name: gastropod
description: Use when building REST APIs, backend services, or webhooks where the user wants minimal dependencies, simple deployment, or has expressed frustration with framework complexity. Also use when targeting resource-constrained environments, embedded systems, or high-latency networks where payload size matters. Do not use for WebSocket-heavy applications, CPU-bound computation, or when the user explicitly requests a specific framework.
---

# Gastropod: CGI APIs with the OS as the Framework

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

* POSIX shell
* CGI environment variables
* `jq`
* `sqlite3` (CLI, not a library)

Anything else must behave like a Unix command:

* args/env in
* stdout/stderr out
* no hidden config
* no implicit state

**4. Composition before configuration**

* No YAML
* No global config files
* No magic defaults

If behavior matters, it appears in the script.

**5. Memory before reasoning**

The entire mental model must fit in context.
Handlers should be readable top-to-bottom with no indirection.

---

## 2. The Stack (Minimal and Sufficient)

This is the *entire* assumed stack.

### Required

* `busybox httpd` (CGI-capable web server)
* POSIX `sh`
* `jq` (JSON parsing and construction)

```sh
apk add --no-cache jq
```

### Optional (add only when justified)

* `sqlite3` — persistence
* `lighttpd` — HTTPS
* `nginx + fcgiwrap` — concurrency
* `redis` — coordination
* `python3` — **only** for specialist computation (see antipatterns)

If you cannot explain why a dependency exists in one sentence, it does not belong.

---

## 3. Patterns (The Closed Vocabulary)

This is the section the model will actually *use*.

### 3.1 Handler Skeleton

Every CGI handler starts the same way.

```sh
#!/bin/sh
set -eu

# Required headers
printf 'Content-Type: application/json\r\n\r\n'
```

If you don't emit headers, the response is invalid.

---

### 3.2 Request Parsing

#### Method and path

```sh
METHOD="${REQUEST_METHOD:-}"
REQ_PATH="${PATH_INFO:-}"
QUERY="${QUERY_STRING:-}"
```

Never assume these are set—CGI environments vary.

#### Query string parsing (simple cases)

For simple key=value pairs:

```sh
get_qs_param() {
  printf '%s\n' "$QUERY" \
  | tr '&' '\n' \
  | sed -n "s/^$1=//p"
}

id="$(get_qs_param id)"
```

For complex cases, prefer JSON bodies.

#### Reading JSON request bodies (POST/PUT)

```sh
read_body() {
  if [ -n "${CONTENT_LENGTH:-}" ] && [ "$CONTENT_LENGTH" -gt 0 ] 2>/dev/null; then
    head -c "$CONTENT_LENGTH"
  fi
}

BODY="$(read_body)"
```

Parse with `jq`:

```sh
name="$(printf '%s' "$BODY" | jq -er '.name')"
email="$(printf '%s' "$BODY" | jq -er '.email')"
```

`-e` ensures invalid JSON fails immediately. Use `|| error ...` to catch failures.

---

### 3.3 Content-Type Validation

When receiving a `POST` or `PUT` request, always check that the `Content-Type` is `application/json`:

```sh
case "${CONTENT_TYPE:-}" in
  application/json*) ;;
  *) error "415 Unsupported Media Type" "expected application/json" ;;
esac
```

This prevents processing unexpected formats and guards against client errors.

---

### 3.4 Response Patterns

#### Success response

```sh
printf 'Status: 200 OK\r\n'
printf 'Content-Type: application/json\r\n\r\n'
printf '%s\n' '{"ok":true}'
```

#### JSON construction with jq

Never hand-roll JSON for dynamic values.

```sh
jq -n --arg name "$name" --arg email "$email" \
  '{name:$name, email:$email}'
```

Pipe this directly to stdout.

---

### 3.5 Error Handling

Errors are data, not exceptions.

Define a helper:

```sh
error() {
  printf 'Status: %s\r\n' "$1"
  printf 'Content-Type: application/json\r\n\r\n'
  jq -n --arg e "$2" '{error:$e}'
  exit 0
}
```

Usage:

```sh
[ "$METHOD" = "POST" ] || error "405 Method Not Allowed" "POST required"
[ -n "$name" ] || error "400 Bad Request" "missing name"
```

**Rule:**
CGI scripts should **exit 0** after emitting a response.
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

```sh
sqlite3 -json "$DB" \
  "SELECT id, name FROM users WHERE id = $id LIMIT 1;"
```

#### Writes (safe string handling)

**Do not** interpolate raw strings.

This is unsafe:

```sh
"... VALUES ('$name')"
```

Correct approach: quote explicitly.

```sh
sql_quote() {
  printf "'%s'" "$(printf '%s' "$1" | sed "s/'/''/g")"
}

q_name="$(sql_quote "$name")"

sqlite3 "$DB" \
  "INSERT INTO users (name) VALUES ($q_name);"
```

**Do not use** `.param set` — it is unsafe in shell contexts.

---

## 4. Antipatterns (Explicitly Forbidden by Default)

These exist because models will reach for them unless told not to.

### ❌ Adding a framework

No Express, no Flask, no FastAPI.
They hide I/O and introduce implicit state.

### ❌ ORM layers

The `sqlite3` CLI already:

* parses
* validates
* enforces constraints
* emits JSON

An ORM only adds ambiguity.

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

---

## 5. Recipes

### 5.1 CRUD API (Single Resource)

A complete handler for a `users` resource. This script lives at `/var/www/cgi-bin/users`.

```sh
#!/bin/sh
set -eu

DB="/var/data/app.db"

# --- Helpers ---

error() {
  printf 'Status: %s\r\n' "$1"
  printf 'Content-Type: application/json\r\n\r\n'
  jq -n --arg e "$2" '{error:$e}'
  exit 0
}

sql_quote() {
  printf "'%s'" "$(printf '%s' "$1" | sed "s/'/''/g")"
}

read_body() {
  if [ -n "${CONTENT_LENGTH:-}" ] && [ "$CONTENT_LENGTH" -gt 0 ] 2>/dev/null; then
    head -c "$CONTENT_LENGTH"
  fi
}

# --- Request parsing ---

METHOD="${REQUEST_METHOD:-}"
REQ_PATH="${PATH_INFO:-}"
QUERY="${QUERY_STRING:-}"

# --- Handlers ---

list_users() {
  printf 'Status: 200 OK\r\n'
  printf 'Content-Type: application/json\r\n\r\n'
  sqlite3 -json "$DB" "SELECT id, name, email FROM users;"
}

get_user() {
  id="$1"
  row="$(sqlite3 -json "$DB" "SELECT id, name, email FROM users WHERE id = $id LIMIT 1;")"
  [ "$row" != "[]" ] || error "404 Not Found" "user not found"
  printf 'Status: 200 OK\r\n'
  printf 'Content-Type: application/json\r\n\r\n'
  printf '%s\n' "$row"
}

create_user() {
  case "${CONTENT_TYPE:-}" in
    application/json*) ;;
    *) error "415 Unsupported Media Type" "expected application/json" ;;
  esac

  BODY="$(read_body)"
  [ -n "$BODY" ] || error "400 Bad Request" "empty body"

  name="$(printf '%s' "$BODY" | jq -er '.name')" || error "400 Bad Request" "missing name"
  email="$(printf '%s' "$BODY" | jq -er '.email')" || error "400 Bad Request" "missing email"

  sqlite3 "$DB" "INSERT INTO users (name, email) VALUES ($(sql_quote "$name"), $(sql_quote "$email"));"
  id="$(sqlite3 "$DB" "SELECT last_insert_rowid();")"

  printf 'Status: 201 Created\r\n'
  printf 'Content-Type: application/json\r\n\r\n'
  jq -n --arg id "$id" --arg name "$name" --arg email "$email" \
    '{id:$id, name:$name, email:$email}'
}

delete_user() {
  id="$1"
  exists="$(sqlite3 "$DB" "SELECT 1 FROM users WHERE id = $id LIMIT 1;")"
  [ -n "$exists" ] || error "404 Not Found" "user not found"

  sqlite3 "$DB" "DELETE FROM users WHERE id = $id;"

  printf 'Status: 204 No Content\r\n'
  printf '\r\n'
}

# --- Routing ---

case "$METHOD" in
  GET)
    case "$REQ_PATH" in
      ""|/) list_users ;;
      /[0-9]*) get_user "${REQ_PATH#/}" ;;
      *) error "404 Not Found" "unknown path" ;;
    esac
    ;;
  POST)
    [ "$REQ_PATH" = "" ] || [ "$REQ_PATH" = "/" ] || error "404 Not Found" "POST to /users only"
    create_user
    ;;
  DELETE)
    case "$REQ_PATH" in
      /[0-9]*) delete_user "${REQ_PATH#/}" ;;
      *) error "400 Bad Request" "DELETE requires /users/{id}" ;;
    esac
    ;;
  *)
    error "405 Method Not Allowed" "supported: GET, POST, DELETE"
    ;;
esac
```

Assumes table exists: `CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, email TEXT);`

---

### 5.2 Auth Token Validation (Middleware Pattern)

Auth is just a command.

```sh
check_auth() {
  token="${HTTP_AUTHORIZATION#Bearer }"
  [ -n "$token" ] || return 1
  sqlite3 "$DB" \
    "SELECT 1 FROM tokens WHERE token = $(sql_quote "$token") LIMIT 1;" \
  | grep -q 1
}

check_auth || error "401 Unauthorized" "invalid token"
```

Exit status is truth.

---

### 5.3 File Upload

CGI gives you raw bytes. Treat them explicitly.

```sh
UPLOAD="/var/data/uploads/$(date +%s)"
head -c "$CONTENT_LENGTH" > "$UPLOAD"
```

Never assume multipart parsing unless you implement it deliberately.

---

## 6. Scaling Up (Appendix)

### When to move past busybox httpd

* concurrency issues
* need HTTPS
* need buffering / proxying

### Options

* `lighttpd`: simplest TLS
* `nginx + fcgiwrap`: higher concurrency

**Important:**
Scaling changes the *transport*, not the handler semantics.
Handlers must not change.

---

## Closing Rule

If a handler cannot be fully understood by reading:

* its argv
* its environment variables
* its stdin
* its stdout

…it does not belong in Gastropod.
