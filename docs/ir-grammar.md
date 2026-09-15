# HTLK IR grammar

The authoritative Draft 0.1 language uses `.htlk` source files and typed,
declarative executable graphs:

- [Specification index](../specs/README.md)
- [Grammar and execution model](../specs/htlk-grammar-spec.md)
- [IR syntax reference](../specs/htlk-ir-syntax-reference.md)
- [Compiler specification](../specs/compiler-spec.md)
- [Runtime specification](../specs/runtime-spec.md)

`when`, `preconditions`, `postconditions`, loop termination, and pure calculations
use the same restricted expression model implemented in Rust. Prompt rendering
uses explicit canonical template parts and named slots. There are no embedded
script blocks or mutable shared scripting contexts.
