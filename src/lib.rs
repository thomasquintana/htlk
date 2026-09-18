#![doc = include_str!("../README.md")]

/// Compiler functionality.
pub use htlk_compiler as compiler;
/// Runtime functionality.
pub use htlk_rt as rt;

// Keep the cross-crate reference examples checked without creating a runtime API.
#[cfg(doctest)]
#[doc = include_str!("../docs/executable-api.md")]
mod executable_reference {}
