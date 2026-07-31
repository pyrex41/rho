# RFC 0001: A bounded execution stack for Rho, td, SCUD, and Shen-Backpressure

- Status: Draft
- Owners: Rho, td, SCUD, and Shen-Backpressure maintainers
- Last updated: 2026-07-31
- SCUD source inspected: `pyrex41/scud` at `096584565389afb76abda57402b11b18dcab7785`

## Decision

Keep the execution protocol and its reference implementation in the Rho repository until two independent hosts consume it. Narrow each project to one responsibility:

| Project | Owns | Does not own |
| --- | --- | --- |
| Rho | One bounded provider/tool run; Claude, OpenAI, and xAI adapters; capability enforcement; normalized events | Task DAGs, leases, ticket lifecycle, acceptance policy |
| td | Durable tickets, identity, claims, leases, fencing, worktrees, retries, attention, audit | Provider wire formats, tool implementations, task dependency planning |
| SCUD | Local task DAG, readiness, waves, graph templates, task artifacts | Provider clients, recursive agent runtime, generic process supervision, acceptance commands |
| Shen-Backpressure | Gate definitions, discharge reports, and the accept/retry/block decision | Provider calls, task scheduling, worktree lifecycle |

The dependency rule is strict: Rho imports none of the other projects. SCUD and td speak a versioned Rho subprocess protocol. Shen-Backpressure exchanges structured gate artifacts with the scheduler/control plane, not with provider adapters.

Do not create a fifth umbrella project now. Extract the protocol into a neutral repository only after td and SCUD both run it in production and at least one non-Rho executor has an adapter.

## Motivation from the current code

Rho already contains most of the executor:

- `crates/rho-core/src/agent_loop.rs` runs provider turns and tools, but `AgentLoopConfig` mixes public run inputs, provider secrets, callbacks, hooks, and runtime wiring.
- `crates/rho-core/src/types.rs` has `Message`, `Usage`, and `AgentEvent`, but `AgentEvent` is not serializable and is tailored to in-process UI consumption.
- `crates/rho-core/src/provider_types.rs` exposes provider streaming as a closure type instead of a capability-bearing provider interface.
- `crates/rho-provider/src/lib.rs` selects model adapters, including OpenAI-compatible and xAI Responses support.
- `crates/rho-tools/src/bash.rs` and sibling tools are the effect boundary, but today a selected Bash tool receives broad ambient workspace authority.
- `crates/rho-server/src/lib.rs` owns an in-memory session HTTP/SSE API. This is a frontend, not the durable control plane.
- `src/main.rs` already emits `stream-json`, so a migration can preserve existing callers.

td already contains the durable supervisor:

- `agents/orchestrator/orchestrator.py` claims tickets, carries fencing tokens, creates/adopts children, renews leases, applies retry budgets, detects stalls, and kills process groups.
- `agents/orchestrator/workflow.py` deliberately models an executor as an argv list and supports stdin, argv, and file prompt delivery.
- `agents/orchestrator/WORKFLOW.example.md` documents the current contract: the child inherits ticket variables and optionally `TD_TOKEN`, then drives journal/ask/submit itself.

SCUD already contains both the intended scheduler and the duplication to remove:

- `pkg/model` and `pkg/wave` are the core DAG/task surfaces to retain.
- `pkg/swarm/executor.go` plans waves but also launches Rho, validates, attributes failures, and runs sequential repair loops.
- `internal/rho/rho.go` shells into `rho-cli --output-format stream-json`, but its event struct recognizes only a small, unstable subset.
- `pkg/llm/provider.go`, `pkg/llm/client.go`, and xAI-specific files duplicate provider/auth logic that belongs in Rho.
- `pkg/heavy` encodes an ensemble as bespoke orchestration instead of an inspectable DAG template.
- `pkg/swarm/backpressure.go` executes shell strings and invents an ad hoc validation result instead of consuming a discharge report.

## Target architecture

```text
ticket/signal
     |
     v
td: authorize, claim, fence, isolate, supervise, journal
     |
     | RunRequest + ExecutionGrant
     v
rho: provider stream <-> bounded tool loop
     |
     | RunEvent JSONL
     v
SCUD/td: update task or ticket, collect artifacts
     |
     | GateRequest / DischargeReport
     v
Shen-Backpressure: pass, retry, ask, or block
```

SCUD may run locally without td. In that mode it is responsible for scheduling and process lifetime, but it still must not implement providers or tools. In managed mode, td owns process lifetime and SCUD contributes a DAG or next-ready work item.

## Rho workspace changes

### New `rho-protocol` crate

Add `crates/rho-protocol/` and register it in the root `Cargo.toml`. It must have minimal dependencies (`serde`, `serde_json`, and optionally `uuid`) and no Tokio, HTTP, filesystem, provider, or tool dependency.

Move or duplicate only stable transport types from `crates/rho-core/src/types.rs`:

- `Message`, `Content`, and `Usage` after normalizing their serde names;
- `RunRequest`, `RunLimits`, `ExecutionGrant`, `ToolGrant`, `RunEvent`, `RunOutcome`, `RunFailure`, and `Artifact`;
- protocol version parsing and compatibility tests.

Do not expose `AgentLoopConfig`, `StreamFn`, hooks, `CancellationToken`, or Rust trait objects in this crate.

### Rename `rho-core` to `rho-engine` in two steps

First create a `rho-engine` crate that depends on `rho-protocol`, `rho-provider`, and a tool registry. Keep `rho-core` as a deprecated re-export facade for one minor release so the GUI, server, and external Rust users keep compiling. Then migrate and remove the facade in the next breaking release.

The engine API should be approximately:

```rust
pub trait CredentialSource: Send + Sync {
    fn credential(&self, provider: &str) -> CredentialFuture;
}

pub trait PolicyEnforcer: Send + Sync {
    fn authorize(&self, request: &ToolInvocation, grant: &ExecutionGrant)
        -> AuthorizationFuture;
}

pub async fn run(
    request: RunRequest,
    credentials: Arc<dyn CredentialSource>,
    tools: ToolRegistry,
    cancel: CancellationToken,
) -> Result<impl Stream<Item = RunEvent>, EngineError>;
```

`AgentLoopConfig` becomes internal runtime configuration. Existing hooks in `crates/rho-core/src/hooks.rs` remain engine extensions, but pre-tool hooks cannot widen the execution grant.

### Provider boundary

Replace the closure alias in `crates/rho-core/src/provider_types.rs` with a provider trait that reports capabilities before execution. Native first-class adapters are required for:

- Anthropic Messages (Claude);
- OpenAI Responses;
- xAI Responses (Grok).

OpenAI-compatible chat completion remains an optional compatibility adapter, not the common semantic model. Provider-specific request features live under a namespaced `extensions` object and must not be silently dropped. Capability preflight fails before `run.started` if the selected model lacks a requested feature.

### Capability enforcement

`ExecutionGrant` is a restriction, never a credential or proof of identity by itself. It includes:

- allowed provider and model patterns;
- allowed tool names;
- canonical read and write roots;
- command policy reference or explicit command rules;
- network destinations or `provider_only`;
- deadline, turn, token, and cost ceilings;
- opaque authorization witness and expiry;
- td ticket, claim-attempt/fence, realm, and principal metadata when present.

Enforce grants in constructors and immediately before every effect. Update every tool in `crates/rho-tools/src/` to accept a restricted `ToolContext`; never let a model-supplied path choose the root. `bash.rs` must kill its process group on cancellation and apply the same deadline as the run. Read/write/find/grep/edit must canonicalize paths and reject escapes. Web tools must honor destination policy.

### CLI and server

Add a non-interactive command without changing the existing positional CLI initially:

```sh
rho-cli run --request-file request.json --events jsonl
# or
rho-cli run --request - --events jsonl
```

Stdout is protocol-only. Human diagnostics go to stderr. Each JSONL line is flushed immediately. `SIGINT` and `SIGTERM` cancel the run, terminate child process groups, emit `run.cancelled` when possible, and exit with a documented status.

Keep `--output-format stream-json` for two minor releases as a compatibility renderer backed by the new event stream. Deprecate `src/loop_runner.rs`; move `src/autoresearch.rs` to an optional application crate. Treat `crates/rho-server/src/lib.rs` as a thin frontend: add `POST /v1/runs` and event streaming over SSE, but do not add durable queue or lease state there.

## JSONL wire contract, version 1

### Framing and evolution

- UTF-8, one complete JSON object per line, maximum line size 1 MiB.
- The request is one JSON document supplied by file/stdin; events are JSONL on stdout.
- Every request has `protocol: "rho.run/v1"`.
- Every event has `protocol`, `run_id`, monotonically increasing `seq`, RFC 3339 `time`, `type`, and `data`.
- Producers may add fields. Consumers must ignore unknown fields and unknown event types while still advancing `seq`.
- Required-field removal, meaning changes, or type changes require `v2`.
- Event delivery is at-least-once across a service reconnect. Consumers deduplicate by `(run_id, seq)`.
- Secrets, raw credentials, authorization bearer tokens, and hidden reasoning are forbidden in events.

### Request

```json
{
  "protocol": "rho.run/v1",
  "run_id": "01J...",
  "model": {"provider": "anthropic", "id": "claude-sonnet-4-5"},
  "input": [{"role": "user", "content": [{"type": "text", "text": "Implement T-42"}]}],
  "system": "You are working in an isolated checkout.",
  "limits": {
    "max_turns": 24,
    "max_input_tokens": 400000,
    "max_output_tokens": 64000,
    "max_cost_micros": 500000,
    "deadline": "2026-08-01T01:00:00Z"
  },
  "grant": {
    "grant_id": "grant-...",
    "expires_at": "2026-08-01T01:00:00Z",
    "providers": ["anthropic"],
    "models": ["claude-*"],
    "tools": ["read", "grep", "find", "edit", "write", "bash"],
    "read_roots": ["/srv/worktrees/T-42-a7"],
    "write_roots": ["/srv/worktrees/T-42-a7"],
    "network": {"mode": "provider_only"},
    "witness": "opaque-or-signed-policy-result"
  },
  "context": {
    "ticket_id": "T-42",
    "realm": "acme",
    "claim_attempt": 7,
    "workspace": "/srv/worktrees/T-42-a7"
  }
}
```

Credential references may be included (for example `credential_ref: "env:ANTHROPIC_API_KEY"`) but credential values may not. The host resolves them. A managed deployment should prefer a short-lived brokered credential scoped to the provider and run.

### Events

The required stable event types are:

| Type | Purpose |
| --- | --- |
| `run.started` | Resolved provider/model and accepted limits |
| `assistant.text.delta` | User-visible streamed text |
| `assistant.reasoning.summary.delta` | Provider-authorized summary only, never hidden chain of thought |
| `tool.requested` | Model-requested tool name and redacted arguments |
| `tool.authorized` / `tool.denied` | Grant decision and policy reason code |
| `tool.started` / `tool.output.delta` / `tool.completed` | Effect progress and result metadata |
| `usage.updated` | Cumulative token, cache, and cost counters |
| `context.compacted` | Compaction counts and retained artifact reference |
| `approval.requested` | A bounded run cannot proceed without external authority |
| `artifact.created` | Diff, log, report, patch, or message transcript reference |
| `run.completed` | Exactly one successful terminal event |
| `run.failed` | Exactly one failed terminal event with typed retryability |
| `run.cancelled` | Exactly one cancellation terminal event |

Example:

```json
{"protocol":"rho.run/v1","run_id":"01J...","seq":12,"time":"2026-07-31T20:03:04Z","type":"tool.completed","data":{"call_id":"call_7","tool":"bash","ok":true,"exit_code":0,"output_bytes":847}}
{"protocol":"rho.run/v1","run_id":"01J...","seq":19,"time":"2026-07-31T20:04:10Z","type":"run.completed","data":{"status":"succeeded","stop_reason":"complete","usage":{"input_tokens":21000,"output_tokens":3200,"cost_micros":18400},"artifacts":[{"kind":"git_diff","uri":"file:artifacts/diff.patch","sha256":"..."}]}}
```

`run.failed.data` must contain `code`, `message`, and `retryable`; optional `retry_after_ms` distinguishes provider throttling from an immediate continuation. Standard codes initially include `invalid_request`, `grant_denied`, `grant_expired`, `credential_unavailable`, `provider_rate_limited`, `provider_unavailable`, `context_exhausted`, `limit_exceeded`, `tool_failed`, `cancelled`, and `internal`.

Process exit status is a fallback transport signal, not the business outcome: 0 after a terminal event, 2 invalid request/protocol, 3 authorization/credential preflight, 4 provider failure, 5 tool/engine failure, and 130 cancellation. A missing terminal event is `transport_lost` and retryable according to the host's policy.

## td integration

No hub schema change is required for the first stage.

### Stage td-1: configured executor compatibility

Add a Rho workflow example later in td (not in this RFC's Rho change):

```toml
[agent]
command = ["rho-cli", "run", "--request-file", "{{request_file}}", "--events", "jsonl"]
prompt_delivery = "file"
pass_token = false
```

However, `prompt_file` currently contains Markdown, not `RunRequest`. Therefore first add a Rho wrapper command, `rho-cli td-executor`, which reads the existing rendered prompt, constructs a local grant rooted at `cwd`, and preserves current behavior. This proves Rho under td without changing `workflow.py`.

### Stage td-2: protocol-aware adapter

Then extend `agents/orchestrator/workflow.py` with an explicit `agent.protocol = "opaque" | "rho-jsonl-v1"` (default `opaque`). For Rho mode, `orchestrator.py` writes a request file separate from the human prompt and parses stdout JSONL.

Map events as follows:

- any valid event updates stall activity; `tool.*` and `usage.updated` are especially useful heartbeats;
- `usage.updated` and selected tool summaries become journal entries with idempotency key `rho:<run_id>:<seq>`;
- `approval.requested` invokes td `ask`, parks the ticket, and terminates the current run cleanly;
- `run.completed` invokes `submit` with artifact/diff summary;
- non-retryable `run.failed` invokes `block`;
- retryable failure supplies the orchestrator's existing retry/backoff machinery;
- a fence mismatch, expired grant, owner cancellation, or lost lease cancels Rho immediately.

Keep td as the sole owner of retries and hard/stall timeouts. Rho enforces per-run ceilings defensively but must not start a new attempt itself. Keep td as the sole owner of worktree creation/removal. Rho receives exactly that canonical root in its grant.

During migration, leave `pass_token=true` available for opaque executors. In `rho-jsonl-v1`, set it false: Rho emits intent and outcomes; the orchestrator performs hub writes. This removes the provider/tool worker's ambient hub authority.

Include `claim_attempt` in the request and every terminal journal mapping. Before acting on an event, td verifies that the child/run still matches the current fence. A stale child may write logs but cannot transition the ticket.

## SCUD migration

### Preserve first

Keep CLI behavior for `init`, `list`, `show`, `next`, `set-status`, `stats`, `waves`, `create`, `generate`, `check-deps`, `tags`, `assign`, `commit`, `warmup`, `doctor`, `mermaid`, and MCP task tools. Preserve `pkg/model`, `pkg/scg`, and `pkg/wave` as the stable library surface.

Create a small executor interface under `pkg/executor`:

```go
type Executor interface {
    Run(ctx context.Context, req protocol.RunRequest, emit func(protocol.RunEvent)) (protocol.RunOutcome, error)
}
```

Initially adapt `internal/rho/rho.go` to this interface and the v1 JSONL schema. Stop silently skipping malformed lines: return a protocol error including the line number, enforce monotonic `seq`, require one terminal event, and retain stderr separately.

### Replace bespoke orchestration

- Refactor `pkg/swarm/executor.go` into a pure scheduler that selects a ready wave and submits each node to `Executor`.
- Replace `pkg/heavy` runtime logic with versioned graph templates (route, specialists, synthesize, verify). Keep `scud heavy` as a deprecated command that expands/runs the template.
- Make `pkg/attractor` consume the same graph/executor interface or remove it if it duplicates task readiness and checkpointing.
- Move prompt-to-task AI generation behind the executor interface; SCUD should ask for structured output but not own provider clients.
- Store `run_id`, outcome, usage, and artifact references on task attempts. Do not copy full provider transcripts into the task model.

### Delete after compatibility window

After all call sites use `pkg/executor`, delete:

- `pkg/llm/provider.go`, provider implementations, and duplicated Anthropic/OpenAI/xAI wire types;
- `pkg/llm/xai_token.go` and related credential resolution;
- `internal/rho`'s legacy flag-building and partial `StreamEvent` schema;
- xAI-native branches in `pkg/heavy`; express them as Rho model/tool capabilities;
- `pkg/swarm/backpressure.go` after the gate adapter below is live;
- bespoke adaptive timeout code in `internal/rho`; the scheduler uses context cancellation, while Rho events provide activity and td owns managed supervision.

Keep a generic argv executor for testing and non-Rho engines. It should implement the same protocol through an adapter and receive no implicit claim that it has Rho semantics.

## Shen-Backpressure integration

Define one JSON artifact boundary rather than embedding Shen in Rho. SCUD or td invokes the configured gate runner after a successful execution attempt:

```json
{
  "protocol": "shen.backpressure/gate-request/v1",
  "run_id": "01J...",
  "workspace": "/srv/worktrees/T-42-a7",
  "artifacts": [{"kind": "git_diff", "uri": "file:artifacts/diff.patch", "sha256": "..."}],
  "gates": ["build", "test", "policy"]
}
```

The runner returns a discharge report:

```json
{
  "protocol": "shen.backpressure/discharge-report/v1",
  "run_id": "01J...",
  "decision": "accept",
  "gates": [{"id":"test","status":"passed","duration_ms":8123,"evidence":{"uri":"file:artifacts/test.log","sha256":"..."}}],
  "failed_obligations": []
}
```

Allowed decisions are `accept`, `retry`, `ask`, and `block`. The report must include evidence hashes and a policy/specification digest. Rho does not interpret it. In local mode SCUD maps it to task status and chooses the next DAG node. In managed mode td journals it and owns retry/attention transitions. A retry is a new `run_id`, bounded by td or SCUD's attempt policy; it is never an unbounded loop inside Rho.

Replace SCUD's shell-string validation in `pkg/swarm/backpressure.go` with an argv-only gate adapter. Existing command arrays can be translated into a generated gate specification for one compatibility release.

## Authorization and OpenResty/shen-lua

The managed path should issue a short-lived execution grant after principal, realm, ticket, claim, provider, budget, and tool policy are proven. The signed or opaque witness is passed to Rho, but enforcement uses explicit fields in the grant so the worker can fail closed without understanding every Shen datatype.

Recommended sequence:

1. OpenResty authenticates the node and reads durable facts.
2. shen-lua evaluates a ground authorization query.
3. The control plane returns a time-limited grant bound to ticket ID, claim fence, workspace digest/path, provider/model set, tools, and budget.
4. td writes that grant into the `RunRequest`; credentials are resolved separately.
5. Rho verifies signature/expiry/bindings through a pluggable verifier, narrows tool constructors, and emits the grant ID (not witness contents) in `run.started`.
6. td rejects all terminal events whose fence or run binding is stale.

Start with a local unsigned grant accepted only by an explicit `--allow-local-grant` mode. Do not imply cryptographic security until issuer verification, clock handling, canonical serialization, and revocation semantics are specified and tested.

## Staged delivery and compatibility

### Phase 0: freeze and fixtures

- Capture current `stream-json` output fixtures and SCUD parser behavior.
- Add a cross-language fixture directory under `docs/protocol/fixtures/`.
- Record the Rho/SCUD/td versions used in integration tests.
- Declare that new consumers must target `rho.run/v1`, not legacy `stream-json`.

Exit: golden fixtures round-trip in Rust and Go; no behavior change.

### Phase 1: protocol and engine seam

- Add `rho-protocol` and serialization tests.
- Add an adapter from internal `AgentEvent` to `RunEvent`.
- Add run limits and cumulative usage accounting.
- Add `rho-cli run` while preserving the old CLI.

Exit: a fixture request can run with a fake provider and always produces ordered events plus exactly one terminal event.

### Phase 2: grants and first-class providers

- Restrict all tools through `ToolContext`.
- Complete cancellation/process-group cleanup.
- Add capability preflight and native Claude/OpenAI Responses/xAI Responses conformance tests.
- Add secret/redaction tests.

Exit: adversarial path/network/command tests cannot escape the grant; provider contract tests pass with recorded fixtures.

### Phase 3: td executor

- Ship `td-executor` compatibility mode.
- Add protocol-aware td adapter behind `agent.protocol`.
- Map events to journals/findings/submission without giving Rho `TD_TOKEN`.
- Exercise crash adoption, stale fencing, cancellation, retryability, and truncated JSONL against td's existing orchestrator loop suite.

Exit: td can run Rho for a full ticket/ask/resume/submit path; opaque executors are unchanged.

### Phase 4: SCUD simplification

- Add `pkg/executor` and v1 Go types generated from or tested against protocol fixtures.
- Route swarm, generation, and heavy graph nodes through it.
- Add Shen-Backpressure adapter and structured discharge storage.
- Mark `pkg/llm`, native-heavy providers, and ad hoc backpressure deprecated.

Exit: SCUD has no provider HTTP calls on its default build path and schedules Rho or a fake executor identically.

### Phase 5: deletion

- Remove deprecated SCUD provider/auth and backpressure code.
- Remove Rho `loop_runner` from the main binary and legacy stream renderer after two minor releases.
- Decide whether `rho-gui` and session HTTP endpoints remain supported frontends or move to a separate repository.
- Extract the protocol repository only if the multi-host extraction criteria are met.

## Conformance and failure tests

The protocol suite must cover:

- unknown additive fields and event types;
- malformed/oversize/truncated JSONL;
- duplicate, missing, and out-of-order sequence numbers;
- process exit without a terminal event and terminal event followed by output;
- cancellation during provider streaming and during Bash grandchildren;
- deadline, turn, token, and cost exhaustion;
- read/write symlink and `..` escapes;
- network destination denial;
- expired, mismatched, and stale-fence grants;
- provider 429/5xx retry classification;
- redaction of credentials and hidden reasoning;
- td crash adoption without duplicate run transition;
- SCUD wave cancellation and a gate-directed retry bounded by attempts.

The compatibility suite should launch a real Rho child from both a small Go harness matching SCUD and a Python harness matching td. Recorded provider fixtures keep CI deterministic; optional live-provider smoke tests use secrets and never gate ordinary pull requests.

## Explicit non-goals

- Reproducing OpenCode's desktop/TUI/plugin platform.
- Making Rho a durable job queue.
- Letting SCUD or Rho mint authority.
- Running model calls inside OpenResty workers.
- Standardizing every possible agent protocol before this stack has two real consumers.
- Preserving provider-specific behavior through an OpenAI-compatible lowest common denominator.

## Immediate implementation tickets

1. Add `rho-protocol` with v1 request/event/outcome types and golden fixtures.
2. Add `AgentEvent -> RunEvent` adapter and terminal-event invariant tests.
3. Implement `rho-cli run --request-file/- --events jsonl` with stdout purity and flushing.
4. Introduce `ToolContext` and path-root enforcement for read/write/edit/find/grep.
5. Enforce cancellation and process-group teardown in Bash and the engine.
6. Add provider capability traits and contract tests for Claude, OpenAI Responses, and xAI Responses.
7. Implement `rho-cli td-executor` against the existing td prompt contract.
8. In SCUD, add `pkg/executor` and parse the full v1 protocol with strict terminal/sequence checks.
9. Convert one SCUD swarm path to the executor interface without deleting legacy paths.
10. Specify and fixture `shen.backpressure/discharge-report/v1`, then adapt one SCUD validation flow.

These tickets are intentionally ordered so the protocol and enforcement become real before duplicated code is deleted.
