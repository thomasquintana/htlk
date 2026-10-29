# Protected MCP evidence and passive inference

Tool results use the [uniform result contract](mcp-result-design.md). Successful
protocol processing, accepted tool output, and task verification are separate facts.
An action may have happened even when its result cannot satisfy a required check.

## Durable local normalization

The runtime records the original bounded response before extraction or acceptance.
Every tool attempt then records an immutable Observation named
`<attempt-id>:normalization` and yields before publishing successful artifacts.
The record binds the original response and binding digests, normalized envelope
digest, independent schema assessments, extraction state, candidate counts,
bounded value-digest/span references, processing/semantic failure, reception,
conservative effect uncertainty, and trusted protection labels.

Candidate and parse details share the pinned `diagnostic_bytes` allowance. Fixed
decision/provenance metadata is separately bounded by the complete storage codec.
Truncated details are explicitly marked. Original response bytes and pinned
implementations retain enough information to reconstruct the full decision without
calling the provider. Diagnostic metadata never becomes a successful tool artifact.

After restart, local reconstruction checks the immutable decision before accepting
output. A previously recorded operational processing failure remains a failure;
temporary local recovery does not authorize another external operation. Current
authorization/deadlines can still prevent publication. They do not change the
frozen extraction/validation meaning of a dispatched invocation.

## Explicit authorized evidence reads

`Runtime::inspect_mcp_evidence` selects one exact run/invocation/attempt. It checks
current node/evidence authorization and unions source, response, decision and
outcome labels. It does not dispatch, retry, publish an artifact or adopt a schema.

```rust
let evidence = runtime
    .inspect_mcp_evidence(&caller, &run_id, invocation_id, attempt_id, 65_536)
    .await?;
```

The returned `McpEvidenceView` distinguishes:

- Original `failure_code` and immutable `recorded_normalization`.
- Exact `raw_digest` and authorized original result-object bytes.
- `raw_truncated` when only a byte prefix fits; that prefix is not a valid result
  to salvage or parse as though it were complete.
- Reconstructed `normalized` evidence and `normalized_truncated`.
- A separate `reconstruction_failure`, including missing pinned implementations,
  without rewriting the original outcome.
- `response_received`, delivery evidence, and `effect_outcome_uncertain`.

The byte ceiling bounds returned raw bytes, persisted normalization representation
and reconstructed envelope representation, not exact process heap use. It must be
positive and no larger than the runtime codec ceiling. Underlying trusted storage
metadata scans may internally decode records; their projections expose no raw
contents before authorization. Accepted artifact reads retain their separate
metadata-before-blob authorization guarantee.

Debug formatting omits original and extracted values. Applications must preserve
the returned labels when handling or publishing explicitly requested evidence.
Full raw results are not automatically copied into public history, inference
reports, or graph-visible error strings.

For `isError:true`, useful literal JSON can appear in reconstructed evidence while
the invocation remains failed and has no successful output artifact. Receiving or
parsing that error is not evidence that an external action did not occur.

## Passive schema proposals from actual invocations

`Runtime::infer_mcp_results` accepts explicit `McpInferenceSelection` values:

```rust
let selection = McpInferenceSelection { run_id, invocation_id };
let proposal = runtime
    .infer_mcp_results(&caller, &[selection], &InferencePolicy::default())
    .await?;
```

Ordinary sampling requires an accepted, complete, unambiguously selected JSON
value from a genuine MCP `NodeOutput`. The runtime independently checks producer,
port, native result type, original executable and binding context. An arbitrary
graph JSON object claiming to be a tool response is insufficient. Every source
still requires current inspection and artifact access. Missing, ambiguous or
incomplete data is rejected instead of creating a fictional sample.

The returned `ProtectedInferenceRevision` includes the proposed immutable revision,
unioned `SecurityLabels`, exact source selections, and conservative external-effect
uncertainty. Schema/evidence reports contain source/value identities and structural
observations, not the original private values. Use existing protected artifact and
provenance mechanisms when publishing a proposal; plain graph data cannot assign
its own classification or authorization.

`InferenceSampleKind::ToolError` explicitly selects the separate error-body path.
It requires authoritative `isError:true` evidence and complete selected JSON.
A known complete JSON error observation can coexist with an uncertain external
effect. Observation eligibility and effect certainty are different facts; neither
authorizes replay. The default successful-result inference never silently mixes
these error samples into its schema.

Sampling and inference read already recorded observations and perform no MCP
calls. A live sample remains a separately authored, authorized, budgeted execution.
An inference mismatch or proposal never repeats its source action.

## Explicit adoption and evolution

New observations produce proposals, not modifications to active executables.
For adoption, explicitly select the revision and Observe/Enforce semantics in
trusted compiler inputs, supply its schema/evidence documents, recompile, and
independently select the same tool policy in the runtime host.

Observe records deviations without granting a refined type. Enforce requires the
actual selected value to satisfy the adopted schema. Mandatory application checks
always apply. Old revisions and their immutable identities remain inspectable;
`InferenceRevision::difference` reports bounded changed schema paths. Additional
fields often already satisfy an older open-object contract and need no adoption.
