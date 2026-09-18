# HTLK Executable

The shared canonical executable model for HTLK. This crate owns bounded
construction, normalization, representation invariants, canonical CBOR/JSON,
content digests and executable envelopes.

Semantic analysis lives in **htlk-analyzer**. Host policy/implementation admission,
native registries, evaluation and actual-value enforcement live in **htlk-runtime**.
The model depends on neither crate.

## Construction and validation boundaries

- `Identifier`, `ValueType`, `Port`, `Expression` and `PromptTemplate` provide
  immutable normalized declarations with read-only views.
- `Scope`, `Node`, `Edge`, `PortTable` and `Operation` preserve canonical graph
  records, including closed shapes, positive bounds, unique IDs and array order.
- `ExecutionProfile`, `Library`, `FunctionSignature`, `ServerIdentity` and
  `McpBinding` describe pinned declarations rather than loading implementations.
- `CanonicalDocument` checks complete bounded representation, asserted record
  identities and retrieval-URI spelling. It does not resolve references, infer
  types, enforce use roles or compute a structural summary.
- `JsonDocument` implements bounded canonical external JSON; `PolicyDocument`
  validates the policy record's shape and numeric bounds without authorizing it.
- `ExecutableEnvelope` checks format/version, opaque payload and fingerprint.
  Its nested payload must separately pass canonical document decoding.

Consequently, a representable document with an unknown root, undeclared endpoint
or invalid contextual reference can reach the analyzer and receive a semantic
diagnostic. Existing context-taking model APIs retain their signatures, but their
context argument does not establish semantic validity. The analyzer checks every
actual ordinary/loop-body and expression use context.

## Canonical CBOR

The public `cbor` module replaces the standalone codec package. Its adapter uses
pinned **cbor2 1.1.5** primitives and Serde while retaining HTLK's restricted
deterministic profile, resource accounting, fallible allocation and strict ingress.
It rejects nonminimal encodings, negative zero, nonfinite floats, duplicate or
out-of-order keys, nontext keys, tags, indefinite lengths and trailing data.

```rust
use htlk_executable::cbor::{self, FiniteFloat, Limits, Map, Value};

let value = Value::Map(Map::try_from_entries([
    ("answer".into(), Value::Integer(42)),
    ("confidence".into(), Value::Float(FiniteFloat::new(1.0)?)),
])?);
let limits = Limits::default();
let bytes = cbor::encode(&value, &limits)?;
assert_eq!(cbor::decode(&bytes, &limits)?, value);
# Ok::<(), cbor::Error>(())
```

Integers and floats remain distinct, including integral floats. Unicode spelling
is preserved exactly. Limits count containers and map keys as values and include
all text/byte payloads. The default depth is 64 and implementation ceiling 128;
controlled-stack tests cover construction, encoding, decoding and cleanup.

## Serde

Canonical records implement `serde::Serialize` through explicit wire mappings,
rather than Rust's default enum tags. Byte strings remain bytes, expressions keep
their canonical tagged arrays, and record maps retain canonical key order.
Record serialization prepares a wire view under default codec limits. For custom
ceilings, call `to_value` with those limits and serialize the resulting `cbor::Value`.
Generic serializers do not themselves establish canonical JSON or CBOR validity.
Use the bounded `encode`/`decode` and `JsonDocument` APIs at canonical boundaries.

```rust
use htlk_executable::{Expression, ScalarLiteral, ExpressionContext, cbor::Limits};

let expression = Expression::literal(ScalarLiteral::Integer(42));
assert_eq!(serde_json::to_string(&expression)?, r#"["literal",42]"#);
let bytes = expression.encode(ExpressionContext::Eval, &Limits::default())?;
assert_eq!(Expression::decode(&bytes, ExpressionContext::Eval, &Limits::default())?, expression);
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Digests and envelopes

`digest::hash_bytes` hashes exact bytes; `hash_cbor` hashes bounded canonical CBOR.
`record_digest` uses the closed record-domain prefixes. A `Digest` is a typed
32-byte identity with strict lowercase `sha256:` text. These are representation
and hashing APIs, not authorization or proof of matching external content.

```rust
use htlk_executable::{ExecutableEnvelope, cbor::Limits};

let limits = Limits::default();
let envelope = ExecutableEnvelope::new(vec![0xa0], &limits)?;
let encoded = envelope.encode(&limits)?;
assert_eq!(ExecutableEnvelope::decode(&encoded, &limits)?, envelope);
# Ok::<(), htlk_executable::EnvelopeError>(())
```

The repository's `docs/executable-api.md` contains the detailed cross-crate API
reference, including canonical field shapes, domains, resource accounting,
schema semantics and runtime contracts. Its Rust examples are doctested by the facade.

Licensed under the Apache License, Version 2.0.
