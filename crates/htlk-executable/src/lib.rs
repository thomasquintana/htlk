#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

pub mod digest;

mod envelope;

pub use envelope::{EXECUTABLE_FORMAT, EXECUTABLE_VERSION, EnvelopeError, ExecutableEnvelope};
