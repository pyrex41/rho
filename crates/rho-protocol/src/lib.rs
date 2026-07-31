//! Stable JSON/JSONL transport types for one bounded Rho execution.

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, error::Error, fmt};

pub const PROTOCOL: &str = "rho.run/v1";

fn protocol() -> String {
    PROTOCOL.to_owned()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunRequest {
    pub protocol: String,
    pub run_id: String,
    pub model: ModelRef,
    #[serde(rename = "input")]
    pub messages: Vec<Message>,
    #[serde(rename = "system", default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(default)]
    pub limits: RunLimits,
    pub grant: ExecutionGrant,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub context: BTreeMap<String, Value>,
    /// Provider-specific options must be explicitly namespaced by the producer.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extensions: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_ref: Option<String>,
}

impl RunRequest {
    pub fn new(
        run_id: impl Into<String>,
        model: ModelRef,
        messages: Vec<Message>,
        grant: ExecutionGrant,
    ) -> Self {
        Self {
            protocol: protocol(),
            run_id: run_id.into(),
            model,
            messages,
            system_prompt: None,
            limits: RunLimits::default(),
            grant,
            context: BTreeMap::new(),
            extensions: BTreeMap::new(),
            credential_ref: None,
        }
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_protocol(&self.protocol)?;
        nonempty("run_id", &self.run_id)?;
        nonempty("model.provider", &self.model.provider)?;
        nonempty("model.id", &self.model.id)?;
        self.grant.validate()?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRef {
    pub provider: String,
    #[serde(rename = "id")]
    pub id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cost_micros: Option<u64>,
    /// RFC 3339 absolute deadline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionGrant {
    pub grant_id: String,
    pub expires_at: String,
    #[serde(default)]
    pub providers: Vec<String>,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(flatten)]
    pub tools: ToolGrant,
    #[serde(default)]
    pub read_roots: Vec<String>,
    #[serde(default)]
    pub write_roots: Vec<String>,
    #[serde(default)]
    pub network: NetworkGrant,
    /// Opaque policy result, not a provider or service credential.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub witness: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_policy_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command_rules: Vec<CommandRule>,
}

impl ExecutionGrant {
    pub fn validate(&self) -> Result<(), ValidationError> {
        nonempty("grant.grant_id", &self.grant_id)?;
        nonempty("grant.expires_at", &self.expires_at)?;
        if self.providers.is_empty() {
            return Err(ValidationError::InvalidField("grant.providers"));
        }
        if self.models.is_empty() {
            return Err(ValidationError::InvalidField("grant.models"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolGrant {
    /// An empty list grants no tools.
    #[serde(default)]
    pub tools: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandRule {
    pub effect: RuleEffect,
    pub argv_prefix: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleEffect {
    Allow,
    Ask,
    Deny,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkGrant {
    #[serde(default)]
    pub mode: NetworkMode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub destinations: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkMode {
    #[default]
    None,
    ProviderOnly,
    AllowList,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: MessageRole,
    pub content: Vec<ContentBlock>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text { text: String },
    Image { source: ImageSource },
    ToolCall(ToolCall),
    ToolResult(ToolResult),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImageSource {
    Url { url: String },
    Base64 { media_type: String, data: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub call_id: String,
    pub tool: String,
    /// Arguments included in events must already be redacted.
    pub arguments: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    pub tool: String,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Value>,
}

/// Uniform JSONL envelope. `event_type` is an open string so consumers can
/// ignore future event types while still advancing `seq`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunEvent {
    pub protocol: String,
    pub run_id: String,
    pub seq: u64,
    /// RFC 3339 timestamp supplied by the runtime.
    pub time: String,
    #[serde(rename = "type")]
    pub event_type: String,
    pub data: Value,
}

impl RunEvent {
    pub fn new<T: Serialize>(
        run_id: impl Into<String>,
        seq: u64,
        time: impl Into<String>,
        event_type: impl Into<String>,
        data: &T,
    ) -> Result<Self, serde_json::Error> {
        Ok(Self {
            protocol: protocol(),
            run_id: run_id.into(),
            seq,
            time: time.into(),
            event_type: event_type.into(),
            data: serde_json::to_value(data)?,
        })
    }

    pub fn decode_data<T: DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_value(self.data.clone())
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self.event_type.as_str(),
            "run.completed" | "run.failed" | "run.cancelled"
        )
    }

    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_protocol(&self.protocol)?;
        nonempty("run_id", &self.run_id)?;
        nonempty("time", &self.time)?;
        nonempty("type", &self.event_type)?;
        match self.event_type.as_str() {
            "run.completed" => validate_payload::<RunOutcome>(&self.data, "run.completed.data"),
            "run.failed" => validate_payload::<RunFailure>(&self.data, "run.failed.data"),
            "run.cancelled" => validate_payload::<RunCancelled>(&self.data, "run.cancelled.data"),
            _ => Ok(()),
        }
    }
}

/// Validates ordering and the exactly-one-terminal invariant incrementally.
#[derive(Debug, Default)]
pub struct EventStreamValidator {
    run_id: Option<String>,
    last_seq: Option<u64>,
    terminal_seen: bool,
}

impl EventStreamValidator {
    pub fn push(&mut self, event: &RunEvent) -> Result<(), ValidationError> {
        event.validate()?;
        if let Some(run_id) = &self.run_id {
            if run_id != &event.run_id {
                return Err(ValidationError::RunIdChanged);
            }
        } else {
            self.run_id = Some(event.run_id.clone());
        }
        if self.terminal_seen {
            return Err(ValidationError::EventAfterTerminal);
        }
        if let Some(last) = self.last_seq {
            if event.seq <= last {
                return Err(ValidationError::NonMonotonicSequence {
                    previous: last,
                    received: event.seq,
                });
            }
        }
        self.last_seq = Some(event.seq);
        self.terminal_seen = event.is_terminal();
        Ok(())
    }

    pub fn finish(self) -> Result<(), ValidationError> {
        if self.terminal_seen {
            Ok(())
        } else {
            Err(ValidationError::MissingTerminal)
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_micros: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunOutcome {
    pub status: RunStatus,
    pub stop_reason: String,
    pub usage: Usage,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<Artifact>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Succeeded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    pub kind: String,
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunFailure {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub details: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunCancelled {
    pub reason: String,
    #[serde(default)]
    pub usage: Usage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    UnsupportedProtocol(String),
    InvalidField(&'static str),
    InvalidPayload(&'static str),
    RunIdChanged,
    NonMonotonicSequence { previous: u64, received: u64 },
    EventAfterTerminal,
    MissingTerminal,
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedProtocol(value) => write!(f, "unsupported protocol {value:?}"),
            Self::InvalidField(field) => write!(f, "invalid or missing field {field}"),
            Self::InvalidPayload(field) => write!(f, "invalid payload at {field}"),
            Self::RunIdChanged => write!(f, "run_id changed within event stream"),
            Self::NonMonotonicSequence { previous, received } => write!(
                f,
                "event sequence is not monotonic: {received} follows {previous}"
            ),
            Self::EventAfterTerminal => write!(f, "event received after terminal event"),
            Self::MissingTerminal => write!(f, "event stream ended without a terminal event"),
        }
    }
}

impl Error for ValidationError {}

fn validate_protocol(value: &str) -> Result<(), ValidationError> {
    if value == PROTOCOL {
        Ok(())
    } else {
        Err(ValidationError::UnsupportedProtocol(value.to_owned()))
    }
}

fn nonempty(field: &'static str, value: &str) -> Result<(), ValidationError> {
    if value.trim().is_empty() {
        Err(ValidationError::InvalidField(field))
    } else {
        Ok(())
    }
}

fn validate_payload<T: DeserializeOwned>(
    value: &Value,
    field: &'static str,
) -> Result<(), ValidationError> {
    serde_json::from_value::<T>(value.clone())
        .map(|_| ())
        .map_err(|_| ValidationError::InvalidPayload(field))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn grant() -> ExecutionGrant {
        ExecutionGrant {
            grant_id: "grant-1".into(),
            expires_at: "2026-08-01T01:00:00Z".into(),
            providers: vec!["anthropic".into()],
            models: vec!["claude-*".into()],
            tools: ToolGrant {
                tools: vec!["read".into(), "bash".into()],
            },
            read_roots: vec!["/srv/worktrees/T-42".into()],
            write_roots: vec!["/srv/worktrees/T-42".into()],
            network: NetworkGrant {
                mode: NetworkMode::ProviderOnly,
                destinations: vec![],
            },
            witness: Some("opaque-policy-result".into()),
            command_policy_ref: None,
            command_rules: vec![],
        }
    }

    fn request() -> RunRequest {
        let mut request = RunRequest::new(
            "01JTEST",
            ModelRef {
                provider: "anthropic".into(),
                id: "claude-sonnet-4-5".into(),
            },
            vec![Message {
                role: MessageRole::User,
                content: vec![ContentBlock::Text {
                    text: "Implement T-42".into(),
                }],
            }],
            grant(),
        );
        request.system_prompt = Some("Work in the isolated checkout.".into());
        request.limits.max_turns = Some(24);
        request.context.insert("ticket_id".into(), json!("T-42"));
        request
    }

    #[test]
    fn request_has_rfc_shape_and_round_trips() {
        let request = request();
        request.validate().unwrap();
        let value = serde_json::to_value(&request).unwrap();
        assert_eq!(value["protocol"], "rho.run/v1");
        assert_eq!(value["model"]["id"], "claude-sonnet-4-5");
        assert_eq!(value["input"][0]["content"][0]["type"], "text");
        assert_eq!(value["system"], "Work in the isolated checkout.");
        assert_eq!(value["grant"]["network"]["mode"], "provider_only");
        assert!(value.get("messages").is_none());
        assert!(value.get("system_prompt").is_none());
        assert_eq!(
            serde_json::from_value::<RunRequest>(value).unwrap(),
            request
        );
    }

    #[test]
    fn unsupported_protocol_is_rejected_by_validation() {
        let mut request = request();
        request.protocol = "rho.run/v2".into();
        assert_eq!(
            request.validate(),
            Err(ValidationError::UnsupportedProtocol("rho.run/v2".into()))
        );
    }

    #[test]
    fn protocol_field_is_required_on_the_wire() {
        let mut request = serde_json::to_value(request()).unwrap();
        request.as_object_mut().unwrap().remove("protocol");
        assert!(serde_json::from_value::<RunRequest>(request).is_err());

        let mut event = serde_json::to_value(terminal(1)).unwrap();
        event.as_object_mut().unwrap().remove("protocol");
        assert!(serde_json::from_value::<RunEvent>(event).is_err());
    }

    #[test]
    fn event_envelope_has_exact_stable_shape() {
        let event = RunEvent::new(
            "01JTEST",
            12,
            "2026-07-31T20:03:04Z",
            "tool.completed",
            &json!({
                "call_id": "call_7",
                "tool": "bash",
                "ok": true,
                "exit_code": 0,
                "output_bytes": 847
            }),
        )
        .unwrap();
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(
            value,
            json!({
                "protocol": "rho.run/v1",
                "run_id": "01JTEST",
                "seq": 12,
                "time": "2026-07-31T20:03:04Z",
                "type": "tool.completed",
                "data": {
                    "call_id": "call_7", "tool": "bash", "ok": true,
                    "exit_code": 0, "output_bytes": 847
                }
            })
        );
        assert_eq!(serde_json::from_value::<RunEvent>(value).unwrap(), event);
    }

    fn terminal(seq: u64) -> RunEvent {
        RunEvent::new(
            "01JTEST",
            seq,
            "2026-07-31T20:04:10Z",
            "run.completed",
            &RunOutcome {
                status: RunStatus::Succeeded,
                stop_reason: "complete".into(),
                usage: Usage::default(),
                artifacts: vec![],
            },
        )
        .unwrap()
    }

    #[test]
    fn stream_requires_monotonic_sequence_and_one_terminal() {
        let started = RunEvent::new(
            "01JTEST",
            1,
            "2026-07-31T20:03:04Z",
            "run.started",
            &json!({}),
        )
        .unwrap();
        let mut validator = EventStreamValidator::default();
        validator.push(&started).unwrap();
        validator.push(&terminal(2)).unwrap();
        assert_eq!(
            validator.push(&terminal(3)),
            Err(ValidationError::EventAfterTerminal)
        );

        let mut validator = EventStreamValidator::default();
        validator.push(&terminal(2)).unwrap();
        assert!(validator.finish().is_ok());

        let mut validator = EventStreamValidator::default();
        validator.push(&started).unwrap();
        assert_eq!(validator.finish(), Err(ValidationError::MissingTerminal));
    }

    #[test]
    fn terminal_payload_and_run_identity_are_validated() {
        let invalid = RunEvent::new(
            "01JTEST",
            1,
            "2026-07-31T20:04:10Z",
            "run.failed",
            &json!({"message": "missing required fields"}),
        )
        .unwrap();
        assert_eq!(
            invalid.validate(),
            Err(ValidationError::InvalidPayload("run.failed.data"))
        );

        let mut validator = EventStreamValidator::default();
        validator.push(&terminal(1)).unwrap();
        let other = RunEvent {
            run_id: "other".into(),
            ..terminal(2)
        };
        // Terminal invariant wins after a completed stream; a fresh validator
        // independently checks changing identities.
        assert_eq!(validator.push(&other), Err(ValidationError::RunIdChanged));
    }

    #[test]
    fn unknown_event_types_remain_decodable() {
        let value = json!({
            "protocol": "rho.run/v1", "run_id": "01JTEST", "seq": 8,
            "time": "2026-07-31T20:03:04Z", "type": "future.event",
            "data": {"new_field": true}, "future_envelope_field": 42
        });
        let event: RunEvent = serde_json::from_value(value).unwrap();
        assert_eq!(event.event_type, "future.event");
        event.validate().unwrap();
    }
}
