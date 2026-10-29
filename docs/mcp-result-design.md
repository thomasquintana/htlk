# Unified MCP tool results: implementation contract

This document fixes the semantics for the general MCP interoperability redesign.
It describes the target implementation; the task checkpoint and acceptance report
identify which parts have actually shipped and which checks have actually run.
The redesign is a first-version breaking change, with no legacy wire decoder,
binding variant, compatibility mode, or automatic development-state migration.

## Boundaries

Protocol validity establishes the shape of the exchange. Extraction identifies
candidate data. Inference describes observations. Runtime validation checks the
adopted contract. Task verification establishes whether the work succeeded.

All accepted `tools/call` outputs have the native type `McpToolResult`, regardless
of whether the upstream descriptor advertises `outputSchema`. Prompts/resources
retain their distinct existing native types. An advertised tool input schema is
still mandatory and object-rooted. Declared output schemas follow the pinned MCP
descriptor contract; inferred and application schemas may constrain any supported
JSON root. A server's absent output schema remains absent in its descriptor.

## Result value

The native record has the following fields. `?` denotes an absent optional field,
not a present JSON null. Snake-case names are the canonical envelope field names.

```text
McpToolResult {
  protocol_result: json,
  extraction: {
    status: "selected" | "none" | "ambiguous" | "exhausted",
    candidates: list<Candidate>,
    selected_candidate_id?: string,
    exhausted_limit?: string
  },
  data?: json,
  validation: {
    declared_schema: Assessment,
    inferred_schema: Assessment,
    application_schema: Assessment
  }
}

Candidate {
  id: string,
  value: json,
  sources: list<Source>,
  validation: "parsed"
}

Source {
  origin: "structured_content" | "text",
  method: "structured_content" | "whole_block" | "fence" | "embedded",
  content_block_index?: integer,
  byte_start?: integer,
  byte_end?: integer
}

Assessment {
  status: "not_configured" | "valid" | "missing" | "invalid",
  schema?: string,
  mode?: "enforce" | "report" | "observe"
}
```

`protocol_result` is the original admitted MCP result value with original field
names, content blocks, structured content, and permitted extensions. The durable
response retains exact received bytes separately. Native JSON ingress's documented
number normalization does not constitute inventing application content.

Candidates are unique by canonical JCS value. Their identifiers are `candidate_0`,
`candidate_1`, and so on in deterministic first-discovery order. All identical
occurrences are recorded in `sources`, deduplicated by the complete source record.
Overlapping occurrences remain identifiable through their spans; a valid enclosing
document suppresses independent nested extraction from inside it. Candidate
validation means strict JSON parsing only; schema compliance is an independent
assessment and must not be implied by the word "parsed".

`data` exists exactly when selection succeeds, and equals the selected candidate's
value. A selected `null` therefore has a present `data: null`. Cross-field checks
reject forged IDs, mismatching data, invalid source spans, duplicate candidates,
and inconsistent extraction states at independent native-value admission.

Immutable diagnostic evidence is referenced through existing run/invocation/
attempt and protected artifact mechanisms, rather than copying host timestamps,
mutable diagnostic objects, or raw secrets into executable meaning. Assessments
and bounded detailed diagnostic records carry schema/candidate/span references.

## Tool binding and host controls

One tool binding contains exact server identity, original descriptor digest, exact
tool name, input-schema digest, an explicitly nullable declared output-schema
digest, and a required `result_policy` record. Null in the binding means the
descriptor has no output schema; canonical ingress never supplies missing defaults.

The result policy contains:

- `extraction`: a complete canonical extraction policy record, including the
  implementation profile, limits, and selection record;
- `declared_contract`: `enforce` or `report`;
- `inferred_schema`: null or `{schema, evidence, mode}` with schema/evidence
  digests and `observe`/`enforce` mode;
- `application_schema`: null or an application-schema digest.

Authored host constructors may choose documented defaults. Canonical records
contain every field explicitly and reject unknown algorithms, malformed values,
and unsupported policy versions. Binding identity commits to the full record.
The linked native core identity commits to parser/inference/normalization source.

The compiler receives bounded tool policy selections from trusted explicit host
inputs keyed by exact server/descriptor selection. A catalog inventory is not
rewritten. Policies are frozen before dispatch and cannot be selected by tool
output. Source/canonical compilation and module inspection report adopted policies
and schema origins accurately. Conflicting exact selections are rejected.

Schema closure includes input schemas, actual declared output schemas, adopted
inferred schemas, required application schemas, and genuine transitive references.
The inference evidence document is a separate reached JSON document, not a schema
or an upstream descriptor. Its descriptor/schema/extraction-policy associations
are checked independently. Sample artifact authority belongs to the host's normal
provenance/authorization mechanisms; inference is not producer authentication.

## Extraction profile `htlk.json-extraction/1`

The default positive ceilings are 1 MiB inspected text, 64 distinct candidates,
depth 32, 8 MiB work units, 2 MiB cumulative parsed candidate bytes, 16 MiB charged
derived allocation, 4 MiB extracted representation, and 64 KiB detailed diagnostics.
Trusted hosts can tighten/raise supported limits explicitly. Codec, evaluator,
response-storage, and whole-executable limits remain independent upper bounds.
Every scan byte, parse attempt byte, and retained representation contributes to a
documented monotonic counter before the corresponding work/reservation. Parsing
depth cannot exceed the codec's supported depth. Never reset a budget per block.

Discovery order is original structured content, then content blocks by protocol
index. Inside a text block, attempt its trimmed whole value, then fenced regions
by byte position, then remaining embedded regions by byte position. Preserve
UTF-8 byte offsets into the original untrimmed text, with exclusive end offsets.
Do not concatenate blocks or inspect embedded-resource text as an unrelated text
block. Original image/audio/resource blocks remain in the protocol result.

Whole-block JSON accepts every JSON root. Fences use at least three matching
backticks or tildes, an equal-or-longer closing fence, and an empty or `json`
case-insensitive info string. Indentation is allowed; no heuristic stripping of
meaningful body characters. An unterminated fence does not establish a fenced
scalar boundary, though independent embedded containers can still be inspected.

Embedded scanning considers object/array starts only. It is quote/escape-aware,
uses bounded delimiter stacks, and never starts a separate object inside a quoted
string. Plain prose `42`, `true`, `false`, `null`, or quoted words do not create
scalar candidates; whole-block/fenced scalars do. Embedded arrays qualify as
containers, including arrays of scalar values.

Strict parsing uses HTLK's duplicate-aware JSON ingress and safe-number rules.
Balanced invalid containers do not stop inspection of later independent values.
If scanning nested starts for recovery from malformed input, charge every visit
and proposed parse before work; a fixed total work ceiling prevents quadratic
unbounded behavior. Nested candidates from a successfully parsed outer document
are not separate answers. Do not salvage malformed protocol envelopes at all.

Selection records support:

- `unique`: prefer original structured content, else exactly one distinct value;
- `candidate`: explicitly name a deterministic candidate ID;
- `block`: require exactly one candidate originating in a selected text block;
- `schema`: explicitly require exactly one candidate satisfying an adopted schema.

Any optional JSON-pointer projection is a separate checked, metered graph operation;
it does not silently transform an extracted candidate. Explicit selection is
recorded in executable meaning. More than one matching distinct value is ambiguous;
zero is none. Identical repeated documents are one candidate with several sources.
If any extraction ceiling is exhausted, retain bounded evidence but withhold
automatic and explicit text-derived selection: partial inspection cannot establish
complete extraction. `status` is exhausted, never none/selected. This first profile
also withholds selection of structured content on incomplete processing for one
simple conservative acceptance rule; original structured content remains available.

No candidates or ambiguity alone do not invalidate a protocol-success envelope.
Exhaustion may retain a generic envelope marked exhausted when the complete
bounded envelope itself fits all host limits; serialization/allocation/operational
failures remain failures. Required application/inferred constraints fail when
normalized data is unavailable. No such failure authorizes effect replay.

## Assessments and error precedence

Protocol validity is required before extraction. Invalid protocol has no accepted
envelope and is not searched for plausible JSON. For an admitted tool result,
bounded extraction/evidence can be computed even when the tool reports failure
or a declared contract is violated.

`isError:true` always means a failed tool invocation and no successful output
artifact. Successful local response processing is a separate fact. Tool-error
classification takes precedence over data/contract problems, while authorized
diagnostics retain secondary assessments and literal JSON candidates. Exhaustion
of mandatory host processing resources is never relabeled a schema mismatch.

With a declared schema, assess only original `structuredContent`: absent is
missing; a nonmatching value is invalid. Parsing matching text cannot establish
declared compliance. Enforce rejects missing/invalid. Report may publish the
generic envelope with the violation if all application-required checks pass.
No declared schema means not_configured, not a violation.

Adopted inference validates selected normalized data. Observe records valid/
missing/invalid and grants no refined application type. Enforce requires a present
matching value. Application schemas are always required constraints and use
enforce. Required constraint failure prevents success publication; raw response
and protected normalization evidence remain available for recovery.

Actual typed narrowing is already supported by checked expression/port boundaries:
project the optional `data` JSON into an explicitly required application result
type. The analyzer retains runtime obligations; it does not infer universal
guarantees from samples. Metered linked-library helpers additionally expose
original text, content kinds, candidates, explicit selection, and strict JSON
parsing, using complete pinned manifests and existing native registry mechanisms.

## Inference profile `htlk.schema-inference/1`

Inputs are bounded explicit sample bundles or host-selected eligible runtime/
historical artifacts. Defaults: 256 samples, depth 32, 1 MiB cumulative canonical
sample bytes, 65,536 visited nodes, 4 MiB charged derived allocation, and 1 MiB
schema/evidence output. Bounds count both merge work and serialization.

Sort immutable sample references/digests for stable evidence; count each explicitly
identified eligible observation once. Repeated references do not increase presence
counts. The same value from distinct actual observations can count more than once.
Sort resulting unions/property metadata deterministically; sample order does not
change schema/evidence identity. No timestamps enter inference meaning.

Structural rules:

- Null, boolean, string, integer, and number produce corresponding JSON Schema
  types. Mixed integer/nonintegral number generalizes to number; no ranges/enums.
- Merge object properties recursively. Presence counts use observations of that
  object position as denominator. Properties present in every relevant observation
  enter `required` as observed-required candidates. Keep `additionalProperties:true`.
- Missing properties do not imply null. Explicit null participates in type unions.
- Merge array element observations; empty arrays add no element constraint. If
  every observed array is empty, `items:true` means unknown, not an empty union.
- Mixed incompatible families use stable `anyOf` alternatives; compatible object/
  array structures merge within their family. Empty objects remain open objects.
- Do not infer string enums, formats, patterns, lengths, maxima/minima, or universal
  server promises from incidental observed values.

Produce an immutable revision: schema document plus evidence containing algorithm,
policy digest, sample count, source/artifact references and value digests, selected
descriptor digest, extraction-policy digest, per-path presence/type observations,
and completeness/error/truncation/uncertainty flags. Default admission excludes
error, truncated, uncertain, and incomplete observations. Explicitly separate error
inference from ordinary successful-result inference.

Sample selection respects source labels and existing access checks. Bundles do not
automatically capture entire private responses. Sanitized samples are explicitly
identified, with exclusions recorded; redaction must not silently alter inferred
meaning. Compiler admission never discovers or calls providers.

New observations can generate proposed immutable revisions and bounded diffs.
Existing executables, including future runs and crash recovery, keep their adopted
revision. Adopting another revision requires recompilation and a new executable
identity. Existing schemas generally accept added fields already; explicitly
defining even an optional property's type can change accepted values.

## Durability, first-version reset, and delivery

Persist exact received bounded responses and original labels before local
assessment. Persist/reconstruct normalization decisions under pinned implementations
without provider recall. Atomic artifact publication remains conditional on node
contracts, current authorization, and all mandatory checks. Preserve run/server
session ownership across local waits, cancellation, and explicit resource cleanup.

Advance storage interpretation/version and reject old stores without migration,
relabeling, deletion, or unsafe run resumption. Regenerate reviewed executable
fixtures with new implementation identities. The user authorizes an explicit
inventory and removal of obsolete disposable HTLK development artifacts; stop
owners and name exact paths before removal. Credentials, sibling application data,
personal browser profiles, and unrelated external resources are not reset targets.

Acceptance must use actual unchanged sibling Chrome DevTools MCP, controlled page
markers, real text/image/error responses, exact descriptors and same-session calls.
There is no separate Browser MCP/extension requirement. Independent stdio/HTTP
JSON/SSE peers prove guaranteed adversarial/recovery cases separately. Record
actual revisions, fingerprint, run/call counts, checks, cleanup and blockers;
fixture success is not product evidence. Full repository quality, implementation
identity, coverage, packaging and license gates remain unchanged.
