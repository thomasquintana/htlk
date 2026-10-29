# Metered MCP content and JSON helpers

The explicitly linked pure library `htlk.mcp-content`, version `1`, provides a
single consumer API for text, structured and multimodal `McpToolResult` values.
It uses the existing complete-manifest/native-registry mechanism. Creating an
empty `NativeRegistry` does not implicitly install it.

## Link the exact implementation

```rust
let library = link_mcp_content_library(&mut registry, &limits)?;
config.libraries.insert(library.implementation_digest(), library.clone());
```

Use the returned identity in an ordinary source library declaration:

```rust
let source = format!(r#"
ir_version="0.1"
predicate_library content=predicates.library("htlk.mcp-content") {{
    version="1"
    digest="{}"
}}
graph example.title {{
    inputs={{response=McpToolResult}}
    outputs={{title=string}}
    nodes {{
        title=eval(string,content.checked(
            content.json_at(content.data(inputs.response),"/title")
        )) {{ inputs={{response=McpToolResult}} }}
    }}
    edges {{
        edge input {{ from=inputs.response to=title.inputs.response }}
        edge title {{ from=title.outputs.value to=outputs.title }}
    }}
}}
"#, library.implementation_digest());
```

The language's `predicate_library` declaration selects complete pure libraries;
their function signatures are not restricted to Boolean results. The example's
`checked` return type is inferred from the explicit string annotation and is
validated on the actual returned value. Missing data fails the required check.

Print the actual current manifest without inventing a digest:

```console
cargo run -p htlk-runtime --example mcp_content_manifest --locked
```

Library identity binds its source and model implementation contribution. The
executable separately pins the evaluator/regex engines and the complete manifest.
No compiled regex, function pointer or dynamically loaded code is serialized.

## Complete version-1 inventory

`optional(json)` below denotes legitimate absence, not a present JSON null.

| Function | Signature | Behavior |
|---|---|---|
| `text_blocks` | `McpToolResult -> list(string)` | Original direct text blocks, in protocol order; no joining |
| `content_kinds` | `McpToolResult -> list(string)` | Every original block kind, including mixed media/resources |
| `structured_content` | `McpToolResult -> optional(json)` | Original `structuredContent`, independently of normalized data |
| `candidates` | `McpToolResult -> list(json)` | Candidate records with original evidence locations |
| `data` | `McpToolResult -> optional(json)` | Only the binding policy's selected normalized value |
| `validation` | `McpToolResult -> json` | Separate declared/inferred/application assessments |
| `select_candidate` | `(McpToolResult, string) -> optional(json)` | Explicit exact candidate ID; does not rewrite the envelope/policy |
| `parse_json` | `string -> json` | Strict duplicate-aware HTLK JSON ingress; no semantic repair |
| `json_at` | `(json, string) -> optional(json)` | Explicit RFC 6901 pointer; missing target is absent, malformed/index/type traversal errors fail |
| `checked` | `json -> T` | Identity plus the registry's actual instantiated return-type validation; no coercion |
| `join_text` | `(list(string), string) -> string` | Explicit deterministic separator joining, including cross-block assembly |
| `text_contains` | `(string, string) -> boolean` | Literal substring check, useful for task markers |
| `regex_capture` | `(string, regex, integer) -> optional(string)` | Require at most one full regex match, then select a capture; ambiguity/index errors fail |

For regex capture, index zero denotes the whole match. No match or an unmatched
optional capture is absent. Several matches never silently choose the first.
The pinned Rust regex profile and compiler-size limits apply. This is an explicit
authored text operation, not automatic scalar extraction or prose interpretation.

## Metering and protection

Variable scanning, parsing, collection visits, copying, joining and regex work are
charged before the corresponding operation. JSON parsing/pointers preserve typed
redacted causes; resource exhaustion remains `E_EXPRESSION_LIMIT`, never a false
match or successful empty value. Regex compilation reserves its configured size
allowance, so host step budgets must accommodate the requested operations.

All functions are pure. They inspect no clocks, filesystem, credentials, network,
mutable learning state or language model. Standard checked call boundaries validate
arguments before dispatch and results afterward. Normal actual-read influence and
artifact lineage carry input protection into outputs; parsing cannot declassify it.

An explicit `select_candidate` or `parse_json` result remains unrefined JSON until
checked. It cannot establish original declared-schema compliance, change
`isError:true`, adopt an inferred revision, authorize a tool, or replay an effect.
Supplying a well-shaped result as ordinary graph data does not authenticate its
MCP producer; [passive inference](mcp-evidence.md) independently checks provenance.
