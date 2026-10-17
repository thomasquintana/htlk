# Runtime artifact preparation, publication and reads

Authority: [runtime §§3, 8 and 8.1](../specs/runtime-spec.md). The `artifact`
module separates proposed values, immutable accepted metadata and content bytes.
These APIs build on checked native evaluation and trusted transactional storage.

## Preparation and identities

`PreparedArtifact::new` takes the admitted executable, declared source type, native
value, original producer, accepted-input lineage, trusted protection and proposed
acceptance timestamp. It checks actual type/schema/representation under that
executable's native policy before producing a proposal. The coordinator selects
the actual declared port and conservative influence labels; application value
fields cannot supply those authority decisions.

Three identities stay distinct:

| Identity | Covered data |
| --- | --- |
| `content_digest` | Specified `htlk.value` domain, normalized source type, native value |
| `artifact_id` | Specified producer/port domain and typed content digest |
| `storage_reference` | SHA-256 of exact canonical value bytes |

Thus identical native bytes can share storage even under different declared types,
while preserving different typed content and producer histories. Type narrowing at
a destination never rewrites an existing artifact's source type or ID.

Inline run inputs use run ID, extension number (zero initially), input port and
typed content. Node outputs use invocation, output port and typed content. Inputs
supplied by accepted artifact reference retain their original ID. Inline inputs
have no prior input-artifact lineage; republishing nodes retain sorted unique
accepted parent IDs. Full per-port input manifests remain separate evidence.

## Metadata and integrity

The closed record contains exactly the runtime §8 facts: artifact ID, type, content
digest, storage reference, producer, input artifact IDs, security labels, accepted
timestamp and validation profile. Private `validation_profile` is the origin
executable fingerprint, referencing its exact profile/schema/library closure.
Metadata accessors are read-only; decoding establishes representation and producer
identity, not authorization or acceptance.

`ArtifactMetadata::verify_value` checks bounded canonical bytes, raw storage hash
and typed content hash without coercion. Present null stays present, native bytes
stay bytes, and integral native floats retain their representation. Noncanonical
wide floats cannot be smuggled in by updating only a storage pointer. Metadata
unknown fields, malformed producers, unsorted labels/lineage and changed IDs fail.
Size/depth failures remain distinct from integrity corruption.

## Staging and atomic acceptance

`SqliteStore::stage_artifact` uploads checked immutable bytes under their raw digest.
An upload alone publishes nothing. It is not readable through `read_artifact`
until final metadata exists; interrupted proposals can leave unreferenced staged
bytes for later reference-safe collection.

Accepted metadata is a final `RecordFamily::Artifact` record in the same storage
transaction as its successful invocation and complete output inventory. Storage
requires the registered validation origin and staged bytes. Node-output metadata
must match a final succeeded invocation's exact output-manifest entry; every value
entry of a final successful invocation must resolve to its accepted output metadata
before commit. A failed invocation cannot publish normal outputs, and success
cannot publish an incomplete inventory.

A unique artifact-ID index and a rebuildable producer-slot index prevent accepting
different content for one run-input `(run, extension, port)` or node-output
`(invocation, port)`. Producer-index writes participate in the same transaction
through deferred foreign keys. Metadata cannot be revised after acceptance.
Lineage parents must already be accepted. Scope/loop boundaries and contract/value
ordering are coordinator responsibilities delivered with their execution phases.

## Authorized reads

`Runtime::read_artifact` reads accepted metadata, asks current policy to authorize
`Authorization::ReadArtifact`, checks service limits, and only then loads immutable
bytes and verifies their identities. The result is an `AuthorizedValue` snapshot,
not a storage capability. Its Debug output omits value contents. Policy denial is
checked before blob loading, including when the underlying blob is corrupted.

Reading previously accepted native bytes does not repeat a tool call or rerun an
old library. Starting work from them still requires destination validation and
current authorization. Conservative input/guard/boundary influence propagation is
enforced by the coordinator; `ArtifactInput` is a trusted coordinator proposal.

## Persistence and recovery boundary

Migration 3 adds immutable value blobs, global accepted artifact identity and the
producer uniqueness index. Canonical metadata remains in authoritative journaled
records. Recovery must verify referenced bytes and rebuild producer indexes from
that metadata; blobs themselves are durable content-addressed storage, not journal
instructions. Artifact-aware recovery and retention integration remain later tasks.

## Verification

Real compiler fixture tests cover staged invisibility, current-policy reads,
distinct source types/provenance with shared bytes, null, bytes, metadata limits,
producer/content tampering and canonical float ingress. SQLite tests additionally
cover missing staged values, mutable metadata rejection, immutable producer slots,
corrupted blobs, denial before blob loading, failed-node rejection, complete
multi-record publication and late extra-output rejection.
