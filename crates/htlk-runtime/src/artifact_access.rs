//! Narrow authorized artifact-read capability, independent of execution services.
use crate::{
    Authorization, HostContext, RuntimeError, RuntimePolicy,
    artifact::AuthorizedValue,
    failure::FailureCode,
    identity::ArtifactId,
    store::{SqliteStore, StoreError},
};
use htlk_executable::cbor::Limits;
use std::sync::Arc;

pub(crate) struct ArtifactReader {
    store: SqliteStore,
    policy: Arc<dyn RuntimePolicy>,
    limits: Limits,
}
impl ArtifactReader {
    pub(crate) fn new(store: SqliteStore, policy: Arc<dyn RuntimePolicy>, limits: Limits) -> Self {
        Self {
            store,
            policy,
            limits,
        }
    }
    pub(crate) async fn read(
        &self,
        context: &HostContext,
        id: ArtifactId,
    ) -> Result<AuthorizedValue, RuntimeError> {
        let metadata = self.store.artifact_metadata(id).await.map_err(|error| {
            if error == StoreError::Missing {
                RuntimeError::failure(FailureCode::Input)
            } else {
                error.into()
            }
        })?;
        self.policy.authorize(
            context,
            Authorization::ReadArtifact {
                metadata: &metadata,
            },
        )?;
        metadata.to_value(&self.limits)?;
        let bytes = self
            .store
            .artifact_bytes(id, metadata.storage_reference())
            .await?;
        let value = metadata.verify_value(&bytes, &self.limits)?;
        Ok(AuthorizedValue { metadata, value })
    }
}
