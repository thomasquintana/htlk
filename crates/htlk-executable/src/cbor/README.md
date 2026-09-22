# HTLK executable CBOR

Shared deterministic CBOR encoding and strict decoding for the HTLK compiler
and runtime. Provides validated values, bounded operations, reusable limits,
and structured errors with input offsets.

The adapter uses pinned `cbor2` primitives for integer/header and float decoding
and preferred scalar/header encoding into fixed nine-byte scratch buffers.
HTLK owns the restricted profile, canonical-ingress checks, traversal and resource
accounting. `Value`, `Map`, and `FiniteFloat` expose no generic Serde serialization;
use `encode` and `decode` at bounded canonical wire boundaries.

## HTLK 0.1 encoding profile

- RFC 8949 section 4.2.1 core deterministic encoding.
- Shortest integer and length headers; definite-length strings and collections.
- Exact UTF-8 string keys only, ordered lexicographically by encoded key bytes.
- Integers restricted to signed 64-bit values.
- Finite binary64 values encoded in the shortest exact binary16/32/64 width.
  Floats remain floats, including integral floats.
- Negative zero normalized at construction; canonical ingress rejects it.
- Text retains its exact Unicode spelling, with no normalization.
- Supported values: null, Boolean, integer, float, text, bytes, array, and map.
- No tags, undefined, absence sentinel, duplicate keys, or trailing bytes.

Examples of encoded bytes (hexadecimal):

| Value | Bytes |
|---|---|
| null | `f6` |
| integer 1 | `01` |
| float 1.0 | `f9 3c 00` |
| float 1.5 | `f9 3e 00` |
| float +0.0 | `f9 00 00` |
| map `{"aa": 2, "z": 1}` | `a2 61 7a 01 62 61 61 02` |

## Constructing values

```rust
use htlk_executable::cbor::{self, FiniteFloat, Limits, Map, Value};

let value = Value::Map(Map::try_from_entries([
    ("question".into(), Value::Text("What is HTLK?".into())),
    ("temperature".into(), Value::Float(FiniteFloat::new(0.5)?)),
])?);
let bytes = cbor::encode(&value, &Limits::default())?;
let decoded = cbor::decode(&bytes, &Limits::default())?;
assert_eq!(decoded, value);
assert!(matches!(value, Value::Map(_)));
# Ok::<(), cbor::Error>(())
```

Maps reject duplicates rather than overwriting entries. Their iteration and
equality are independent of construction order. `FiniteFloat` rejects nonfinite
values. Codec limits apply to complete operations, not to allocations a
caller already performed while assembling a value.

## Strict decoding

`decode(&bytes, &limits)` accepts exactly one canonical value and returns owned
data. It rejects trailing bytes, nonminimal headers and float widths, negative
zero, nonfinite floats, invalid UTF-8, non-string keys, duplicate/out-of-order
keys, indefinite lengths, tags, and unsupported simple values. It never sorts
or normalizes received bytes into compliance. Encoding a successfully decoded
value reproduces the original bytes under the same limits.

Input errors carry zero-based offsets: ordinarily the offending value/key
header, the first invalid UTF-8 byte for invalid text, or the first trailing
byte. Truncation points to the input length, the first missing byte. Collection
preflight errors point to the collection header; document-limit errors point
to zero. Invalid limit configuration has no input offset. Diagnostics contain
categories and positions rather than protected input contents.

`Error` Display is human-facing prose, for example, “CBOR input ended before the
value was complete at byte 3.” Canonicality messages explain nonminimal headers,
indefinite-length markers, unnecessary float/simple-value widths, and negative
zero. All remain `ErrorKind::NonCanonicalEncoding`. Error equality compares only
the category and offset, not private diagnostic details.

Use `kind()` and `offset()` for programmatic handling; do not parse Display text.
Applications choose prose or structured logging and own the log format. Display
and Debug contain no map keys, text values, byte payloads, or document excerpts.

```rust
use htlk_executable::cbor::{decode, ErrorKind, Limits};

let error = decode(&[0x81, 0x18], &Limits::default()).unwrap_err();
assert!(matches!(error.kind(), ErrorKind::UnexpectedEnd));
assert_eq!(error.offset(), Some(2));
```

A byte string remains opaque even if it contains CBOR. Registration must
explicitly decode its nested payload; outer canonicality says nothing about
the payload's canonicality.

## Resource accounting

`Limits` has two fields: `max_document_bytes` and `max_depth`. Defaults are
implementation policy, not wire-format options: 16 MiB per document and depth 64.
Document size includes headers; a byte string of exactly 16 MiB therefore
requires a higher document ceiling.

Root depth is zero. Array elements and both map keys and values have parent
depth plus one. All headers, text (including keys), and byte-string contents
count toward the complete document size. There are no separate value-count,
collection-size, string-size, or aggregate-payload ceilings. Zero ceilings
are valid restrictions. Default depth is 64; the current implementation ceiling
is 128. Invalid configuration is rejected before traversing the value.

The encoder checks lengths and aggregates with checked arithmetic before output
allocation, and checks depth before recursive descent. String headers and their
complete payloads are budgeted together before reservation. Output reservations
are fallible; requested buffer capacity never exceeds the document ceiling.
Allocators may provide more capacity than requested, so this is not exact heap
accounting. Traversal uses O(depth) stack space and constant-sized scalar scratch;
maps already have canonical order and need no sorting or collection copies.
Caller-owned values are borrowed. Only complete output is returned on success.

The decoder shares the same accounting rules. It checks advertised lengths,
minimum child counts, available input, and depth before collection traversal.
Collections grow incrementally only as children finish decoding; an advertised
length never causes an upfront reservation of the entire collection. Requested
collection capacity is at most twice the completed entries, capped by declared
length. Text/byte payloads are checked before copying, and UTF-8/order checks
precede owned map-key allocation. Every value requires at least one input byte,
so the byte ceiling also bounds the number of decoded values. This is not an
exact heap limit: value representations and allocator overhead can exceed their
encoded size. All reservations are fallible, and failed partial
values are dropped internally.

Registration will need shared accounting across envelope and nested payload decodes.

### Depth assessment

The default and implementation ceiling serve different purposes. Default 64 is
an ordinary acceptance policy; ceiling 128 bounds the current recursive traversal.
Subprocess-isolated regression tests exercise nested arrays and maps at depths
0, 64, 65, and 128 on explicitly requested 512 KiB and 2 MiB thread stacks,
including encoding, decoding, cloning, Debug formatting, equality, and destruction.
They also check rejection and destruction at depth 129. Decoder cleanup tests
retain completed subtrees at each active frame before a deepest-child failure,
and reject trailing bytes after a complete depth-128 value. Passing with a 512 KiB
stack provides headroom against the 2 MiB regression baseline; it is not a
measurement of a universal safe maximum or a guarantee for arbitrarily small
caller stacks. The tests run with the repository's normal test suite.

The encoder borrows its input and does not recursively destroy rejected values.
Callers can still construct values deeper than the codec ceiling; limits do not
make arbitrary operations on such caller-owned values stack-safe. Decoder
construction and error-cleanup paths passed this separate assessment at the
same ceiling. Changes to either traversal require rerunning these regressions.

### Float conversion

Binary16/32 conversion and preferred float serialization use `cbor2` 1.1.5
(MIT), with default features disabled. A narrower float is selected only when
the conversion is lossless. Tests independently calculate all finite binary16
patterns and cover binary32/binary64 precision boundaries and subnormals.
Decoder tests classify every binary16 bit pattern, including nonfinite values
and negative zero, and reject unnecessarily wide binary32/binary64 encodings.

## Consumer responsibilities

Executable record schemas, unknown-field rejection, graph verification, JSON
constraints, runtime type validation, authorization, and SHA-256 identities are
consumer responsibilities. Codec errors contain categories and optional byte
offsets, never input contents. Runtime error codes are assigned by the consumer
according to the operation being performed.
