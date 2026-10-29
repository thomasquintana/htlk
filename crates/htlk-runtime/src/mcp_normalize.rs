//! Pure assessment of admitted tool evidence under one frozen executable policy.

use crate::{
    EvaluationError, ExtractionError, ExtractionReport, NativeSchemas, extract_mcp_json,
    failure::FailureCode,
};
use htlk_executable::{
    DeclaredContractPolicy, EvaluatorLimits, InferredSchemaPolicy, JsonDocument, McpBinding,
    McpBindingKind, McpResultError, McpToolResult, TypeContext, ValueType, ValueTypeKind,
    cbor::{self, Limits, Map, Value},
    digest::Digest,
};
use std::fmt;

/// Required acceptance rejection; processing errors also have their own typed causes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolResultFailure {
    /// The admitted protocol result says isError:true.
    ToolError,
    /// Original structured content violates an enforced declared contract.
    DeclaredContract,
    /// Selected data fails an explicitly enforced inferred constraint.
    InferredSchema,
    /// Required normalized application data is absent or invalid.
    ApplicationSchema,
    /// Required normalized data is unavailable because extraction could not finish.
    RequiredExtraction,
}
impl ToolResultFailure {
    /// Stable graph-visible classification; protected diagnostics retain the evidence.
    pub const fn code(self) -> FailureCode {
        match self {
            Self::ToolError => FailureCode::McpToolError,
            Self::DeclaredContract => FailureCode::DeclaredContract,
            Self::InferredSchema => FailureCode::InferredSchema,
            Self::ApplicationSchema => FailureCode::ApplicationSchema,
            Self::RequiredExtraction => FailureCode::Budget,
        }
    }
}
/// Operational assessment failure. Exhaustion is never an ordinary schema mismatch.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum ToolNormalizationError {
    /// Wrong binding category.
    Binding,
    /// Protocol, policy, extraction engine or allocation failure.
    Extraction(ExtractionError),
    /// Actual-value checking could not complete.
    Evaluation(EvaluationError),
    /// Native result model/encoding failed.
    Model(McpResultError),
    /// Native record encoding failed.
    Codec(cbor::Error),
}
impl fmt::Display for ToolNormalizationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Binding => f.write_str("tool normalization requires a tool binding"),
            Self::Extraction(error) => write!(f, "tool normalization: {error}"),
            Self::Evaluation(error) => write!(f, "tool normalization check: {error}"),
            Self::Model(error) => write!(f, "tool normalization model: {error}"),
            Self::Codec(error) => write!(f, "tool normalization codec: {error}"),
        }
    }
}
impl std::error::Error for ToolNormalizationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Extraction(error) => Some(error),
            Self::Evaluation(error) => Some(error),
            Self::Model(error) => Some(error),
            Self::Codec(error) => Some(error),
            _ => None,
        }
    }
}
impl ToolNormalizationError {
    /// Stable operational classification, without treating processing limits as mismatch.
    pub fn code(&self) -> FailureCode {
        match self {
            Self::Binding
            | Self::Extraction(ExtractionError::Policy(_) | ExtractionError::SchemaUnavailable) => {
                FailureCode::Profile
            }
            Self::Extraction(ExtractionError::Schema(
                htlk_analyzer::NativeSchemaError::UnknownRoot,
            )) => FailureCode::Profile,
            Self::Extraction(ExtractionError::Protocol(_)) => FailureCode::McpProtocol,
            Self::Evaluation(error) => {
                FailureCode::for_evaluation(error, crate::failure::FailureBoundary::Output)
            }
            Self::Model(McpResultError::Shape(_)) => FailureCode::Invariant,
            _ => FailureCode::Budget,
        }
    }
}
/// Complete immutable evidence, even when it cannot be published as tool success.
#[derive(Clone, Debug, PartialEq)]
pub struct NormalizedToolResult {
    envelope: McpToolResult,
    extraction: ExtractionReport,
    failure: Option<ToolResultFailure>,
}
impl NormalizedToolResult {
    /// Protected result evidence. Its existence does not establish accepted success.
    pub fn envelope(&self) -> &McpToolResult {
        &self.envelope
    }
    /// Bounded original candidate locations, parse diagnostics and resource charges.
    pub fn extraction(&self) -> &ExtractionReport {
        &self.extraction
    }
    /// Required semantic rejection, if any. Callers must check before publication.
    pub fn failure(&self) -> Option<ToolResultFailure> {
        self.failure
    }
}

/// Assesses exact protocol evidence without dispatch, repair, inference, or policy changes.
/// The caller must have authorized/frozen the binding's policy before dispatch and
/// must retain labels, durability, node contracts and atomic acceptance separately.
///
/// # Errors
/// Returns invalid bindings/protocol, unavailable engines, resource exhaustion or
/// malformed native evidence. Schema mismatches themselves remain in the result.
pub fn normalize_mcp_tool_result(
    document: &JsonDocument,
    binding: &McpBinding,
    schemas: &NativeSchemas,
    limits: &Limits,
    evaluator: &EvaluatorLimits,
) -> Result<NormalizedToolResult, ToolNormalizationError> {
    let McpBindingKind::Tool {
        output_schema,
        result_policy: policy,
        ..
    } = binding.kind()
    else {
        return Err(ToolNormalizationError::Binding);
    };
    let extraction = extract_mcp_json(document, &policy.extraction, Some(schemas), limits)
        .map_err(ToolNormalizationError::Extraction)?;
    let Value::Map(original) = document.value() else {
        return Err(ToolNormalizationError::Binding);
    };
    let tool_error = original.get("isError") == Some(&Value::Bool(true));
    let declared_mode = match policy.declared_contract {
        DeclaredContractPolicy::Enforce => "enforce",
        DeclaredContractPolicy::Report => "report",
    };
    let declared = assess(
        *output_schema,
        original.get("structuredContent"),
        declared_mode,
        schemas,
        limits,
        evaluator,
    )?;
    let inferred = if let Some(inferred) = &policy.inferred_schema {
        let mode = match inferred.mode {
            InferredSchemaPolicy::Observe => "observe",
            InferredSchemaPolicy::Enforce => "enforce",
        };
        assess(
            Some(inferred.schema),
            extraction.data(),
            mode,
            schemas,
            limits,
            evaluator,
        )?
    } else {
        Assessment::unconfigured()?
    };
    let application = assess(
        policy.application_schema,
        extraction.data(),
        "enforce",
        schemas,
        limits,
        evaluator,
    )?;
    let failure = if tool_error {
        Some(ToolResultFailure::ToolError)
    } else if declared.rejected && policy.declared_contract == DeclaredContractPolicy::Enforce {
        Some(ToolResultFailure::DeclaredContract)
    } else if extraction.status() == crate::ExtractionStatus::Exhausted
        && (policy.application_schema.is_some()
            || policy
                .inferred_schema
                .as_ref()
                .is_some_and(|adopted| adopted.mode == InferredSchemaPolicy::Enforce))
    {
        Some(ToolResultFailure::RequiredExtraction)
    } else if inferred.rejected
        && policy
            .inferred_schema
            .as_ref()
            .is_some_and(|adopted| adopted.mode == InferredSchemaPolicy::Enforce)
    {
        Some(ToolResultFailure::InferredSchema)
    } else if application.rejected {
        Some(ToolResultFailure::ApplicationSchema)
    } else {
        None
    };
    let validation = object(vec![
        ("declared_schema", declared.value),
        ("inferred_schema", inferred.value),
        ("application_schema", application.value),
    ])?;
    let extraction_value = extraction
        .to_value(limits)
        .map_err(ToolNormalizationError::Extraction)?;
    let mut fields = vec![
        ("protocol_result", document.value().clone()),
        ("extraction", extraction_value),
        ("validation", validation),
    ];
    if let Some(value) = extraction.data() {
        fields.push(("data", value.clone()));
    }
    let envelope = McpToolResult::from_value(&object(fields)?, limits)
        .map_err(ToolNormalizationError::Model)?;
    Ok(NormalizedToolResult {
        envelope,
        extraction,
        failure,
    })
}
struct Assessment {
    value: Value,
    rejected: bool,
}
impl Assessment {
    fn unconfigured() -> Result<Self, ToolNormalizationError> {
        Ok(Self {
            value: object(vec![("status", Value::Text("not_configured".into()))])?,
            rejected: false,
        })
    }
}
fn assess(
    schema: Option<Digest>,
    value: Option<&Value>,
    mode: &str,
    schemas: &NativeSchemas,
    limits: &Limits,
    evaluator: &EvaluatorLimits,
) -> Result<Assessment, ToolNormalizationError> {
    let Some(schema) = schema else {
        return Assessment::unconfigured();
    };
    let status = if let Some(value) = value {
        let ty = ValueType::new(ValueTypeKind::Schema(schema), TypeContext::Value, limits)
            .map_err(|error| ToolNormalizationError::Evaluation(error.into()))?;
        match crate::validate_typed_value(value, &ty, Some(schemas), limits, evaluator) {
            Ok(_) => "valid",
            Err(EvaluationError::OperandType) => "invalid",
            Err(error) => return Err(ToolNormalizationError::Evaluation(error)),
        }
    } else {
        "missing"
    };
    Ok(Assessment {
        rejected: status != "valid",
        value: object(vec![
            ("status", Value::Text(status.into())),
            ("schema", Value::Text(schema.to_string())),
            ("mode", Value::Text(mode.into())),
        ])?,
    })
}
fn object(fields: Vec<(&str, Value)>) -> Result<Value, ToolNormalizationError> {
    Ok(Value::Map(
        Map::try_from_entries(
            fields
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value)),
        )
        .map_err(ToolNormalizationError::Codec)?,
    ))
}
