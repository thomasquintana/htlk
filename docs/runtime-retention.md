# Protected retention and garbage collection

Authority: runtime §§8 and 14. Accepted artifact metadata, producer identities and
lineage stay immutable. Retention expiry is an explicit authorized operation;
passing the audit deadline alone does not make a value unreadable.

`RuntimeConfig::audit_retention_ms` advertises the minimum post-completion audit
window and must be at least `idempotency_retention_ms`. Run admission persists that
promise in run usage. Reopening with a shorter deployment setting cannot shorten
an existing promise; a longer current setting can extend protection.

## Expiry

`expire_artifact(context, artifact_id)` authorizes the immutable artifact's trusted
metadata and returns `RetentionDisposition::Protected` or `Expired { at_ms }`.
In an immediate transaction, it checks:

- The producing run is terminal and its original completion audit window elapsed.
- No active run manifest, request, checkpoint, accepted result or other durable
  record references the artifact.
- No other referencing run still promises its post-completion audit window.

References are checked against authoritative records in the same SQLite transaction
as expiry. Protection is deliberately conservative: retained run records can keep
values longer than the minimum. Terminal no-op receipts and previous expiry records
do not reset the original completion timestamp used for retention.

Expiry atomically inserts a final `Tombstone` record and an `artifact_expired`
journal/outbox event without acquiring execution ownership. It cannot change the
winning lifecycle, root or immutable artifact. Repeated requests return the saved
timestamp without another event. Journal replay validates the tombstone against its
accepted source metadata and reconstructs it without external capabilities.

New start/extension input references race with expiry through database authority.
A committed tombstone prevents installation of a newly consuming manifest. Artifact
reads authorize first, then check tombstone and bytes in one read snapshot, returning
`E_RETENTION_EXPIRED` for deliberate removal and `E_DIGEST` for corrupt/missing
retained bytes. Unauthorized callers do not receive expiry details.

## Shared bytes and bounded collection

`collect_garbage(context, after, page_size)` requires trusted housekeeping policy and
returns a bounded cursor page with scanned/removed counts. Untombstoned accepted
artifacts protect their shared storage address, even when another producer of the
same value has expired. Only unreferenced physical bytes are removed. Staging-only
orphans are collected while no run is active; metadata publication still checks
staging transactionally, so a concurrent new admission can retry a lost upload.

Collection never removes accepted metadata, tombstones, producer-slot uniqueness,
journal history or labels. A fresh producer may later upload identical bytes under
a new artifact ID. The old ID remains expired even when those bytes reappear.

`tests/runtime_retention.rs` verifies saved promises across configuration changes,
active and newly completed cross-run references, shared-byte protection, authorized
expiry, historical tombstones, replay equality, rejection of expired inputs,
physical collection and fresh-producer identity without reviving expired IDs.
