//! Read-only, pinned runtime inspection without an engine or native registry.
//!
//! The host establishes [`InspectionContext`] and implements [`InspectionPolicy`].
//! Discovery, structure, and content are separate current decisions. Every page
//! rechecks policy; references and continuations are identities, never grants.
//! Structural results contain an allowlist of metadata, not runtime payloads.
//! Use [`Inspector::read_content`] for embedded values, raw journal data, or blobs.
//!
//! Snapshots cover the storage inspection domain: headers, runtime records
//! (including recorded MCP observations), artifact metadata/expiry, and journal
//! bounds. Independently mutable MCP adjuncts and source attachment inventories
//! are excluded. A pin is neither a byte-retention hold nor frozen authorization.
//! Definition navigation checks canonical envelopes, but performs no native
//! admission and makes no claim to provide analyzed dependency information.
//!
//! Candidate pages are capped at 256 and storage ceilings. Allowlisted response
//! metadata is byte-accounted under the store codec ceiling; [`InspectionError::Limit`]
//! asks the caller to reduce its candidate page size without skipping results.
//! Selected content is capped at 1 MiB and is never truncated. Reference/cursor
//! conversions are additionally capped at 16 KiB. A host can wrap these versioned
//! bytes in its own transport encoding; there are no public serde models.

use crate::{
    HostContext,
    artifact::{ArtifactMetadata, AuthorizedValue},
    identity::RunId,
    records::{RunStatus, SecurityLabels},
    store::{
        InspectionRunFilter, InspectionSnapshot, JournalEntry, JournalEvent, RecordFamily,
        RecordKey, RunHeader, Store, StoreError, StorePosition, StoredRecord,
    },
};
use htlk_executable::{
    CanonicalDocument, ExecutableEnvelope, Identifier, JsonPointer,
    cbor::{self, Limits, Value},
    digest::{Digest, hash_bytes},
};
use std::{fmt, sync::Arc};

#[path = "inspection_navigation.rs"]
mod navigation;
#[path = "inspection_tokens.rs"]
mod tokens;

/// Trusted host identity and optional executing run. Never populate this from
/// untrusted inspection arguments. A caller run enables [`RunSelector::Caller`]
/// but does not bypass any policy check.
#[derive(Clone, Debug)]
pub struct InspectionContext {
    host: HostContext,
    caller_run: Option<RunId>,
}
impl InspectionContext {
    /// Retains host-established context; construction does not authenticate it.
    pub fn new(host: HostContext, caller_run: Option<RunId>) -> Self {
        Self { host, caller_run }
    }
    /// Authenticated identity established by the embedding application.
    pub fn host(&self) -> &HostContext {
        &self.host
    }
    /// Host-established executing run, if any.
    pub fn caller_run(&self) -> Option<&RunId> {
        self.caller_run.as_ref()
    }
}

/// Explicit target selection. Self identity comes only from trusted context.
#[derive(Clone, Debug)]
pub enum RunSelector {
    /// Resolve the executing run supplied by the host.
    Caller,
    /// Select an exact store run; possession grants no access.
    Id(RunId),
}

/// Canonical definition or recorded runtime identity within a pinned run.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum EntityId {
    /// Run header.
    Run,
    /// Immutable executable envelope fingerprint.
    Graph(Digest),
    /// Original reusable scope definition.
    ScopeDefinition {
        /// Original executable.
        executable: Digest,
        /// Canonical scope identity.
        scope: Digest,
    },
    /// Original node occurrence within a reusable definition.
    NodeDefinition {
        /// Original executable.
        executable: Digest,
        /// Containing scope definition.
        scope: Digest,
        /// Source-local node identity.
        node: Identifier,
    },
    /// Authored dataflow edge, distinct from a recorded selection.
    EdgeDefinition {
        /// Original executable.
        executable: Digest,
        /// Containing scope definition.
        scope: Digest,
        /// Source-local edge identity.
        edge: Identifier,
    },
    /// Any authoritative runtime family, including scopes, invocations, attempts,
    /// waits, artifacts, decisions, extensions, and recorded MCP observations.
    Record(RecordKey),
    /// Retained committed journal event, identified by per-run sequence.
    Event(i64),
}

/// One category per entity-list request; no complete-run aggregation is needed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntityKind {
    /// The run root only.
    Run,
    /// The active graph only; original graphs remain reachable through instances.
    Graph,
    /// Scope/node/edge definitions in the active bounded canonical document.
    Definitions,
    /// One runtime record family in stable insertion order.
    Records(RecordFamily),
    /// Events in committed journal order.
    Events,
}

/// Transportable pinned identity. Fields are private so local callers cannot
/// retarget an issued reference accidentally. Decoding is bounded representation
/// validation only; [`Inspector::resolve_reference`] rechecks existence and policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reference {
    namespace: String,
    run: RunId,
    position: i64,
    entity: EntityId,
    witness: Option<RecordKey>,
}
impl Reference {
    /// Original run identity.
    pub fn run_id(&self) -> &RunId {
        &self.run
    }
    /// Exact entity selector.
    pub fn entity(&self) -> &EntityId {
        &self.entity
    }
    /// Persistent store namespace.
    pub fn namespace(&self) -> &str {
        &self.namespace
    }
    /// Committed boundary, not a journal sequence.
    pub const fn position_number(&self) -> i64 {
        self.position
    }
}

/// Immutable pinned view. Refresh returns a different view, leaving this intact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    stored: InspectionSnapshot,
}
impl Snapshot {
    /// Pinned run identity.
    pub fn run_id(&self) -> &RunId {
        &self.stored.header().run_id
    }
    /// Store-wide committed boundary.
    pub fn position(&self) -> &StorePosition {
        self.stored.position()
    }
    /// Transportable run identity at this pin.
    pub fn reference(&self) -> Reference {
        self.reference_for(EntityId::Run)
    }
    fn reference_for(&self, entity: EntityId) -> Reference {
        Reference {
            namespace: self.position().namespace().into(),
            run: self.run_id().clone(),
            position: self.position().number(),
            entity,
            witness: None,
        }
    }
}

/// Bounded continuation bound to store, pin, and exact operation selectors.
/// It is independent of every other consumer and of outbox acknowledgments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cursor {
    namespace: String,
    position: i64,
    query: Digest,
    after: i64,
}

/// A candidate scan bound, not a promise to return that many authorized matches.
#[derive(Clone, Debug)]
pub struct PageRequest {
    /// Maximum candidates examined, between one and 256 and within store limits.
    pub limit: usize,
    /// Previous continuation for exactly the same query and pin.
    pub cursor: Option<Cursor>,
}
impl PageRequest {
    /// Start a bounded scan. Validation occurs at the service boundary.
    pub const fn new(limit: usize) -> Self {
        Self {
            limit,
            cursor: None,
        }
    }
}

/// A page may be empty with a continuation when candidates were inaccessible or
/// did not match. Always continue using `next`, not the returned item count.
#[derive(Clone, Debug)]
pub struct Page<T> {
    /// Common committed upper bound.
    pub position: StorePosition,
    /// Authorized results only.
    pub items: Vec<T>,
    /// Exclusive continuation, or exhaustion. Every continuation reauthorizes.
    pub next: Option<Cursor>,
}

/// Safe discovery result, without ownership, attribution, or runtime values.
#[derive(Clone, Debug)]
pub struct RunSummary {
    /// Pinned run reference.
    pub reference: Reference,
    /// Committed lifecycle.
    pub status: RunStatus,
}

/// Protection retained from the selected source, without relabeling for a viewer.
/// Labels in record data are evidence, not grants. `artifact` retains the exact
/// accepted source type/provenance/protection for application result-label policy.
/// For nonartifact payloads the host must also interpret the selected record/event
/// supplied to its policy hook; empty labels do not assert public content.
#[derive(Clone, Debug, Default)]
pub struct Protection {
    /// Conservative union of coordinator-owned label fields and selected recorded
    /// owner protection at the same pin. Artifact labels remain exactly as accepted.
    pub labels: SecurityLabels,
    /// Exact source metadata for accepted artifact content.
    pub artifact: Option<ArtifactMetadata>,
}

/// Trusted policy input. Raw records/events are visible only to the host hook,
/// never to structural results. Hosts can use their own per-entity grants or
/// interpret protected embedded data without adding an authorization service here.
pub struct InspectionTarget<'a> {
    /// Selected immutable identity.
    pub reference: &'a Reference,
    /// Historical header, for trusted policy evaluation only.
    pub header: &'a RunHeader,
    /// Selected source protection.
    pub protection: &'a Protection,
    /// Selected record, if applicable; contains protected values.
    pub record: Option<&'a StoredRecord>,
    /// Selected event, if applicable; contains protected raw writes.
    pub event: Option<&'a JournalEntry>,
}

/// Identity-only discovery input, checked before interpreting selected payloads.
/// The application owns its entity/run discovery index and grants. Structural and
/// content checks receive source protection through [`InspectionTarget`].
pub struct DiscoveryTarget<'a> {
    /// Selected immutable identity.
    pub reference: &'a Reference,
    /// Historical header for trusted policy evaluation only.
    pub header: &'a RunHeader,
}

/// Application-owned current grants. No default allow policy is supplied.
/// Hooks must be safe to call repeatedly and must not infer a grant from possession
/// of a reference. Discovery denial omits candidates from pages.
pub trait InspectionPolicy: Send + Sync {
    /// Permit disclosure that the selected run/entity exists.
    fn discover(&self, context: &InspectionContext, target: &DiscoveryTarget<'_>) -> bool;
    /// Permit allowlisted structural metadata and navigation.
    fn structure(&self, context: &InspectionContext, target: &InspectionTarget<'_>) -> bool;
    /// Permit the exact selected full payload or artifact value.
    fn content(
        &self,
        context: &InspectionContext,
        target: &InspectionTarget<'_>,
        kind: ContentKind,
    ) -> bool;
}

/// Selected content domain. Runtime payloads and journal writes require content
/// authorization just as artifact blobs do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContentKind {
    /// Full selected header, definition, record, or event (including embedded values).
    Payload,
    /// Integrity-checked value of one accepted artifact record.
    ArtifactValue,
}

/// An independently meaningful typed relationship. Authored possibilities are
/// never reported as observed reads or selections. No analyzed dependencies are
/// fabricated when original native admission is unavailable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelationKind {
    /// Graph to scopes; scope to nodes/edges.
    AuthoredChild,
    /// Edge to one explicitly selected authored endpoint definition.
    AuthoredDataflow(DataflowEnd),
    /// Scope/loop node to reusable body definition.
    DefinitionUse,
    /// Run to active root; scope to invocations; invocation to recorded child scopes.
    ExecutionChild,
    /// Recorded immediate container/owner; never reconstructed from potential edges.
    /// A response's parent is its same-key attempt; an attempt's parent is its
    /// recorded invocation. Both steps resolve and authorize the target at the pin.
    ExecutionParent,
    /// Recorded scope/invocation to original canonical definition.
    InstanceOf,
    /// Invocation to recorded attempts.
    Attempts,
    /// Invocation to recorded waits.
    Waits,
    /// Wait to its recorded request artifact, whose value requires content access.
    WaitRequest,
    /// Invocation to frozen input binding decisions.
    Bindings,
    /// Invocation to accepted output artifacts.
    Produces,
    /// Binding decision to the actually selected artifact (not every potential source).
    SelectedArtifact,
    /// Binding decision to its recorded authored edge.
    SelectedEdge,
    /// Explicit recorded causal invocation links.
    Causes,
    /// Accepted artifact to original producing invocation and input artifacts.
    Provenance,
    /// Extension record to preserved scope/wrapper and previous graph.
    Reuses,
    /// Definition or recorded instance to its original immutable graph.
    OriginExecutable,
}

/// An authored edge endpoint. Endpoint direction never implies an actual read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataflowEnd {
    /// Potential source of the authored edge.
    Source,
    /// Authored destination of the edge.
    Destination,
}

/// One currently authorized edge; endpoints retain original identity.
#[derive(Clone, Debug)]
pub struct Relation {
    /// Exact relationship category.
    pub kind: RelationKind,
    /// Authorized source.
    pub source: Reference,
    /// Authorized target.
    pub target: Reference,
}

/// Canonical identity retained through loop iteration and extension mounting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DefinitionIdentity {
    /// Original executable, independent of the run's currently active graph.
    pub executable: Digest,
    /// Canonical scope/node digest or graph fingerprint. Edges have no independent
    /// digest domain; their identity is the canonical containing scope plus local ID.
    pub digest: Option<Digest>,
    /// Original scope/node/edge selector within that executable.
    pub definition: EntityId,
    /// Exact canonical document location (empty pointer for the graph document).
    pub canonical_path: JsonPointer,
}

/// Allowlisted structural metadata. No arbitrary runtime strings, values,
/// messages, request parameters, response data, or raw write arrays are copied.
#[derive(Clone, Debug)]
pub struct EntityInspection {
    /// Pinned identity.
    pub reference: Reference,
    /// Selected source protection, for application response labeling.
    pub protection: Protection,
    /// Canonical original identity, when the entity defines or instantiates one.
    pub definition: Option<DefinitionIdentity>,
    /// Committed run lifecycle for the run entity only.
    pub run_status: Option<RunStatus>,
    /// Exact committed journal upper bound for the run entity only.
    pub sequence: Option<i64>,
    /// Record revision, if applicable.
    pub revision: Option<i64>,
    /// Record immutability, if applicable.
    pub final_record: Option<bool>,
    /// Closed, recognized lifecycle spelling only.
    pub status: Option<&'static str>,
    /// Recorded iteration number; does not alter definition identity.
    pub iteration: Option<u64>,
    /// Recorded extension generation, if applicable.
    pub extension_number: Option<u64>,
    /// Original analysis use site for a scope instance, retained across mounting.
    pub scope_use: Option<htlk_analyzer::ScopeUse>,
    /// Historical committed artifact expiry, not a prediction of current byte availability.
    pub expired_at_ms: Option<i64>,
    /// One-based recorded attempt count, for attempts only.
    pub attempt_number: Option<u64>,
    /// Recorded wait generation, for waits only.
    pub generation: Option<u64>,
    /// Recorded nonnegative absolute deadline, for scopes/invocations/waits.
    pub deadline_ms: Option<u64>,
    /// Selected event category, without its raw writes or attribution.
    pub event: Option<JournalEvent>,
}

/// History summary. The corresponding event reference can be selected separately
/// with `ContentKind::Payload` to request its full protected journal entry.
#[derive(Clone, Debug)]
pub struct HistoryEntry {
    /// Selected event identity at the history pin.
    pub reference: Reference,
    /// Per-run committed sequence, not an interior store position.
    pub sequence: i64,
    /// Closed event category.
    pub event: JournalEvent,
    /// Authoritative committed timestamp.
    pub timestamp_ms: i64,
    /// Selected event's explicit source protection.
    pub protection: Protection,
}

/// A store-wide committed change after current discovery and structure checks.
#[derive(Clone, Debug)]
pub struct Change {
    /// Exact changed entity at its actual commit boundary.
    pub reference: Reference,
    /// Actual store-wide commit boundary, suitable for opening a snapshot.
    pub position: StorePosition,
    /// Independent exclusive feed ordinal.
    pub cursor: i64,
    /// Final per-run journal sequence at that transaction boundary.
    pub sequence: i64,
}

/// Independent live feed offset. Unlike page cursors, this deliberately has no
/// frozen upper bound: each changes call captures a fresh bound unless supplied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeCursor {
    namespace: String,
    after: i64,
}

/// Bounded independent feed result; `next` advances past examined denied entries.
#[derive(Clone, Debug)]
pub struct ChangePage {
    /// Commit boundary captured for this call.
    pub through: StorePosition,
    /// Authorized change metadata only.
    pub items: Vec<Change>,
    /// Resume from here, including when no results were visible.
    pub next: ChangeCursor,
}

/// Full selected typed data; never returned by structural methods.
#[derive(Clone, Debug)]
pub enum ContentValue {
    /// Full protected run header.
    Run(RunHeader),
    /// Complete canonical definition.
    Definition(Value),
    /// Full selected record, including embedded runtime values.
    Record(StoredRecord),
    /// Complete raw event including writes and attribution.
    Event(JournalEntry),
    /// Source-type integrity-checked accepted artifact value.
    Artifact(AuthorizedValue),
}

/// Explicit selected content availability. Pins do not keep bytes alive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnavailableReason {
    /// Current or historical committed expiry.
    Expired,
    /// Selected bytes are no longer retained.
    NotRetained,
    /// Full content exceeds the requested byte bound; never truncated.
    TooLarge,
}

/// Authorized content with its unchanged source protection, or explicit absence.
#[derive(Clone, Debug)]
pub enum ContentResult {
    /// Full selected value under the byte budget.
    Available(Box<ProtectedContent>),
    /// Selected content cannot presently be returned after authorization.
    Unavailable(UnavailableReason),
}

/// Full authorized selected payload with its original source protection.
#[derive(Clone, Debug)]
pub struct ProtectedContent {
    /// Selected typed payload.
    pub value: ContentValue,
    /// Original protection for host result-label policy.
    pub protection: Protection,
}

/// Inspection failures contain no protected payloads or database diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InspectionError {
    /// Current policy denied this exact operation.
    Denied,
    /// Selected identity is absent at this pin.
    Unknown,
    /// Foreign store, wrong snapshot/entity, or malformed reference.
    InvalidReference,
    /// Continuation belongs to a different query, store, or pin.
    InvalidCursor,
    /// Invalid bounds, selector, or missing trusted self context.
    InvalidRequest,
    /// Allowlisted response metadata exceeds the store codec byte ceiling. Reduce
    /// the candidate page size; results are never silently truncated or skipped.
    Limit,
    /// Retained facts violate canonical identity or schema invariants.
    Corrupt,
    /// Backend operational failure, without backend diagnostics.
    Storage(StoreError),
}
impl fmt::Display for InspectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Denied => "inspection access denied",
            Self::Unknown => "inspection identity not found",
            Self::InvalidReference => "invalid inspection reference",
            Self::InvalidCursor => "invalid inspection continuation",
            Self::InvalidRequest => "invalid inspection request",
            Self::Limit => "inspection response exceeds byte limit",
            Self::Corrupt => "corrupt inspection data",
            Self::Storage(_) => "inspection storage failure",
        })
    }
}
impl std::error::Error for InspectionError {}
impl From<StoreError> for InspectionError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::Missing => Self::Unknown,
            StoreError::Corrupt => Self::Corrupt,
            StoreError::Invalid => Self::InvalidRequest,
            _ => Self::Storage(e),
        }
    }
}
type Result<T> = std::result::Result<T, InspectionError>;

/// Narrow read-only service. Owns only a store handle and application policy;
/// construction neither attaches an engine nor admits/links native operations.
#[derive(Clone)]
pub struct Inspector {
    store: Store,
    policy: Arc<dyn InspectionPolicy>,
}

struct Selected {
    record: Option<StoredRecord>,
    event: Option<JournalEntry>,
    definition: Option<Value>,
    identity: Option<DefinitionIdentity>,
    protection: Protection,
    expired_at_ms: Option<i64>,
}
impl Selected {
    fn target<'a>(
        &'a self,
        snapshot: &'a Snapshot,
        reference: &'a Reference,
    ) -> InspectionTarget<'a> {
        InspectionTarget {
            reference,
            header: snapshot.stored.header(),
            protection: &self.protection,
            record: self.record.as_ref(),
            event: self.event.as_ref(),
        }
    }
}

impl Inspector {
    /// Use the store's validated codec/page ceilings, additionally capping public
    /// candidate pages at 256 and selected content requests at 1 MiB.
    pub fn new(store: Store, policy: Arc<dyn InspectionPolicy>) -> Self {
        Self { store, policy }
    }

    /// Resolve a transported store position, rejecting another store's namespace.
    /// A resolved position is an identity only and discloses no run data.
    ///
    /// # Errors
    /// Returns invalid namespace/position or storage failure.
    pub async fn resolve_position(&self, namespace: &str, number: i64) -> Result<StorePosition> {
        let position = self.store.inspection_position().await?;
        if position.namespace() != namespace {
            return Err(InspectionError::InvalidReference);
        }
        if position.number() == number {
            Ok(position)
        } else {
            Ok(self.store.inspection_position_at(number).await?)
        }
    }

    /// List/filter historical runs, examining at most the requested candidates.
    /// The first page captures a pin; continuations retain that exact boundary.
    ///
    /// # Errors
    /// Returns invalid filters/bounds/continuation or storage failure.
    pub async fn list_runs(
        &self,
        context: &InspectionContext,
        filter: InspectionRunFilter,
        page: PageRequest,
    ) -> Result<Page<RunSummary>> {
        self.limit(page.limit)?;
        let query = tokens::run_query(&filter, self.limits())?;
        let pin = self.page_pin(page.cursor.as_ref()).await?;
        let after = self.after(&page, &pin, query)?;
        let result = self
            .store
            .inspection_runs(Some(&pin), filter, after, page.limit)
            .await?;
        let mut items = Vec::new();
        let mut bytes = 256;
        for header in result.runs {
            let snapshot = Snapshot {
                stored: self
                    .store
                    .open_inspection_snapshot(&header.run_id, Some(&pin))
                    .await?,
            };
            let reference = snapshot.reference();
            if self.run_visible(context, &snapshot) {
                self.charge(
                    &mut bytes,
                    tokens::run_summary_value(&reference, header.status),
                )?;
                items.push(RunSummary {
                    reference,
                    status: header.status,
                });
            }
        }
        Ok(Page {
            position: pin.clone(),
            items,
            next: result.next.map(|n| self.cursor(&pin, query, n)),
        })
    }

    /// Open latest committed or retained historical facts, including an active
    /// caller run. Only the host-established context can select `Caller`.
    ///
    /// # Errors
    /// Returns denial, unknown run, missing caller context, foreign pin, or storage failure.
    pub async fn open_snapshot(
        &self,
        context: &InspectionContext,
        run: RunSelector,
        at: Option<&StorePosition>,
    ) -> Result<Snapshot> {
        let run = match run {
            RunSelector::Caller => context
                .caller_run()
                .ok_or(InspectionError::InvalidRequest)?
                .clone(),
            RunSelector::Id(run) => run,
        };
        let snapshot = Snapshot {
            stored: self.store.open_inspection_snapshot(&run, at).await?,
        };
        if !self.run_visible(context, &snapshot) {
            return Err(InspectionError::Denied);
        }
        Ok(snapshot)
    }

    /// Capture a new latest view after checking the old pin and current grants.
    ///
    /// # Errors
    /// Returns invalid foreign snapshot, denial, unknown run, or storage failure.
    pub async fn refresh_snapshot(
        &self,
        context: &InspectionContext,
        snapshot: &Snapshot,
    ) -> Result<Snapshot> {
        self.check_snapshot(context, snapshot).await?;
        self.open_snapshot(context, RunSelector::Id(snapshot.run_id().clone()), None)
            .await
    }

    /// Select one typed identity at a pin, checking current discovery and structure.
    /// This is also the safe constructor for references from host-held typed IDs.
    ///
    /// # Errors
    /// Returns denial, unknown entity, invalid pin/selector, corruption, or storage failure.
    pub async fn select(
        &self,
        context: &InspectionContext,
        snapshot: &Snapshot,
        entity: EntityId,
    ) -> Result<EntityInspection> {
        self.inspect_entity(context, snapshot, &snapshot.reference_for(entity))
            .await
    }

    /// Resolve a transported reference at its original pin under current policy.
    ///
    /// # Errors
    /// Returns foreign/forged reference, denial, unknown identity, corruption, or storage failure.
    pub async fn resolve_reference(
        &self,
        context: &InspectionContext,
        reference: &Reference,
    ) -> Result<(Snapshot, EntityInspection)> {
        let pin = self
            .resolve_position(&reference.namespace, reference.position)
            .await?;
        let snapshot = self
            .open_snapshot(context, RunSelector::Id(reference.run.clone()), Some(&pin))
            .await?;
        let entity = self.inspect_entity(context, &snapshot, reference).await?;
        Ok((snapshot, entity))
    }

    /// Inspect allowlisted metadata only. This never reads artifact value bytes.
    ///
    /// # Errors
    /// Returns denial, mismatched pin/reference, unknown identity, corruption, or storage failure.
    pub async fn inspect_entity(
        &self,
        context: &InspectionContext,
        snapshot: &Snapshot,
        reference: &Reference,
    ) -> Result<EntityInspection> {
        self.check_reference(context, snapshot, reference).await?;
        self.check_discovery(context, snapshot, reference)?;
        let selected = self.load(snapshot, reference).await?;
        self.authorize_structure(context, snapshot, reference, &selected)?;
        let summary = self.summary(snapshot, reference.clone(), &selected);
        self.charge(&mut 0, tokens::inspection_value(&summary, self.limits())?)?;
        Ok(summary)
    }

    /// Page one entity family or the bounded decoded active definition document.
    /// Denied discovery/structure candidates are omitted without copying payloads.
    ///
    /// # Errors
    /// Returns invalid pin/bounds/continuation, denial of run discovery, corruption, or storage failure.
    pub async fn list_entities(
        &self,
        context: &InspectionContext,
        snapshot: &Snapshot,
        kind: EntityKind,
        page: PageRequest,
    ) -> Result<Page<EntityInspection>> {
        self.check_snapshot(context, snapshot).await?;
        self.limit(page.limit)?;
        let query = tokens::entity_query(&snapshot.reference(), &kind, self.limits())?;
        let after = self.after(&page, snapshot.position(), query)?;
        let (references, next) = self.candidates(snapshot, &kind, after, page.limit).await?;
        let mut items = Vec::new();
        let mut bytes = 256;
        for reference in references {
            if self.check_discovery(context, snapshot, &reference).is_err() {
                continue;
            }
            let selected = self.load(snapshot, &reference).await?;
            if self
                .authorize_structure(context, snapshot, &reference, &selected)
                .is_ok()
            {
                let summary = self.summary(snapshot, reference, &selected);
                self.charge(
                    &mut bytes,
                    tokens::inspection_value(&summary, self.limits())?,
                )?;
                items.push(summary);
            }
        }
        Ok(Page {
            position: snapshot.position().clone(),
            items,
            next: next.map(|n| self.cursor(snapshot.position(), query, n)),
        })
    }

    /// Follow exactly one authored or recorded relation. Candidate scans and
    /// direct-edge pages are bounded, reauthorize both endpoints, and never fetch blobs.
    ///
    /// # Errors
    /// Returns denial, invalid/mismatched reference/continuation, unknown identity, corruption, or storage failure.
    pub async fn follow_relations(
        &self,
        context: &InspectionContext,
        snapshot: &Snapshot,
        source: &Reference,
        relation: RelationKind,
        page: PageRequest,
    ) -> Result<Page<Relation>> {
        self.check_reference(context, snapshot, source).await?;
        self.limit(page.limit)?;
        self.check_discovery(context, snapshot, source)?;
        let selected = self.load(snapshot, source).await?;
        self.authorize_structure(context, snapshot, source, &selected)?;
        let query = tokens::relation_query(source, relation, self.limits())?;
        let after = self.after(&page, snapshot.position(), query)?;
        let (targets, next) = self
            .related(snapshot, source, &selected, relation, after, page.limit)
            .await?;
        let mut items = Vec::new();
        let mut bytes = 256;
        for target in targets {
            let target_snapshot = if target.run_id() == snapshot.run_id() {
                snapshot.clone()
            } else {
                match self
                    .open_snapshot(
                        context,
                        RunSelector::Id(target.run.clone()),
                        Some(snapshot.position()),
                    )
                    .await
                {
                    Err(InspectionError::Denied | InspectionError::Unknown) => continue,
                    result => result?,
                }
            };
            if self
                .check_discovery(context, &target_snapshot, &target)
                .is_err()
            {
                continue;
            }
            let selected = match self.load(&target_snapshot, &target).await {
                Err(InspectionError::Unknown) => continue,
                result => result?,
            };
            if self
                .authorize_structure(context, &target_snapshot, &target, &selected)
                .is_ok()
            {
                self.charge(
                    &mut bytes,
                    tokens::relation_value(source, &target, relation),
                )?;
                items.push(Relation {
                    kind: relation,
                    source: source.clone(),
                    target,
                });
            }
        }
        Ok(Page {
            position: snapshot.position().clone(),
            items,
            next: next.map(|n| self.cursor(snapshot.position(), query, n)),
        })
    }

    /// Read exactly one full selected payload or accepted artifact value. Discovery
    /// and content are checked before blob access; structure permission is independent.
    /// `max_bytes` bounds canonical encoded content, including record/event wrappers.
    ///
    /// # Errors
    /// Returns denial, invalid selection/byte bound/reference, unknown entity, corruption, or storage failure.
    pub async fn read_content(
        &self,
        context: &InspectionContext,
        snapshot: &Snapshot,
        reference: &Reference,
        kind: ContentKind,
        max_bytes: usize,
    ) -> Result<ContentResult> {
        if max_bytes == 0 || max_bytes > self.limits().max_document_bytes.min(1_048_576) {
            return Err(InspectionError::InvalidRequest);
        }
        self.check_reference(context, snapshot, reference).await?;
        self.check_discovery(context, snapshot, reference)?;
        let selected = self.load(snapshot, reference).await?;
        let target = selected.target(snapshot, reference);
        self.check_discovery(context, snapshot, reference)?;
        if !self.policy.content(context, &target, kind) {
            return Err(InspectionError::Denied);
        }
        if kind == ContentKind::ArtifactValue {
            let Some(metadata) = selected.protection.artifact.as_ref() else {
                return Err(InspectionError::InvalidRequest);
            };
            let bytes = match self
                .store
                .artifact_bytes_limited(
                    metadata.artifact_id(),
                    metadata.storage_reference(),
                    max_bytes,
                )
                .await
            {
                Ok(Some(bytes)) => bytes,
                Ok(None) => return Ok(ContentResult::Unavailable(UnavailableReason::TooLarge)),
                Err(StoreError::Expired) => {
                    return Ok(ContentResult::Unavailable(UnavailableReason::Expired));
                }
                Err(StoreError::Missing) => {
                    return Ok(ContentResult::Unavailable(UnavailableReason::NotRetained));
                }
                Err(error) => return Err(error.into()),
            };
            let value = metadata
                .verify_value(&bytes, self.limits())
                .map_err(|_| InspectionError::Corrupt)?;
            return Ok(ContentResult::Available(Box::new(ProtectedContent {
                value: ContentValue::Artifact(AuthorizedValue {
                    metadata: metadata.clone(),
                    value,
                }),
                protection: selected.protection,
            })));
        }
        let (value, encoded) = if let Some(record) = selected.record {
            let encoded = tokens::record_value(&record);
            (ContentValue::Record(record), encoded)
        } else if let Some(event) = selected.event {
            let encoded = tokens::event_value(&event);
            (ContentValue::Event(event), encoded)
        } else if let Some(definition) = selected.definition {
            (ContentValue::Definition(definition.clone()), definition)
        } else {
            let header = snapshot.stored.header().clone();
            let encoded = tokens::header_value(&header);
            (ContentValue::Run(header), encoded)
        };
        let mut budget = self.limits().clone();
        budget.max_document_bytes = max_bytes;
        if cbor::encode(&encoded, &budget).is_err() {
            return Ok(ContentResult::Unavailable(UnavailableReason::TooLarge));
        }
        Ok(ContentResult::Available(Box::new(ProtectedContent {
            value,
            protection: selected.protection,
        })))
    }

    /// Page structural event summaries at the exact snapshot journal bound. Raw
    /// writes, attribution, and embedded values require a separate content call.
    ///
    /// # Errors
    /// Returns invalid pin/bounds/continuation, denial of run discovery, corruption, or storage failure.
    pub async fn read_history(
        &self,
        context: &InspectionContext,
        snapshot: &Snapshot,
        entity: Option<&Reference>,
        page: PageRequest,
    ) -> Result<Page<HistoryEntry>> {
        self.check_snapshot(context, snapshot).await?;
        self.limit(page.limit)?;
        if let Some(entity) = entity {
            self.inspect_entity(context, snapshot, entity).await?;
            if !matches!(
                entity.entity,
                EntityId::Run | EntityId::Record(_) | EntityId::Event(_)
            ) {
                return Err(InspectionError::InvalidRequest);
            }
        }
        let query = tokens::history_query(&snapshot.reference(), entity, self.limits())?;
        let after = self.after(&page, snapshot.position(), query)?;
        let events = self
            .store
            .inspection_journal(&snapshot.stored, after, page.limit)
            .await?;
        let next = events.last().map(|e| e.sequence);
        let mut items = Vec::new();
        let mut bytes = 256;
        for event in events {
            if !navigation::history_matches(&event, entity.map(|r| &r.entity))? {
                continue;
            }
            let reference = snapshot.reference_for(EntityId::Event(event.sequence));
            if self.check_discovery(context, snapshot, &reference).is_err() {
                continue;
            }
            let selected = self.selected_event(snapshot, event).await?;
            if self
                .authorize_structure(context, snapshot, &reference, &selected)
                .is_ok()
            {
                let event = selected.event.as_ref().expect("selected event");
                self.charge(
                    &mut bytes,
                    tokens::history_value(&reference, event, &selected.protection, self.limits())?,
                )?;
                items.push(HistoryEntry {
                    reference,
                    sequence: event.sequence,
                    event: event.event,
                    timestamp_ms: event.timestamp_ms,
                    protection: selected.protection,
                });
            }
        }
        Ok(Page {
            position: snapshot.position().clone(),
            items,
            next: next.map(|n| self.cursor(snapshot.position(), query, n)),
        })
    }

    /// Read an independent, resumable store-wide committed feed. Captures a fresh
    /// upper bound by default. No shared acknowledgments or run aggregation.
    ///
    /// # Errors
    /// Returns foreign cursor/pin, invalid limit, corruption, or storage failure.
    pub async fn read_changes(
        &self,
        context: &InspectionContext,
        after: Option<&ChangeCursor>,
        through: Option<&StorePosition>,
        limit: usize,
    ) -> Result<ChangePage> {
        self.limit(limit)?;
        let pin = match through {
            Some(pin) => self.resolve_position(pin.namespace(), pin.number()).await?,
            None => self.store.inspection_position().await?,
        };
        let mut ordinal = 0;
        if let Some(cursor) = after {
            if cursor.namespace != pin.namespace() || cursor.after < 0 {
                return Err(InspectionError::InvalidCursor);
            }
            ordinal = cursor.after;
        }
        let changes = self
            .store
            .inspection_changes(ordinal, Some(&pin), limit)
            .await?;
        let mut items = Vec::new();
        let mut bytes = 256;
        for change in changes {
            ordinal = change.cursor;
            let snapshot = Snapshot {
                stored: self
                    .store
                    .open_inspection_snapshot(&change.run_id, Some(&change.position))
                    .await?,
            };
            if !self.run_visible(context, &snapshot) {
                continue;
            }
            let reference =
                snapshot.reference_for(change.key.map_or(EntityId::Run, EntityId::Record));
            if self
                .check_discovery(context, &snapshot, &reference)
                .is_err()
            {
                continue;
            }
            let selected = self.load(&snapshot, &reference).await?;
            if self
                .authorize_structure(context, &snapshot, &reference, &selected)
                .is_ok()
            {
                self.charge(
                    &mut bytes,
                    tokens::change_value(
                        &reference,
                        &change.position,
                        change.cursor,
                        change.sequence,
                    ),
                )?;
                items.push(Change {
                    reference,
                    position: change.position,
                    cursor: change.cursor,
                    sequence: change.sequence,
                });
            }
        }
        Ok(ChangePage {
            through: pin.clone(),
            items,
            next: ChangeCursor {
                namespace: pin.namespace().into(),
                after: ordinal,
            },
        })
    }

    fn limits(&self) -> &Limits {
        &self.store.config().limits
    }
    fn charge(&self, bytes: &mut usize, value: Value) -> Result<()> {
        let encoded = cbor::encode(&value, self.limits()).map_err(|_| InspectionError::Limit)?;
        *bytes = bytes
            .checked_add(encoded.len())
            .ok_or(InspectionError::Limit)?;
        if *bytes > self.limits().max_document_bytes {
            return Err(InspectionError::Limit);
        }
        Ok(())
    }
    fn limit(&self, limit: usize) -> Result<()> {
        if limit == 0 || limit > self.store.config().max_page_size.min(256) {
            return Err(InspectionError::InvalidRequest);
        }
        Ok(())
    }
    fn run_visible(&self, context: &InspectionContext, snapshot: &Snapshot) -> bool {
        self.check_discovery(context, snapshot, &snapshot.reference())
            .is_ok()
    }
    fn check_discovery(
        &self,
        context: &InspectionContext,
        snapshot: &Snapshot,
        reference: &Reference,
    ) -> Result<()> {
        if !self.policy.discover(
            context,
            &DiscoveryTarget {
                reference,
                header: snapshot.stored.header(),
            },
        ) {
            return Err(InspectionError::Denied);
        }
        Ok(())
    }
    async fn check_snapshot(&self, context: &InspectionContext, snapshot: &Snapshot) -> Result<()> {
        self.resolve_position(
            snapshot.position().namespace(),
            snapshot.position().number(),
        )
        .await?;
        if !self.run_visible(context, snapshot) {
            return Err(InspectionError::Denied);
        }
        Ok(())
    }
    async fn check_reference(
        &self,
        context: &InspectionContext,
        snapshot: &Snapshot,
        reference: &Reference,
    ) -> Result<()> {
        if reference.namespace != snapshot.position().namespace()
            || reference.position != snapshot.position().number()
            || reference.run != *snapshot.run_id()
        {
            return Err(InspectionError::InvalidReference);
        }
        self.check_snapshot(context, snapshot).await
    }
    fn authorize_structure(
        &self,
        context: &InspectionContext,
        snapshot: &Snapshot,
        reference: &Reference,
        selected: &Selected,
    ) -> Result<()> {
        let target = selected.target(snapshot, reference);
        self.check_discovery(context, snapshot, reference)?;
        if !self.policy.structure(context, &target) {
            return Err(InspectionError::Denied);
        }
        Ok(())
    }
    async fn page_pin(&self, cursor: Option<&Cursor>) -> Result<StorePosition> {
        match cursor {
            Some(cursor) => self
                .resolve_position(&cursor.namespace, cursor.position)
                .await
                .map_err(|_| InspectionError::InvalidCursor),
            None => Ok(self.store.inspection_position().await?),
        }
    }
    fn after(&self, page: &PageRequest, pin: &StorePosition, query: Digest) -> Result<i64> {
        match &page.cursor {
            Some(c)
                if c.namespace == pin.namespace()
                    && c.position == pin.number()
                    && c.query == query
                    && c.after >= 0 =>
            {
                Ok(c.after)
            }
            Some(_) => Err(InspectionError::InvalidCursor),
            None => Ok(0),
        }
    }
    fn cursor(&self, pin: &StorePosition, query: Digest, after: i64) -> Cursor {
        Cursor {
            namespace: pin.namespace().into(),
            position: pin.number(),
            query,
            after,
        }
    }
    async fn document(&self, fingerprint: Digest) -> Result<CanonicalDocument> {
        let bytes = self.store.graph_bytes(fingerprint).await?;
        let envelope = ExecutableEnvelope::decode(&bytes, self.limits())
            .map_err(|_| InspectionError::Corrupt)?;
        if envelope.fingerprint() != fingerprint {
            return Err(InspectionError::Corrupt);
        }
        CanonicalDocument::from_envelope(&envelope, self.limits())
            .map_err(|_| InspectionError::Corrupt)
    }
}

#[cfg(all(test, feature = "sqlite"))]
#[path = "inspection_tests.rs"]
mod tests;
