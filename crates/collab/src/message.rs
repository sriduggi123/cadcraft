//! Wire messages between a client and the session server, and their binary encoding (postcard;
//! property values travel as JSON text inside, see [`crate::Json`]).

use serde::{Deserialize, Serialize};

use crate::{Change, Error, Result};

/// Largest message accepted: a welcome carries a whole snapshot.
pub const MAX_MESSAGE_BYTES: usize = crate::snapshot::MAX_BYTES + (1 << 20);

/// What a client sends.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ClientMsg {
    /// The first message: who is joining, and, from the person who starts the session, the
    /// drawing to start it with (a [`crate::snapshot`]); ignored once the session has one.
    /// `create`: this must be a new file; the server refuses if the name is taken.
    Hello { name: String, seed: Option<Vec<u8>>, create: bool },
    /// One command's changes. `batch` numbers the client's batches, so replies can name them.
    Batch { batch: u64, changes: Vec<Change> },
    /// On the server's file list (`/files`): ask for the list.
    List,
}

/// A file in the cloud: a saved or live session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileInfo {
    pub name: String,
    /// Last change, in seconds since 1970.
    pub modified: u64,
    /// Who has it open now.
    pub people: Vec<String>,
}

/// Someone in the session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Peer {
    pub client: u32,
    pub name: String,
}

/// What the server sends.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ServerMsg {
    /// The reply to `Hello`: the client's id (its handle space) and the drawing as of `seq`.
    Welcome { client: u32, seq: u64, snapshot: Vec<u8> },
    /// A batch the server accepted, numbered `seq`, sent to everyone (its author included).
    Accepted { seq: u64, client: u32, batch: u64, changes: Vec<Change> },
    /// A batch the server refused, sent to its author only; nobody else ever sees it.
    Rejected { batch: u64, reason: String },
    /// Who is in the session, after someone joins or leaves.
    Peers { peers: Vec<Peer> },
    /// The server can't go on with this client (it closes the link after this).
    Error { message: String },
    /// The reply to `List`, newest first.
    Files { files: Vec<FileInfo> },
}

/// Encode a message for the wire.
pub fn encode<T: Serialize>(msg: &T) -> Result<Vec<u8>> {
    postcard::to_allocvec(msg).map_err(|e| Error::Encode(e.to_string()))
}

/// Decode a message from the wire.
pub fn decode<'a, T: Deserialize<'a>>(bytes: &'a [u8]) -> Result<T> {
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(Error::TooLarge(MAX_MESSAGE_BYTES));
    }
    postcard::from_bytes(bytes).map_err(|e| Error::Decode(e.to_string()))
}
