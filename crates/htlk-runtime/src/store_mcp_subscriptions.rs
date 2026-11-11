//! Child of store_mcp: subscription transactions reuse its independent authority
//! checks. Claims are uncertain before RPC dispatch, including abandoned futures.
use super::*;

/// Validated cumulative subscription-row and per-command transition bounds.
/// Tombstones and operation evidence count toward the quotas; eviction never
/// authorizes a retry. Byte accounting covers keys and fixed row allowances,
/// independently of SQLite file overhead and the owner's lifecycle journal.
#[derive(Clone, Copy, Debug)]
pub struct McpSubscriptionLimits {
    resources: usize,
    consumers: usize,
    operations: usize,
    bytes: usize,
    transitions: usize,
}
impl McpSubscriptionLimits {
    /// Checks positive finite quotas; byte limits are also checked against the store.
    ///
    /// # Errors
    /// Returns invalid input for zero or unrepresentable bounds.
    pub fn new(
        resources: usize,
        consumers: usize,
        operations: usize,
        bytes: usize,
        transitions: usize,
    ) -> Result<Self, StoreError> {
        if [resources, consumers, operations, transitions]
            .into_iter()
            .any(|n| n == 0 || n > 65_536)
            || bytes == 0
            || bytes > isize::MAX as usize
        {
            return Err(StoreError::Invalid);
        }
        Ok(Self {
            resources,
            consumers,
            operations,
            bytes,
            transitions,
        })
    }
    /// Maximum RPC transitions performed by one explicitly invoked host command.
    pub fn transitions(&self) -> usize {
        self.transitions
    }
}

/// Immutable subscriber-keyed desire. Construction validates representation;
/// transactional admission separately validates actual owner, server, and epoch.
#[derive(Clone)]
pub struct McpSubscriptionIntent {
    session: Digest,
    server: Digest,
    uri: String,
    subscriber: String,
    desired: bool,
}
impl McpSubscriptionIntent {
    /// Creates bounded nonempty URI/subscriber keys, preserving their exact text.
    /// URIs are never expanded or fetched during validation.
    ///
    /// # Errors
    /// Returns invalid input for empty or oversized metadata.
    pub fn new(
        session: Digest,
        server: Digest,
        uri: String,
        subscriber: String,
        desired: bool,
        limits: &Limits,
    ) -> Result<Self, StoreError> {
        validate_keys(&uri, &subscriber, limits)?;
        Ok(Self {
            session,
            server,
            uri,
            subscriber,
            desired,
        })
    }
}

/// Remote state observed for one URI. Desired local consumers are independent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpSubscriptionState {
    /// A positive unsubscribe acknowledgement, or no subscribe was sent.
    Inactive,
    /// A positive subscribe acknowledgement in the still-live epoch.
    Active,
    /// A unique operation claim may have been sent; never blindly repeat it.
    Uncertain,
    /// The previous epoch closed or its owner generation was superseded.
    Gap,
}
impl McpSubscriptionState {
    fn parse(value: &str) -> Result<Self, StoreError> {
        match value {
            "inactive" => Ok(Self::Inactive),
            "active" => Ok(Self::Active),
            "uncertain" => Ok(Self::Uncertain),
            "gap" => Ok(Self::Gap),
            _ => Err(StoreError::Corrupt),
        }
    }
    fn as_str(self) -> &'static str {
        match self {
            Self::Inactive => "inactive",
            Self::Active => "active",
            Self::Uncertain => "uncertain",
            Self::Gap => "gap",
        }
    }
}

/// Remote action bound to an immutable persisted operation identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpSubscriptionAction {
    /// Begin remote monitoring for a URI with one or more desired consumers.
    Subscribe,
    /// End remote monitoring after the final consumer releases its intent.
    Unsubscribe,
}
impl McpSubscriptionAction {
    fn as_str(self) -> &'static str {
        match self {
            Self::Subscribe => "subscribe",
            Self::Unsubscribe => "unsubscribe",
        }
    }
    fn parse(value: &str) -> Result<Self, StoreError> {
        match value {
            "subscribe" => Ok(Self::Subscribe),
            "unsubscribe" => Ok(Self::Unsubscribe),
            _ => Err(StoreError::Corrupt),
        }
    }
}

/// Protected subscription observation. Debug excludes URI and subscriber keys.
#[derive(Clone)]
pub struct McpStoredSubscription {
    id: Digest,
    server: Digest,
    uri: String,
    session: Digest,
    generation: i64,
    revision: i64,
    state: McpSubscriptionState,
    desired_consumers: usize,
    gap_count: u64,
    operation: Option<Digest>,
}
impl McpStoredSubscription {
    /// Stable owner/server/URI identity, independent of transport epoch.
    pub fn id(&self) -> Digest {
        self.id
    }
    /// Exact compound server digest.
    pub fn server(&self) -> Digest {
        self.server
    }
    /// Protected exact resource URI; inspection does not fetch content.
    pub fn uri(&self) -> &str {
        &self.uri
    }
    /// Last targeted transport epoch identity, retained across disconnects.
    pub fn session(&self) -> Digest {
        self.session
    }
    /// Generation that last targeted the remote state.
    pub fn generation(&self) -> i64 {
        self.generation
    }
    /// Optimistic record revision, including subscriber desire changes.
    pub fn revision(&self) -> i64 {
        self.revision
    }
    /// Effective remote state; closed/superseded epochs are always gaps.
    pub fn state(&self) -> McpSubscriptionState {
        self.state
    }
    /// Number of distinct currently desired local consumer keys.
    pub fn desired_consumers(&self) -> usize {
        self.desired_consumers
    }
    /// Observed epoch delivery gaps. Zero never asserts server replay guarantees.
    pub fn gap_count(&self) -> u64 {
        self.gap_count
    }
    /// Latest possibly-sent operation, absent after a settled acknowledgement.
    pub fn operation(&self) -> Option<Digest> {
        self.operation
    }
    /// Whether current desired intent and known remote state agree.
    pub fn converged(&self) -> bool {
        matches!(
            (self.desired_consumers > 0, self.state),
            (true, McpSubscriptionState::Active) | (false, McpSubscriptionState::Inactive)
        )
    }
}
impl fmt::Debug for McpStoredSubscription {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpStoredSubscription")
            .field("id", &self.id)
            .field("state", &self.state)
            .field("desired_consumers", &self.desired_consumers)
            .field("gap_count", &self.gap_count)
            .field("revision", &self.revision)
            .finish_non_exhaustive()
    }
}

/// An unforgeable-by-construction operation claim. Its identity and all metadata
/// are independently rechecked against persisted facts during acknowledgement.
#[derive(Clone, Debug)]
pub struct McpSubscriptionOperation {
    id: Digest,
    subscription: Digest,
    session: Digest,
    generation: i64,
    action: McpSubscriptionAction,
}
impl McpSubscriptionOperation {
    /// Durable exact attempt identity; it is not a remote idempotency guarantee.
    pub fn id(&self) -> Digest {
        self.id
    }
    /// Exact admitted protocol action.
    pub fn action(&self) -> McpSubscriptionAction {
        self.action
    }
    /// Session epoch to which this claim is bound.
    pub fn session(&self) -> Digest {
        self.session
    }
}
/// Only Dispatch authorizes a new remote request. Existing uncertainty never does.
#[derive(Debug)]
pub enum McpSubscriptionClaim {
    /// Intent and unique operation were committed before this claim was returned.
    Dispatch {
        /// Protected observation after committing the possibly-sent operation.
        record: McpStoredSubscription,
        /// Exact one-time local dispatch claim.
        operation: McpSubscriptionOperation,
    },
    /// No new request is permitted; inspect state for convergence, uncertainty/gap.
    Retained(McpStoredSubscription),
}
/// Trusted observed delivery fact. Correlated RPC errors remain uncertain because
/// the protocol does not prove they left subscription state unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpSubscriptionOutcome {
    /// A valid positive protocol acknowledgement was observed.
    Acknowledged,
    /// The request was definitely not sent.
    NotSent,
    /// It may have been sent or its remote state remains unknown.
    Uncertain,
}
impl McpSubscriptionOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Acknowledged => "acknowledged",
            Self::NotSent => "not_sent",
            Self::Uncertain => "uncertain",
        }
    }
}

impl SqliteStore {
    /// Persists subscriber desire and, only when needed, a unique uncertain remote
    /// operation in one immediate transaction. Duplicate consumer keys do not add
    /// references. Closed/foreign epochs require explicit restoration.
    ///
    /// # Errors
    /// Returns invalid/quota input, stale owner/session, corruption or storage failure.
    pub async fn set_mcp_subscription_intent(
        &self,
        owner: &McpOwner,
        intent: McpSubscriptionIntent,
        budget: McpSubscriptionLimits,
        now_ms: i64,
    ) -> Result<McpSubscriptionClaim, StoreError> {
        host_owner(owner)?;
        timestamp(now_ms)?;
        validate_keys(&intent.uri, &intent.subscriber, &self.config.limits)?;
        validate_budget(budget, &self.config.limits)?;
        let (owner, limits) = (owner.clone(), self.config.limits.clone());
        self.connection.call(move |db| -> Result<_, StoreError> {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let now = authority(&tx, &owner, now_ms)?;
            check_session(&tx, &owner, intent.session, intent.server)?;
            let id = subscription_id(&owner, intent.server, &intent.uri, &limits)?;
            tx.execute("INSERT INTO mcp_resource_subscriptions(subscription_id,owner_key,server_digest,uri,session_id,generation,revision,state,gap_count) VALUES(?1,?2,?3,?4,?5,?6,1,'inactive',0) ON CONFLICT(subscription_id) DO NOTHING", params![id.to_string(),owner.key().to_string(),intent.server.to_string(),intent.uri,intent.session.to_string(),owner.generation()])?;
            let previous: Option<bool> = tx.query_row("SELECT desired FROM mcp_subscription_consumers WHERE subscription_id=?1 AND subscriber_key=?2", params![id.to_string(),intent.subscriber], |r| r.get(0)).optional()?;
            if previous != Some(intent.desired) {
                tx.execute("INSERT INTO mcp_subscription_consumers VALUES(?1,?2,?3) ON CONFLICT(subscription_id,subscriber_key) DO UPDATE SET desired=excluded.desired",params![id.to_string(),intent.subscriber,intent.desired])?;
                bump(&tx, id)?;
                journal(&tx,&owner,id,"subscription_desire_changed",now,&limits)?;
            }
            check_budget(&tx,&owner,budget)?;
            let record = load(&tx,&owner,id,&limits)?.ok_or(StoreError::Corrupt)?;
            let claim = if record.session != intent.session || record.generation != owner.generation() {
                McpSubscriptionClaim::Retained(record)
            } else { claim(&tx,&owner,record,budget,now,&limits)? };
            tx.commit()?; Ok(claim)
        }).await.map_err(worker_error)
    }

    /// Claims a necessary follow-up after a concurrent desire change. Uncertain
    /// operations are never reclaimed, and this operation never rebinds epochs.
    ///
    /// # Errors
    /// Returns stale authority, missing selection, quota or storage failure.
    pub async fn claim_mcp_subscription(
        &self,
        owner: &McpOwner,
        session: Digest,
        id: Digest,
        budget: McpSubscriptionLimits,
        now_ms: i64,
    ) -> Result<McpSubscriptionClaim, StoreError> {
        host_owner(owner)?;
        timestamp(now_ms)?;
        validate_budget(budget, &self.config.limits)?;
        let (owner, limits) = (owner.clone(), self.config.limits.clone());
        self.connection
            .call(move |db| -> Result<_, StoreError> {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let now = authority(&tx, &owner, now_ms)?;
                let record = load(&tx, &owner, id, &limits)?.ok_or(StoreError::Missing)?;
                check_session(&tx, &owner, session, record.server)?;
                if record.session != session || record.generation != owner.generation() {
                    return Err(StoreError::Fenced);
                }
                let result = claim(&tx, &owner, record, budget, now, &limits)?;
                tx.commit()?;
                Ok(result)
            })
            .await
            .map_err(worker_error)
    }

    /// Commits a positive/negative delivery fact under exact owner generation,
    /// current live epoch and unique operation identity. Desired-reference changes
    /// do not erase valid ACKs; a later bounded claim converges the new desire.
    ///
    /// # Errors
    /// Returns stale/closed epoch, conflicting settlement, corruption or storage failure.
    pub async fn settle_mcp_subscription(
        &self,
        owner: &McpOwner,
        operation: &McpSubscriptionOperation,
        outcome: McpSubscriptionOutcome,
        now_ms: i64,
    ) -> Result<McpStoredSubscription, StoreError> {
        host_owner(owner)?;
        timestamp(now_ms)?;
        let (owner, operation, limits) =
            (owner.clone(), operation.clone(), self.config.limits.clone());
        self.connection.call(move |db| -> Result<_,StoreError> {
            let tx=db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let now=authority(&tx,&owner,now_ms)?;
            let record=load(&tx,&owner,operation.subscription,&limits)?.ok_or(StoreError::Missing)?;
            check_session(&tx,&owner,operation.session,record.server)?;
            if operation.generation!=owner.generation() || record.session!=operation.session || record.generation!=owner.generation() {return Err(StoreError::Fenced);}
            let saved:(String,String,i64,String,String,String)=tx.query_row("SELECT subscription_id,session_id,generation,action,prior_state,outcome FROM mcp_subscription_operations WHERE operation_id=?1",[operation.id.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional()?.ok_or(StoreError::Missing)?;
            if saved.0!=operation.subscription.to_string() || saved.1!=operation.session.to_string() || saved.2!=operation.generation || McpSubscriptionAction::parse(&saved.3)?!=operation.action {return Err(StoreError::Corrupt);}
            if saved.5!="uncertain" {
                if saved.5!=outcome.as_str() {return Err(StoreError::IdempotencyConflict);}
                tx.commit()?; return Ok(record);
            }
            if record.operation!=Some(operation.id) || record.state!=McpSubscriptionState::Uncertain {return Err(StoreError::Conflict);}
            if outcome!=McpSubscriptionOutcome::Uncertain {
                let state=match outcome {
                    McpSubscriptionOutcome::Acknowledged=>match operation.action {McpSubscriptionAction::Subscribe=>McpSubscriptionState::Active,McpSubscriptionAction::Unsubscribe=>McpSubscriptionState::Inactive},
                    McpSubscriptionOutcome::NotSent=>McpSubscriptionState::parse(&saved.4)?,
                    McpSubscriptionOutcome::Uncertain=>unreachable!(),
                };
                if !matches!(state,McpSubscriptionState::Active|McpSubscriptionState::Inactive) {return Err(StoreError::Corrupt);}
                tx.execute("UPDATE mcp_resource_subscriptions SET state=?2,operation_id=NULL,revision=revision+1 WHERE subscription_id=?1",params![record.id.to_string(),state.as_str()])?;
                tx.execute("UPDATE mcp_subscription_operations SET outcome=?2 WHERE operation_id=?1",params![operation.id.to_string(),outcome.as_str()])?;
                journal(&tx,&owner,record.id,match outcome {McpSubscriptionOutcome::Acknowledged=>"subscription_acknowledged",_=>"subscription_not_sent"},now,&limits)?;
            } else { journal(&tx,&owner,record.id,"subscription_uncertain",now,&limits)?; }
            let record=load(&tx,&owner,record.id,&limits)?.ok_or(StoreError::Corrupt)?;
            tx.commit()?;Ok(record)
        }).await.map_err(worker_error)
    }

    /// Explicitly authorizes restoration from the exact previous epoch. A still-
    /// live epoch cannot be replaced in the same generation. New generations may
    /// restore old desired intent while all old acknowledgement claims stay fenced.
    /// No prior RPC or content acquisition is replayed by this transaction.
    ///
    /// # Errors
    /// Returns stale grant/epoch, quota, missing record or storage failure.
    pub async fn restore_mcp_subscription(
        &self,
        owner: &McpOwner,
        id: Digest,
        previous_session: Digest,
        session: Digest,
        budget: McpSubscriptionLimits,
        now_ms: i64,
    ) -> Result<McpSubscriptionClaim, StoreError> {
        host_owner(owner)?;
        timestamp(now_ms)?;
        validate_budget(budget, &self.config.limits)?;
        let (owner, limits) = (owner.clone(), self.config.limits.clone());
        self.connection.call(move|db|->Result<_,StoreError>{
            let tx=db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let now=authority(&tx,&owner,now_ms)?;
            let record=load(&tx,&owner,id,&limits)?.ok_or(StoreError::Missing)?;
            check_session(&tx,&owner,session,record.server)?;
            if record.session!=previous_session || session==previous_session || record.state!=McpSubscriptionState::Gap {return Err(StoreError::Conflict);}
            let old_epoch:i64=tx.query_row("SELECT epoch FROM mcp_epochs WHERE session_id=?1",[previous_session.to_string()],|r|r.get(0))?;
            let new_epoch:i64=tx.query_row("SELECT epoch FROM mcp_epochs WHERE session_id=?1",[session.to_string()],|r|r.get(0))?;
            if new_epoch<=old_epoch {return Err(StoreError::Fenced);}
            tx.execute("UPDATE mcp_resource_subscriptions SET session_id=?2,generation=?3,state='inactive',operation_id=NULL,gap_count=?4,revision=revision+1 WHERE subscription_id=?1",params![id.to_string(),session.to_string(),owner.generation(),record.gap_count])?;
            fact(&tx,&owner,None,"subscription_restoration_gap",now,&Value::Array(vec![
                Value::Text(id.to_string()),Value::Text(previous_session.to_string()),
                Value::Text(session.to_string()),Value::Integer(i64::try_from(record.gap_count).map_err(|_|StoreError::Corrupt)?),
            ]),&limits)?;
            let record=load(&tx,&owner,id,&limits)?.ok_or(StoreError::Corrupt)?;
            let result=claim(&tx,&owner,record,budget,now,&limits)?;
            tx.commit()?;Ok(result)
        }).await.map_err(worker_error)
    }

    /// Reads one protected owner/server/URI observation after live owner validation.
    /// Epoch closure/takeover is exposed as Gap without changing desired consumers.
    ///
    /// # Errors
    /// Returns invalid keys, stale owner, corrupt metadata or storage failure.
    pub async fn mcp_subscription(
        &self,
        owner: &McpOwner,
        server: Digest,
        uri: &str,
        now_ms: i64,
    ) -> Result<Option<McpStoredSubscription>, StoreError> {
        host_owner(owner)?;
        timestamp(now_ms)?;
        validate_keys(uri, "inspection", &self.config.limits)?;
        let (owner, uri, limits) = (owner.clone(), uri.to_string(), self.config.limits.clone());
        self.connection
            .call(move |db| -> Result<_, StoreError> {
                let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                authority(&tx, &owner, now_ms)?;
                let record = load(
                    &tx,
                    &owner,
                    subscription_id(&owner, server, &uri, &limits)?,
                    &limits,
                )?;
                tx.commit()?;
                Ok(record)
            })
            .await
            .map_err(worker_error)
    }

    /// Enumerates a bounded protected subscription page; continuation is the final
    /// returned stable ID. Inspection performs no RPC or content acquisition.
    ///
    /// # Errors
    /// Returns invalid paging, stale owner, representation or storage failure.
    pub async fn mcp_subscriptions(
        &self,
        owner: &McpOwner,
        after: Option<Digest>,
        maximum: usize,
        now_ms: i64,
    ) -> Result<Vec<McpStoredSubscription>, StoreError> {
        host_owner(owner)?;
        timestamp(now_ms)?;
        if maximum == 0 || maximum > self.config.max_page_size {
            return Err(StoreError::Invalid);
        }
        let count = i64::try_from(maximum).map_err(|_| StoreError::Invalid)?;
        let (owner, limits) = (owner.clone(), self.config.limits.clone());
        self.connection.call(move|db|->Result<_,StoreError>{
            let tx=db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            authority(&tx,&owner,now_ms)?;
            let mut statement=tx.prepare("SELECT subscription_id FROM mcp_resource_subscriptions WHERE owner_key=?1 AND subscription_id>?2 ORDER BY subscription_id LIMIT ?3")?;
            let mut rows=statement.query(params![owner.key().to_string(),after.map_or_else(String::new,|v|v.to_string()),count])?;
            let mut result=Vec::new();let mut bytes=0usize;
            while let Some(row)=rows.next()? {
                let id=row.get::<_,String>(0)?.parse().map_err(|_|StoreError::Corrupt)?;
                let record=load(&tx,&owner,id,&limits)?.ok_or(StoreError::Corrupt)?;
                bytes=bytes.checked_add(record.uri.len()+512).ok_or(StoreError::Invalid)?;
                if bytes>limits.max_document_bytes {if result.is_empty(){return Err(StoreError::Invalid);}break;}
                result.push(record);
            }
            drop(rows);drop(statement);tx.commit()?;Ok(result)
        }).await.map_err(worker_error)
    }
}

fn validate_keys(uri: &str, subscriber: &str, limits: &Limits) -> Result<(), StoreError> {
    if uri.is_empty()
        || subscriber.is_empty()
        || uri.len() > limits.max_document_bytes / 8
        || subscriber.len() > limits.max_document_bytes / 8
    {
        return Err(StoreError::Invalid);
    }
    cbor::encode(
        &Value::Array(vec![
            Value::Text(uri.into()),
            Value::Text(subscriber.into()),
        ]),
        limits,
    )?;
    Ok(())
}
fn worker_error(error: crate::sqlite_worker::Error<StoreError>) -> StoreError {
    match error {
        crate::sqlite_worker::Error::Error(error) => error,
        _ => StoreError::Closed,
    }
}
fn validate_budget(budget: McpSubscriptionLimits, limits: &Limits) -> Result<(), StoreError> {
    if budget.bytes > limits.max_document_bytes {
        Err(StoreError::Invalid)
    } else {
        Ok(())
    }
}
fn subscription_id(
    owner: &McpOwner,
    server: Digest,
    uri: &str,
    limits: &Limits,
) -> Result<Digest, StoreError> {
    identity(
        "resource-subscription",
        vec![
            Value::Text(owner.key().to_string()),
            Value::Text(server.to_string()),
            Value::Text(uri.into()),
        ],
        limits,
    )
}
fn check_session(
    db: &Db,
    owner: &McpOwner,
    session: Digest,
    server: Digest,
) -> Result<(), StoreError> {
    session_authority(db, owner, session, true)?;
    let actual: String = db.query_row(
        "SELECT server_digest FROM mcp_epochs WHERE session_id=?1",
        [session.to_string()],
        |r| r.get(0),
    )?;
    if actual != server.to_string() {
        return Err(StoreError::Fenced);
    }
    Ok(())
}
fn bump(db: &Db, id: Digest) -> Result<(), StoreError> {
    db.execute(
        "UPDATE mcp_resource_subscriptions SET revision=revision+1 WHERE subscription_id=?1",
        [id.to_string()],
    )?;
    Ok(())
}
fn journal(
    db: &Db,
    owner: &McpOwner,
    id: Digest,
    kind: &str,
    now: i64,
    limits: &Limits,
) -> Result<(), StoreError> {
    fact(
        db,
        owner,
        None,
        kind,
        now,
        &Value::Text(id.to_string()),
        limits,
    )
}
type SubscriptionRow = (
    String,
    String,
    String,
    i64,
    i64,
    String,
    u64,
    Option<String>,
    bool,
    i64,
);
fn load(
    db: &Db,
    owner: &McpOwner,
    id: Digest,
    limits: &Limits,
) -> Result<Option<McpStoredSubscription>, StoreError> {
    let row:Option<SubscriptionRow>=db.query_row(
        "SELECT s.server_digest,s.uri,s.session_id,s.generation,s.revision,s.state,s.gap_count,s.operation_id,e.closed,(SELECT coalesce(sum(desired),0) FROM mcp_subscription_consumers c WHERE c.subscription_id=s.subscription_id) FROM mcp_resource_subscriptions s JOIN mcp_epochs e ON e.session_id=s.session_id WHERE s.owner_key=?1 AND s.subscription_id=?2",params![owner.key().to_string(),id.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?))).optional()?;
    let Some((
        server,
        uri,
        session,
        generation,
        revision,
        state,
        gaps,
        operation,
        closed,
        consumers,
    )) = row
    else {
        return Ok(None);
    };
    validate_keys(&uri, "stored", limits).map_err(|_| StoreError::Corrupt)?;
    let server = server.parse().map_err(|_| StoreError::Corrupt)?;
    if id != subscription_id(owner, server, &uri, limits)?
        || revision <= 0
        || generation <= 0
        || generation > owner.generation()
        || !(0..=65_536).contains(&consumers)
    {
        return Err(StoreError::Corrupt);
    }
    let epoch_matches: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM mcp_epochs WHERE session_id=?1 AND owner_key=?2 AND generation=?3 AND server_digest=?4)",
        params![session,owner.key().to_string(),generation,server.to_string()],|r|r.get(0),
    )?;
    if !epoch_matches {
        return Err(StoreError::Corrupt);
    }
    let mut state = McpSubscriptionState::parse(&state)?;
    if (state == McpSubscriptionState::Uncertain) != operation.is_some() {
        return Err(StoreError::Corrupt);
    }
    if let Some(operation) = &operation {
        let operation_matches: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM mcp_subscription_operations WHERE operation_id=?1 AND subscription_id=?2 AND session_id=?3 AND generation=?4 AND outcome='uncertain')",
            params![operation,id.to_string(),session,generation],|r|r.get(0),
        )?;
        if !operation_matches {
            return Err(StoreError::Corrupt);
        }
    }
    let mut gap_count = gaps;
    if (closed || generation != owner.generation()) && state != McpSubscriptionState::Gap {
        state = McpSubscriptionState::Gap;
        gap_count = gap_count.checked_add(1).ok_or(StoreError::Corrupt)?;
    }
    let operation = operation
        .map(|s| s.parse().map_err(|_| StoreError::Corrupt))
        .transpose()?;
    if state == McpSubscriptionState::Uncertain && operation.is_none() {
        return Err(StoreError::Corrupt);
    }
    Ok(Some(McpStoredSubscription {
        id,
        server,
        uri,
        session: session.parse().map_err(|_| StoreError::Corrupt)?,
        generation,
        revision,
        state,
        desired_consumers: consumers as usize,
        gap_count,
        operation,
    }))
}
fn check_budget(
    db: &Db,
    owner: &McpOwner,
    budget: McpSubscriptionLimits,
) -> Result<(), StoreError> {
    let key = owner.key().to_string();
    let (resources,resource_bytes):(i64,i64)=db.query_row("SELECT count(*),coalesce(sum(length(CAST(uri AS BLOB))+512),0) FROM mcp_resource_subscriptions WHERE owner_key=?1",[&key],|r|Ok((r.get(0)?,r.get(1)?)))?;
    let (consumers,consumer_bytes):(i64,i64)=db.query_row("SELECT count(*),coalesce(sum(length(CAST(c.subscriber_key AS BLOB))+128),0) FROM mcp_subscription_consumers c JOIN mcp_resource_subscriptions s USING(subscription_id) WHERE s.owner_key=?1",[&key],|r|Ok((r.get(0)?,r.get(1)?)))?;
    let operations:i64=db.query_row("SELECT count(*) FROM mcp_subscription_operations o JOIN mcp_resource_subscriptions s USING(subscription_id) WHERE s.owner_key=?1",[&key],|r|r.get(0))?;
    let bytes = resource_bytes
        .checked_add(consumer_bytes)
        .and_then(|n| operations.checked_mul(512).and_then(|m| n.checked_add(m)))
        .ok_or(StoreError::Budget)?;
    if resources > budget.resources as i64
        || consumers > budget.consumers as i64
        || operations > budget.operations as i64
        || bytes > budget.bytes as i64
    {
        return Err(StoreError::Budget);
    }
    Ok(())
}
fn claim(
    db: &Db,
    owner: &McpOwner,
    record: McpStoredSubscription,
    budget: McpSubscriptionLimits,
    now: i64,
    limits: &Limits,
) -> Result<McpSubscriptionClaim, StoreError> {
    let action = match (record.state, record.desired_consumers > 0) {
        (McpSubscriptionState::Inactive, true) => McpSubscriptionAction::Subscribe,
        (McpSubscriptionState::Active, false) => McpSubscriptionAction::Unsubscribe,
        _ => return Ok(McpSubscriptionClaim::Retained(record)),
    };
    let revision = record.revision.checked_add(1).ok_or(StoreError::Corrupt)?;
    let id = identity(
        "subscription-operation",
        vec![
            Value::Text(record.id.to_string()),
            Value::Text(record.session.to_string()),
            Value::Integer(owner.generation()),
            Value::Integer(revision),
            Value::Text(action.as_str().into()),
        ],
        limits,
    )?;
    db.execute(
        "INSERT INTO mcp_subscription_operations VALUES(?1,?2,?3,?4,?5,?6,'uncertain')",
        params![
            id.to_string(),
            record.id.to_string(),
            record.session.to_string(),
            owner.generation(),
            action.as_str(),
            record.state.as_str()
        ],
    )?;
    db.execute("UPDATE mcp_resource_subscriptions SET state='uncertain',operation_id=?2,revision=?3 WHERE subscription_id=?1",params![record.id.to_string(),id.to_string(),revision])?;
    check_budget(db, owner, budget)?;
    journal(db, owner, record.id, "subscription_intent", now, limits)?;
    let operation = McpSubscriptionOperation {
        id,
        subscription: record.id,
        session: record.session,
        generation: owner.generation(),
        action,
    };
    Ok(McpSubscriptionClaim::Dispatch {
        record: load(db, owner, record.id, limits)?.ok_or(StoreError::Corrupt)?,
        operation,
    })
}
