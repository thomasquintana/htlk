# Runtime registration and producer trust

Authority: [runtime §2](../specs/runtime-spec.md),
[compiler §9](../specs/compiler-spec.md), and the shared executable/analyzer APIs.

## Delivered API

`Runtime::new` binds a trusted `SqliteStore`, shared immutable `NativeRegistry`,
current `RuntimePolicy`, executable limits and a finite admitted-cache count.
`Runtime::register_graph(context, bytes, attestations)` implements the logical
check-and-save operation. `Runtime::executable(context, fingerprint)` authorizes
inspection and returns immutable admitted definitions and checked plans.

These methods do not execute nodes or start runs. Run/artifact APIs are the next
implementation slice.

## Configuration and authenticated context

The embedding application supplies `HostContext` from its authenticated session
or transport. Calling its constructor is a representation check, not a login.
Ordinary graph inputs cannot select the principal. `RuntimePolicy::authorize`
receives this context and the actual action. There is no default allow policy.
For registration it receives the completely admitted executable and detached
attestations; deployment signature/producer verification belongs in that hook.

Compiler configuration should use the profile selected through
`NativeRegistry::link_policy`, the exact same policy and complete linked library
manifests. Native schema preparation currently uses default backend options in
both runtime verification entry points. Compiler success and its retained analysis
do not establish actual host implementation availability or producer trust.

## Registration transaction

1. Bound detached evidence as a complete metadata record.
2. Decode and fully verify authoritative executable bytes through
   `verify_executable_owned`: canonical envelope/document, exact fingerprints and
   record identities, semantic linkage, schemas, expressions, graphs, structural
   limits and actual native policy/library/engine matching.
3. Invoke the current policy with the authenticated principal and verified graph.
   A valid signature cannot bypass malformed graph rejection.
4. Re-encode the immutable admitted document to check exact byte correspondence at
   the storage boundary.
5. In one immediate transaction, compare or insert the immutable graph and store
   detached provenance separately. Commit both or neither.
6. Retain admitted data in the bounded optional cache.

Registrations are keyed by fingerprint. Identical envelope bytes return the same
`GraphRegistration` containing fingerprint and authored graph ID. Different bytes
under an existing fingerprint return integrity failure; the old bytes are never
overwritten. Distinct valid provenance does not conflict with semantic registration.

## Detached provenance

`Attestation` carries a deployment-selected format and opaque evidence bytes.
The runtime does not invent a universal signature format or trust an asserted
signer field. Policy verifies real evidence under its configured trust contract.
Debug output omits evidence bytes.

The private provenance record is `[authenticated_principal, attestations]`, where
each attestation is `{format, evidence: bytes}`. Its identity is SHA-256 of those
canonical CBOR bytes. The record is stored under the graph fingerprint in a
separate table; equal evidence is idempotent, different evidence can coexist.
Source maps, descriptions and later observation sidecars follow the same separate
provenance principle and do not enter executable identity.

## Cache and reload

Admitted entries retain shared registry ownership and privately recomputed analysis.
The cache has a finite count and each entry passes complete per-document limits;
this is not a claim of an exact native-backend heap bound. Zero disables caching.
Capacity eviction drops the lowest fingerprint key; active users retain their Arc.
This policy has no execution-semantic meaning.

Every inspection checks current authorization, including cache hits. A fresh
service or miss loads exact registered bytes and independently re-admits them
against the current linked registry. A runtime lacking the exact policy or native
implementation fails instead of trusting a serialized compiler report. Requested
fingerprint and recovered envelope fingerprint must agree.

## Storage migration and verification

Schema migration 2 adds `graphs` and `graph_provenance` to the private authority
store. Version-1 run, journal, fence and outbox records remain unchanged.

Facade tests compile real source/worked-example executables and verify idempotency,
detached provenance, denied callers, current-policy changes on cache hits, separate
producer trust, invalid bytes before trust evaluation and fresh native re-admission.
Storage tests inject provenance failure, preserve old bytes on integrity conflict,
deduplicate exact evidence and exercise migration while preserving existing runs.
