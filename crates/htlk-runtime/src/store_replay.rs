//! Capability-free reconstruction of private journal transitions.
use super::transitions::{command_digest, next_revision};
use super::*;
use std::collections::BTreeMap;

/// Bounded journal reconstruction, with no database connection or dispatch
/// capability. This is a verification/recovery aid, not compiler admission proof.
/// Apply successive bounded pages rather than retaining the complete history.
pub struct ReplayState {
    header: RunHeader,
    records: BTreeMap<RecordKey, (StoredRecord, usize)>,
    commands: BTreeMap<String, (Digest, i64)>,
    start_receipt: Option<ReplayedStartReceipt>,
    retained_bytes: usize,
}
/// Reconstructed authenticated host start identity and retention facts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayedStartReceipt {
    /// Authenticated original principal.
    pub principal: String,
    /// Registered target graph.
    pub fingerprint: Digest,
    /// Original required idempotency key.
    pub idempotency_key: String,
    /// Complete semantic host request digest.
    pub request_digest: Digest,
    /// Advertised receipt expiry in authoritative coordinator time.
    pub expires_at_ms: i64,
}
impl ReplayState {
    /// Starts reconstruction from sequence one's creation event.
    ///
    /// # Errors
    /// Returns wrong version/schema/sequence or bounded-representation failures.
    pub fn begin(entry: &JournalEntry, limits: &Limits) -> Result<Self, StoreError> {
        if entry.sequence != 1
            || entry.event != JournalEvent::RunCreated
            || entry.record_version != "0.1"
            || entry.command_id.is_some()
        {
            return Err(StoreError::Corrupt);
        }
        timestamp(entry.timestamp_ms).map_err(|_| StoreError::Corrupt)?;
        let [fingerprint, root] = array::<2>(&entry.payload)?;
        let fingerprint = string(fingerprint)?
            .parse()
            .map_err(|_| StoreError::Corrupt)?;
        let root_scope = string(root)?.parse().map_err(|_| StoreError::Corrupt)?;
        cbor::encode(&entry.payload, limits)?;
        Ok(Self {
            header: RunHeader {
                run_id: entry.run_id.clone(),
                fingerprint,
                root_scope,
                status: RunStatus::Running,
                sequence: 1,
                owner: None,
                fence: 0,
                expires_at_ms: None,
                last_time_ms: entry.timestamp_ms,
            },
            records: BTreeMap::new(),
            commands: BTreeMap::new(),
            start_receipt: None,
            retained_bytes: 0,
        })
    }
    /// Reconstructs one subsequent committed decision. No MCP operation is invoked.
    /// Malformed events must be treated as failed recovery; discard the replay
    /// state after an error rather than continuing from a partially checked event.
    ///
    /// # Errors
    /// Returns gaps, altered ownership/revisions/final outcomes, wrong event schemas,
    /// changed command identities or bounded state/payload limits.
    pub fn apply(&mut self, entry: &JournalEntry, limits: &Limits) -> Result<(), StoreError> {
        if entry.run_id != self.header.run_id
            || entry.sequence
                != self
                    .header
                    .sequence
                    .checked_add(1)
                    .ok_or(StoreError::Corrupt)?
            || entry.record_version != "0.1"
            || entry.timestamp_ms < self.header.last_time_ms
        {
            return Err(StoreError::Corrupt);
        }
        cbor::encode(&entry.payload, limits)?;
        match entry.event {
            JournalEvent::TerminalReceipt | JournalEvent::ArtifactExpired => {
                if !self.header.status.is_terminal() {
                    return Err(StoreError::Corrupt);
                }
                let [writes, status, root, expected] = array::<4>(&entry.payload)?;
                if !matches!(
                    (status, root, expected),
                    (Value::Null, Value::Null, Value::Null)
                ) {
                    return Err(StoreError::Corrupt);
                }
                let [write] = array::<1>(writes)?;
                let [family, id, revision, final_record, payload] = array::<5>(write)?;
                let expected_family = if entry.event == JournalEvent::TerminalReceipt {
                    "reply"
                } else {
                    "tombstone"
                };
                if string(family)? != expected_family
                    || !matches!((revision, final_record), (Value::Null, Value::Bool(true)))
                    || entry.command_id.as_deref() != Some(string(id)?)
                {
                    return Err(StoreError::Corrupt);
                }
                let Value::Map(fields) = payload else {
                    return Err(StoreError::Corrupt);
                };
                if entry.event == JournalEvent::TerminalReceipt
                    && (fields.get("principal") != Some(&Value::Text(entry.actor.clone()))
                        || fields.get("method") != Some(&Value::Text(entry.causation.clone())))
                {
                    return Err(StoreError::Corrupt);
                }
                if entry.event == JournalEvent::ArtifactExpired
                    && (fields.get("artifact_id") != Some(id)
                        || fields.get("expired_at_ms") != Some(&Value::Integer(entry.timestamp_ms))
                        || fields.get("reason") != Some(&Value::Text("retention_expired".into()))
                        || entry.causation != "expire_artifact")
                {
                    return Err(StoreError::Corrupt);
                }
                if entry.event == JournalEvent::ArtifactExpired {
                    let source = self
                        .records
                        .get(&RecordKey {
                            family: RecordFamily::Artifact,
                            id: string(id)?.into(),
                        })
                        .ok_or(StoreError::Corrupt)?;
                    let metadata =
                        crate::artifact::ArtifactMetadata::from_value(&source.0.payload, limits)
                            .map_err(|_| StoreError::Corrupt)?;
                    if !source.0.final_record
                        || metadata.artifact_id().to_string() != string(id)?
                        || fields.get("influence_labels")
                            != Some(
                                &metadata
                                    .security_labels()
                                    .to_value(limits)
                                    .map_err(|_| StoreError::Corrupt)?,
                            )
                    {
                        return Err(StoreError::Corrupt);
                    }
                }
                self.apply_body(&entry.payload, limits, false)?;
            }
            JournalEvent::RunCreated => return Err(StoreError::Corrupt),
            JournalEvent::RunInitialized => self.apply_initial(entry, limits)?,
            JournalEvent::OwnershipAcquired | JournalEvent::OwnershipRenewed => {
                if self.header.status.is_terminal() || entry.command_id.is_some() {
                    return Err(StoreError::Corrupt);
                }
                let [owner, fence, expiry] = array::<3>(&entry.payload)?;
                let (owner, fence, expiry) = (string(owner)?, integer(fence)?, integer(expiry)?);
                if owner != entry.actor || expiry <= entry.timestamp_ms {
                    return Err(StoreError::Corrupt);
                }
                match entry.event {
                    JournalEvent::OwnershipAcquired => {
                        if fence
                            != self
                                .header
                                .fence
                                .checked_add(1)
                                .ok_or(StoreError::Corrupt)?
                            || (self.header.owner.is_some()
                                && self
                                    .header
                                    .expires_at_ms
                                    .is_some_and(|t| t > entry.timestamp_ms))
                        {
                            return Err(StoreError::Corrupt);
                        }
                    }
                    JournalEvent::OwnershipRenewed => {
                        let lease = RunLease {
                            run_id: entry.run_id.clone(),
                            owner: owner.into(),
                            fence,
                        };
                        check_authority(&self.header, &lease, entry.timestamp_ms)
                            .map_err(|_| StoreError::Corrupt)?;
                        if self.header.expires_at_ms.is_some_and(|old| expiry < old) {
                            return Err(StoreError::Corrupt);
                        }
                    }
                    _ => return Err(StoreError::Corrupt),
                }
                self.header.owner = Some(owner.into());
                self.header.fence = fence;
                self.header.expires_at_ms = Some(expiry);
            }
            JournalEvent::RecordsCommitted => self.apply_records(entry, limits)?,
        }
        self.header.sequence = entry.sequence;
        self.header.last_time_ms = entry.timestamp_ms;
        Ok(())
    }
    fn apply_records(&mut self, entry: &JournalEntry, limits: &Limits) -> Result<(), StoreError> {
        let [owner, fence, body] = array::<3>(&entry.payload)?;
        let lease = RunLease {
            run_id: entry.run_id.clone(),
            owner: string(owner)?.into(),
            fence: integer(fence)?,
        };
        check_authority(&self.header, &lease, entry.timestamp_ms)
            .map_err(|_| StoreError::Corrupt)?;
        if self.header.status.is_terminal() {
            return Err(StoreError::Corrupt);
        }
        let command = entry.command_id.as_ref().ok_or(StoreError::Corrupt)?;
        if self.commands.contains_key(command) {
            return Err(StoreError::Corrupt);
        }
        let digest = command_digest(&entry.run_id, &entry.actor, &entry.causation, body, limits)?;
        self.apply_body(body, limits, false)?;
        self.retained_bytes = self
            .retained_bytes
            .checked_add(command.len() + 96)
            .ok_or(StoreError::Invalid)?;
        if self.retained_bytes > limits.max_document_bytes {
            return Err(StoreError::Invalid);
        }
        self.commands
            .insert(command.clone(), (digest, entry.sequence));
        Ok(())
    }
    fn apply_initial(&mut self, entry: &JournalEntry, limits: &Limits) -> Result<(), StoreError> {
        if self.header.sequence != 1
            || self.header.owner.is_some()
            || self.header.fence != 0
            || !self.records.is_empty()
            || self.start_receipt.is_some()
        {
            return Err(StoreError::Corrupt);
        }
        let [body, host] = array::<2>(&entry.payload)?;
        let [principal, method, target, key, digest, expiry] = array::<6>(host)?;
        let (principal, method, target, key) = (
            string(principal)?,
            string(method)?,
            string(target)?,
            string(key)?,
        );
        let expires_at_ms = integer(expiry)?;
        if principal != entry.actor
            || method != "start_run"
            || entry.causation != method
            || entry.command_id.as_deref() != Some(key)
            || key.is_empty()
            || target != self.header.fingerprint.to_string()
            || expires_at_ms <= entry.timestamp_ms
        {
            return Err(StoreError::Corrupt);
        }
        let request_digest = string(digest)?.parse().map_err(|_| StoreError::Corrupt)?;
        self.apply_body(body, limits, true)?;
        let root = RecordKey {
            family: RecordFamily::Scope,
            id: self.header.root_scope.to_string(),
        };
        let scope = self.records.get(&root).ok_or(StoreError::Corrupt)?;
        let Value::Map(fields) = &scope.0.payload else {
            return Err(StoreError::Corrupt);
        };
        let expected = if self.header.status == RunStatus::Failed {
            "failed"
        } else {
            "active"
        };
        if fields.get("status") != Some(&Value::Text(expected.into()))
            || fields.get("origin_executable")
                != Some(&Value::Text(self.header.fingerprint.to_string()))
        {
            return Err(StoreError::Corrupt);
        }
        self.retained_bytes = self
            .retained_bytes
            .checked_add(principal.len() + target.len() + key.len() + 96)
            .ok_or(StoreError::Invalid)?;
        if self.retained_bytes > limits.max_document_bytes {
            return Err(StoreError::Invalid);
        }
        self.start_receipt = Some(ReplayedStartReceipt {
            principal: principal.into(),
            fingerprint: self.header.fingerprint,
            idempotency_key: key.into(),
            request_digest,
            expires_at_ms,
        });
        Ok(())
    }
    fn apply_body(
        &mut self,
        body: &Value,
        limits: &Limits,
        initial: bool,
    ) -> Result<(), StoreError> {
        let [writes, status, root, expected] = array::<4>(body)?;
        if initial
            && (!matches!(root, Value::Null)
                || !matches!(status, Value::Null | Value::Text(_))
                || matches!(expected, Value::Null))
        {
            return Err(StoreError::Corrupt);
        }
        if !matches!(expected, Value::Null)
            && string(expected)? != self.header.fingerprint.to_string()
        {
            return Err(StoreError::Corrupt);
        }
        let Value::Array(writes) = writes else {
            return Err(StoreError::Corrupt);
        };
        let mut previous = None;
        for write in writes {
            let [family, id, expected, final_record, payload] = array::<5>(write)?;
            let key = RecordKey {
                family: RecordFamily::parse(string(family)?)?,
                id: string(id)?.into(),
            };
            if initial
                && !matches!(
                    key.family,
                    RecordFamily::Scope
                        | RecordFamily::Artifact
                        | RecordFamily::Usage
                        | RecordFamily::Timer
                )
            {
                return Err(StoreError::Corrupt);
            }
            if previous.as_ref().is_some_and(|old| old >= &key) {
                return Err(StoreError::Corrupt);
            }
            previous = Some(key.clone());
            let expected = if matches!(expected, Value::Null) {
                None
            } else {
                Some(integer(expected)?)
            };
            if initial && expected.is_some() {
                return Err(StoreError::Corrupt);
            }
            let Value::Bool(final_record) = final_record else {
                return Err(StoreError::Corrupt);
            };
            let old = self.records.get(&key);
            if old.is_some_and(|(r, _)| r.final_record) || old.map(|(r, _)| r.revision) != expected
            {
                return Err(StoreError::Corrupt);
            }
            let revision = next_revision(expected).map_err(|_| StoreError::Corrupt)?;
            let bytes = cbor::encode(write, limits)?.len();
            self.retained_bytes = self
                .retained_bytes
                .checked_sub(old.map_or(0, |(_, size)| *size))
                .and_then(|n| n.checked_add(bytes))
                .ok_or(StoreError::Invalid)?;
            if self.retained_bytes > limits.max_document_bytes {
                return Err(StoreError::Invalid);
            }
            self.records.insert(
                key.clone(),
                (
                    StoredRecord {
                        key,
                        revision,
                        final_record: *final_record,
                        payload: payload.clone(),
                    },
                    bytes,
                ),
            );
        }
        if !matches!(status, Value::Null) {
            let next = match string(status)? {
                "succeeded" => RunStatus::Succeeded,
                "failed" => RunStatus::Failed,
                "cancelled" => RunStatus::Cancelled,
                _ => return Err(StoreError::Corrupt),
            };
            if initial && next != RunStatus::Failed {
                return Err(StoreError::Corrupt);
            }
            if !self.header.status.can_transition_to(next) {
                return Err(StoreError::Corrupt);
            }
            self.header.status = next;
        }
        if !matches!(root, Value::Null) {
            let [fingerprint, root] = array::<2>(root)?;
            self.header.fingerprint = string(fingerprint)?
                .parse()
                .map_err(|_| StoreError::Corrupt)?;
            self.header.root_scope = string(root)?.parse().map_err(|_| StoreError::Corrupt)?;
        }
        Ok(())
    }
    /// Reconstructed authority header, for comparison with committed rows.
    pub fn header(&self) -> &RunHeader {
        &self.header
    }
    /// Authenticated host start receipt reconstructed from initialization, if any.
    pub fn start_receipt(&self) -> Option<&ReplayedStartReceipt> {
        self.start_receipt.as_ref()
    }
    /// Reconstructed authoritative record; derived indexes are not necessary.
    pub fn record(&self, key: &RecordKey) -> Option<&StoredRecord> {
        self.records.get(key).map(|(record, _)| record)
    }
    /// Reconstructed internal command receipt and its semantic request digest.
    pub fn command(&self, key: &str) -> Option<(Digest, CommitReceipt)> {
        self.commands.get(key).map(|(digest, sequence)| {
            (
                *digest,
                CommitReceipt {
                    sequence: *sequence,
                },
            )
        })
    }
}
fn array<const N: usize>(value: &Value) -> Result<&[Value; N], StoreError> {
    let Value::Array(values) = value else {
        return Err(StoreError::Corrupt);
    };
    values
        .as_slice()
        .try_into()
        .map_err(|_| StoreError::Corrupt)
}
fn string(value: &Value) -> Result<&str, StoreError> {
    if let Value::Text(text) = value {
        Ok(text)
    } else {
        Err(StoreError::Corrupt)
    }
}
fn integer(value: &Value) -> Result<i64, StoreError> {
    if let Value::Integer(n) = value {
        Ok(*n)
    } else {
        Err(StoreError::Corrupt)
    }
}
