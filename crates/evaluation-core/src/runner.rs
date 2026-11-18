use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;
use std::future::Future;
use std::pin::Pin;

pub type RunnerFuture<'a> =
    Pin<Box<dyn Future<Output = Result<RunnerResult, RunnerError>> + Send + 'a>>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordReference {
    pub record_id: String,
    pub record_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<String>,
}

impl RecordReference {
    fn is_valid(&self) -> bool {
        !self.record_id.trim().is_empty() && !self.record_type.trim().is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerType {
    Local,
    Cloud,
    SelfHosted,
    External,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerDescriptor {
    pub runner_id: String,
    pub runner_type: RunnerType,
    pub runner_version: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionDisposition {
    Finished,
    Errored,
    TimedOut,
    Cancelled,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum NetworkPolicy {
    #[default]
    DenyAll,
    Allowlist {
        hosts: Vec<String>,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum SideEffectPolicy {
    #[default]
    DenyAll,
    Allowlist {
        operations: Vec<String>,
        approval_ref: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionBudget {
    pub max_wall_time_ms: u64,
    pub max_output_bytes: u64,
    pub max_artifact_bytes: u64,
    pub max_tool_calls: u64,
    pub max_input_tokens: Option<u64>,
    pub max_output_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionRequest {
    pub request_id: String,
    pub idempotency_key: String,
    pub run_ref: RecordReference,
    pub evaluation_ref: RecordReference,
    pub trial_ref: RecordReference,
    pub task_revision_ref: RecordReference,
    pub system_fingerprint_ref: RecordReference,
    /// Agent-visible task input only; hidden grader material must not be included.
    pub task_input: Value,
    pub budget: ExecutionBudget,
    #[serde(default)]
    pub network_policy: NetworkPolicy,
    #[serde(default)]
    pub side_effect_policy: SideEffectPolicy,
    #[serde(default)]
    pub capability_grant_refs: Vec<RecordReference>,
}

/// `Default` fails closed: every flag is false and every limit is `None`, so a
/// runner must explicitly declare the enforcement capabilities it has.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerCapabilities {
    pub supports_network_deny_all: bool,
    pub supports_network_allowlist: bool,
    pub supports_side_effect_deny_all: bool,
    pub supports_side_effect_allowlist: bool,
    pub max_wall_time_ms: Option<u64>,
    pub max_output_bytes: Option<u64>,
    pub max_artifact_bytes: Option<u64>,
    pub max_tool_calls: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerEvent {
    pub sequence: u64,
    pub occurred_at: String,
    pub actor: String,
    pub event_type: String,
    /// Must contain only observable, authorized, appropriately redacted data.
    pub payload: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RedactionState {
    Redacted,
    NotRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactManifest {
    pub artifact_id: String,
    pub content_type: String,
    pub byte_length: u64,
    /// Lowercase or uppercase SHA-256 digest without a prefix.
    pub sha256_hex: String,
    pub redaction_state: RedactionState,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceUsage {
    pub wall_time_ms: u64,
    pub output_bytes: u64,
    pub artifact_bytes: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub tool_calls: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerFailure {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerResult {
    pub request_id: String,
    pub disposition: ExecutionDisposition,
    pub events: Vec<RunnerEvent>,
    pub artifacts: Vec<ArtifactManifest>,
    pub resource_usage: ResourceUsage,
    pub output: Option<Value>,
    pub failure: Option<RunnerFailure>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerErrorKind {
    InvalidRequest,
    UnsupportedPolicy,
    Unavailable,
    ProtocolViolation,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerError {
    pub kind: RunnerErrorKind,
    pub retryable: bool,
    pub message: String,
}

impl RunnerError {
    fn new(kind: RunnerErrorKind, retryable: bool, message: impl Into<String>) -> Self {
        Self {
            kind,
            retryable,
            message: message.into(),
        }
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self::new(RunnerErrorKind::InvalidRequest, false, message)
    }

    fn unsupported(message: impl Into<String>) -> Self {
        Self::new(RunnerErrorKind::UnsupportedPolicy, false, message)
    }

    fn protocol(message: impl Into<String>) -> Self {
        Self::new(RunnerErrorKind::ProtocolViolation, false, message)
    }
}

impl fmt::Display for RunnerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)
    }
}

impl Error for RunnerError {}

/// Provider-independent execution boundary. One call represents exactly one run
/// attempt; retries and planned replicates are orchestrated above this interface.
pub trait Runner: Send + Sync {
    fn descriptor(&self) -> &RunnerDescriptor;
    fn capabilities(&self) -> &RunnerCapabilities;
    fn execute<'a>(&'a self, request: &'a ExecutionRequest) -> RunnerFuture<'a>;
}

/// Validate policy compatibility before any workload is launched.
pub fn validate_request(
    descriptor: &RunnerDescriptor,
    capabilities: &RunnerCapabilities,
    request: &ExecutionRequest,
) -> Result<(), RunnerError> {
    if descriptor.runner_id.trim().is_empty() {
        return Err(RunnerError::invalid("runner_id must not be empty"));
    }
    if request.request_id.trim().is_empty() || request.idempotency_key.trim().is_empty() {
        return Err(RunnerError::invalid(
            "request_id and idempotency_key are required",
        ));
    }
    for (name, reference) in [
        ("run_ref", &request.run_ref),
        ("evaluation_ref", &request.evaluation_ref),
        ("trial_ref", &request.trial_ref),
        ("task_revision_ref", &request.task_revision_ref),
        ("system_fingerprint_ref", &request.system_fingerprint_ref),
    ] {
        if !reference.is_valid() {
            return Err(RunnerError::invalid(format!("{name} is incomplete")));
        }
    }
    if request.budget.max_wall_time_ms == 0 {
        return Err(RunnerError::invalid(
            "max_wall_time_ms must be greater than zero",
        ));
    }
    let mut grant_keys = BTreeSet::new();
    for reference in &request.capability_grant_refs {
        if !reference.is_valid() {
            return Err(RunnerError::invalid(
                "capability grant reference is incomplete",
            ));
        }
        if !grant_keys.insert((reference.record_type.clone(), reference.record_id.clone())) {
            return Err(RunnerError::invalid(
                "capability grant references must be unique",
            ));
        }
    }

    match &request.network_policy {
        NetworkPolicy::DenyAll if !capabilities.supports_network_deny_all => {
            return Err(RunnerError::unsupported(
                "runner cannot enforce deny-all network policy",
            ));
        }
        NetworkPolicy::Allowlist { hosts } => {
            if !capabilities.supports_network_allowlist {
                return Err(RunnerError::unsupported(
                    "runner cannot enforce network allowlists",
                ));
            }
            if hosts.is_empty()
                || hosts.iter().any(|host| {
                    host.trim().is_empty()
                        || host.contains('*')
                        || host.contains('/')
                        || host.contains("://")
                })
            {
                return Err(RunnerError::invalid("network allowlist requires exact, non-empty host entries without wildcards or URLs"));
            }
        }
        _ => {}
    }

    match &request.side_effect_policy {
        SideEffectPolicy::DenyAll if !capabilities.supports_side_effect_deny_all => {
            return Err(RunnerError::unsupported(
                "runner cannot enforce deny-all side-effect policy",
            ));
        }
        SideEffectPolicy::Allowlist {
            operations,
            approval_ref,
        } => {
            if !capabilities.supports_side_effect_allowlist {
                return Err(RunnerError::unsupported(
                    "runner cannot enforce side-effect allowlists",
                ));
            }
            if operations.is_empty() || operations.iter().any(|item| item.trim().is_empty()) {
                return Err(RunnerError::invalid(
                    "side-effect allowlist requires non-empty operations",
                ));
            }
            if approval_ref.trim().is_empty() {
                return Err(RunnerError::invalid(
                    "side-effect allowlist requires an explicit approval reference",
                ));
            }
        }
        _ => {}
    }

    ensure_within(
        "max_wall_time_ms",
        request.budget.max_wall_time_ms,
        capabilities.max_wall_time_ms,
    )?;
    ensure_within(
        "max_output_bytes",
        request.budget.max_output_bytes,
        capabilities.max_output_bytes,
    )?;
    ensure_within(
        "max_artifact_bytes",
        request.budget.max_artifact_bytes,
        capabilities.max_artifact_bytes,
    )?;
    ensure_within(
        "max_tool_calls",
        request.budget.max_tool_calls,
        capabilities.max_tool_calls,
    )?;
    Ok(())
}

fn ensure_within(name: &str, requested: u64, maximum: Option<u64>) -> Result<(), RunnerError> {
    if maximum.is_some_and(|limit| requested > limit) {
        return Err(RunnerError::invalid(format!(
            "requested {name} exceeds runner capability limit"
        )));
    }
    Ok(())
}

/// Check the normalized output before it is accepted by the application layer.
pub fn validate_result(
    request: &ExecutionRequest,
    result: &RunnerResult,
) -> Result<(), RunnerError> {
    if result.request_id != request.request_id {
        return Err(RunnerError::protocol(
            "runner result request_id does not match the submitted request",
        ));
    }
    let mut previous_sequence = 0;
    for event in &result.events {
        if event.sequence == 0 || event.sequence <= previous_sequence {
            return Err(RunnerError::protocol(
                "runner event sequence numbers must be positive and strictly increasing",
            ));
        }
        if event.occurred_at.trim().is_empty()
            || event.actor.trim().is_empty()
            || event.event_type.trim().is_empty()
        {
            return Err(RunnerError::protocol(
                "runner events require occurred_at, actor, and event_type",
            ));
        }
        previous_sequence = event.sequence;
    }

    let mut artifact_ids = BTreeSet::new();
    let mut artifact_bytes = 0_u64;
    for artifact in &result.artifacts {
        if artifact.artifact_id.trim().is_empty() || artifact.content_type.trim().is_empty() {
            return Err(RunnerError::protocol(
                "artifact manifest requires artifact_id and content_type",
            ));
        }
        if !artifact_ids.insert(artifact.artifact_id.as_str()) {
            return Err(RunnerError::protocol(
                "artifact IDs must be unique within a run result",
            ));
        }
        if artifact.sha256_hex.len() != 64
            || !artifact
                .sha256_hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(RunnerError::protocol(
                "artifact SHA-256 digest must contain exactly 64 hexadecimal characters",
            ));
        }
        artifact_bytes = artifact_bytes
            .checked_add(artifact.byte_length)
            .ok_or_else(|| RunnerError::protocol("artifact byte count overflow"))?;
    }
    if artifact_bytes > request.budget.max_artifact_bytes {
        return Err(RunnerError::protocol(
            "artifact payloads exceed the requested artifact byte budget",
        ));
    }
    if result.resource_usage.artifact_bytes < artifact_bytes {
        return Err(RunnerError::protocol(
            "reported artifact byte usage is lower than the artifact manifests",
        ));
    }
    if result.resource_usage.artifact_bytes > request.budget.max_artifact_bytes {
        return Err(RunnerError::protocol(
            "runner artifact usage exceeds the requested artifact byte budget",
        ));
    }
    if result.resource_usage.wall_time_ms > request.budget.max_wall_time_ms
        && result.disposition != ExecutionDisposition::TimedOut
    {
        return Err(RunnerError::protocol(
            "runner exceeded the wall-time budget without reporting timed_out",
        ));
    }
    if result.resource_usage.output_bytes > request.budget.max_output_bytes {
        return Err(RunnerError::protocol(
            "runner output exceeds the requested output byte budget",
        ));
    }
    if result.resource_usage.tool_calls > request.budget.max_tool_calls {
        return Err(RunnerError::protocol(
            "runner tool calls exceed the requested tool-call budget",
        ));
    }
    if result.resource_usage.input_tokens > request.budget.max_input_tokens.unwrap_or(u64::MAX) {
        return Err(RunnerError::protocol(
            "runner input tokens exceed the requested token budget",
        ));
    }
    if result.resource_usage.output_tokens > request.budget.max_output_tokens.unwrap_or(u64::MAX) {
        return Err(RunnerError::protocol(
            "runner output tokens exceed the requested token budget",
        ));
    }

    match result.disposition {
        ExecutionDisposition::Finished if result.failure.is_some() => {
            return Err(RunnerError::protocol(
                "finished runner result must not include a failure",
            ));
        }
        ExecutionDisposition::Errored if result.failure.is_none() => {
            return Err(RunnerError::protocol(
                "errored runner result must include a failure description",
            ));
        }
        ExecutionDisposition::Errored
            if result.failure.as_ref().is_some_and(|failure| {
                failure.code.trim().is_empty() || failure.message.trim().is_empty()
            }) =>
        {
            return Err(RunnerError::protocol(
                "runner failure requires a non-empty code and sanitized message",
            ));
        }
        _ => {}
    }
    Ok(())
}

/// Preflight, execute one attempt, then validate its normalized result.
pub async fn execute_with_runner(
    runner: &dyn Runner,
    request: &ExecutionRequest,
) -> Result<RunnerResult, RunnerError> {
    validate_request(runner.descriptor(), runner.capabilities(), request)?;
    let result = runner.execute(request).await?;
    validate_result(request, &result)?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::task::{Context, Poll, Waker};

    fn reference(record_type: &str, record_id: &str) -> RecordReference {
        RecordReference {
            record_id: record_id.into(),
            record_type: record_type.into(),
            schema_version: Some("1.0.0".into()),
        }
    }

    fn request() -> ExecutionRequest {
        ExecutionRequest {
            request_id: "REQ-1".into(),
            idempotency_key: "IDEMP-1".into(),
            run_ref: reference("run", "RUN-1"),
            evaluation_ref: reference("evaluation", "EVAL-1"),
            trial_ref: reference("trial", "TRIAL-1"),
            task_revision_ref: reference("task_revision", "TASK-1"),
            system_fingerprint_ref: reference("system_fingerprint", "SYS-1"),
            task_input: serde_json::json!({"prompt": "summarize this public fixture"}),
            budget: ExecutionBudget {
                max_wall_time_ms: 30_000,
                max_output_bytes: 16_384,
                max_artifact_bytes: 1_048_576,
                max_tool_calls: 5,
                max_input_tokens: Some(10_000),
                max_output_tokens: Some(2_000),
            },
            network_policy: NetworkPolicy::DenyAll,
            side_effect_policy: SideEffectPolicy::DenyAll,
            capability_grant_refs: vec![],
        }
    }

    fn descriptor() -> RunnerDescriptor {
        RunnerDescriptor {
            runner_id: "test-runner".into(),
            runner_type: RunnerType::Local,
            runner_version: Some("test".into()),
        }
    }

    fn capabilities() -> RunnerCapabilities {
        RunnerCapabilities {
            supports_network_deny_all: true,
            supports_network_allowlist: false,
            supports_side_effect_deny_all: true,
            supports_side_effect_allowlist: false,
            ..RunnerCapabilities::default()
        }
    }

    fn block_on<F: Future>(future: F) -> F::Output {
        let mut context = Context::from_waker(Waker::noop());
        let mut future = Box::pin(future);
        loop {
            match future.as_mut().poll(&mut context) {
                Poll::Ready(output) => return output,
                Poll::Pending => std::thread::yield_now(),
            }
        }
    }

    struct MockRunner {
        descriptor: RunnerDescriptor,
        capabilities: RunnerCapabilities,
        result: RunnerResult,
    }

    impl Runner for MockRunner {
        fn descriptor(&self) -> &RunnerDescriptor {
            &self.descriptor
        }
        fn capabilities(&self) -> &RunnerCapabilities {
            &self.capabilities
        }
        fn execute<'a>(&'a self, _request: &'a ExecutionRequest) -> RunnerFuture<'a> {
            let result = self.result.clone();
            Box::pin(async move { Ok(result) })
        }
    }

    fn success_result() -> RunnerResult {
        RunnerResult {
            request_id: "REQ-1".into(),
            disposition: ExecutionDisposition::Finished,
            events: vec![RunnerEvent {
                sequence: 1,
                occurred_at: "2026-11-17T12:00:00Z".into(),
                actor: "agent".into(),
                event_type: "completed".into(),
                payload: serde_json::json!({"summary": "done"}),
            }],
            artifacts: vec![],
            resource_usage: ResourceUsage::default(),
            output: Some(serde_json::json!({"result": "ok"})),
            failure: None,
        }
    }

    #[test]
    fn runner_capabilities_fail_closed_until_explicitly_declared() {
        let mut req = request();
        let caps = RunnerCapabilities::default();
        assert_eq!(
            validate_request(&descriptor(), &caps, &req)
                .unwrap_err()
                .kind,
            RunnerErrorKind::UnsupportedPolicy
        );
        req.network_policy = NetworkPolicy::DenyAll;
        assert!(validate_request(&descriptor(), &capabilities(), &req).is_ok());
    }

    #[test]
    fn allowlist_requires_runner_support_and_exact_hosts() {
        let mut req = request();
        req.network_policy = NetworkPolicy::Allowlist {
            hosts: vec!["*".into()],
        };
        let mut caps = capabilities();
        caps.supports_network_allowlist = true;
        assert_eq!(
            validate_request(&descriptor(), &caps, &req)
                .unwrap_err()
                .kind,
            RunnerErrorKind::InvalidRequest
        );
        req.network_policy = NetworkPolicy::Allowlist {
            hosts: vec!["api.example.test".into()],
        };
        assert_eq!(validate_request(&descriptor(), &caps, &req), Ok(()));
    }

    #[test]
    fn side_effect_allowlist_requires_explicit_approval_reference() {
        let mut req = request();
        req.side_effect_policy = SideEffectPolicy::Allowlist {
            operations: vec!["write_workspace".into()],
            approval_ref: " ".into(),
        };
        let mut caps = capabilities();
        caps.supports_side_effect_allowlist = true;
        assert_eq!(
            validate_request(&descriptor(), &caps, &req)
                .unwrap_err()
                .kind,
            RunnerErrorKind::InvalidRequest
        );
    }

    #[test]
    fn result_rejects_duplicate_or_non_monotonic_event_sequences() {
        let req = request();
        let mut result = success_result();
        result.events.push(result.events[0].clone());
        assert_eq!(
            validate_result(&req, &result).unwrap_err().kind,
            RunnerErrorKind::ProtocolViolation
        );
    }

    #[test]
    fn async_runner_boundary_performs_preflight_and_result_validation() {
        let runner = MockRunner {
            descriptor: descriptor(),
            capabilities: capabilities(),
            result: success_result(),
        };
        let result = block_on(execute_with_runner(&runner, &request())).unwrap();
        assert_eq!(result.disposition, ExecutionDisposition::Finished);
    }
}
