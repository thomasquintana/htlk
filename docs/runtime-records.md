# Runtime record foundations (Draft 0.1)

The normative execution and portable value rules are in
[runtime §§3–4, 8 and 16](../specs/runtime-spec.md). The runtime's `identity`,
`records` and `failure` modules supply bounded representation foundations.
Constructing these records establishes neither authorization nor artifact acceptance.
The transactional coordinator must establish those facts before admission.

## Identities

Runtime domains encode an array `['htlk.' + domain, '0.1', ...arguments]` and hash
its exact deterministic CBOR. They are distinct from compiler record domains,
which hash a text prefix plus canonical record bytes. Run IDs retain opaque exact
strings; other runtime IDs wrap strict SHA-256 digests. Counters fit signed i64;
attempts/extensions are one-based, iteration/approval generation zero-based.

The runtime accounts the complete array, including nested type/value depth, before
hashing a short array header and canonical child encodings. Value trees are
borrowed rather than cloned. `ValueDigest` identifies typed content, while
`ArtifactId` identifies producer provenance. Neither proves actual-value validity.

## Lifecycle and source states

`InvocationStatus::can_transition_to` admits exactly pending-to-active/skipped/
failed/cancelled and active-to-succeeded/failed/cancelled. No terminal state can
transition, including to itself. Idempotent duplicate commands return their stored
receipt rather than applying a second transition. MCP retries stay within active.

`RunStatus` permits running-to-terminal transitions only. Blocked is derived.
`SourceState` distinguishes pending, accepted artifact, absence and unavailable
causal failure. Null is an ordinary present artifact value.

## Frozen manifests and influence

`InputManifest::freeze` requires exactly one entry for every declared input,
including explicit optional absence. Unknown, duplicate and missing ports fail.
Required absence is a skipped admission decision and cannot create an admitted
manifest. There is no mutable manifest/refreeze API.

Logical `to_value` entries are exactly `{state: 'absent'}` or
`{state: 'value', artifact_id: ...}`. The manifest separately retains content and
trusted label metadata. It automatically includes present artifact labels in its
influence set; the coordinator additionally supplies evaluated guard, binding,
absence and boundary-control influences. Trusted labels are sorted unique decoded
UTF-8 strings, without Unicode normalization. An empty set grants no access.

The implementation-defined input preimage is:

```text
[
  'htlk.input', '0.1',
  [
    {
      name: declared_port_name,
      port: {type: normalized_type, required: boolean},
      state: 'absent'
    }
    OR
    {
      name: declared_port_name,
      port: {type: normalized_type, required: boolean},
      state: 'value',
      content_digest: typed_source_content_digest,
      label_digest: security_label_digest
    }, ...
  ],
  frozen_influence_label_digest
]
```

The port inventory is sorted by decoded UTF-8 name. A label digest hashes
`['htlk.security_labels', '0.1', sorted_unique_label_array]`. Artifact IDs are
retained in the manifest but excluded from input hashing: copying accepted typed
content with the same labels does not change this content-equivalence hash.
Destination declarations, requiredness, absence, content and labels do change it.
A matching hash never grants permission to read/reuse a value.

Construction shares a finite temporary-metadata allowance across retained entry
facts and the derived hash inventory. Encoders independently check the complete
representation, including every key/header and embedded depth.

## Private checkpoints

`InputManifest::encode` persists a closed record with `record_version: '0.1'`,
`entries`, `influence_labels` and `input_hash`. Value entries retain artifact ID,
content digest and security labels. `decode` takes authoritative port declarations,
checks canonical CBOR and exact version/shape, reconstructs the inventory and
verifies the frozen hash and explicit influence set. It cannot substitute a
different port declaration or silently repair missing influence metadata.

Referenced artifact integrity and current authorization are checked separately
against authoritative records. Portable executable schemas do not prescribe this
private checkpoint layout.

## Host requests

`host_request_digest` hashes
`['htlk.host_request', '0.1', principal, method, target, semantic_arguments]`.
The host supplies authenticated principal/method/target context and all semantic
arguments. An idempotency key selects a separately scoped receipt; it is not part
of the semantic request. Changed method, target, principal or arguments changes
the request digest. Merely calling this hashing helper authenticates nobody.

## Failures

`FailureCode` covers runtime §16. `RuntimeFailure::new` uses a fixed redacted
message and exposes only code/message as graph data. Protected `FailureCause`
locations and causal invocation IDs are separate. Decoding trusted stored error
metadata preserves its committed message rather than replacing it with the current
deployment's wording. Viewer redaction cannot alter graph-visible stored outcomes.

`FailureCode::for_evaluation` distinguishes input/output type mismatch from an
ordinary expression error, and preserves dependency, profile, absence and limit
failures. Contract false results are explicit pre/postcondition codes; operational
failures never become a Boolean false or a successful schema check.

## Verification

Runtime `identity` tests and the independent Node fixture checker verify every
specified runtime identity domain. Runtime `records` tests exercise complete
coverage, provenance-independent hashes, protected absence, label unions, checkpoint
tampering, complete encoding limits, tagged host data and terminal immutability.
