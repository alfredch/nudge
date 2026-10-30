//! Client for the Pushover Open Client API.
//!
//! This is an unofficial client; it is not released or supported by Pushover.
//! API reference: <https://pushover.net/api/client>.
//!
//! The crate contains no retry, backoff or reconnect logic. Callers decide
//! when to retry, which keeps this layer deterministic and easy to test.

mod client;
mod error;
mod stream;
mod types;

pub use client::{Client, DEFAULT_API_BASE, Login, user_agent};
pub use error::{Error, Result};
pub use stream::{DEFAULT_PUSH_URL, Frame, PushStream};
pub use types::{
    DeviceId, DeviceName, Fetched, Malformed, Message, MessageId, Receipt, ResourceName, Secret,
};
