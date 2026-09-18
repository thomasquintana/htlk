#![doc = include_str!("../README.md")]

/// Shared semantic analysis and structured compiler-facing diagnostics.
pub use htlk_analyzer as analyzer;
/// Canonical executable records and bounded wire construction.
pub use htlk_executable as executable;
