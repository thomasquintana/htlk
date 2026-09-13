# HTLK CBOR

Shared deterministic CBOR encoding and strict decoding for the HTLK compiler
and runtime. Provides validated values, bounded operations, reusable limits,
and structured errors with input offsets.

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
use htlk_cbor::{FiniteFloat, Limits, Map, Value};

let value = Value::Map(Map::try_from_entries([
    ("question".into(), Value::Text("What is HTLK?".into())),
    ("temperature".into(), Value::Float(FiniteFloat::new(0.5)?)),
])?);
let bytes = htlk_cbor::encode(&value, &Limits::default())?;
let decoded = htlk_cbor::decode(&bytes, &Limits::default())?;
assert_eq!(decoded, value);
assert!(matches!(value, Value::Map(_)));
# Ok::<(), htlk_cbor::Error>(())
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

A byte string remains opaque even if it contains CBOR. Registration must
explicitly decode its nested payload; outer canonicality says nothing about
the payload's canonicality.

## Resource accounting

Defaults are implementation policy, not wire-format options: 16 MiB per
document, 1 MiB per text string, 16 MiB per byte string, depth 64, 100,000 entries
per collection, 1,000,000 total values, and 16 MiB aggregate payload bytes.
Document size includes headers; a byte string of exactly 16 MiB therefore
requires a higher document ceiling.

Root depth is zero. Array elements and both map keys and values have parent
depth plus one. Containers, scalars, and map keys each count as values. Payload
bytes count all text (including keys) and byte-string contents. Zero ceilings
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
precede owned map-key allocation. This bounds requested storage by aggregate
value counts, payload bytes, and recursion depth; allocator overhead is outside
these logical counters. All reservations are fallible, and failed partial
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

Binary16 conversion uses `half` 2.7.1 (MIT OR Apache-2.0), with default features
disabled and explicit software conversion methods. A narrower float is selected
only if converting it back to binary64 preserves the exact bits. Binary32 uses
the same round-trip check. Tests independently calculate all finite binary16
patterns and cover binary32/binary64 precision boundaries and subnormals.
Decoder tests classify every binary16 bit pattern, including nonfinite values
and negative zero, and reject unnecessarily wide binary32/binary64 encodings.

## Consumer responsibilities

Executable record schemas, unknown-field rejection, graph verification, JSON
constraints, runtime type validation, authorization, and SHA-256 identities are
consumer responsibilities. Codec errors contain categories and optional byte
offsets, never input contents. Runtime error codes are assigned by the consumer
according to the operation being performed.
