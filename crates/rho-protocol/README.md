# rho-protocol

`rho-protocol` is the language-neutral wire contract for one bounded Rho agent
execution. It follows `docs/RFC-0001-bounded-execution-stack.md` and contains
data and validation only—no provider client, async runtime, filesystem access,
or tool implementation.

## Version 1

Requests carry `"protocol":"rho.run/v1"`. Events are UTF-8 JSONL objects with
the uniform envelope:

```json
{"protocol":"rho.run/v1","run_id":"01J...","seq":12,"time":"2026-07-31T20:03:04Z","type":"tool.completed","data":{"call_id":"call_7","tool":"bash","ok":true}}
```

`RunRequest::validate` rejects unsupported protocol versions and incomplete
grants. `RunEvent::validate` checks envelope fields and typed terminal payloads.
`EventStreamValidator` enforces one run identity, monotonically increasing
sequence numbers, no events after a terminal event, and a terminal event before
the stream finishes.

Event type names are intentionally open strings. This lets an older consumer
ignore an event introduced by a newer compatible producer while still advancing
its sequence cursor. Typed payload structs are provided for messages, tool
calls/results, usage, completion, failure, cancellation, and artifacts.

## Stability and safety

- Required-field removal, meaning changes, or type changes require a new
  protocol identifier.
- Model, provider, tool, failure, artifact, and event names remain open strings.
- Durations use integer milliseconds; costs use integer micros; timestamps are
  RFC 3339 strings.
- The empty `ToolGrant` grants no tools. A grant restricts authority; it is not
  itself a credential or proof of identity.
- Credential values, bearer tokens, and hidden reasoning must never be emitted
  in events. `credential_ref` names a host-resolved credential without carrying
  its value.
- Producers may add object fields. Serde consumers ignore unknown fields by
  default, as required by the RFC.

The embedding engine remains responsible for checking grant expiry, resolving
canonical roots, enforcing effects, applying limits, and redacting event data.

## Rho CLI provider mapping

The protocol keeps provider and model identifiers open, while the Rho CLI's
`rho.run/v1` engine currently supports `anthropic`, `openai`, and `xai`.
Documented Rho model aliases are resolved before making the provider request;
raw model IDs remain valid. In particular, xAI models registered for its
OpenAI-compatible chat endpoint retain the protocol identity `xai` rather than
being misreported as OpenAI. This distinction also selects `XAI_API_KEY` and
prevents cross-provider credential references.
