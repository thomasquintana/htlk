#![doc = include_str!("README.md")]
#![forbid(unsafe_code)]

mod accounting;
mod decode;
mod encode;
mod error;
mod limits;
mod value;

pub use decode::decode;
pub use encode::encode;
pub use error::{Error, ErrorKind, LimitKind};
pub use limits::Limits;
pub use value::{FiniteFloat, Map, Value};
