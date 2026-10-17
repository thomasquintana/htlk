//! Explicit protected artifact expiry and bounded physical garbage collection.
use crate::{Authorization, HostContext, Runtime, RuntimeError, identity::ArtifactId};
use htlk_executable::digest::Digest;
/// Result of an authorized retention request. Protection is checked transactionally.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RetentionDisposition {
    /// Active references or the promised audit window still retain this value.
    Protected,
    /// Deliberate expiry, retaining this authoritative tombstone timestamp on retry.
    Expired {
        /// Authoritative persisted coordinator time.
        at_ms: i64,
    },
}
/// One bounded garbage-collection page. Metadata, identity and tombstones survive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GarbageCollection {
    /// Candidate byte-storage addresses examined.
    pub scanned: usize,
    /// Unreferenced byte rows removed.
    pub removed: usize,
    /// Exclusive cursor for the next page; None ends this pass.
    pub next: Option<Digest>,
}
impl Runtime {
    /// Saves an immutable expiry tombstone only after terminal/audit and active
    /// manifest/wait/checkpoint protections permit it. IDs and metadata remain.
    ///
    /// # Errors
    /// Returns authorization, unknown/corrupt metadata or storage failures.
    pub async fn expire_artifact(
        &self,
        context: &HostContext,
        id: ArtifactId,
    ) -> Result<RetentionDisposition, RuntimeError> {
        let metadata = self.store.artifact_metadata(id).await?;
        self.policy.authorize(
            context,
            Authorization::ExpireArtifact {
                metadata: &metadata,
            },
        )?;
        let now = self.store.coordinator_time(self.clock.now_ms()?).await?;
        Ok(self
            .store
            .expire_artifact(
                &metadata,
                context.principal(),
                now,
                self.config.audit_retention_ms as i64,
            )
            .await?)
    }
    /// Removes only unreferenced byte rows in one bounded cursor page. Untombstoned
    /// accepted metadata always protects shared bytes; staging-only orphans are
    /// collected only while no run is active. Publication still checks staging in
    /// its transaction, so a concurrent new admission can retry a lost upload.
    ///
    /// # Errors
    /// Returns authorization, invalid page/cursor or storage/integrity failure.
    pub async fn collect_garbage(
        &self,
        context: &HostContext,
        after: Option<Digest>,
        page_size: usize,
    ) -> Result<GarbageCollection, RuntimeError> {
        self.policy
            .authorize(context, Authorization::CollectGarbage)?;
        Ok(self.store.collect_garbage(after, page_size).await?)
    }
}
