//! The transport between a client and the server, as the engine sees it: messages in and out,
//! never blocking. `cadcraft-collab-net` implements it over WebSockets; tests use memory.

use crate::Result;
use crate::message::{ClientMsg, ServerMsg};

pub trait Link: Send {
    /// Queue a message for the server.
    fn send(&mut self, msg: &ClientMsg) -> Result<()>;
    /// The next message from the server, if one has arrived.
    fn recv(&mut self) -> Option<Result<ServerMsg>>;
    /// Whether the connection is still up.
    fn is_open(&self) -> bool;
}
