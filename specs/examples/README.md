# Decision-brief example

This advanced workflow accompanies the [user guide](../../docs/user-guide.md#study-the-decision-brief-workflow)
and targets HTLK IR Draft 0.1. It researches opportunities and risks, then drafts
and reviews a brief in a bounded loop.

| File | Purpose |
|---|---|
| [decision_brief.htlk](decision_brief.htlk) | Complete source: types, prompts, tasks, and entry graph. |
| [decision_brief.catalogs.json](decision_brief.catalogs.json) | Three compiler catalogs with illustrative tool descriptors. |
| [decision_brief.inputs.json](decision_brief.inputs.json) | Tagged host input map containing the question. |
| [modules/](modules/README.md) | Equivalent five-file, three-package source bundle. |

The catalogs contain no service implementations, endpoints, credentials, or model
configuration. Tests provide controlled tools matching these contracts. For real
services, discover actual descriptors, adapt the source/policy, and recompile.
HTLK's [Rust host APIs](../../docs/runtime-guide.md) execute the result; there is no
general `htlk run` command.

## Read the workflow

`research.find_evidence` accepts a query and returns checked nonempty evidence.
`brief.refine` carries the prior draft and review feedback through at most three
iterations. A valid negative review is a successful response and permits revision;
an explicit tool error or enforced output-contract violation is an operational
failure. The final graph selects an accepted brief or an explicit needs-attention
result through ordinary guarded edges.

Every tool returns `McpToolResult`. These fixtures advertise output schemas, so
default Enforce checks original `structuredContent`. The source explicitly reads
and narrows selected `data`; recovered text does not establish declared compliance.
Schemas check representation, not factual correctness or usefulness of feedback.

## Bounds and behavioral evidence

There are two research calls and at most three draft/review pairs, with no authored
MCP retries. Acceptance on review one/two/three uses 4/6/8 calls; all-rejected fallback
uses 8. The root ceiling is eight calls, the refinement ceiling six. Tighter host
limits and deadlines can stop execution earlier. Maximum scope depth is three;
the expanded invocation bound is 34, including all guarded branches and iterations.

| Scenario | Expected result | Test source |
|---|---|---|
| Accept at review one, two, or three | Accepted reviewed draft; exact call bound | `tests/runtime_mcp.rs` |
| Reject all three | `E_LOOP_LIMIT` in refinement; successful root `needs_attention` | `tests/runtime_mcp.rs` |
| Retrieval or structured-schema failure | Ordinary failed dependency/fallback path | `tests/runtime_mcp.rs` |
| Empty question | Root precondition failure before child work | `tests/runtime_mcp.rs` |
| Host cancellation | Fenced cancellation, without promised fallback | `tests/runtime_mcp.rs` |
| Standalone and modular native HTTP JSON/SSE | Same outputs and exact call bounds | `tests/deep_e2e.rs` |
| Standalone/modular compilation | Equal canonical/envelope bytes; depth 3 and bound 34 | `tests/compiler.rs` |

These are controlled runtime regressions, not claims about live model quality.
Run the focused suite from the repository root:

```console
cargo test --test runtime_mcp decision_brief_ --locked
cargo test --test compiler --locked
```

Enclosing deadlines/budget stops need not produce fallback. A successful
`needs_attention` report is not an accepted recommendation. General response recovery,
unsafe delivery, extension, and retained-artifact cases use the shared engine suites
listed in [testing](../../docs/testing.md#conformance-map).

`node specs/validate-specs.mjs` checks source syntax, referenced tool fixtures,
module snapshots/digests, and standalone/modular correspondence. Compilation and
execution are separate Rust checks. The source file is the example authority;
the guide links to it instead of maintaining a second full copy.
