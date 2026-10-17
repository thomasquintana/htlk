//! Authenticated host-facing registration over immutable native admission.

use crate::{
    ExecutableVerificationError, NativeRegistry, VerifiedExecutable,
    artifact::{ArtifactError, ArtifactMetadata, AuthorizedValue},
    clock::{CoordinatorClock, SystemClock},
    failure::{FailureBoundary, FailureCode, RuntimeFailure},
    identity::ArtifactId,
    records::{RuntimeRecordError, SecurityLabels},
    store::{SqliteStore, StoreError},
    verify_executable_owned,
};
use htlk_executable::{
    Identifier,
    cbor::{self, Limits, Map, Value},
    digest::Digest,
    record_accounting::RecordAccounting,
};
use std::{
    collections::BTreeMap,
    fmt,
    sync::{Arc, Mutex},
};

/// Identity established by the embedding transport/session. Construction does not
/// authenticate a user; the trusted host must supply its actual caller identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostContext {
    principal: String,
}
impl HostContext {
    /// Bounds and retains an authenticated host identity without normalization.
    ///
    /// # Errors
    /// Returns invalid codec configuration or principal size limits.
    pub fn new(principal: String, limits: &Limits) -> Result<Self, RuntimeError> {
        RecordAccounting::new(limits)?
            .text(&principal, 0)
            .map_err(|_| RuntimeError::failure(FailureCode::Input))?;
        Ok(Self { principal })
    }
    /// Exact authenticated user/service identity supplied by the host.
    pub fn principal(&self) -> &str {
        &self.principal
    }
}
/// Detached producer evidence, outside executable semantic identity. The trusted
/// deployment policy interprets its format and verifies its contents and signer.
#[derive(Clone, PartialEq, Eq)]
pub struct Attestation {
    /// Deployment-selected evidence format.
    pub format: String,
    /// Opaque detached evidence bytes, never an authorization claim by themselves.
    pub evidence: Vec<u8>,
}
impl fmt::Debug for Attestation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Attestation")
            .field("format", &self.format)
            .field("evidence_bytes", &self.evidence.len())
            .finish()
    }
}
/// An action requested of current deployment authorization.
#[non_exhaustive]
pub enum Authorization<'a> {
    /// Store a fully structurally verified executable after checking producer trust.
    RegisterGraph {
        /// Exact admitted document/profile. No node executes during registration.
        executable: &'a VerifiedExecutable<'static>,
        /// Detached evidence to verify independently of structural admission.
        attestations: &'a [Attestation],
    },
    /// Inspect a registered graph's definitions and checked plans.
    InspectGraph {
        /// Exact requested graph identity.
        fingerprint: Digest,
    },
    /// Read an accepted value under its immutable protection/provenance.
    ReadArtifact {
        /// Verified accepted metadata, before loading value bytes.
        metadata: &'a ArtifactMetadata,
    },
    /// Start work under the exact registered graph and requested ceilings.
    StartRun {
        /// Selected registered executable.
        fingerprint: Digest,
        /// Normalized caller ceilings; an empty record inherits trusted defaults.
        limits: &'a htlk_executable::ExecutionLimits,
    },
}
/// Trusted host policy. Implementations authenticate evidence and evaluate grants;
/// ordinary graph data cannot implement or replace this policy. Registration trust
/// never permits invalid bytes to bypass complete independent verification.
pub trait RuntimePolicy: Send + Sync {
    /// Authorizes an authenticated principal's exact action. Registration must also
    /// enforce any required detached-signature/producer policy here.
    ///
    /// # Errors
    /// Returns a fixed redacted policy failure.
    fn authorize(
        &self,
        context: &HostContext,
        action: Authorization<'_>,
    ) -> Result<(), RuntimeFailure>;
    /// Assigns trusted ingress/boundary labels when run inputs are admitted. None
    /// represents actual absence, not null. Values cannot assign their own
    /// protection through fields such as `security_labels`.
    ///
    /// # Errors
    /// Returns a redacted policy failure.
    fn input_labels(
        &self,
        context: &HostContext,
        fingerprint: Digest,
        port: &Identifier,
        value: Option<&Value>,
    ) -> Result<SecurityLabels, RuntimeFailure>;
    /// Additional trusted principal ceilings. Omission inherits; it does not grant
    /// effects or replace authorize. Hard external limits still require enforcement.
    ///
    /// # Errors
    /// Returns a redacted current-policy failure.
    fn execution_limits(
        &self,
        _context: &HostContext,
    ) -> Result<htlk_executable::ExecutionLimits, RuntimeFailure> {
        Ok(htlk_executable::ExecutionLimits::new())
    }
}
/// Trusted service admission/cache ceilings. Per-run execution limits are separate.
#[derive(Clone, Debug)]
pub struct RuntimeConfig {
    /// Complete canonical executable/metadata bounds.
    pub limits: Limits,
    /// Maximum cached admitted executables; zero disables the cache. Individual
    /// analysis bounds remain in force, without promising exact process heap usage.
    pub max_cached_executables: usize,
    /// Trusted deployment ceilings in addition to the selected profile/run graph.
    pub execution_limits: htlk_executable::ExecutionLimits,
    /// Advertised host receipt lifetime in milliseconds, positive and finite.
    pub idempotency_retention_ms: u64,
}
impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            limits: Limits::default(),
            max_cached_executables: 16,
            execution_limits: htlk_executable::ExecutionLimits::new(),
            idempotency_retention_ms: 24 * 60 * 60 * 1000,
        }
    }
}
/// Logical immutable graph-registration result; provenance is stored separately.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphRegistration {
    /// Exact canonical envelope fingerprint.
    pub fingerprint: Digest,
    /// Authored canonical graph ID.
    pub graph_id: String,
}
/// Runtime host service, retaining immutable native implementations and current
/// trusted policy. Registration executes no graph work. Execution consumers build
/// on the same admitted-cache/storage boundary.
pub struct Runtime {
    pub(crate) store: SqliteStore,
    pub(crate) registry: Arc<NativeRegistry>,
    pub(crate) policy: Arc<dyn RuntimePolicy>,
    pub(crate) config: RuntimeConfig,
    pub(crate) clock: Arc<dyn CoordinatorClock>,
    cache: Mutex<BTreeMap<Digest, Arc<VerifiedExecutable<'static>>>>,
}
impl Runtime {
    /// Constructs a service from trusted store, immutable registry and host policy.
    ///
    /// # Errors
    /// Returns unsupported codec configuration or cache limits.
    pub fn new(
        store: SqliteStore,
        registry: Arc<NativeRegistry>,
        policy: Arc<dyn RuntimePolicy>,
        config: RuntimeConfig,
    ) -> Result<Self, RuntimeError> {
        config.limits.validate()?;
        config
            .execution_limits
            .to_value(&config.limits)
            .map_err(|_| RuntimeError::failure(FailureCode::Profile))?;
        if config.max_cached_executables > 1024
            || config.idempotency_retention_ms == 0
            || config.idempotency_retention_ms > i64::MAX as u64
        {
            return Err(RuntimeError::failure(FailureCode::Profile));
        }
        Ok(Self {
            store,
            registry,
            policy,
            config,
            clock: Arc::new(SystemClock),
            cache: Mutex::new(BTreeMap::new()),
        })
    }
    /// Selects an explicit trusted clock before sharing the service with actors.
    /// Durable timestamp normalization remains enforced by the store.
    pub fn with_clock(mut self, clock: Arc<dyn CoordinatorClock>) -> Self {
        self.clock = clock;
        self
    }
    /// Borrows trusted service settings, including the receipt retention bound
    /// that an embedding transport must advertise to its callers.
    pub fn config(&self) -> &RuntimeConfig {
        &self.config
    }
    /// Independently verifies authoritative envelope bytes, authorizes registration
    /// and producer evidence, and commits the exact immutable envelope plus detached
    /// provenance. Identical bytes are idempotent; equal fingerprint/different bytes
    /// is an integrity failure. Compiler analysis never substitutes for this check.
    ///
    /// # Errors
    /// Returns structural/profile/schema admission, policy, representation or storage
    /// failures. No authoritative graph/provenance is saved on verification/denial.
    pub async fn register_graph(
        &self,
        context: &HostContext,
        bytes: &[u8],
        attestations: &[Attestation],
    ) -> Result<GraphRegistration, RuntimeError> {
        let evidence = attestation_value(context, attestations, &self.config.limits)?;
        let admitted = Arc::new(verify_executable_owned(
            bytes,
            Arc::clone(&self.registry),
            &self.config.limits,
        )?);
        self.policy.authorize(
            context,
            Authorization::RegisterGraph {
                executable: &admitted,
                attestations,
            },
        )?;
        let result = GraphRegistration {
            fingerprint: admitted.fingerprint(),
            graph_id: admitted.document().fields().graph_id.clone(),
        };
        self.store.store_graph(&admitted, bytes, &evidence).await?;
        self.cache(admitted)?;
        Ok(result)
    }
    /// Authorizes graph inspection and loads/re-admits its saved envelope against
    /// actual linked native implementations. A cache entry is immutable admitted
    /// data, never serialized compiler proof. Current authorization is checked even
    /// on cache hits.
    ///
    /// # Errors
    /// Returns current policy denial, missing/corrupt registration, changed profile
    /// or complete independent admission failure.
    pub async fn executable(
        &self,
        context: &HostContext,
        fingerprint: Digest,
    ) -> Result<Arc<VerifiedExecutable<'static>>, RuntimeError> {
        self.policy
            .authorize(context, Authorization::InspectGraph { fingerprint })?;
        self.admitted(fingerprint).await
    }
    pub(crate) async fn admitted(
        &self,
        fingerprint: Digest,
    ) -> Result<Arc<VerifiedExecutable<'static>>, RuntimeError> {
        if let Some(cached) = self
            .cache
            .lock()
            .map_err(|_| RuntimeError::failure(FailureCode::Invariant))?
            .get(&fingerprint)
            .cloned()
        {
            return Ok(cached);
        }
        let bytes = self.store.graph_bytes(fingerprint).await.map_err(|e| {
            if e == StoreError::Missing {
                Self::missing_graph()
            } else {
                RuntimeError::from(e)
            }
        })?;
        let admitted = Arc::new(verify_executable_owned(
            &bytes,
            Arc::clone(&self.registry),
            &self.config.limits,
        )?);
        if admitted.fingerprint() != fingerprint {
            return Err(RuntimeError::failure(FailureCode::Digest));
        }
        self.cache(Arc::clone(&admitted))?;
        Ok(admitted)
    }
    /// Authorizes accepted artifact metadata before loading its immutable bytes,
    /// then checks raw content and original typed/provenance identities. A staged
    /// blob without committed metadata is not readable through this API.
    ///
    /// # Errors
    /// Returns missing/retained data, current policy denial or integrity failures.
    pub async fn read_artifact(
        &self,
        context: &HostContext,
        id: ArtifactId,
    ) -> Result<AuthorizedValue, RuntimeError> {
        let metadata = self.store.artifact_metadata(id).await.map_err(|e| {
            if e == StoreError::Missing {
                RuntimeError::failure(FailureCode::Input)
            } else {
                RuntimeError::from(e)
            }
        })?;
        self.policy.authorize(
            context,
            Authorization::ReadArtifact {
                metadata: &metadata,
            },
        )?;
        metadata.to_value(&self.config.limits)?;
        let bytes = self
            .store
            .artifact_bytes(metadata.storage_reference())
            .await?;
        let value = metadata.verify_value(&bytes, &self.config.limits)?;
        Ok(AuthorizedValue { metadata, value })
    }
    fn cache(&self, executable: Arc<VerifiedExecutable<'static>>) -> Result<(), RuntimeError> {
        if self.config.max_cached_executables == 0 {
            return Ok(());
        }
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| RuntimeError::failure(FailureCode::Invariant))?;
        if !cache.contains_key(&executable.fingerprint())
            && cache.len() >= self.config.max_cached_executables
        {
            cache.pop_first();
        }
        cache.insert(executable.fingerprint(), executable);
        Ok(())
    }
    fn missing_graph() -> RuntimeError {
        RuntimeError::failure(FailureCode::Executable)
    }
}
fn attestation_value(
    context: &HostContext,
    attestations: &[Attestation],
    limits: &Limits,
) -> Result<Value, RuntimeError> {
    let mut accounting = RecordAccounting::new(limits)?;
    accounting
        .collection(2, 0)
        .map_err(|_| RuntimeError::failure(FailureCode::Input))?;
    accounting
        .text(context.principal(), 1)
        .map_err(|_| RuntimeError::failure(FailureCode::Input))?;
    accounting
        .collection(attestations.len(), 1)
        .map_err(|_| RuntimeError::failure(FailureCode::Input))?;
    let mut values = Vec::new();
    for attestation in attestations {
        if attestation.evidence.len() > limits.max_document_bytes
            || attestation.format.len() > limits.max_document_bytes
        {
            return Err(RuntimeError::failure(FailureCode::Input));
        }
        let value = Value::Map(Map::try_from_entries([
            ("format".into(), Value::Text(attestation.format.clone())),
            (
                "evidence".into(),
                Value::Bytes(attestation.evidence.clone()),
            ),
        ])?);
        accounting
            .value(&value, 2)
            .map_err(|_| RuntimeError::failure(FailureCode::Input))?;
        values.push(value);
    }
    Ok(Value::Array(vec![
        Value::Text(context.principal().into()),
        Value::Array(values),
    ]))
}

/// Runtime boundary failure with stable public classification and structured
/// redacted stage causes. No submitted executable or value bytes are retained.
#[derive(Debug)]
#[non_exhaustive]
pub enum RuntimeError {
    /// Complete native admission failed.
    Verification(Box<ExecutableVerificationError>),
    /// Trusted storage operation failed.
    Storage(StoreError),
    /// Stable policy/execution/ingress failure.
    Failure(RuntimeFailure),
    /// Runtime record representation failed.
    Record(RuntimeRecordError),
    /// Proposed or stored artifact failed value/metadata integrity checks.
    Artifact(Box<ArtifactError>),
}
impl RuntimeError {
    pub(crate) fn failure(code: FailureCode) -> Self {
        Self::Failure(RuntimeFailure::new(code))
    }
    /// Stable error classification, independent of backend text.
    pub fn code(&self) -> FailureCode {
        match self {
            Self::Failure(f) => f.code(),
            Self::Storage(e) => e.code(),
            Self::Record(RuntimeRecordError::Integrity) => FailureCode::Digest,
            Self::Record(_) => FailureCode::Input,
            Self::Artifact(error) => error.code(FailureBoundary::Input),
            Self::Verification(error) => match error.as_ref() {
                ExecutableVerificationError::Envelope(
                    htlk_executable::EnvelopeError::FingerprintMismatch,
                ) => FailureCode::Digest,
                ExecutableVerificationError::Document(error)
                    if matches!(
                        error.as_ref(),
                        htlk_executable::DocumentError::DigestMismatch(_)
                    ) =>
                {
                    FailureCode::Digest
                }
                ExecutableVerificationError::Registry(_)
                | ExecutableVerificationError::Policy(_) => FailureCode::Profile,
                _ => FailureCode::Executable,
            },
        }
    }
}
impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code(), self.code().message())
    }
}
impl std::error::Error for RuntimeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Verification(e) => Some(e.as_ref()),
            Self::Storage(e) => Some(e),
            Self::Failure(e) => Some(e),
            Self::Record(e) => Some(e),
            Self::Artifact(e) => Some(e.as_ref()),
        }
    }
}
impl From<ExecutableVerificationError> for RuntimeError {
    fn from(e: ExecutableVerificationError) -> Self {
        Self::Verification(Box::new(e))
    }
}
impl From<StoreError> for RuntimeError {
    fn from(e: StoreError) -> Self {
        Self::Storage(e)
    }
}
impl From<RuntimeFailure> for RuntimeError {
    fn from(e: RuntimeFailure) -> Self {
        Self::Failure(e)
    }
}
impl From<RuntimeRecordError> for RuntimeError {
    fn from(e: RuntimeRecordError) -> Self {
        Self::Record(e)
    }
}
impl From<cbor::Error> for RuntimeError {
    fn from(e: cbor::Error) -> Self {
        Self::Record(RuntimeRecordError::Codec(e))
    }
}
impl From<ArtifactError> for RuntimeError {
    fn from(error: ArtifactError) -> Self {
        Self::Artifact(Box::new(error))
    }
}
