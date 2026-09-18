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
`cargo run -p htlk-rt --example native_fixture --locked`, update the fixed
fixture from its output, and rerun both independent and Rust checks. Never repair
or regenerate fixtures automatically as part of tests.

Source identities include package-owned model and analyzer contributions as well
as runtime adapters. The independent checker maintains explicit source lists and
hash framing for each contribution, including the pinned MCP schema snapshot.
