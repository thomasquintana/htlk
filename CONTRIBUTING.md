# Contributing

Contributions to Harness Toolkit are welcome.

## Development

The repository's `rust-toolchain.toml` selects latest stable Rust and installs
the required formatting and linting components.

Before opening a pull request, run:

```console
node specs/validate-specs.mjs
node tools/validate-executable-fixtures.mjs
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
cargo package --workspace --locked --target-dir "$(mktemp -d)"
```

Every public API should include rustdoc. Behavior changes should include tests
and an entry under the `Unreleased` section of `CHANGELOG.md`.

Package all workspace crates together: Cargo stages local packages so dependent
crates can be verified before their new versions are published. For a local
pre-commit package check, add `--allow-dirty`; CI checks the clean checkout.
Use a fresh packaging target directory when checking edited unpublished versions:
Cargo can otherwise reuse a cached same-version staged dependency from a previous
package check. Archives are written below the chosen target's `package/` directory.
CI and release validation use commit-specific temporary target directories.

When `Cargo.lock` changes, install `cargo-about` and regenerate third-party
notices:

```console
cargo install cargo-about --locked --features cli
cargo about generate about.hbs --workspace --locked --fail --output-file THIRD_PARTY_LICENSES
```

## Releases

### CI documentation and validation artifacts

Pull requests and pushes to `main` build all workspace API docs with warnings
treated as errors. Download the `rustdoc-<commit SHA>` artifact from the CI run;
after extracting it, open `htlk/index.html` or `htlk_executable/index.html`.
CI artifacts are retained for 30 days. CI also checks workflow syntax and the
tracked Draft 0.1 specifications.

The Release workflow builds the same docs and uploads `release-rustdoc-<SHA>`
and `crates-<SHA>` artifacts, retained for 90 days. Documentation and package
validation must succeed before the publishing job can start.

Run release validation without publishing from **Actions → Release → Run workflow**,
leaving `dry_run` enabled, or with GitHub CLI:

```console
gh workflow run release.yml --ref main -f dry_run=true
```

This runs formatting, Clippy, tests, docs, spec checks, dependency/license policy,
notice comparison, and verified workspace packaging. It does not require the
crates.io environment or token. It uses workspace packaging rather than individual
`cargo publish --dry-run` calls so unpublished local dependency versions can be
validated together. The workflow must first be present on the default branch.

### crates.io configuration

Create a GitHub Actions environment named `crates-io` and add its secret
`CARGO_REGISTRY_TOKEN`, containing a crates.io token authorized to publish all five
workspace crates. Existing crates also require the corresponding crates.io account
or team ownership. Only the publishing job receives this token.

All packages opt into all-feature docs.rs builds. After each package is published,
docs.rs builds and hosts its versioned documentation automatically, for example
`https://docs.rs/htlk-executable/0.1.0/htlk_executable/`. This is separate from the
downloadable CI documentation; no GitHub Pages setup is required.

### Publishing a version

All five crates share one version. To release:

1. Replace `TBD` in `CHANGELOG.md` with the release date.
2. Update every workspace dependency version when changing the workspace version.
3. Commit and push the release contents; run the complete local and CI validation suite.
4. Create and push an annotated tag matching the package version, such as `v0.1.0`:

   ```console
   git tag -a v0.1.0 -m "Release v0.1.0"
   git push origin v0.1.0
   ```

Pushing a `vMAJOR.MINOR.PATCH` tag starts validation followed by publication.
The tag must match the single version reported by all workspace packages.
Manual validation can run on a branch; manual publishing requires a matching tag:

```console
gh workflow run release.yml --ref v0.1.0 -f dry_run=false
```

The release workflow publishes `htlk-executable` first and waits for registry
availability, then publishes `htlk-analyzer` and waits for it, then publishes
`htlk-compiler` and `htlk-rt`, waits for both, and publishes the `htlk` facade.
All five packages are verified together before publication. If publication stops
partway through, rerun the failed publishing job after fixing its cause; the workflow
skips crate versions already published. A changed package needs a new version and tag.
