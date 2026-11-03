//! The transport between a client and the server, as the engine sees it: messages in and out,
//! never blocking. `cadcraft-collab-net` implements it over WebSockets; tests use memory.

use crate::Result;
use crate::message::{ClientMsg, ServerMsg};

/// `Send` natively; nothing in the browser, where a WebSocket stays on its one thread.
#[cfg(not(target_arch = "wasm32"))]
pub trait MaybeSend: Send {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Send> MaybeSend for T {}
#[cfg(target_arch = "wasm32")]
pub trait MaybeSend {}
#[cfg(target_arch = "wasm32")]
impl<T> MaybeSend for T {}

pub trait Link: MaybeSend {
    /// Queue a message for the server.
    fn send(&mut self, msg: &ClientMsg) -> Result<()>;
    /// The next message from the server, if one has arrived.
    fn recv(&mut self) -> Option<Result<ServerMsg>>;
    /// Whether the connection is still up (or still opening).
    fn is_open(&self) -> bool;
}
