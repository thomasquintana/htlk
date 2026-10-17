#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

use htlk_analyzer::*;
use htlk_executable::*;

pub mod artifact;
pub mod binding;
mod checked_evaluate;
pub mod clock;
mod evaluate;
pub mod failure;
pub mod identity;
mod mcp_protocol;
mod native_profile;
mod native_registry;
pub mod records;
mod run;
mod runtime;
mod runtime_type;
mod schema_projection;
pub mod store;
mod uri_template;
mod verified;

pub use checked_evaluate::{CallbackArgument, CallbackInvocation, CheckedExpression};
pub use evaluate::{
    ConditionResult, ConditionValue, EvaluationArgument, EvaluationContext, EvaluationError,
    EvaluationFrame, EvaluationMeter, EvaluationOutcome, EvaluationResult, EvaluationUsage,
    EvaluationValue, evaluate, evaluate_condition,
};
pub use mcp_protocol::{validate_mcp_prompt_result, validate_resource_snapshot};
pub use native_profile::native_profile;
pub use native_registry::{
    NativeCallContext, NativeFunction, NativeFunctionImpl, NativeRegistry, NativeRegistryError,
};
pub use run::RunStarted;
pub use runtime::{
    Attestation, Authorization, GraphRegistration, HostContext, Runtime, RuntimeConfig,
    RuntimeError, RuntimePolicy,
};
pub use runtime_type::{project_typed_value, validate_typed_value};
pub use schema_projection::SchemaProjection;
pub use uri_template::expand_uri_template;
pub use verified::{
    ExecutableVerificationError, VerifiedExecutable, verify_executable, verify_executable_owned,
};
