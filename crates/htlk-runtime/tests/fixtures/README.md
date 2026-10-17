# Complete native executable fixture

`native-empty.hex` is a complete canonical Draft 0.1 envelope and graph document,
with a host-selected finite policy and the shipped native implementation profile.
The first two lines pin the envelope fingerprint and root-scope record digest.
The remaining lines are exact lowercase hexadecimal envelope bytes.

The Rust `verified` integration suite admits and round-trips this fixed fixture.
`node tools/validate-executable-fixtures.mjs` independently decodes its CBOR subset,
checks shortest arguments/map order, reconstructs policy JCS, and verifies envelope,
scope, policy and native adapter identities using Node's SHA-256 implementation.
The script is a fixture checker, not a second general executable verifier.

Native adapter source changes intentionally change implementation identities.
After reviewing such a change, run
`cargo run -p htlk-runtime --example native_fixture --locked`, update the fixed
fixture from its output, and rerun both independent and Rust checks. Never repair
or regenerate fixtures automatically as part of tests.

Source identities include package-owned model and analyzer contributions as well
as runtime adapters. The independent checker maintains explicit source lists and
hash framing for each contribution, including the pinned MCP schema snapshot.

The canonical `McpResourceResult` naming change regenerates `native-empty.hex`
solely because of source-derived implementation identities: this empty graph
contains no MCP result type. The model contribution changes the schema-validator
and URI-template identities; the core identity also includes the updated runtime
documentation source. Those three profile digests change the envelope fingerprint.
The root scope, policy, regex identity, and all other fixture data remain unchanged.

The type-error diagnostic and `UnknownBuiltin` cleanup also refreshes this fixture
through the generation command above. The model source contribution changes the
core, schema-validator, and URI-template implementation identities and therefore
the envelope fingerprint, without changing type encodings or graph contents.

Native float prompt parameters refresh this fixture through the same generator.
The changed model sources affect core, schema-validator, and URI-template
identities; the render adapter in `evaluate.rs` also affects core and regex
identities because that file contributes to both hashes. These four profile
identities change the envelope fingerprint. The empty graph has no templates;
its root scope, policy, and graph contents retain their original identities.
Existing non-float template bytes and digests are covered by unchanged goldens.

The expression API and diagnostic cleanup refreshes this fixture through the same
generator. Model/analyzer source edits affect core, schema-validator, and
URI-template identities; removing the unused construction context in `evaluate.rs`
also changes the regex identity because that adapter source contributes to its
hash. These four source-based identity changes alter the envelope fingerprint,
not the expression wire format. The root scope, policy, and graph contents remain
unchanged, as do the existing expression/template exact-byte and digest goldens.

Removing unused graph-model `ScopeContext` arguments refreshes the fixture through
the same generator. Graph/document model and analyzer source changes affect the
core, schema-validator, and URI-template implementation identities and therefore
the envelope fingerprint. The regex identity, root-scope digest, policy, and graph
contents remain unchanged. Graph record encodings and digests are independent of
use-site roles; the existing wire-format goldens are retained.

The graph-diagnostic prose cleanup refreshes this fixture through the same
generator. Editing the graph model's diagnostic source changes the core,
schema-validator, and URI-template implementation identities and the envelope
fingerprint. The regex identity, root-scope digest, policy, graph wire bytes,
and existing wire-format goldens remain unchanged.

Analyzer completion refreshes the fixture for policy-aware regex admission,
schema-root/structural hints, located reports and final retained-plan checks. The
regex identity now includes the analyzer contribution that owns shared compilation
rules. The independent checker explicitly covers the new diagnostic/regex modules.
Model pointer lookup changes also contribute to native identities. Regeneration
changes profile digests and the envelope fingerprint, while the empty root scope,
policy and existing canonical record goldens retain their identities.

Shared-owned admission refreshes the core identity through `verified.rs` and the
runtime exports. The borrowed and owned paths share exact verification logic;
schema, regex and URI-template engine identities retain their previous values.

Runtime identity support adds `identity.rs` to the explicit core source lists in
Rust and Node. Updating `native_profile.rs` also refreshes the regex/URI-template
source identities. `runtime-identities.json` is a separate known-answer corpus
checked by both Rust and an independent small CBOR encoder in the Node checker.
It covers all runtime §3/§8 domains with run `example-run`, node `work`, string
value `hello`, output `value`, and input `seed`; counters start at their specified
zero/one-based boundaries. These vectors do not depend on native source identity.

Manifest/lifecycle and stable-failure records add `records.rs` and `failure.rs` to
the core source coverage. Private input/request hashing is covered by `identity.rs`.
The fixture is refreshed through the same generator; graph and policy records and
the independent runtime identity vectors are unchanged.

SQLite authority adds `store.rs` and `store.sql` to both explicit core source lists.
The private schema is implementation data, not a portable executable graph table.
Updating profile coverage changes core/regex/URI-template source digests; native
schema, policy, root-scope bytes and runtime identity goldens are unchanged.

Atomic record transitions/reconstruction extend the explicit source coverage with
`store_transitions.rs` and `store_replay.rs`. Both contribute to the core digest;
the profile-source edit also refreshes regex/URI-template identities. Regenerate
and independently check the same native fixture after reviewing these modules.
