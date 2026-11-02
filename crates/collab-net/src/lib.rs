//! The client side of the collaboration transport: a [`Link`] to a session server over a
//! WebSocket (`ws://`, or `wss://` through rustls with the web PKI roots). A background thread
//! moves binary messages between the socket and two channels, so the engine never blocks on the
//! network. The browser build has no link yet (P2): [`connect`] says so.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![forbid(unsafe_code)]

use cadcraft_collab::{Link, Result};

#[cfg(not(target_arch = "wasm32"))]
mod native;

/// Connect to the session at `url` (e.g. `wss://cad.example.com/s/office-plan`).
#[cfg(not(target_arch = "wasm32"))]
pub fn connect(url: &str) -> Result<Box<dyn Link>> {
    native::connect(url)
}

#[cfg(target_arch = "wasm32")]
pub fn connect(_url: &str) -> Result<Box<dyn Link>> {
    Err(cadcraft_collab::Error::Invalid("shared sessions aren't available in the browser yet".into()))
}
