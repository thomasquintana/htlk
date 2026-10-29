# First-version executable and development-state rebuild

The unified tool-result redesign is a breaking first-version change. There are no
legacy binding variants, old-format decoders, automatic store migrations, or
runtime cleanup-on-open. Private SQLite schema version 7 rejects earlier stores.
Native interpretation identities also change when their pinned source changes.

## Rebuild disposable state explicitly

Identify the exact disposable HTLK store and stop its coordinators first. Complete
any needed test-owned external cleanup/reconciliation using its existing evidence.
The following is an operator command for an already identified disposable store;
it is not executed by the runtime:

```console
HTLK_DISPOSABLE_DB="/absolute/path/to/identified-disposable-htlk.sqlite"
rm -f -- "$HTLK_DISPOSABLE_DB" "$HTLK_DISPOSABLE_DB-wal" "$HTLK_DISPOSABLE_DB-shm"
```

This reset discards that development history; it does not replay or undo effects.
Credentials, `.env` files, unrelated databases, sibling application data and
personal browser profiles are not HTLK reset targets. An incompatible open never
deletes or silently reinterprets anything. Existing effectful runs must not resume
under a different interpretation merely because new binaries were built.

Build and register newly compiled executables under the current host selections:

```console
cargo build --workspace --all-features --locked
node specs/regenerate-module-example.mjs --write
node specs/validate-specs.mjs
```

Compiler hosts provide the new explicit `mcp_result_policies` and
`mcp_result_documents` fields; runtime hosts independently select the corresponding
`McpPolicy.result_policy`. Every tool returns `McpToolResult`; application graphs
explicitly select/narrow `data`. No per-provider compatibility shim is required.

## Review implementation fixtures

After an intentional native implementation change, print and review a new golden:

```console
cargo run -p htlk-runtime --example native_fixture --locked
```

Update the fixed fixture from reviewed output, then independently verify it:

```console
node tools/validate-executable-fixtures.mjs
cargo test -p htlk-runtime --test verified --locked
```

Tests must not regenerate their own goldens. Source-package manifests and captured
bundle contents must remain synchronized; the explicit module-example generator
updates only that fixture. New inference revisions likewise require deliberate
adoption/recompilation and never alter old executables or dispatch sample calls.
