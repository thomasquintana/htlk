# Contributing

Contributions to Harness Toolkit are welcome.

## Development

The repository's `rust-toolchain.toml` selects latest stable Rust and installs
the required formatting and linting components.

Before opening a pull request, run:

```console
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
cargo package --workspace --locked
```

Every public API should include rustdoc. Behavior changes should include tests
and an entry under the `Unreleased` section of `CHANGELOG.md`.

Package all workspace crates together: Cargo stages local packages so dependent
crates can be verified before their new versions are published. For a local
pre-commit package check, add `--allow-dirty`; CI checks the clean checkout.

When `Cargo.lock` changes, install `cargo-about` and regenerate third-party
notices:

```console
cargo install cargo-about --locked --features cli
cargo about generate about.hbs --workspace --locked --fail --output-file THIRD_PARTY_LICENSES
```

## Releases

All four crates share one version. To release:

1. Replace `TBD` in `CHANGELOG.md` with the release date.
2. Update every workspace dependency version when changing the workspace version.
3. Run the complete local and CI validation suite.
4. Create and push an annotated tag matching the package version, such as `v0.1.0`.

The release workflow publishes `htlk-cbor` first and waits for registry
availability, then publishes `htlk-compiler` and `htlk-rt`, waits for both, and
publishes the `htlk` facade. All four packages are verified together before
publication. The GitHub repository must provide a protected `crates-io`
environment with a `CARGO_REGISTRY_TOKEN` secret authorized to publish them.
