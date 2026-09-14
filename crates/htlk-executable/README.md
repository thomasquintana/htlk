# HTLK Executable

Shared executable-format foundations for the HTLK compiler and runtime.
Provides validated local identifiers, canonical types and ports, a checked
version-0.1 executable envelope, and SHA-256 digest primitives in the public
`digest` module. Envelope checks cover
the outer schema, supported
format/version, and fingerprint; registration separately verifies graph contents
and producer trust.

Execution-limit and retry-policy records describe the controls later enforced
by the runtime; these are configuration records, not running counters or timers.

Depends on `htlk-cbor` for deterministic encoding and RustCrypto's `sha2` for
SHA-256; the codec does not depend on this crate.

## Local identifiers

`Identifier` is an immutable local name with exact ASCII spelling
`[a-z][a-z0-9]*(_[a-z0-9]+)*`. Names begin with a lowercase letter; later segments
may begin with digits. Leading, trailing, and repeated underscores are invalid.
Parsing performs no trimming, case folding, or Unicode normalization.

```rust
use std::collections::HashMap;
use htlk_executable::{Identifier, ParseIdentifierError};

let name = Identifier::new("draft_2".to_owned())?;
assert_eq!(name.as_str(), "draft_2");
assert!("Draft_2".parse::<Identifier>().is_err());
let lookup = HashMap::from([(name.clone(), 42)]);
assert_eq!(lookup.get("draft_2"), Some(&42));
assert_eq!(name.into_string(), "draft_2");
# Ok::<(), ParseIdentifierError>(())
```

Owned construction (`new` or `TryFrom<String>`) validates without copying.
`FromStr` and `TryFrom<&str>` validate before fallible allocation. The type also
supports Display, Debug, equality, lexical ordering, hashing, `AsRef<str>`, and
`Borrow<str>`. Consuming `into_string` provides the exact name for serialization;
there is no additional CBOR tag or wrapper.

`ParseIdentifierError` distinguishes empty names, invalid first bytes, invalid
characters, invalid underscore placement, and allocation failure. Character and
separator errors carry zero-based byte offsets. Checks proceed left-to-right,
then reject a trailing underscore. Errors contain no submitted name. Callers
apply source/codec size ceilings; the lexical rule has no separate length cap.

Identifier validity is not node/declaration validity. Reserved roots such as
`inputs` and contextual keywords such as `graph` are lexically valid; consumers
apply restrictions for node names, library aliases, declarations, or imports.
Scope resolution and uniqueness are likewise separate checks. Qualified names,
module paths, and arbitrary quoted record-field names need their own handling.
Identifier ordering is ordinary ASCII/UTF-8 order; canonical CBOR map ordering
is independently applied by the codec.

## Canonical types and ports

`ValueType` describes permitted data, whereas `htlk_cbor::Value` holds actual
data. It exposes a read-only `ValueTypeKind`: primitive, list, map, record, union,
enum, schema digest, signature variable, or function type. `PrimitiveType` contains
the exact closed primitive names from the CDDL. Source aliases (including `text`)
must be resolved by the compiler before reaching this canonical model.

```rust
use htlk_cbor::Limits;
use htlk_executable::{Port, PrimitiveType, TypeContext, ValueType, ValueTypeKind};

let limits = Limits::default();
let context = TypeContext::Value;
let nullable = ValueType::new(ValueTypeKind::Union(vec![
    ValueType::primitive(PrimitiveType::String),
    ValueType::primitive(PrimitiveType::Null),
]), context, &limits)?;
let port = Port::new(nullable, false);
assert!(!port.required());
let bytes = port.encode(context, &limits)?;
assert_eq!(Port::decode(&bytes, context, &limits)?, port);
# Ok::<(), htlk_executable::TypeError>(())
```

Construction with `ValueType::new` normalizes authored shapes: record fields
acquire canonical map order, unions flatten/sort/deduplicate, and enums sort by
decoded UTF-8 bytes. The syntax specification requires **at least two distinct
union members after normalization**: singleton/empty results are errors, not a
coercion to their member. Enums must be nonempty and unique; duplicates are errors.
Enum strings and record field names retain exact Unicode spelling, including
empty strings. A record field is a `Port`; graph/node port-table names will use
`Identifier` in their containing records.

`decode` and `from_value` require canonical records and never normalize malformed
or noncanonical type structure into acceptance. `to_value` produces the canonical
CBOR value; `encode` produces its bytes. All these boundaries take `TypeContext`
and the existing CBOR `Limits`. `Port::new` combines an already normalized type
with its presence flag; context and whole-port limits are checked when it is
encoded, decoded, or embedded in a larger type.

| Model | Canonical representation |
|---|---|
| Primitive string | `"string"` |
| List of strings | `["list", "string"]` |
| String-keyed integer map | `["map", "integer"]` |
| Nullable string | `["union", ["null", "string"]]` |
| String enumeration | `["enum", ["brief", "detailed"]]` |
| Record fields | `["record", { field_name: { type: ..., required: ... } }]` |
| Schema reference | `["schema", "sha256:..."]` |
| Signature variable | `["var", "t"]` |
| Signature function | `["function", [parameter_ports...], return_port]` |

Ports contain exactly `type` and `required`. Both fields are mandatory, including
`required: false`; unknown metadata fields fail. Requiredness is independent of
nullability: optional permits absence, while nullable permits a present null.
Function parameter order is preserved, and result requiredness describes whether
the function can return absence. Union members sort by encoded type bytes, not
by their names; this differs from enum string ordering.

### Context and validation boundary

`TypeContext::Value` recursively prohibits variable and function constructors.
`TypeContext::Signature` permits those shapes for library signatures. Context is
chosen by the consuming schema, not supplied by a field in untrusted data.

Schema lookup, reached-schema-root restrictions, generic-variable declarations,
unification/recursive-type rejection, assignability, and validation of actual
runtime values remain graph-verifier/evaluator work. A shape-valid schema digest
does not prove that its document exists; a parsed signature variable does not
prove that a library declared it.

### Type resource accounting and errors

Before allocating conversion strings or growing collections, a private builder
checks the actual CBOR text/array/map/Boolean representation against codec limits,
including map keys, tag strings, presence flags, encoded lengths, and depth.
Parsing begins only after bounded CBOR decoding (or bounded encoding for an
existing Value). Normalization temporarily holds source members and sorting keys,
bounded by the checked input's total values/bytes; its final result must also fit
the configured limits, including an expanded flattened-union array. Codec byte
string limits do not constrain a type named `bytes`, which is text metadata.

`TypeError` retains codec, digest, and identifier causes and static schema error
categories. Codec errors preserve offsets; a graph/source consumer can attach its
own record location to higher-level type errors. Errors contain no submitted
field names, enum contents, or other input values. Conversion-limit errors are
reported before constructing an oversized temporary CBOR value.

Depth regression tests run in subprocesses on 512 KiB and 2 MiB thread stacks.
They exercise the 128 codec-depth ceiling with list chains, function signatures,
unions, construction/encoding/cloning/formatting, and cleanup after a nested field
failure. Collection-specific parsing/conversion helpers keep recursive dispatch
frames small. These tests provide headroom, not a guarantee for arbitrary caller
stack sizes. Clone/Debug of caller-owned objects are ordinary Rust operations.

## Execution limits and retry policies

`ExecutionLimits` represents the optional local `limits` record on a node or
scope. `new()` and `default()` produce an empty map: each omitted field inherits
its applicable ceiling. An explicit zero call/token/cost budget remains present
and is different from omission. Timeouts and concurrency must be positive.
All values are integers in `0..=i64::MAX`; floats, strings, and null are rejected.

| Field | Meaning | Zero permitted? |
|---|---|---|
| `timeout_ms` | Admitted node/scope duration ceiling | No |
| `attempt_timeout_ms` | Duration ceiling for each MCP attempt | No |
| `max_mcp_calls` | Scope/descendant dispatch-count budget | Yes |
| `max_tokens` | Token budget when enforceable | Yes |
| `max_cost_units` | Budget in the profile's cost units | Yes |
| `max_concurrency` | Concurrent descendant MCP dispatch ceiling | No |

Checked, consuming `with_*` methods set fields; matching read-only getters return
`Option<u64>`. `to_value`/`from_value` and `encode`/`decode` use the existing codec
`Limits`. Unknown fields fail; optional fields are omitted rather than emitted
as null. Numeric validation follows field-name UTF-8 order after unknown-field
checking. An empty local record is valid; the containing policy-profile validator
will require its defaults to include timeout, attempt timeout, and concurrency.

```rust
use htlk_cbor::Limits;
use htlk_executable::{ExecutionLimits, RetryPolicy};

let codec_limits = Limits::default();
let local = ExecutionLimits::new()
    .with_timeout_ms(60000)?
    .with_max_mcp_calls(3)?
    .with_max_concurrency(1)?;
assert_eq!(local.max_tokens(), None); // Inherit; not zero or unlimited.
let wire = local.encode(&codec_limits)?;
assert_eq!(ExecutionLimits::decode(&wire, &codec_limits)?, local);

let retry = RetryPolicy::new(3,
    vec!["MCP_TRANSPORT".into(), "MCP_TIMEOUT".into()],
    vec![1000, 5000], &codec_limits)?;
assert_eq!(retry.on(), ["MCP_TIMEOUT", "MCP_TRANSPORT"]);
assert_eq!(retry.backoff_ms(), [1000, 5000]);
assert_eq!(RetryPolicy::decode(&retry.encode(&codec_limits)?, &codec_limits)?, retry);
# Ok::<(), htlk_executable::ExecutionOptionsError>(())
```

`RetryPolicy` always serializes all three fields: positive `max_attempts`, array
`on`, and array `backoff_ms`. The attempt count includes the initial dispatch;
there must be exactly `max_attempts - 1` delays. Delay entry k-2 precedes attempt
k (one-based). Delays are nonnegative i64-range integers, may decrease or be zero,
and retain their authored order. The parser never allocates from an attempt count.

Construction checks codec limits on the raw policy before sorting/deduplicating
the code set. Codes are opaque exact strings (including custom codes), not
identifiers, patterns, or authority grants. Canonical ingress rejects duplicate
or out-of-order codes rather than repairing them. Unicode spelling is preserved.
`RetryPolicy::no_retry()` and `default()` represent one attempt and empty lists:

```text
{ max_attempts: 1, on: [], backoff_ms: [] }
```

Strict parsing checks unknown fields, missing fields, and top-level native types,
then numeric range, delay cardinality/content, and code types/order. Missing/type
checks use `backoff_ms`, `max_attempts`, `on` order. No exponential policy, jitter,
wildcard matching, or default list contents are inferred from supplied records.

`ExecutionOptionsError` distinguishes schema, numeric, delay-count, order, codec,
conversion-limit, and allocation failures. Errors omit submitted field names and
code strings; codec errors retain byte offsets through their error source.
Private `RecordAccounting` is shared with type conversion and bounds complete
record sizes, text, integers, collections, and depth before conversion copies.
This accounting is per codec operation, not runtime usage accounting.

The runtime still computes effective ceilings, starts persisted deadlines after
admission, reserves and accounts for dispatch usage, checks hard token/cost
enforceability, and authorizes replay based on delivery state, operation policy,
and exact approvals. A valid policy alone does not authorize a retry or revive
a terminal invocation. Retry attachment to MCP-only operations is checked by
the future containing operation schema.

## Executable envelope API

```rust
use htlk_cbor::Limits;
use htlk_executable::ExecutableEnvelope;

let limits = Limits::default();
// Payload is opaque here; CBOR null is not a compiled graph.
let envelope = ExecutableEnvelope::new(vec![0xf6], &limits)?;
assert_eq!(envelope.format(), "htlk.executable.graph");
assert_eq!(envelope.version(), "0.1");
assert_eq!(envelope.fingerprint().to_string(),
    "sha256:9b039893d4db25c42f0765d6a31877987d90ff86f07ca593af128ca23500120d");
let bytes = envelope.encode(&limits)?;
let decoded = ExecutableEnvelope::decode(&bytes, &limits)?;
assert_eq!(decoded.payload(), &[0xf6]);
assert_eq!(decoded, envelope);
# Ok::<(), htlk_executable::EnvelopeError>(())
```

`ExecutableEnvelope` has private fields and read-only `format`, `version`,
`fingerprint`, and `payload` accessors. It retains a checked record for encoding
without copying its payload; Debug shows metadata and payload length only.
`EnvelopeError` exposes structured codec, schema, format/version, and fingerprint
failures. `EXECUTABLE_FORMAT` and
`EXECUTABLE_VERSION` expose the supported constants.

## Envelope wire contract — version 0.1

Exactly four required fields form a canonical CBOR map:

| Field | Required representation |
|---|---|
| `format` | Text, exactly `htlk.executable.graph` |
| `version` | Text, exactly `0.1` |
| `fingerprint` | Text, `sha256:` plus exactly 64 lowercase hex digits |
| `payload` | Byte string, preserved exactly; empty is permitted |

Unknown fields are rejected. Canonical wire key order is `format`, `payload`,
`version`, `fingerprint`. The fingerprint is:

```text
SHA256(UTF8("htlk.executable.graph/0.1\n") || payload)
```

The prefix ends in exactly one LF byte; the raw payload follows it directly,
without a CBOR array or byte-string header. Payload bytes are never rewritten
before hashing. All current HTLK-owned format and identity version labels use
`0.1`. Only this envelope contract is supported: alternate format names, version
field aliases, and fingerprint formulas are rejected.

### Golden vectors

An empty payload has fingerprint
`sha256:ebbad418b6ddd9ead246e32dc337b19276b2c709701d2ad69a9992098f9fa14c`.
Payload `f6` has fingerprint
`sha256:9b039893d4db25c42f0765d6a31877987d90ff86f07ca593af128ca23500120d`.
Its complete 137-byte envelope is the following hex (concatenate lines):

```text
a466666f726d61747568746c6b2e65786563757461626c652e6772617068
677061796c6f616441f6
6776657273696f6e63302e31
6b66696e6765727072696e747847
7368613235363a39623033393839336434646232356334326630373635643661333138373739383764393066663836663037636135393361663132386361323335303031323064
```

### Validation and resource accounting

Decode first applies `htlk_cbor::decode` with the supplied `Limits`. Schema
errors then follow this precedence: non-record, unknown fields, missing fields,
wrong field types. Missing/type failures choose the first of `fingerprint`,
`format`, `payload`, `version` (decoded UTF-8 order). Format checking precedes
version checking, then fingerprint parsing, then fingerprint comparison.
Errors do not retain untrusted names, metadata values, or payload contents.
Wrapped codec errors preserve their offsets and remain accessible via Error's
`source`; fingerprint parsing errors likewise retain their structured cause.

Construction takes ownership of the payload and checks its bounds using the
codec before hashing, then verifies that the complete envelope encodes within
the supplied limits. The temporary payload encoding is discarded and never
used as the hash preimage. Encoding borrows the checked record. Decoding hashes
the payload directly after outer validation. A private incremental SHA-256
operation processes the prefix and payload without concatenating or copying
them. Codec encoding allocates bounded temporary output; fixed-size metadata
and hash state are additional overhead, not exact heap accounting.

Every codec operation has fresh accounting. No `RegistrationLimits` type or
shared registration allowance is introduced. A valid outer envelope can contain
malformed CBOR or an invalid graph;
payload interpretation and aggregate registration accounting belong to later
registration stages.

## Digest API

Import these items from `htlk_executable::digest`:

- `Digest`: exactly 32 bytes, with byte construction/access, equality, ordering,
  hashing, canonical Display/Debug, and strict FromStr parsing.
- `ParseDigestError`: invalid prefix, length, or hexadecimal text.
- `hash_bytes`: SHA-256 of exact supplied bytes.
- `hash_cbor`: bounded deterministic CBOR encoding followed by SHA-256.

```rust
use htlk_cbor::{Limits, Value};
use htlk_executable::digest::{Digest, hash_bytes, hash_cbor};

let digest = hash_bytes(b"abc");
assert_eq!(digest.to_string(),
    "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
assert_eq!(digest.to_string().parse::<Digest>()?, digest);
assert_eq!(Digest::from_bytes(*digest.as_bytes()), digest);

let value = Value::Text("abc".into());
let limits = Limits::default();
let encoded = htlk_cbor::encode(&value, &limits)?;
assert_eq!(hash_cbor(&value, &limits)?, hash_bytes(&encoded));
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Canonical text

A digest is `sha256:` followed by exactly 64 lowercase hexadecimal digits
(71 ASCII bytes total). Parsing checks the prefix, then byte length, then hex
digits. Uppercase, whitespace, other algorithm prefixes, and non-ASCII hex
lookalikes are rejected. Errors retain no input text. Display and parsing do not
allocate internally; `to_string()` allocates the caller's resulting string.

## Preimages and domain separation

These functions add no labels or versions implicitly. Consumers construct the
exact preimage required by their specification. For example, the runtime's
root-scope identity formula uses this canonical list:

```rust
use htlk_cbor::{Limits, Value};
use htlk_executable::digest::hash_cbor;

let preimage = Value::Array(vec![
    Value::Text("htlk.root_scope".into()),
    Value::Text("0.1".into()),
    Value::Text("example-run".into()),
]);
let root_scope_id = hash_cbor(&preimage, &Limits::default())?;
assert!(root_scope_id.to_string().starts_with("sha256:"));
# Ok::<(), htlk_cbor::Error>(())
```

Hashing `Value::Text("abc")` is different from hashing the raw UTF-8 bytes
`b"abc"`: the former includes the CBOR text header. Integers and floats also
remain distinct. Canonical map ordering makes construction order irrelevant.
The envelope API constructs its agreed preimage above. Artifact and other
runtime-identity preimages remain consumer responsibilities; digest functions
alone do not impose an executable schema or validation policy.

## Limits and trust

`hash_cbor` uses the existing codec limits and allocates a bounded temporary
encoding. Configuration, limit, and allocation errors propagate unchanged; an
unsuccessful encoding returns no digest. Each call starts fresh accounting.
`hash_bytes` borrows its input and uses fixed-size hash state; callers bound raw
input size and work. A valid parsed digest is a representation, not evidence of
matching content, authorization, or producer authenticity.

Licensed under the Apache License, Version 2.0. See `LICENSE`.
