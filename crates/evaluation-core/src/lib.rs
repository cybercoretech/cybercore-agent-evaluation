//! Provider-independent domain and execution primitives for CyberCore Agent Evaluation.
//!
//! The core has no Cloudflare, HTTP, database, MCP, or UI dependency. Runtime
//! adapters depend on these types; the core does not depend on adapters.

pub mod domain_invariants;
pub mod local_process;
pub mod orchestration;
pub mod run_state;
pub mod runner;
pub mod workspace_outputs;

pub use domain_invariants::{validate_domain_fixture, validate_domain_fixtures};
pub use local_process::{
    LocalProcessInputFile, LocalProcessRunner, LocalProcessRunnerConfig, LocalProcessTaskInput,
};
pub use orchestration::{
    AttemptAction, AttemptDecision, AttemptIdentity, AttemptReservation, AttemptStatus,
    EvaluationSchedule, OrchestrationError, RetryPolicy, RetryReason, TrialAttempt,
    TrialCoordinator, TrialDefinition, TrialExecutionState, TrialSnapshot,
};
pub use run_state::{ExecutionStatus, RunOutcome, RunState, RunStateError, VerificationState};
pub use runner::{
    execute_with_runner, ArtifactManifest, ExecutionBudget, ExecutionDisposition, ExecutionRequest,
    NetworkPolicy, RecordReference, RedactionState, ResourceUsage, Runner, RunnerCapabilities,
    RunnerDescriptor, RunnerError, RunnerErrorKind, RunnerEvent, RunnerFailure, RunnerFuture,
    RunnerResult, RunnerType, SideEffectPolicy,
};
pub use workspace_outputs::{
    artifact_staging_dir, collect_outputs, CollectedFile, CollectedOutputs,
};

/// Version of the core's public Rust API surface.
pub const CORE_API_VERSION: &str = "0.3.0";
