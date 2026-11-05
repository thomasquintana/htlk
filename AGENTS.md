# Repository instructions for coding agents

Use [CONTRIBUTING.md](CONTRIBUTING.md) for prerequisites, Rust design conventions,
validation, and releases. Use [docs/README.md](docs/README.md) to find the current
technical authorities. README and user guides address human readers.

## Working constraints

- During the active MCP client implementation, consult
  `.opencode/.tmp/plans/mcp-client.md` after compaction and after every task. Update
  its progress after verification, commit each completed task, and follow its
  milestone dependencies. The temporary plan is local and ignored by Git.

- Preserve compiler/runtime independent admission and transactional trust checks.
- Preserve lazy reads: unused branches and unselected sources must not access values,
  inspect irrelevant checkpoints, or dispatch operations.
- Native identities hash explicit source contributions. Review both Rust identity
  manifests and the independent Node checker when touching covered sources. Follow
  [fixture maintenance](docs/testing.md#native-identity-and-fixtures); tests must never
  regenerate their own goldens.
- Module source files and captured bundle strings are intentional test assets.
  Keep snapshots and exact package pins synchronized using the explicit generator.
- Put agent-only instructions here, shared contracts in specifications, and human
  procedures in guides. Remove completed working plans instead of retaining handoffs.
- For behavior changes, test an independent observable invariant and update the
  project changelog. For documentation changes, run documentation/spec checks and
  affected doctests/examples, including packaging when embedded assets move.
