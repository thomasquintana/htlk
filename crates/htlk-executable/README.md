# HTLK Executable

Shared executable-format foundations for the HTLK compiler and runtime.
Provides a checked version-0.1 executable envelope and SHA-256 digest primitives
in the public `digest` module. Envelope checks cover the outer schema, supported
format/version, and fingerprint; registration separately verifies graph contents
and producer trust.

Depends on `htlk-cbor` for deterministic encoding and RustCrypto's `sha2` for
SHA-256; the codec does not depend on this crate.

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
