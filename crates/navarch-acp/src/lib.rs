//! ACP adapter boundary. Public APIs contain only Navarch and standard Rust types.
#![forbid(unsafe_code)]
mod driver;
mod mapping;
mod types;
pub use driver::AcpDriver;
pub use types::*;
#[cfg(feature = "testing")]
mod fake;
#[cfg(feature = "testing")]
pub use fake::{FakeDriver, FakeStep};
