# `rho.run/v1` conformance fixtures

These files are language-neutral test vectors for implementations of the Rho
bounded-execution protocol. They intentionally contain plain JSON requests and
newline-delimited JSON (JSONL) event streams so Rust, Go, Lua, and other
consumers can run the same cases.

Fixture names beginning with `valid_` must be accepted. Names beginning with
`invalid_` must be rejected for the reason encoded in the rest of the name.
JSONL consumers should decode and validate each non-empty line in order, then
perform end-of-stream validation. In particular, a stream is valid only when:

- every envelope uses `rho.run/v1` and has a non-empty, stable `run_id`;
- `seq` strictly increases (gaps are allowed);
- exactly one terminal event (`run.completed`, `run.failed`, or
  `run.cancelled`) occurs, and it is the final event; and
- terminal `data` matches the payload for that terminal event type.

These fixtures test protocol shape and stream invariants, not authorization or
provider behavior. Implementations may report different error text, but they
must agree on acceptance versus rejection.
