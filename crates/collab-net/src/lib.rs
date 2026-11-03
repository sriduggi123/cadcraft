//! The client side of the collaboration transport: a [`Link`] to a session server over a
//! WebSocket (`ws://`, or `wss://` through rustls with the web PKI roots). A background thread
//! moves binary messages between the socket and two channels, so the engine never blocks on the
//! network. In the browser the page's own WebSocket does the same through callbacks (`web.rs`).
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
mod web;

#[cfg(target_arch = "wasm32")]
pub fn connect(url: &str) -> Result<Box<dyn Link>> {
    web::connect(url)
}
