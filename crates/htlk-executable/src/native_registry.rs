//! Exact host-linked pure-library admission and checked dispatch.
use crate::{
    CheckedExpression, EvaluationArgument, EvaluationContext, EvaluationError, EvaluationMeter,
    EvaluationOutcome, EvaluationResult, EvaluationValue, EvaluatorLimits, Expression,
    ExpressionCallType, ExpressionCallbackType, Identifier, Library, MetadataError, NativeSchemas,
    PathStep, PromptTemplate, ValueReference, digest::Digest,
};
use htlk_cbor::Limits;
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

/// A trusted pure native implementation. It must charge variable work through the
/// supplied meter before performing it and invoke callbacks through the supplied
/// context. Native Rust implementations are trusted code, not sandboxed programs.
pub type NativeFunctionImpl = for<'a, 'b> fn(
    &[EvaluationArgument],
    &NativeCallContext<'a>,
    &mut EvaluationMeter<'b>,
) -> Result<EvaluationValue, EvaluationError>;

/// One linked function with a mandatory deterministic dispatch charge.
#[derive(Clone, Copy)]
pub struct NativeFunction {
    implementation: NativeFunctionImpl,
    base_work: u64,
}
impl NativeFunction {
    /// Declares a trusted pure implementation and its positive base work charge.
    ///
    /// # Errors
    /// Returns an invalid-work error for zero base work.
    pub fn new(
        implementation: NativeFunctionImpl,
        base_work: u64,
    ) -> Result<Self, NativeRegistryError> {
        if base_work == 0 {
            return Err(NativeRegistryError::InvalidWork);
        }
        Ok(Self {
            implementation,
            base_work,
        })
    }
}
struct LinkedLibrary {
    manifest: Library,
    functions: BTreeMap<Identifier, NativeFunction>,
}

/// Bounded host-owned registry. Unreferenced linked libraries are permitted;
/// every reached supplied manifest must match its complete linked manifest.
/// The empty registry supports the specified evaluator core without inventing a
/// standard-library inventory. Registration never runs library code.
pub struct NativeRegistry {
    libraries: BTreeMap<Digest, LinkedLibrary>,
    limits: Limits,
    bytes: usize,
    functions: usize,
    policies: BTreeSet<Digest>,
}
impl NativeRegistry {
    /// Creates an empty registry under validated host admission limits.
    ///
    /// # Errors
    /// Returns unsupported codec configuration.
    pub fn new(limits: &Limits) -> Result<Self, NativeRegistryError> {
        limits.validate()?;
        Ok(Self {
            libraries: BTreeMap::new(),
            limits: limits.clone(),
            bytes: 0,
            functions: 0,
            policies: BTreeSet::new(),
        })
    }
    /// Selects an exact host policy and returns its shipped native profile.
    /// Selection is idempotent and must come from trusted host configuration;
    /// untrusted executable policy contents do not select themselves.
    ///
    /// # Errors
    /// Returns invalid policy/profile metadata or registry admission limits.
    /// Failure leaves the registry unchanged.
    pub fn link_policy(
        &mut self,
        policy: &crate::PolicyDocument,
    ) -> Result<crate::ExecutionProfile, NativeRegistryError> {
        let profile = crate::native_profile(policy, &self.limits)?;
        if !self.policies.contains(&policy.digest()) {
            let bytes = self
                .bytes
                .checked_add(32)
                .ok_or(NativeRegistryError::Limit)?;
            if bytes > self.limits.max_document_bytes
                || self.policies.len().saturating_add(self.libraries.len())
                    >= self.limits.max_collection_entries
            {
                return Err(NativeRegistryError::Limit);
            }
            self.policies.insert(policy.digest());
            self.bytes = bytes;
        }
        Ok(profile)
    }
    /// Links exactly the complete manifest's functions under its implementation
    /// identity. The host vouches for code identity, purity and metering; submitted
    /// executable metadata cannot register or replace implementations.
    ///
    /// # Errors
    /// Rejects duplicate identities, missing/extra functions, or admission limits.
    /// Failure leaves the registry unchanged.
    pub fn register(
        &mut self,
        manifest: Library,
        functions: BTreeMap<Identifier, NativeFunction>,
    ) -> Result<(), NativeRegistryError> {
        let encoded = manifest.encode(&self.limits)?;
        let id = manifest.implementation_digest();
        if self.libraries.contains_key(&id) {
            return Err(NativeRegistryError::DuplicateLibrary);
        }
        if functions.len() != manifest.functions().len()
            || !functions
                .keys()
                .zip(manifest.functions())
                .all(|(a, (b, _))| a == b)
        {
            return Err(NativeRegistryError::FunctionCoverage);
        }
        let bytes = self
            .bytes
            .checked_add(encoded.len())
            .ok_or(NativeRegistryError::Limit)?;
        let count = self
            .functions
            .checked_add(functions.len())
            .ok_or(NativeRegistryError::Limit)?;
        if bytes > self.limits.max_document_bytes
            || count > self.limits.max_collection_entries
            || self.libraries.len().saturating_add(self.policies.len())
                >= self.limits.max_collection_entries
        {
            return Err(NativeRegistryError::Limit);
        }
        self.libraries.insert(
            id,
            LinkedLibrary {
                manifest,
                functions,
            },
        );
        self.bytes = bytes;
        self.functions = count;
        Ok(())
    }
    /// Checks complete library metadata, including unused public signatures.
    ///
    /// # Errors
    /// Rejects unknown implementation identities, changed manifests, or limits.
    pub fn verify_library(&self, manifest: &Library) -> Result<(), NativeRegistryError> {
        manifest.to_value(&self.limits)?;
        let linked = self
            .libraries
            .get(&manifest.implementation_digest())
            .ok_or(NativeRegistryError::UnknownLibrary)?;
        if linked.manifest != *manifest {
            return Err(NativeRegistryError::ManifestMismatch);
        }
        Ok(())
    }
    /// Borrows the host-linked complete manifest for an exact implementation.
    pub fn library(&self, identity: &Digest) -> Option<&Library> {
        self.libraries.get(identity).map(|l| &l.manifest)
    }
    /// Verifies the exact shipped native engine profile and policy identity.
    /// The native JSON Schema backend retains its documented capability contract:
    /// no whole-validation fuel counter, hard heap cap, or in-process deadline.
    ///
    /// # Errors
    /// Rejects engine/core/policy identity mismatches or policy/metadata limits.
    pub fn verify_profile(
        &self,
        profile: &crate::ExecutionProfile,
        policy: &crate::PolicyDocument,
    ) -> Result<(), NativeRegistryError> {
        profile.to_value(&self.limits)?;
        let expected = crate::native_profile(policy, &self.limits)?;
        let field = if profile.core_digest() != expected.core_digest() {
            Some("core")
        } else if profile.regex_engine() != expected.regex_engine() {
            Some("regex")
        } else if profile.schema_validator() != expected.schema_validator() {
            Some("schema")
        } else if profile.uri_template_engine() != expected.uri_template_engine() {
            Some("uri_template")
        } else if profile.policy_document() != expected.policy_document() {
            Some("policy")
        } else {
            None
        };
        match field {
            Some(field) => Err(NativeRegistryError::ProfileMismatch(field)),
            None if !self.policies.contains(&policy.digest()) => {
                Err(NativeRegistryError::PolicyNotLinked)
            }
            None => Ok(()),
        }
    }
    /// Evaluates through exact linked native implementations. Frozen data/outcomes
    /// come from `frame`; library dispatch always uses this registry. Every supplied
    /// expression-environment library is checked before any branch executes.
    ///
    /// # Errors
    /// Returns manifest mismatch, execution/type/callback errors, or limits.
    pub fn evaluate(
        &self,
        expression: &CheckedExpression<'_>,
        frame: &impl EvaluationContext,
        schemas: Option<&NativeSchemas>,
        policy: &EvaluatorLimits,
    ) -> Result<EvaluationResult, EvaluationError> {
        let schemas = expression.schemas.or(schemas);
        for manifest in expression.environment.libraries.values() {
            self.verify_library(manifest)?;
        }
        self.evaluate_admitted(expression, frame, schemas, policy)
    }
    pub(crate) fn evaluate_admitted(
        &self,
        expression: &CheckedExpression<'_>,
        frame: &impl EvaluationContext,
        schemas: Option<&NativeSchemas>,
        policy: &EvaluatorLimits,
    ) -> Result<EvaluationResult, EvaluationError> {
        expression.evaluate(
            &RegistryContext {
                registry: self,
                frame,
                schemas,
            },
            schemas,
            policy,
        )
    }
    fn function(
        &self,
        library: Digest,
        name: &Identifier,
        meter: &mut EvaluationMeter<'_>,
    ) -> Result<NativeFunction, EvaluationError> {
        let levels = (usize::BITS - self.libraries.len().leading_zeros()).max(1);
        meter.charge(32 * u64::from(levels))?;
        let library = self
            .libraries
            .get(&library)
            .ok_or(EvaluationError::UnknownFunction)?;
        let levels = (usize::BITS - library.functions.len().leading_zeros()).max(1);
        meter.charge((name.as_str().len() as u64).saturating_mul(u64::from(levels)))?;
        let function = *library
            .functions
            .get(name)
            .ok_or(EvaluationError::UnknownFunction)?;
        meter.charge(function.base_work)?;
        Ok(function)
    }
}

/// Callback capabilities for one admitted native call. Native functions cannot
/// replace the frozen data context or redirect dispatch through this interface.
pub struct NativeCallContext<'a> {
    boundary: Option<&'a ExpressionCallType>,
    callback: Option<&'a ExpressionCallbackType>,
    context: &'a dyn EvaluationContext,
    schemas: Option<&'a NativeSchemas>,
}
impl NativeCallContext<'_> {
    /// Concrete positional types/presence after rank-one generic inference.
    pub fn parameters(&self) -> &[crate::Port] {
        self.signature().0
    }
    /// Concrete result type/presence, including inference from the caller's result
    /// annotation for functions with no value arguments.
    pub fn returns(&self) -> &crate::Port {
        self.signature().1
    }
    /// Canonical origin of this admitted call or static callback reference.
    pub fn expression_path(&self) -> &[usize] {
        if let Some(boundary) = self.boundary {
            &boundary.expression_path
        } else {
            &self
                .callback
                .expect("checked callback context")
                .expression_path
        }
    }
    fn signature(&self) -> (&[crate::Port], &crate::Port) {
        if let Some(boundary) = self.boundary {
            return (&boundary.parameters, &boundary.returns);
        }
        match self
            .callback
            .expect("checked callback context")
            .signature
            .kind()
        {
            crate::ValueTypeKind::Function {
                parameters,
                returns,
            } => (parameters, returns),
            _ => unreachable!("checked callback signature"),
        }
    }
    /// Forwards ordinary values and statically admitted callback slots to a
    /// higher-order callback, retaining checked dispatch for further invocations.
    ///
    /// # Errors
    /// Returns incompatible callback/value boundaries, unknown slots, or limits.
    pub fn invoke_callback_with(
        &self,
        index: usize,
        arguments: &[crate::CallbackArgument<'_>],
        meter: &mut EvaluationMeter<'_>,
    ) -> Result<EvaluationValue, EvaluationError> {
        self.boundary
            .ok_or(EvaluationError::UnresolvedType)?
            .invoke_callback_with(index, arguments, self.context, self.schemas, meter)
    }
    /// Invokes the statically admitted callback at the given argument position.
    ///
    /// # Errors
    /// Returns invalid callback/value boundaries or shared evaluator limits.
    pub fn invoke_callback(
        &self,
        index: usize,
        arguments: &[EvaluationValue],
        meter: &mut EvaluationMeter<'_>,
    ) -> Result<EvaluationValue, EvaluationError> {
        self.boundary
            .ok_or(EvaluationError::UnresolvedType)?
            .invoke_callback(index, arguments, self.context, self.schemas, meter)
    }
}
struct RegistryContext<'a, C> {
    registry: &'a NativeRegistry,
    frame: &'a C,
    schemas: Option<&'a NativeSchemas>,
}
impl<C: EvaluationContext> EvaluationContext for RegistryContext<'_, C> {
    fn resolve(
        &self,
        reference: &ValueReference,
        path: &[PathStep],
        meter: &mut EvaluationMeter<'_>,
    ) -> Result<EvaluationValue, EvaluationError> {
        self.frame.resolve(reference, path, meter)
    }
    fn outcome(&self, node: &Identifier) -> Option<&EvaluationOutcome> {
        self.frame.outcome(node)
    }
    fn template(&self, digest: &Digest) -> Option<&PromptTemplate> {
        self.frame.template(digest)
    }
    fn call_typed(
        &self,
        _: &Expression,
        boundary: &ExpressionCallType,
        arguments: &[EvaluationArgument],
        meter: &mut EvaluationMeter<'_>,
    ) -> Result<EvaluationValue, EvaluationError> {
        let function = self
            .registry
            .function(boundary.library, &boundary.name, meter)?;
        (function.implementation)(
            arguments,
            &NativeCallContext {
                boundary: Some(boundary),
                callback: None,
                context: self,
                schemas: self.schemas,
            },
            meter,
        )
    }
    fn call_callback(
        &self,
        callback: &ExpressionCallbackType,
        arguments: &[EvaluationArgument],
        meter: &mut EvaluationMeter<'_>,
    ) -> Result<EvaluationValue, EvaluationError> {
        let function = self
            .registry
            .function(callback.library, &callback.name, meter)?;
        (function.implementation)(
            arguments,
            &NativeCallContext {
                boundary: None,
                callback: Some(callback),
                context: self,
                schemas: self.schemas,
            },
            meter,
        )
    }
    fn call_callback_typed(
        &self,
        callback: &ExpressionCallbackType,
        boundary: &ExpressionCallType,
        arguments: &[EvaluationArgument],
        meter: &mut EvaluationMeter<'_>,
    ) -> Result<EvaluationValue, EvaluationError> {
        let function = self
            .registry
            .function(callback.library, &callback.name, meter)?;
        (function.implementation)(
            arguments,
            &NativeCallContext {
                boundary: Some(boundary),
                callback: Some(callback),
                context: self,
                schemas: self.schemas,
            },
            meter,
        )
    }
}
/// Redacted linked-registry admission failure.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum NativeRegistryError {
    /// The exact policy was not selected by the trusted host registry.
    PolicyNotLinked,
    /// Profile differs from the shipped native implementation or policy.
    ProfileMismatch(&'static str),
    /// Invalid or oversized policy document.
    Policy(crate::PolicyError),
    /// Codec configuration or bounds.
    Codec(htlk_cbor::Error),
    /// Invalid or oversized manifest.
    Metadata(MetadataError),
    /// A dispatch charge must be positive.
    InvalidWork,
    /// An implementation identity is already registered.
    DuplicateLibrary,
    /// Function implementations do not exactly cover the manifest.
    FunctionCoverage,
    /// Aggregate registration bounds exceeded.
    Limit,
    /// Submitted identity is not linked.
    UnknownLibrary,
    /// Complete submitted manifest differs from the linked manifest.
    ManifestMismatch,
}
impl From<htlk_cbor::Error> for NativeRegistryError {
    fn from(e: htlk_cbor::Error) -> Self {
        Self::Codec(e)
    }
}
impl From<MetadataError> for NativeRegistryError {
    fn from(e: MetadataError) -> Self {
        Self::Metadata(e)
    }
}
impl From<crate::PolicyError> for NativeRegistryError {
    fn from(e: crate::PolicyError) -> Self {
        Self::Policy(e)
    }
}
impl fmt::Display for NativeRegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "native registry: {self:?}")
    }
}
impl std::error::Error for NativeRegistryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Codec(e) => Some(e),
            Self::Metadata(e) => Some(e),
            Self::Policy(e) => Some(e),
            _ => None,
        }
    }
}
